/// Maximum length of the topic prefix (typically just "f" or "t")
pub const MAX_TOPIC_PREFIX_LEN: usize = 8;

/// Maximum length of the UID
pub const MAX_UID_LEN: usize = 9;

/// Maximum length of the total topic string
pub const MAX_TOPIC_LEN: usize = 40;

/// Maximum length of the published message
/// NB: before incrementing, carefully review library/modem specs
pub const MAX_MESSAGE_LEN: usize = 1500;
pub const MAX_FILE_CHUNK_LEN: usize = 600;
pub const MAX_EVENT_LEN: usize = 350;

pub const MAX_ONLINE_PAYLOAD_LEN: usize = 34;

/// Maximum length of the serialized device log payload, topic header included
///
/// The worst-case [`LogMessage`](crate::device_log::LogMessage) (a full target,
/// a full message body and the longest timestamp) serializes to 237 bytes,
/// against a payload budget of `MAX_LOG_LEN` minus the 8-byte topic header. The
/// remaining 11 bytes are the headroom for JSON escaping: a line with more than
/// ~11 escaped characters does not fit and is dropped rather than truncated
/// further. Raising this constant is what buys more headroom, at the cost of
/// RAM per pending record and flash per stored one.
pub const MAX_LOG_LEN: usize = 256;

/// Maximum length of the `target` (module path) of a device log message
pub const MAX_LOG_TARGET_LEN: usize = 24;

/// Maximum length of the message body of a device log message
pub const MAX_LOG_MSG_LEN: usize = 160;
