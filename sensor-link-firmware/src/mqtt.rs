use core::fmt::Debug;

use heapless::{String, Vec};

use sensor_link_protocol::{
    info::VersionString, Error, MAX_FILE_CHUNK_LEN, MAX_MESSAGE_LEN, MAX_ONLINE_PAYLOAD_LEN,
    MAX_TOPIC_LEN,
};

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

    /// Ask client to publish a message on a specific topics.
    async fn publish(
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
