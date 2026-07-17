use serde::de::DeserializeOwned;

use crate::{
    event::EventPayload,
    info::DeviceInfoV2,
    online::Online,
    samples::{self, NChannelSamples},
};

#[derive(Debug)]
pub enum Error {
    /// Error while trying to deserialize data.
    Deserialize(String),
    /// Error while trying to serialize data.
    Serialize(String),
    InvalidTopic,
    /// Error while trying to verify data
    VerifyFailed,
}

impl From<samples::ParseError> for Error {
    fn from(value: samples::ParseError) -> Self {
        match value {
            samples::ParseError::Deserialize(reason) => Error::Deserialize(reason),
            samples::ParseError::Invalid => Error::VerifyFailed,
        }
    }
}

pub fn parse_event<E: DeserializeOwned>(bytes: &[u8]) -> Result<EventPayload<E>, Error> {
    serde_json::from_slice(bytes).map_err(|err| Error::Deserialize(err.to_string()))
}

pub fn parse_device_info_v2<D: DeserializeOwned>(bytes: &[u8]) -> Result<DeviceInfoV2<D>, Error> {
    serde_json::from_slice(bytes).map_err(|err| Error::Deserialize(err.to_string()))
}

pub fn parse_online(bytes: &[u8]) -> Result<Online, Error> {
    serde_json::from_slice(bytes).map_err(|err| Error::Deserialize(err.to_string()))
}

/// Parse data from topics sending uniform samples
pub fn parse_uniform_samples<const N_CH: usize>(
    bytes: &[u8],
) -> Result<NChannelSamples<N_CH>, Error> {
    // Use the new std-only parser that doesn't require MAX_SAMPLES_PER_MESSAGE
    samples::deserialize::parse_q15xl_to_nchannel::<N_CH>(bytes).map_err(|e| e.into())
}

#[cfg(test)]
mod tests {
    use crate::{
        event::Event,
        info::VersionString,
        samples::{
            q15xl::{Uniform, Q15XL},
            UniformSamples,
        },
        Microseconds, Milliseconds, TopicPayloadSerialize,
    };

    use super::*;

    #[test]
    fn test_parse_online() {
        let online = Online::yes(Milliseconds(12345678901234));
        let serialized = online.serialize_topic_payload().unwrap();

        let parsed = parse_online(&serialized).unwrap();
        assert_eq!(parsed.since(), online.since());
        assert!(parsed.since().is_some());

        let offline = Online::no();
        let serialized = offline.serialize_topic_payload().unwrap();
        let parsed = parse_online(&serialized).unwrap();
        assert_eq!(parsed.since(), offline.since());
        assert!(parsed.since().is_none());

        let offline_ungraceful = Online::will();
        let serialized = offline_ungraceful.serialize_topic_payload().unwrap();
        let parsed = parse_online(&serialized).unwrap();
        assert_eq!(parsed.since(), offline_ungraceful.since());
        assert!(parsed.since().is_none());
        assert!(parsed.ungraceful_offline());
    }

    #[test]
    fn test_parse_event() {
        // Unit-variant event round-trips through the events topic.
        let payload = EventPayload::from_event_at(Event::Blink, Microseconds(1_500_000));
        let serialized = payload.serialize_topic_payload().unwrap();

        let parsed = parse_event::<Event>(&serialized).unwrap();
        assert_eq!(parsed.ts, payload.ts);
        assert!(matches!(parsed.event, Event::Blink));

        // Variant carrying a description also round-trips.
        let payload = EventPayload::from_event_at(Event::booted("hello"), Microseconds(2_000_000));
        let serialized = payload.serialize_topic_payload().unwrap();

        let parsed = parse_event::<Event>(&serialized).unwrap();
        assert_eq!(parsed.ts, payload.ts);
        match parsed.event {
            Event::Booted(desc) => assert_eq!(desc.msg.as_str(), "hello"),
            other => panic!("expected Booted event, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_device_info_v2() {
        let info: DeviceInfoV2<String> = DeviceInfoV2 {
            device_type: "jitter".to_string(),
            fw_version: VersionString::try_from("1.2.3-4").unwrap(),
            fw_rev: VersionString::try_from("abcdef0").unwrap(),
            bootloader_version: VersionString::try_from("0.9.0").unwrap(),
            modem_model: VersionString::try_from("EC21").unwrap(),
            modem_fw_version: VersionString::try_from("EC21EFAR06").unwrap(),
        };
        let serialized = info.serialize_topic_payload().unwrap();

        let parsed = parse_device_info_v2::<String>(&serialized).unwrap();
        assert_eq!(parsed, info);
    }

    #[test]
    fn test_parse_uniform_samples() {
        const N_CH: usize = 3;
        const NUM_SAMPLES: usize = 100;

        let mut samples = UniformSamples::<N_CH, 255>::empty_at(1_000_000, 1000.0);
        for i in 0..NUM_SAMPLES {
            for ch in 0..N_CH {
                samples.ch[ch]
                    .push((i as f32 + ch as f32 * 0.1) / 100.0)
                    .unwrap();
            }
        }

        // Serialize via the Q15XL wire format used on the samples topics.
        let q15xl = Uniform::<Q15XL<N_CH, 255>>::from_uniform(&samples);
        let mut buffer = [0u8; 4096];
        let bytes = q15xl.as_topic_data(&mut buffer).unwrap();

        let parsed = parse_uniform_samples::<N_CH>(bytes).unwrap();
        assert_eq!(parsed.fs, 1000.0);
        assert_eq!(parsed.t.len(), NUM_SAMPLES);
        assert_eq!(parsed.ch[0].len(), NUM_SAMPLES);
    }
}
