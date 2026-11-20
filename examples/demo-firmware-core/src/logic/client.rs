//! Demo protocol client: the generic sensor-link client plus the Settings topic.
use heapless::String;
use sensor_link_firmware::{sensor_link_protocol::event::CellInfo, serialize::Sendable};
use serde::Serialize;

use demo_protocol::{
    parse_json_payload, parse_system_topic, parse_topic_to_device, server_status::ServerStatus,
    settings, status::DeviceStatus, CommonTopicFromDevice, Error, EventPayload, Milliseconds,
    SystemTopic, TestPayload, TopicFromDevice, TopicPayloadSerialize, TopicSerializeError,
    TopicToDevice, MAX_EVENT_LEN, MAX_MESSAGE_LEN, MAX_TOPIC_LEN,
};

pub use sensor_link_firmware::{
    logic::client::ClientEvent as CommonClientEvent,
    mqtt::{Event, Message, MqttClient},
};

use crate::{
    logic::{
        network::{FirmwareDownloadStart, NetworkClient, ReadOutcome},
        signal::{CommonSignal, Signal},
        NetworkStatus, SendChannel,
    },
    meta::DeviceMetaDataProvider,
};

/// Events that can be returned by the client.
// No alloc in firmware, so the large variants cannot be boxed
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ClientEvent {
    /// A manufacturer-generic protocol event (commands, time sync, firmware update, etc.).
    Common(CommonClientEvent),
    SettingsReceived(settings::Settings),
}

pub type UIDString = sensor_link_firmware::logic::client::UIDString;

/// Demo protocol client for sensor devices.
pub struct Client<M: MqttClient> {
    pub inner: sensor_link_firmware::logic::client::Client<M, DeviceStatusUpload>,
}

impl<M: MqttClient> Client<M> {
    pub fn new(driver: M, uid: UIDString) -> Self {
        Self {
            inner: sensor_link_firmware::logic::client::Client::new(driver, uid),
        }
    }

    fn topic_name_sub(
        &self,
        topic: TopicToDevice,
    ) -> Result<String<MAX_TOPIC_LEN>, TopicSerializeError> {
        topic.to_topic_string(&self.inner.uid)
    }

    fn topic_name_pub(
        &self,
        topic: TopicFromDevice,
    ) -> Result<String<MAX_TOPIC_LEN>, TopicSerializeError> {
        topic.to_topic_string(&self.inner.uid)
    }

    /// After receiving a response from `await_response`, call this to
    /// handle the response.
    /// This call must not be used in select!
    pub async fn handle_response(&mut self) -> Option<ClientEvent> {
        if self.inner.is_firmware_update() {
            return self.inner.handle_response().await.map(ClientEvent::Common);
        }
        self.handle_normal_state().await
    }

    async fn handle_normal_state(&mut self) -> Option<ClientEvent> {
        match self.inner.driver.handle_response().await {
            Ok(Event::ReceivedMessage(Message { topic, payload })) => {
                if let Some(SystemTopic::ServerStatus) = parse_system_topic(topic.as_str()) {
                    if let Ok(status) = ServerStatus::from_slice(&payload) {
                        return Some(ClientEvent::Common(CommonClientEvent::ServerStatus(status)));
                    }
                }

                let topic = match parse_topic_to_device(topic.as_str()) {
                    Ok(topic_parts) => topic_parts.topic,
                    Err(parse_error) => {
                        log::warn!("Ignored message on topic {topic:?}: {parse_error:?}");
                        return None;
                    }
                };

                match topic {
                    TopicToDevice::Settings => parse_json_payload::<settings::Settings>(&payload)
                        .map(ClientEvent::SettingsReceived),
                    TopicToDevice::Common(inner_topic) => self
                        .inner
                        .handle_common_topic(inner_topic, &payload)
                        .map(ClientEvent::Common),
                }
            }
            Ok(Event::Disconnected) | Err(_) => {
                Some(ClientEvent::Common(CommonClientEvent::Disconnected))
            }
        }
    }

    async fn subscribe_all(&mut self) -> Result<(), Error<M::ClientError>> {
        for topic in TopicToDevice::iter() {
            // If topic_str fails to serialize, we can't be subscribed so just skip ahead
            // to give the other topics a chance to unsubscribe
            let Ok(topic_str) = self.topic_name_sub(topic) else {
                continue;
            };
            self.inner.driver.subscribe(topic_str).await?;
        }
        Ok(())
    }

    async fn unsubscribe_all(&mut self) -> Result<(), Error<M::ClientError>> {
        for topic in TopicToDevice::iter() {
            // If topic_str fails to serialize, we can't be subscribed so just skip ahead
            // to give the other topics a chance to unsubscribe
            let Ok(topic_str) = self.topic_name_sub(topic) else {
                continue;
            };
            self.inner.driver.unsubscribe(topic_str).await?;
        }
        Ok(())
    }
}

/// Protocol-client implementation of [`NetworkClient`].
///
/// Co-located with the struct so the trait methods are the implementation: they
/// reach `self.inner` and the private `subscribe_*`/`unsubscribe_all` helpers
/// directly, instead of forwarding to a parallel set of inherent methods. The
/// client-specific dispatch (config/settings events)
/// lives here; the manufacturer-generic parts funnel through the shared helpers
/// in [`network`](crate::logic::network).
impl<M: MqttClient> NetworkClient for Client<M> {
    type ClientError = M::ClientError;
    type Topic = TopicFromDevice;
    type Signal = Signal;
    type Status = DeviceStatusUpload;

    async fn connect(
        &mut self,
        timestamp_ms: Milliseconds,
    ) -> Result<(), Error<Self::ClientError>> {
        self.inner.connect(timestamp_ms).await?;
        let _ = self.subscribe_all().await;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        self.inner.disconnect().await
    }

    async fn await_response(&mut self, timeout_s: u32) -> Option<()> {
        self.inner.await_response(timeout_s).await
    }

    async fn download_firmware_update(
        &mut self,
    ) -> Result<FirmwareDownloadStart, Error<Self::ClientError>> {
        let _ = self.unsubscribe_all().await;
        self.inner.download_firmware_update().await
    }

    async fn send_sendable(
        &mut self,
        sendable: &dyn Sendable<TopicFromDevice>,
    ) -> Result<(), Error<Self::ClientError>> {
        self.inner.send_sendable(sendable).await
    }

    async fn handle_read<SignalTx: SendChannel<Self::Signal>>(
        &mut self,
        signal_queue: &mut SignalTx,
    ) -> ReadOutcome {
        let Some(event) = self.handle_response().await else {
            return ReadOutcome::Continue;
        };
        match event {
            // Manufacturer-generic events: shared with the generic client.
            ClientEvent::Common(common) => match CommonSignal::try_from(common) {
                Ok(signal) => {
                    signal_queue.send(signal.into()).await.ok();
                    ReadOutcome::Continue
                }
                Err(outcome) => outcome,
            },

            ClientEvent::SettingsReceived(settings) => {
                signal_queue.send(Signal::Settings(settings)).await.ok();
                ReadOutcome::Continue
            }
        }
    }

    async fn send_device_info<P>(&mut self, provider: &P) -> Result<(), Error<Self::ClientError>>
    where
        P: DeviceMetaDataProvider,
        P::DeviceType: Serialize,
    {
        // Manufacturer-generic: delegate to the wrapped generic client's impl.
        self.inner.send_device_info(provider).await
    }

    async fn send_status(
        &mut self,
        status: &mut Self::Status,
    ) -> Result<(), Error<Self::ClientError>> {
        // Manufacturer-generic: delegate to the wrapped generic client's impl.
        self.inner.send_status(status).await
    }

    async fn cell_info(&mut self) -> Option<CellInfo> {
        self.inner.cell_info().await
    }
}

/// Network-status upload payload: pins the generic network-status
/// plumbing to the demo-protocol [`DeviceStatus`].
///
/// A newtype is required because both [`NetworkStatus`] (sensor-link-firmware) and
/// [`DeviceStatus`] (demo-protocol) are foreign to this crate, so the trait
/// cannot be implemented for `DeviceStatus` directly here. `#[serde(transparent)]`
/// keeps the wire format identical to `DeviceStatus`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct DeviceStatusUpload(pub DeviceStatus);

impl core::ops::Deref for DeviceStatusUpload {
    type Target = DeviceStatus;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for DeviceStatusUpload {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl NetworkStatus for DeviceStatusUpload {
    fn set_signal_strength(&mut self, dbm: i32) {
        self.0.signal_strength = dbm;
    }

    /// Constant: nothing waits for a specific status to go out
    fn confirmation_token(&self) -> u64 {
        0
    }
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for DeviceStatusUpload {}

impl<M: MqttClient> Client<M> {
    /// Publish an event payload on the common `Event` topic.
    pub async fn send_event(&mut self, event: &EventPayload) -> Result<(), Error<M::ClientError>> {
        let topic = self.topic_name_pub(TopicFromDevice::Common(CommonTopicFromDevice::Event))?;
        let s = event
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        self.inner.driver.publish(topic, &s).await
    }

    /// Publish a benchmark event on the common `BenchmarkEvent` topic.
    pub async fn send_benchmark_event<T: TopicPayloadSerialize<MAX_EVENT_LEN>>(
        &mut self,
        event: &T,
    ) -> Result<(), Error<M::ClientError>> {
        let topic = self.topic_name_pub(TopicFromDevice::Common(
            CommonTopicFromDevice::BenchmarkEvent,
        ))?;
        let s = event
            .serialize_topic_payload()
            .map_err(|_| Error::Serialize)?;
        self.inner.driver.publish(topic, &s).await
    }

    /// Publish synthetic benchmark data on the common `BenchmarkData` topic.
    pub async fn send_test_payload(&mut self, timestamp: i64) -> Result<(), Error<M::ClientError>> {
        let topic = self.topic_name_pub(TopicFromDevice::Common(
            CommonTopicFromDevice::BenchmarkData,
        ))?;

        let mut values = heapless::Vec::new();
        values.extend_from_slice(&[1.; 2]).ok();
        for val in values.iter_mut() {
            *val *= timestamp as f32;
        }
        let payload = TestPayload {
            ts: timestamp,
            values,
        };

        let mut buf = [0u8; MAX_MESSAGE_LEN];
        match serde_json_core::to_slice(&payload, &mut buf) {
            Ok(len) => self.inner.driver.publish(topic, &buf[..len]).await,
            Err(_) => Err(Error::Serialize),
        }
    }
}
