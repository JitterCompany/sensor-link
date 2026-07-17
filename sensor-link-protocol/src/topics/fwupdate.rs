use core::fmt;

use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use base64::{engine::general_purpose, Engine};
use serde::{de::Visitor, ser::Error as SerError, Deserialize, Serialize};
pub const FW_CHUNK_SIZE: usize = 300;
pub const B64_ENCODED_CHUNK_SIZE: usize = usize::div_ceil(FW_CHUNK_SIZE, 3) * 4;
pub type FWUpdateURL = heapless::String<128>;

#[derive(Debug, Serialize, Deserialize)]
pub struct FWChunk {
    pub sequence_no: u16,
    #[serde(
        serialize_with = "serialize_base64",
        deserialize_with = "deserialize_base64"
    )]
    pub bytes: [u8; FW_CHUNK_SIZE],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FWAnnounce {
    pub url: FWUpdateURL,
    /// Optional timestamp to schedule the update.
    pub timestamp: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum FWStatus {
    /// Started: confirms receipt of FWAnnounce
    Start,

    /// Something whent wrong
    Failed,

    /// All announced chunks have been received
    Received,

    /// Update has been received and decoded
    Complete,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FWConfirm {
    pub status: FWStatus,
}

fn serialize_base64<S>(bytes: &[u8; FW_CHUNK_SIZE], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let encoder = general_purpose::URL_SAFE;
    let mut encoded = [0u8; B64_ENCODED_CHUNK_SIZE];
    let n = encoder
        .encode_slice(bytes, &mut encoded)
        .map_err(|_| S::Error::custom("failed to encode base64"))?;
    serializer.serialize_str(
        core::str::from_utf8(&encoded[..n])
            .map_err(|_| S::Error::custom("failed to decode utf8"))?,
    )
}

struct Base64Visitor;

impl<'de> Visitor<'de> for Base64Visitor {
    type Value = [u8; FW_CHUNK_SIZE];

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a base64 encoded string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        // Note: using an array with length FW_CHUNK_SIZE results in OutputSliceTooSmall error.
        // Deserialization requires two extra bytes that are not needed for the result.
        let mut buffer = [0u8; FW_CHUNK_SIZE + 2];
        // Decode the base64 string
        let res = decode_base64(value, &mut buffer);

        match res {
            // Note: unwrap() cannot fail because FW_CHUNK_SIZE is smaller than buffer.len().
            Ok(_n) => Ok(buffer[0..FW_CHUNK_SIZE].try_into().unwrap()),
            Err(err) => {
                log::error!("Failed to decode base64: {err:?}");
                Err(serde::de::Error::custom("Failed to decode base64"))
            }
        }
    }
}

pub fn decode_base64<T: AsRef<[u8]>>(
    input: T,
    output: &mut [u8],
) -> Result<usize, base64::DecodeSliceError> {
    general_purpose::URL_SAFE.decode_slice(input, output)
}

fn deserialize_base64<'de, D>(deserializer: D) -> Result<[u8; FW_CHUNK_SIZE], D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_str(Base64Visitor)
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for FWChunk {}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for FWAnnounce {}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for FWConfirm {}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for FWStatus {}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_fwchunk_serde() {
        simple_logger::init().ok();
        const N: usize = FW_CHUNK_SIZE;
        let mut array: [u8; N] = [0; N];

        for (i, elem) in array.iter_mut().enumerate() {
            *elem = i as u8;
        }

        let chunk = FWChunk {
            sequence_no: 31,
            bytes: array,
        };

        let payload = chunk.serialize_topic_payload().unwrap();
        let chunk2 = crate::parse_json_payload::<FWChunk>(&payload).unwrap();
        assert_eq!(chunk.sequence_no, chunk2.sequence_no);
        assert_eq!(
            chunk.bytes[FW_CHUNK_SIZE - 1],
            ((FW_CHUNK_SIZE - 1) % 256) as u8
        );
        assert_eq!(chunk.bytes.len(), FW_CHUNK_SIZE);
    }

    #[test]
    fn test_base64_decode() {
        let original = "FILE CONTENT HERE!";
        let input = "RklMRSBDT05URU5UIEhFUkUh";

        let mut output = [0u8; FW_CHUNK_SIZE];
        let n = decode_base64(input, &mut output).unwrap();

        let output_str = core::str::from_utf8(&output[..n]).unwrap();

        println!("{output_str}");

        assert_eq!(original, output_str);

        // String with all possible bytes encoded as url safe base64
        let input = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0-P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6e3x9fn-AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq-wsbKztLW2t7i5uru8vb6_wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t_g4eLj5OXm5-jp6uvs7e7v8PHy8_T19vf4-fr7_P3-_w==";

        let mut output = [0u8; FW_CHUNK_SIZE];
        let n = decode_base64(input, &mut output).unwrap();

        // Map range to array
        let original: Vec<u8> = (0..=255).collect();
        assert_eq!(original, output[..n]);
    }
}
