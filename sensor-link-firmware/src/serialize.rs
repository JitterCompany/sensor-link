use sensor_link_protocol::{Error, Topic, TopicSerializeError, MAX_MESSAGE_LEN};

mod private {
    /// Sealed trait to prevent external implementation
    pub trait Sealed {}
}

pub(crate) mod internal {
    /// ZST marker for pub(crate) internal API
    pub struct Internal;
}

/// Trait marking an item as sendable via FrogwatchLink.
///
/// Implemented by [SerializedSendable].
/// Note: this is a sealed trait by design: it cannot be implemented by the user.
pub trait Sendable<T: Topic>: private::Sealed + Sync + Send + core::fmt::Debug {
    /// Access the serialized data as a slice.
    ///
    /// The slice can be stored and later reconstructed into a `SerializedSendable` using any of:
    /// - `SerializedSendable::try_from_slice`
    /// - `Builder::create_with_total_length`
    fn as_slice(&self) -> &[u8];

    /// Parse the topic from internal data.
    ///
    /// Can only fail if this sendable was recreated from invalid data.
    fn topic(&self) -> Result<T, postcard::Error>;

    /// Access the payload of the message.
    ///
    /// Internal API: not public as the end user should not need to access the payload directly
    #[doc(hidden)]
    fn payload_bytes(&self, _: internal::Internal) -> &[u8];

    /// Access the header of the message.
    ///
    /// Internal API: not public as the end user should not need to access the header directly
    #[doc(hidden)]
    fn header_bytes(&self, _: internal::Internal) -> &[u8; TOPIC_HEADER_SIZE];
}

pub trait AsSendable<const MAX_OUTPUT_LEN: usize, T: Topic> {
    type Error;
    const MAX_SENDABLE_LENGTH: usize = MAX_OUTPUT_LEN;

    fn as_sendable(&self) -> Result<SerializedSendable<{ MAX_OUTPUT_LEN }, T>, Self::Error>;
}

pub const TOPIC_HEADER_SIZE: usize = 8;

pub struct SerializedSendable<const MAX_OUTPUT_LEN: usize, T: Topic> {
    bytes: [u8; MAX_OUTPUT_LEN],
    payload_len: usize,
    _topic: core::marker::PhantomData<T>,
}

#[derive(Debug)]
pub struct Builder<const MAX_OUTPUT_LEN: usize> {
    pub bytes: [u8; MAX_OUTPUT_LEN],
}

#[derive(Debug)]
pub struct BuilderWithTopic<const MAX_OUTPUT_LEN: usize, T: Topic> {
    bytes: [u8; MAX_OUTPUT_LEN],
    topic: T,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthError {
    TooLong,
    TooShort,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BuildError {
    TopicSerializeFailed,
    PayloadTooLong,
}

impl<T: core::fmt::Debug> From<BuildError> for Error<T> {
    fn from(error: BuildError) -> Self {
        match error {
            BuildError::PayloadTooLong => Error::Serialize,
            BuildError::TopicSerializeFailed => Error::SerializeTopic(TopicSerializeError::Other),
        }
    }
}

const fn calc_max_payload_len(output_len: usize) -> usize {
    assert!(
        output_len > TOPIC_HEADER_SIZE,
        "output length must be greater than TOPIC_HEADER_SIZE"
    );
    let payload_len = output_len - TOPIC_HEADER_SIZE;
    assert!(
        payload_len <= MAX_MESSAGE_LEN,
        "payload length must be less than MAX_MESSAGE_LEN"
    );

    payload_len
}

impl<const MAX_OUTPUT_LEN: usize> Builder<MAX_OUTPUT_LEN> {
    pub const MAX_PAYLOAD_LEN: usize = calc_max_payload_len(MAX_OUTPUT_LEN);

    /// Create a new builder to build a `SerializedSendable`.
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::serialize::{Builder, SerializedSendable};
    /// use sensor_link_protocol::TopicFromDevice;
    ///
    /// let mut builder = Builder::<1024>::new();
    ///
    /// // write data (previously created via `SerializedSendable::as_slice`) to this buffer..
    /// let buffer: &mut [u8] = &mut builder.bytes;
    ///
    /// // ...lets assume 33 bytes were written:
    /// let len: usize= 33;
    /// let sendable: SerializedSendable<1024, TopicFromDevice> = builder.create_with_total_length(len).unwrap();
    /// ```
    #[inline]
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_OUTPUT_LEN],
        }
    }

    #[inline]
    pub(crate) const fn with_topic<T: Topic>(topic: T) -> BuilderWithTopic<MAX_OUTPUT_LEN, T> {
        BuilderWithTopic::new(topic)
    }

    #[inline]
    pub const fn create_with_total_length<T: Topic>(
        self,
        total_len: usize,
    ) -> Result<SerializedSendable<MAX_OUTPUT_LEN, T>, LengthError> {
        if total_len > MAX_OUTPUT_LEN {
            return Err(LengthError::TooLong);
        }
        if total_len < TOPIC_HEADER_SIZE {
            return Err(LengthError::TooShort);
        }
        Ok(SerializedSendable::from_bytes_with_payload_len(
            self.bytes,
            total_len - TOPIC_HEADER_SIZE,
        ))
    }
}

impl<const MAX_OUTPUT_LEN: usize, T: Topic> BuilderWithTopic<MAX_OUTPUT_LEN, T> {
    pub const MAX_PAYLOAD_LEN: usize = calc_max_payload_len(MAX_OUTPUT_LEN);

    /// Create a new builder to build a `SerializedSendable`.
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::serialize::{BuilderWithTopic, SerializedSendable};
    /// use sensor_link_protocol::TopicFromDevice;
    /// let mut builder = BuilderWithTopic::<1024, TopicFromDevice>::new(TopicFromDevice::BenchmarkEvent);
    /// let payload = builder.payload_buffer();
    /// // write payload data (33 bytes in this example)...
    /// let len: usize= 33;
    /// let sendable: SerializedSendable<1024, TopicFromDevice> = builder.create_with_payload_length(len).unwrap();
    /// ```
    #[inline]
    pub const fn new(topic: T) -> Self {
        Self {
            bytes: [0; MAX_OUTPUT_LEN],
            topic,
        }
    }

    /// Access the payload buffer for writing.
    ///
    /// After writing the payload, call [create_with_payload_length()](BuilderWithTopic::create_with_payload_length) to create a `SerializedSendable`.
    pub fn payload_buffer(&mut self) -> &mut [u8] {
        &mut self.bytes[TOPIC_HEADER_SIZE..]
    }

    /// Create a `SerializedSendable` from the payload buffer.
    ///
    /// Example:
    /// ```
    /// use sensor_link_firmware::serialize::{SerializedSendable, BuilderWithTopic};
    /// use sensor_link_protocol::TopicFromDevice;
    ///
    /// let mut builder = BuilderWithTopic::<1024, TopicFromDevice>::new(TopicFromDevice::BenchmarkEvent);
    ///
    /// let payload: &mut [u8] = builder.payload_buffer();
    /// // write payload data (this example assume 33 bytes were written)...
    /// let payload_len: usize = 33;
    ///
    /// let sendable: SerializedSendable<1024, TopicFromDevice> = builder.create_with_payload_length(payload_len).unwrap();
    /// ```
    pub fn create_with_payload_length(
        mut self,
        payload_len: usize,
    ) -> Result<SerializedSendable<MAX_OUTPUT_LEN, T>, BuildError> {
        if payload_len > Self::MAX_PAYLOAD_LEN {
            return Err(BuildError::PayloadTooLong);
        }

        // 1. Serialize topic header into the first `TOPIC_HEADER_SIZE` bytes
        serialize_topic_header(&self.topic, &mut self.bytes[..TOPIC_HEADER_SIZE])?;

        // 2. Assume `payload_len` payload bytes were written into the buffer via `self.payload_buffer()`
        Ok(SerializedSendable::from_bytes_with_payload_len(
            self.bytes,
            payload_len,
        ))
    }
}

impl<const MAX_OUTPUT_LEN: usize, T: Topic> SerializedSendable<MAX_OUTPUT_LEN, T> {
    // compile-time calculated and asserted to be valid
    pub const MAX_PAYLOAD_LEN: usize = calc_max_payload_len(MAX_OUTPUT_LEN);

    #[inline]
    const fn from_bytes_with_payload_len(bytes: [u8; MAX_OUTPUT_LEN], payload_len: usize) -> Self {
        assert!(payload_len <= Self::MAX_PAYLOAD_LEN);
        Self {
            bytes,
            payload_len,
            _topic: core::marker::PhantomData,
        }
    }

    /// Reconstruct a `SerializedSendable` from a slice.
    ///
    /// The slice must have been previously created by `SerializedSendable::as_slice`.
    ///
    /// The slice length must not exceed `MAX_OUTPUT_LEN` bytes.
    /// See also: `Builder`
    pub fn try_from_slice(slice: &[u8]) -> Result<Self, LengthError> {
        if slice.len() > MAX_OUTPUT_LEN {
            return Err(LengthError::TooLong);
        }
        // Too short: should not happen unless the user is passing in a slice that was not originally
        // created from `SerializedSendable::as_slice`.
        if slice.len() < TOPIC_HEADER_SIZE {
            return Err(LengthError::TooShort);
        }

        let total_len = slice.len();
        let mut sendable = Self {
            bytes: [0; MAX_OUTPUT_LEN],
            payload_len: total_len - TOPIC_HEADER_SIZE,
            _topic: core::marker::PhantomData,
        };
        sendable.bytes[..total_len].copy_from_slice(slice);
        Ok(sendable)
    }

    /// Access the serialized data as a slice.
    ///
    /// The slice can be stored and later reconstructed into a `SerializedSendable` using any of:
    /// - `SerializedSendable::try_from_slice`
    /// - `Builder::create_with_total_length`
    pub fn as_slice(&self) -> &[u8] {
        // try_from_slice or create_with_total_length are responsible to guarantee the buffer is long enough
        &self.bytes[..TOPIC_HEADER_SIZE + self.payload_len]
    }

    /// Access the payload of the message.
    pub fn payload_bytes(&self) -> &[u8] {
        &self.bytes[TOPIC_HEADER_SIZE..TOPIC_HEADER_SIZE + self.payload_len]
    }

    /// Access the header of the message.
    pub fn header_bytes(&self) -> &[u8; TOPIC_HEADER_SIZE] {
        self.bytes[..TOPIC_HEADER_SIZE].try_into().unwrap()
    }

    /// Parse the topic from internal data.
    ///
    /// Can only fail if this sendable was not created from valid data.
    pub fn topic(&self) -> Result<T, postcard::Error> {
        deserialize_topic_header(self.header_bytes())
    }
}

impl<const N: usize, T: Topic> private::Sealed for SerializedSendable<N, T> {}
impl<const N: usize, T: Topic> Sendable<T> for SerializedSendable<N, T> {
    fn as_slice(&self) -> &[u8] {
        self.as_slice()
    }

    fn topic(&self) -> Result<T, postcard::Error> {
        self.topic()
    }

    fn payload_bytes(&self, _: internal::Internal) -> &[u8] {
        self.payload_bytes()
    }

    fn header_bytes(&self, _: internal::Internal) -> &[u8; TOPIC_HEADER_SIZE] {
        self.header_bytes()
    }
}

impl<const MAX_OUTPUT_LEN: usize, T: Topic> core::fmt::Debug
    for SerializedSendable<MAX_OUTPUT_LEN, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!(
            "Sendable to '{:?}' with {:?}-byte payload",
            self.topic(),
            self.payload_bytes().len()
        ))
    }
}

#[inline]
fn serialize_topic_header<T: Topic>(topic: &T, buffer: &mut [u8]) -> Result<(), BuildError> {
    let _ = postcard::to_slice(&topic, buffer).map_err(|_| BuildError::TopicSerializeFailed)?;

    // little bit of obfuscation to reduce risk of accidental serialization of invalid data
    for byte in buffer {
        *byte ^= 0xA5;
    }
    Ok(())
}

#[inline]
fn deserialize_topic_header<T: Topic>(
    buffer: &[u8; TOPIC_HEADER_SIZE],
) -> Result<T, postcard::Error> {
    // little bit of obfuscation to reduce risk of accidental deserialization of invalid data
    let mut buffer = buffer.clone();
    for byte in buffer.iter_mut() {
        *byte ^= 0xA5;
    }
    postcard::from_bytes(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TopicFromDevice;
    use strum::IntoEnumIterator;

    #[test]
    fn test_empty() {
        // empty slice should not even construct a sendable
        SerializedSendable::<1508, TopicFromDevice>::try_from_slice(&[]).unwrap_err();

        // empty builder should not even construct a sendable
        Builder::<1508>::new()
            .create_with_total_length::<TopicFromDevice>(0)
            .unwrap_err();
        Builder::<1508>::new()
            .create_with_total_length::<TopicFromDevice>(TOPIC_HEADER_SIZE - 1)
            .unwrap_err();
    }

    #[test]
    fn test_sendable_empty_payload() {
        let sendable: SerializedSendable<1508, TopicFromDevice> =
            BuilderWithTopic::<1508, TopicFromDevice>::new(TopicFromDevice::BenchmarkEvent)
                .create_with_payload_length(0)
                .unwrap();

        assert_eq!(sendable.payload_len, 0);
        assert_eq!(sendable.topic().unwrap(), TopicFromDevice::BenchmarkEvent);
        assert_eq!(sendable.as_slice().len(), TOPIC_HEADER_SIZE);
    }

    #[test]
    fn test_sendable_nonzero_payload() {
        let mut builder =
            BuilderWithTopic::<1508, TopicFromDevice>::new(TopicFromDevice::BenchmarkEvent);
        builder.payload_buffer()[0] = 0x4A;
        builder.payload_buffer()[1] = 0x69;
        builder.payload_buffer()[2] = 0x74;
        builder.payload_buffer()[3] = 0x74;
        builder.payload_buffer()[4] = 0x65;
        builder.payload_buffer()[5] = 0x72;
        let sendable: SerializedSendable<1508, TopicFromDevice> =
            builder.create_with_payload_length(6).unwrap();

        assert_eq!(sendable.payload_len, 6);
        assert_eq!(sendable.topic().unwrap(), TopicFromDevice::BenchmarkEvent);
        assert_eq!(sendable.as_slice().len(), TOPIC_HEADER_SIZE + 6);
        assert_eq!(
            sendable.payload_bytes(),
            [0x4A, 0x69, 0x74, 0x74, 0x65, 0x72]
        );
    }

    #[test]
    fn test_serialize_topic_as_bytes() {
        let mut buffer = [0u8; 128];

        // Loop over all veriants of TopicFromDevice and serialize them
        for topic in TopicFromDevice::iter() {
            let serialized: &mut [u8] = postcard::to_slice(&topic, &mut buffer).unwrap();
            println!("{:?} serialized len: {}", topic, serialized.len());

            // Make sure serialized topic fits in topic header size
            assert!(serialized.len() <= crate::serialize::TOPIC_HEADER_SIZE);

            let deserialized = postcard::from_bytes::<TopicFromDevice>(serialized).unwrap();
            assert_eq!(topic, deserialized);
        }
    }
}
