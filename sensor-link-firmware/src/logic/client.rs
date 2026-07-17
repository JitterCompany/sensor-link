//! Generic protocol client for sensor devices.
use core::str::FromStr;

use heapless::String;
use serde::Serialize;

use crate::{
    logic::{
        network::{NetworkClient, ReadOutcome},
        signal::Signal,
        NetworkStatus, SendChannel,
    },
    meta::DeviceMetaDataProvider,
    mqtt::{Event, FileError, Message, MqttClient, Will},
    serialize::{internal::Internal, Sendable},
};
use sensor_link_protocol::{
    cmd,
    fwupdate::{self, FWChunk, FWStatus, FWUpdateURL, B64_ENCODED_CHUNK_SIZE, FW_CHUNK_SIZE},
    info::DeviceInfoV2,
    online::Online,
    parse_json_payload, parse_system_topic, parse_topic_to_device,
    server_status::ServerStatus,
    time, Error, Milliseconds, SystemTopic, Topic, TopicFromDevice, TopicPayloadSerialize,
    TopicSerializeError, TopicToDevice, MAX_TOPIC_LEN,
};

/// Events emitted by [Client].
#[derive(Debug)]
pub enum ClientEvent {
    CommandReceived(cmd::Cmd),
    TimestampReceived(i64),
    FWChunkReceived(fwupdate::FWChunk),
    FWDownloadComplete,
    FWDownloadFailed,
    FirmwareUpdateAnnounced,
    Disconnected,
    ServerStatus(ServerStatus),
}

enum State {
    Normal,
    FirmwareUpdate,
}

pub type UIDString = String<{ sensor_link_protocol::MAX_UID_LEN }>;

/// Generic protocol client for sensor devices.
///
/// `S` is the device-status payload this client publishes on the `status` topic
/// (surfaced as [`NetworkClient::Status`]). It is a type parameter rather than a
/// stored field: the status value is passed to [`send_status`](NetworkClient::send_status)
/// per call, so the client only needs to name the type, not own a value of it.
pub struct Client<M: MqttClient, S> {
    pub driver: M,
    pub uid: UIDString,
    state: State,
    update_url: Option<FWUpdateURL>,
    update_seq_num: u16,
    _status: core::marker::PhantomData<fn() -> S>,
}

impl<M: MqttClient, S> Client<M, S> {
    pub fn new(driver: M, uid: UIDString) -> Self {
        Self {
            driver,
            uid,
            state: State::Normal,
            update_url: None,
            update_seq_num: 0,
            _status: core::marker::PhantomData,
        }
    }

    /// Whether the client is currently streaming firmware update chunks.
    pub fn is_firmware_update(&self) -> bool {
        matches!(self.state, State::FirmwareUpdate)
    }

    fn topic_name_sub(
        &self,
        topic: TopicToDevice,
    ) -> Result<String<MAX_TOPIC_LEN>, TopicSerializeError> {
        topic.to_topic_string(&self.uid)
    }

    fn topic_name_pub(
        &self,
        topic: TopicFromDevice,
    ) -> Result<String<MAX_TOPIC_LEN>, TopicSerializeError> {
        topic.to_topic_string(&self.uid)
    }

    /// Connect, subscribe to the manufacturer-generic topics, and publish Online status.
    ///
    /// Product-specific wrappers should call this and then subscribe to their own topics.
    pub async fn connect(
        &mut self,
        timestamp_ms: Milliseconds,
    ) -> Result<(), Error<M::ClientError>> {
        let will_payload = Online::will().serialize_topic_payload().unwrap_or_default();

        let will = Will {
            topic: self.topic_name_pub(TopicFromDevice::Online)?,
            payload: String::from_utf8(will_payload).unwrap_or_default(),
        };

        self.driver.connect(&self.uid, will).await?;
        let _ = self.subscribe_server_status().await;

        self.subscribe_commands().await?;
        self.subscribe_time().await?;
        self.subscribe_firmware_updates().await?;
        self.send_online(timestamp_ms).await?;
        Ok(())
    }

    pub async fn subscribe_server_status(&mut self) -> Result<(), Error<M::ClientError>> {
        if let Ok(topic) = String::from_str(SystemTopic::ServerStatus.as_str()) {
            self.driver.subscribe(topic).await
        } else {
            Err(Error::SerializeTopic(TopicSerializeError::TotalLength))
        }
    }

    pub async fn disconnect(&mut self) -> Result<(), Error<M::ClientError>> {
        let _ = self.send_offline().await;
        self.driver.disconnect().await
    }

    /// Begin downloading an announced firmware update and start streaming its chunks via
    /// [handle_response].
    ///
    /// The caller is responsible for unsubscribing from all product-specific topics first
    /// (this only handles the manufacturer-generic protocol steps: status publishing + download).
    pub async fn download_firmware_update(&mut self) -> Result<(), ()> {
        if let Err(err) = self.send_firmware_update_status(FWStatus::Start).await {
            log::warn!("Failed to send firmware update status: {err:?}");
        }
        if let Some(url) = &self.update_url {
            match self.driver.download_file(url).await {
                Ok(_) => {
                    self.update_seq_num = 0;
                    self.state = State::FirmwareUpdate;
                }
                Err(_) => {
                    log::error!("Failed to download file");
                    if let Err(err) = self.send_firmware_update_status(FWStatus::Failed).await {
                        log::warn!("Failed to send firmware update status: {err:?}");
                        return Err(());
                    }
                }
            }
            if let Err(err) = self.send_firmware_update_status(FWStatus::Received).await {
                log::warn!("Failed to send firmware update status: {err:?}");
            }
            Ok(())
        } else {
            Err(())
        }
    }

    /// Await internal driver response or timeout.
    /// This call may be selected.
    pub async fn await_response(&mut self, timeout: u32) -> Option<()> {
        match self.state {
            State::Normal => self.driver.await_response(timeout).await.ok(),
            State::FirmwareUpdate => Some(()),
        }
    }

    /// After receiving a response from [await_response](method@Self::await_response), call this to
    /// handle the response.
    /// This call must not be used in select!
    pub async fn handle_response(&mut self) -> Option<ClientEvent> {
        match self.state {
            State::Normal => self.handle_normal_state().await,
            State::FirmwareUpdate => self.handle_update_state().await,
        }
    }

    async fn handle_update_state(&mut self) -> Option<ClientEvent> {
        match self.driver.read_file_chunk(B64_ENCODED_CHUNK_SIZE).await {
            Ok(chunk) => {
                let mut bytes = [0u8; FW_CHUNK_SIZE];
                if let Ok(_res) = fwupdate::decode_base64(&chunk, &mut bytes) {
                    let sequence_no = self.update_seq_num;
                    self.update_seq_num += 1;
                    Some(ClientEvent::FWChunkReceived(FWChunk { sequence_no, bytes }))
                } else {
                    log::error!("Failed to decode base64 chunk");
                    None
                }
            }
            Err(FileError::ReadError) => {
                self.state = State::Normal;
                Some(ClientEvent::FWDownloadFailed)
            }
            Err(FileError::EndOfFile) => {
                log::debug!("End of file");
                self.state = State::Normal;
                if let Err(err) = self.send_firmware_update_status(FWStatus::Complete).await {
                    log::warn!("Failed to send firmware update status: {err:?}");
                }
                Some(ClientEvent::FWDownloadComplete)
            }
            Err(FileError::FileNotFound) => {
                self.state = State::Normal;
                Some(ClientEvent::FWDownloadFailed)
            }
        }
    }

    async fn handle_normal_state(&mut self) -> Option<ClientEvent> {
        match self.driver.handle_response().await {
            Ok(Event::ReceivedMessage(Message { topic, payload })) => {
                if let Some(SystemTopic::ServerStatus) = parse_system_topic(topic.as_str()) {
                    if let Ok(status) = ServerStatus::from_slice(&payload) {
                        return Some(ClientEvent::ServerStatus(status));
                    }
                }

                let topic = match parse_topic_to_device(topic.as_str()) {
                    Ok(topic_parts) => topic_parts.topic,
                    Err(parse_error) => {
                        log::warn!("Ignored message on topic {topic:?}: {parse_error:?}");
                        return None;
                    }
                };

                self.handle_common_topic(topic, &payload)
            }
            Ok(Event::Disconnected) => Some(ClientEvent::Disconnected),
            Err(_err) => Some(ClientEvent::Disconnected),
        }
    }

    /// Handle an already-parsed generic [TopicToDevice] message, producing the corresponding
    /// [ClientEvent] (if any).
    ///
    /// Exposed so that product-specific wrapping clients can delegate their `Common(...)`
    /// topic-variant arms here, after parsing the raw MQTT message with their own union parser.
    pub fn handle_common_topic(
        &mut self,
        topic: TopicToDevice,
        payload: &[u8],
    ) -> Option<ClientEvent> {
        match topic {
            TopicToDevice::Command => parse_json_payload::<cmd::CommandPayload>(payload)
                .map(|c| ClientEvent::CommandReceived(c.cmd)),
            TopicToDevice::Time => parse_json_payload::<time::Timestamp>(payload)
                .map(|t| ClientEvent::TimestampReceived(t.time)),
            TopicToDevice::FWUpdateAnnounce => {
                if let Some(announcement) = parse_json_payload::<fwupdate::FWAnnounce>(payload) {
                    self.update_url = Some(announcement.url);
                    Some(ClientEvent::FirmwareUpdateAnnounced)
                } else {
                    None
                }
            }
        }
    }

    pub async fn send_online(&mut self, since: Milliseconds) -> Result<(), Error<M::ClientError>> {
        let online = Online::yes(since);
        let s = online
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        let topic = self.topic_name_pub(TopicFromDevice::Online)?;
        self.driver.publish(topic, &s).await
    }

    pub async fn send_offline(&mut self) -> Result<(), Error<M::ClientError>> {
        let offline = Online::no();
        let s = offline
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        let topic = self.topic_name_pub(TopicFromDevice::Online)?;
        self.driver.publish(topic, &s).await
    }

    pub async fn send_info<D: Serialize>(
        &mut self,
        info: &DeviceInfoV2<D>,
    ) -> Result<(), Error<M::ClientError>> {
        let s = info
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        let topic = self.topic_name_pub(TopicFromDevice::DeviceInfoV2)?;
        self.driver.publish(topic, &s).await
    }

    pub async fn send_sendable<T: Topic>(
        &mut self,
        sendable: &dyn Sendable<T>,
    ) -> Result<(), Error<M::ClientError>> {
        let topic = sendable
            .topic()
            .map_err(|_| TopicSerializeError::Other)
            .and_then(|t| t.to_topic_string(&self.uid))?;

        self.driver
            .publish(topic, sendable.payload_bytes(Internal))
            .await
    }

    async fn send_firmware_update_status(
        &mut self,
        status: FWStatus,
    ) -> Result<(), Error<M::ClientError>> {
        let s = status
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        let topic = self.topic_name_pub(TopicFromDevice::FWStatus)?;
        self.driver.publish(topic, &s).await
    }

    async fn subscribe_commands(&mut self) -> Result<(), Error<M::ClientError>> {
        let topic = self.topic_name_sub(TopicToDevice::Command)?;
        self.driver.subscribe(topic).await
    }

    async fn subscribe_time(&mut self) -> Result<(), Error<M::ClientError>> {
        let topic = self.topic_name_sub(TopicToDevice::Time)?;
        self.driver.subscribe(topic).await
    }

    async fn subscribe_firmware_updates(&mut self) -> Result<(), Error<M::ClientError>> {
        self.driver
            .subscribe(self.topic_name_sub(TopicToDevice::FWUpdateAnnounce)?)
            .await
    }
}

/// [`NetworkClient`] implementation for the generic protocol [`Client`].
///
/// Drives the network task with the manufacturer-generic `jitter-sensor-link`
/// client: the common protocol surface (commands, time sync, firmware updates,
/// generic [`Sendable`] uploads). Product wrappers that need extra topics
/// implement [`NetworkClient`] on their own client type.
///
/// This makes the generic [`Client`] usable as a `NetworkClient` on its own, so
/// applications can drive [`network_task`] directly on the generic protocol
/// client (plus Sendable uploads) without any product-specific layer.
///
/// [`Client`]: crate::logic::client::Client
impl<M: MqttClient, S: NetworkStatus> NetworkClient for crate::logic::client::Client<M, S> {
    type ClientError = M::ClientError;
    type Topic = sensor_link_protocol::TopicFromDevice;
    type Signal = Signal;
    type Status = S;

    async fn connect(
        &mut self,
        timestamp_ms: Milliseconds,
    ) -> Result<(), Error<Self::ClientError>> {
        self.connect(timestamp_ms).await
    }

    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        self.disconnect().await
    }

    async fn await_response(&mut self, timeout_s: u32) -> Option<()> {
        self.await_response(timeout_s).await
    }

    async fn download_firmware_update(&mut self) -> Result<(), ()> {
        self.download_firmware_update().await
    }

    async fn send_sendable(
        &mut self,
        sendable: &dyn Sendable<Self::Topic>,
    ) -> Result<(), Error<Self::ClientError>> {
        self.send_sendable(sendable).await
    }

    async fn handle_read<SignalTx: SendChannel<Self::Signal>>(
        &mut self,
        signal_queue: &mut SignalTx,
    ) -> ReadOutcome {
        let Some(event) = self.handle_response().await else {
            return ReadOutcome::Continue;
        };
        match Signal::try_from(event) {
            Ok(signal) => {
                signal_queue.send(signal.into()).await.ok();
                ReadOutcome::Continue
            }
            Err(outcome) => outcome,
        }
    }

    async fn send_device_info<P>(&mut self, provider: &P) -> Result<(), Error<Self::ClientError>>
    where
        P: DeviceMetaDataProvider,
        P::DeviceType: Serialize,
    {
        // Inject the modem model and firmware version, which only the driver can
        // sample, then publish the collected metadata.
        let modem_model = self.driver.modem_model().await;
        let modem_fw_version = self.driver.modem_fw_version().await;
        let info = provider.device_info(modem_model, modem_fw_version);
        let result = self.send_info(&info).await;
        if result.is_err() {
            log::error!("Network: Failed to send Device Info");
        }
        result
    }

    async fn send_status(
        &mut self,
        status: &mut Self::Status,
    ) -> Result<(), Error<Self::ClientError>> {
        // Inject the live modem signal strength (only the driver can sample it),
        // then serialize the status as JSON and publish it.
        status.set_signal_strength(self.driver.signal_quality().await.unwrap_or_default() as i32);
        let topic =
            sensor_link_protocol::TopicFromDevice::Status.to_topic_string(self.uid.as_str())?;
        let s = status
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        self.driver.publish(topic, &s).await
    }
}
