//! Integration tests for the generic [sensor_link_firmware::logic::client::Client].
//!
//! These tests exercise the manufacturer-agnostic protocol client (connect/online,
//! command receipt, time sync, firmware update announce) independently of any
//! product-specific wrapper.

use std::collections::VecDeque;

use sensor_link_firmware::{
    logic::client::{Client, ClientEvent, UIDString},
    mqtt::{self, FileError, MqttClient, Will},
};
use sensor_link_protocol::{
    cmd, time, Error, Milliseconds, TopicPayloadSerialize, TopicString, MAX_FILE_CHUNK_LEN,
};

// ── MockMQTT ────────────────────────────────────────────────────────────────

struct MockMQTT {
    pub next_events: VecDeque<mqtt::Event>,
    pub sent: VecDeque<(TopicString, Vec<u8>)>,
}

impl MockMQTT {
    pub fn new() -> Self {
        Self {
            next_events: VecDeque::new(),
            sent: VecDeque::new(),
        }
    }

    fn push_message(&mut self, topic: &str, payload: &[u8]) {
        self.next_events
            .push_back(mqtt::Event::ReceivedMessage(mqtt::Message {
                topic: heapless::String::try_from(topic).unwrap(),
                payload: heapless::Vec::from_slice(payload).unwrap(),
            }));
    }
}

impl MqttClient for &mut MockMQTT {
    type ClientError = ();
    type PollError = ();

    async fn connect(&mut self, _: &str, _: Will) -> Result<(), Error<Self::ClientError>> {
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        Ok(())
    }

    async fn reconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        Ok(())
    }

    async fn subscribe(&mut self, _: TopicString) -> Result<(), Error<Self::ClientError>> {
        Ok(())
    }

    async fn unsubscribe(&mut self, _: TopicString) -> Result<(), Error<Self::ClientError>> {
        Ok(())
    }

    async fn publish(
        &mut self,
        topic: TopicString,
        payload: &[u8],
    ) -> Result<(), Error<Self::ClientError>> {
        self.sent.push_back((topic.into(), payload.to_vec()));
        Ok(())
    }

    async fn await_response(&mut self, _timeout_s: u32) -> Result<(), Self::PollError> {
        if self.next_events.is_empty() {
            Err(())
        } else {
            Ok(())
        }
    }

    async fn handle_response(&mut self) -> Result<mqtt::Event, ()> {
        Ok(self.next_events.pop_front().unwrap())
    }

    async fn download_file(&mut self, _url: &str) -> Result<(), Error<Self::ClientError>> {
        todo!()
    }

    async fn read_file_chunk(
        &mut self,
        _chunk_size: usize,
    ) -> Result<heapless::Vec<u8, MAX_FILE_CHUNK_LEN>, FileError> {
        todo!()
    }
}

// These tests exercise only the inherent protocol surface (connect/receive), not
// status publishing, so the status payload type is irrelevant here.
fn make_client(mqtt: &mut MockMQTT) -> Client<&mut MockMQTT, ()> {
    Client::new(mqtt, UIDString::try_from("mock").unwrap())
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// connect() subscribes to common topics and publishes Online status.
#[tokio::test]
async fn connect_publishes_online() {
    simple_logger::init().ok();

    let mut mqtt = MockMQTT::new();
    let mut client = make_client(&mut mqtt);

    client.connect(Milliseconds(12345678901234)).await.unwrap();

    // The first (and only) published message should be the Online status.
    let (topic, _payload) = mqtt.sent.pop_front().expect("expected Online publish");
    assert_eq!(topic, "f/mock/online");
    assert!(mqtt.sent.is_empty(), "unexpected extra publishes");
}

/// Receiving a Command message yields ClientEvent::CommandReceived.
#[tokio::test]
async fn receive_command() {
    simple_logger::init().ok();

    let mut mqtt = MockMQTT::new();
    mqtt.push_message("t/mock/commands", b"{\"cmd\": \"blink\"}");

    let mut client = make_client(&mut mqtt);
    client.await_response(10).await.unwrap();
    let event = client.handle_response().await.unwrap();

    match event {
        ClientEvent::CommandReceived(cmd) => assert_eq!(cmd, cmd::Cmd::Blink),
        _ => panic!("unexpected event: {event:?}"),
    }
}

/// Receiving a Time message yields ClientEvent::TimestampReceived with the correct value.
#[tokio::test]
async fn receive_time_sync() {
    simple_logger::init().ok();

    let ts = time::Timestamp { time: 99_000_000 };
    let payload = ts.serialize_topic_payload().unwrap();

    let mut mqtt = MockMQTT::new();
    mqtt.push_message("t/mock/time", &payload);

    let mut client = make_client(&mut mqtt);
    client.await_response(10).await.unwrap();
    let event = client.handle_response().await.unwrap();

    match event {
        ClientEvent::TimestampReceived(t) => assert_eq!(t, 99_000_000),
        _ => panic!("unexpected event: {event:?}"),
    }
}

/// An unknown / unparseable topic is silently ignored (returns None).
#[tokio::test]
async fn unknown_topic_ignored() {
    simple_logger::init().ok();

    let mut mqtt = MockMQTT::new();
    mqtt.push_message("t/mock/unknown_future_topic", b"{}");

    let mut client = make_client(&mut mqtt);
    client.await_response(10).await.unwrap();
    let event = client.handle_response().await;

    assert!(
        event.is_none(),
        "expected None for unknown topic, got {event:?}"
    );
}

/// A driver disconnect event yields ClientEvent::Disconnected.
#[tokio::test]
async fn driver_disconnect_event() {
    simple_logger::init().ok();

    let mut mqtt = MockMQTT::new();
    mqtt.next_events.push_back(mqtt::Event::Disconnected);

    let mut client = make_client(&mut mqtt);
    client.await_response(10).await.unwrap();
    let event = client.handle_response().await.unwrap();

    assert!(matches!(event, ClientEvent::Disconnected));
}
