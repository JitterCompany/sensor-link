//! Events sent by the device on the event topic.
//!

#[cfg(feature = "use-std")]
use serde::Deserialize;
use serde::Serialize;

pub use sensor_link_protocol::event::{Desc, Event as CommonEvent, EventPayload};

/// Demo-specific events, plus every event shared with all Jitter devices
/// (wrapped in [Event::Common]).
///
/// `Deserialize` requires the "use-std" feature: deserializing the untagged [Event::Common]
/// variant needs serde's buffering, which is unavailable without `alloc`.
#[derive(Debug, Serialize, Clone)]
#[cfg_attr(feature = "use-std", derive(Deserialize))]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// The button was pressed; the device syncs right away.
    ButtonPressed,

    /// Event shared with all Jitter devices, see [sensor_link_protocol::event::Event].
    ///
    /// Untagged: serializes to the payload defined by the shared protocol, without a
    /// `common` wrapper.
    #[serde(untagged)]
    Common(CommonEvent),
}

impl Event {
    pub const fn is_urgent(&self) -> bool {
        match self {
            Event::ButtonPressed => false,
            Event::Common(event) => event.is_urgent(),
        }
    }

    pub fn contact_timeout(message: &str) -> Self {
        CommonEvent::contact_timeout(message).into()
    }

    pub fn started(message: &str) -> Self {
        CommonEvent::started(message).into()
    }

    pub fn stopped(message: &str) -> Self {
        CommonEvent::stopped(message).into()
    }

    pub fn booted(message: &str) -> Self {
        CommonEvent::booted(message).into()
    }

    pub fn sensor_error(message: &str) -> Self {
        CommonEvent::sensor_error(message).into()
    }

    pub fn processing_warning(message: &str) -> Self {
        CommonEvent::processing_warning(message).into()
    }
}

/// Wrap a manufacturer-generic Jitter event as the corresponding device event.
impl From<CommonEvent> for Event {
    fn from(event: CommonEvent) -> Self {
        Event::Common(event)
    }
}

#[cfg(all(test, feature = "use-std"))]
mod test {
    use super::*;
    use crate::{Microseconds, TopicPayloadSerialize, MAX_EVENT_LEN};
    use sensor_link_protocol::server::parse_event;

    const TS: Microseconds = Microseconds(1_500_000);

    #[test]
    fn test_common_event_payload_is_unchanged() {
        // A shared event must serialize byte-for-byte like it does on any other Jitter device,
        // so servers keep parsing it without knowing about the demo event type.
        let common = sensor_link_protocol::event::EventPayload::from_event_at(
            CommonEvent::booted("hello"),
            TS,
        );
        let wrapped = EventPayload::from_event_at(Event::booted("hello"), TS);

        assert_eq!(
            wrapped.serialize_topic_payload().unwrap(),
            common.serialize_topic_payload().unwrap()
        );
    }

    #[test]
    fn test_button_pressed_roundtrips() {
        let payload = EventPayload::from_event_at(Event::ButtonPressed, TS);
        let serialized = payload.serialize_topic_payload().unwrap();
        assert!(serialized.len() <= MAX_EVENT_LEN);
        assert!(serialized.starts_with(br#"{"e":"button_pressed""#));

        let parsed = parse_event::<Event>(&serialized).unwrap();
        assert_eq!(parsed.ts, payload.ts);
        assert!(matches!(parsed.event, Event::ButtonPressed));
    }

    #[test]
    fn test_common_event_roundtrips() {
        let payload = EventPayload::from_event_at(Event::Common(CommonEvent::Blink), TS);
        let serialized = payload.serialize_topic_payload().unwrap();

        let parsed = parse_event::<Event>(&serialized).unwrap();
        assert!(matches!(parsed.event, Event::Common(CommonEvent::Blink)));
    }
}
