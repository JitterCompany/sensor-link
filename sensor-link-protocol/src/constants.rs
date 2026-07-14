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
