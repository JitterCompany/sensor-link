use core::fmt::Debug;

use heapless::{String, Vec};

use sensor_link_protocol::{
    info::VersionString, Error, MAX_FILE_CHUNK_LEN, MAX_MESSAGE_LEN, MAX_ONLINE_PAYLOAD_LEN,
    MAX_TOPIC_LEN,
};

/// Log target of the records [`MqttPublish::publish`] emits about its own
/// outcome, i.e. once per published message.
///
/// These are never published to the device's MQTT log topic (see the
/// `log_publish` module, behind the `mqtt-log` feature): publishing such a
/// record emits another one, which would publish itself forever. Only
/// [`MqttPublish::publish`] logs under this target, which is why drivers do
/// not report the outcome of a publish themselves — see
/// [`MqttClient::publish_message`].
pub const PUBLISH_LOG_TARGET: &str = "MQTT Publish";

/// Publishing over an [`MqttClient`], with the outcome logged under
/// [`PUBLISH_LOG_TARGET`].
///
/// Blanket-implemented for every [`MqttClient`], which is what makes the target
/// enforceable: an implementor cannot provide its own [`publish`](Self::publish)
/// (a second impl would collide with the blanket one), so every publish in the
/// system reports through this one function, under the one target that the log
/// publisher excludes.
pub trait MqttPublish: MqttClient {
    /// Publish a message on a specific topic and report the outcome.
    async fn publish(
        &mut self,
        topic_name: String<MAX_TOPIC_LEN>,
        message: &[u8],
    ) -> Result<(), Error<Self::ClientError>> {
        let topic_for_log = topic_name.clone();
        let result = self.publish_message(topic_name, message).await;
        match &result {
            Ok(_) => log::debug!(target: PUBLISH_LOG_TARGET, "Mqtt published to {topic_for_log:?}"),
            Err(err) => log::error!(target: PUBLISH_LOG_TARGET, "Failed to publish: {err:?}"),
        }
        result
    }
}

impl<C: MqttClient> MqttPublish for C {}

/// Events the network client implementation can return
// `ReceivedMessage` carries inline topic/payload buffers, which dwarf the empty `Disconnected`.
// Boxing it is not an option: this crate is `no_std` with no global allocator.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    ReceivedMessage(Message),
    Disconnected,
}

#[derive(Debug, Clone, Copy)]
pub enum FileError {
    FileNotFound,
    ReadError,
    EndOfFile,
}

pub struct Message {
    pub topic: String<MAX_TOPIC_LEN>,
    pub payload: Vec<u8, MAX_MESSAGE_LEN>,
}

/// The `MqttClient` should be implemented by a network driver that provides an MQTT connection.
pub trait MqttClient {
    type ClientError: Debug;
    type PollError: Debug;

    /// Connect and get ready
    async fn connect(
        &mut self,
        client_id: &str,
        will: Will,
    ) -> Result<(), Error<Self::ClientError>>;

    /// Disconnect and
    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>>;

    /// Reconnect with clean_session=False
    async fn reconnect(&mut self) -> Result<(), Error<Self::ClientError>>;

    /// Ask client to subscribe to a topic.
    async fn subscribe(
        &mut self,
        topic_name: String<MAX_TOPIC_LEN>,
    ) -> Result<(), Error<Self::ClientError>>;

    /// Ask client to unsubscribe from a topic.
    async fn unsubscribe(
        &mut self,
        topic_name: String<MAX_TOPIC_LEN>,
    ) -> Result<(), Error<Self::ClientError>>;

    /// Ask client to publish a message on a specific topic.
    ///
    /// This is the raw publish primitive. Callers should use
    /// [`MqttPublish::publish`], which wraps it and reports the outcome, so an
    /// implementation of this method must not log that outcome itself: those
    /// records have to carry [`PUBLISH_LOG_TARGET`] to stay out of the
    /// published log stream, and only [`MqttPublish::publish`] can guarantee
    /// that. Logging anything else (the steps of a publish, protocol errors) is
    /// fine.
    async fn publish_message(
        &mut self,
        topic_name: String<MAX_TOPIC_LEN>,
        message: &[u8],
    ) -> Result<(), Error<Self::ClientError>>;

    /// Call to await driver events or incomming messages.
    ///
    /// This call may be selected.
    ///
    /// timeout_s is the timeout in seconds.
    /// After No activity for this amount of time
    /// this should return `Err`
    async fn await_response(&mut self, timeout_s: u32) -> Result<(), Self::PollError>;

    /// After a response has been received, call this to handle the response.
    /// This future may not be used in select!.
    async fn handle_response(&mut self) -> Result<Event, ()>;

    // TODO: Maybe move functions below to a separate trait as they are not MQTT specific.
    // Maybe FileDownloadClient, or HTTPClient?

    /// Ask client to download a file from a specific url.
    /// Return Ok when download is done and temporarily stored for later retrieval.
    async fn download_file(&mut self, url: &str) -> Result<(), Error<Self::ClientError>>;

    /// Read chunks of the downloaded file. Returns None if everything is read or when there is no data.
    /// The caller needs to be able to distinguish between:
    /// - no data available
    /// - end of file
    /// - error
    async fn read_file_chunk(
        &mut self,
        chunk_size: usize,
    ) -> Result<Vec<u8, MAX_FILE_CHUNK_LEN>, FileError>;

    /// Most recent signal quality of the modem, if available.
    ///
    /// Defaults to `None` for drivers without a modem (e.g. test mocks).
    async fn signal_quality(&mut self) -> Option<i16> {
        None
    }

    /// Model ID of the modem.
    ///
    /// Defaults to `"unknown"` for drivers without a modem (e.g. test mocks).
    async fn modem_model(&mut self) -> VersionString {
        VersionString::try_from("unknown").unwrap_or_default()
    }

    /// Firmware version of the modem.
    ///
    /// Defaults to `"unknown"` for drivers without a modem (e.g. test mocks).
    async fn modem_fw_version(&mut self) -> VersionString {
        VersionString::try_from("unknown").unwrap_or_default()
    }
}

pub struct Will {
    pub topic: String<MAX_TOPIC_LEN>,
    pub payload: String<MAX_ONLINE_PAYLOAD_LEN>,
}
