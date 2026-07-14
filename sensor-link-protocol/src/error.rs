use crate::topics::TopicSerializeError;
use core::fmt::Debug;

impl<T: Debug> From<serde_json_core::ser::Error> for Error<T> {
    fn from(_serde_error: serde_json_core::ser::Error) -> Self {
        Error::Serialize
    }
}

#[derive(Debug)]
pub enum Error<T: Debug> {
    /// No simcard detected or non-functioning simcard.
    InvalidSIM, // todo remove: implemtation specific.

    /// Error in client implementation.
    /// e.g. communication error with modem.
    Client(T),

    /// Request timed out.
    TimeOut,

    /// MQTT specific error occurred. Maybe an issue with the broker.
    MQTT(&'static str),

    /// Error while trying to serialize data.
    Serialize,

    /// Error while trying to serialize topic name.
    SerializeTopic(TopicSerializeError),
}
