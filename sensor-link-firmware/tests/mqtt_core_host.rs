//! Host integration tests for the quectel-ppp MQTT core against a real
//! mosquitto broker over plain TCP.
//!
//! Broker: `tests/scripts/host-mqtt-broker.sh start` (docker, localhost:18884),
//! then `cargo test --features quectel-ppp,test-mono -- --ignored`.
//! The tests are `#[ignore]`d so suites without docker stay green.
#![cfg(all(feature = "quectel-ppp", feature = "test-mono"))]

use embedded_io_adapters::tokio_1::FromTokio;
use heapless::{String as HString, Vec as HVec};
use tokio::net::TcpStream;

use sensor_link_firmware::drivers::quectel_ppp::mqtt_core::{MqttCore, Pending, RECEIVE_BUF_LEN};
use sensor_link_firmware::mqtt::Will;
use sensor_link_firmware::std_monotonic_driver;

const BROKER: &str = "127.0.0.1:18884";

fn init() {
    simple_logger::init().ok();
    std_monotonic_driver::start();
}

async fn transport() -> FromTokio<TcpStream> {
    let stream = TcpStream::connect(BROKER)
        .await
        .expect("broker not running — tests/scripts/host-mqtt-broker.sh start");
    stream.set_nodelay(true).unwrap();
    FromTokio::new(stream)
}

fn will(topic: &str) -> Will {
    Will {
        topic: HString::try_from(topic).unwrap(),
        payload: HString::try_from("gone").unwrap(),
    }
}

macro_rules! core {
    ($storage:ident, $bump:ident, $core:ident) => {
        let mut $storage = [0u8; RECEIVE_BUF_LEN];
        let mut $bump = rust_mqtt_bump(&mut $storage);
        let mut $core = MqttCore::new(&mut $bump);
    };
}

fn rust_mqtt_bump(storage: &mut [u8]) -> sensor_link_firmware::drivers::quectel_ppp::mqtt_core::ReceiveBuffer<'_> {
    sensor_link_firmware::drivers::quectel_ppp::mqtt_core::ReceiveBuffer::new(storage)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized broker"]
async fn connect_subscribe_publish_receive() {
    init();
    core!(s1, b1, sub);
    core!(s2, b2, publ);

    sub.connect(transport().await, "host-sub", &will("w/sub"), true)
        .await
        .unwrap();
    sub.subscribe("t/host/data").await.unwrap();

    publ.connect(transport().await, "host-pub", &will("w/pub"), true)
        .await
        .unwrap();
    publ.publish("t/host/data", b"{\"v\":42}").await.unwrap();

    let pending = sub.await_event(10).await.expect("message before timeout");
    assert_eq!(pending, Pending::Message);
    let message = sub.take_message().unwrap();
    assert_eq!(message.topic.as_str(), "t/host/data");
    assert_eq!(message.payload.as_slice(), b"{\"v\":42}");

    publ.disconnect().await;
    sub.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized broker"]
async fn publish_larger_than_qmt_limit() {
    // The old modem path capped publishes at 1500 bytes; the own stack must
    // clear that limit (bounded only by our TX buffers).
    init();
    core!(s1, b1, publ);
    publ.connect(transport().await, "host-big", &will("w/big"), true)
        .await
        .unwrap();
    let payload = [0x55u8; 8 * 1024];
    publ.publish("t/host/big", &payload).await.unwrap();
    publ.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized broker"]
async fn will_is_published_on_ungraceful_disconnect() {
    init();
    core!(s1, b1, sub);
    sub.connect(transport().await, "host-will-sub", &will("w/s"), true)
        .await
        .unwrap();
    sub.subscribe("w/victim").await.unwrap();

    {
        core!(s2, b2, victim);
        victim
            .connect(transport().await, "host-victim", &will("w/victim"), true)
            .await
            .unwrap();
        // Dropped without DISCONNECT: the TCP socket closes and the broker
        // must publish the will.
    }

    let pending = sub.await_event(15).await.expect("will before timeout");
    assert_eq!(pending, Pending::Message);
    let message = sub.take_message().unwrap();
    assert_eq!(message.topic.as_str(), "w/victim");
    assert_eq!(message.payload.as_slice(), b"gone");
    sub.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized broker"]
async fn await_event_times_out_cleanly() {
    init();
    core!(s1, b1, idle);
    idle.connect(transport().await, "host-idle", &will("w/idle"), true)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert!(idle.await_event(2).await.is_err());
    let elapsed = started.elapsed().as_secs_f32();
    assert!((1.5..10.0).contains(&elapsed), "timeout took {elapsed}s");
    idle.disconnect().await;
}

/// poll_header cancellation chaos: cancel `await_event` at random points
/// while a publisher streams sequenced messages; every message must still
/// come through exactly once and in order (QoS 1, single connection).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized broker"]
async fn cancellation_chaos_preserves_stream() {
    use rand::Rng;
    const N: usize = 50;

    init();
    core!(s1, b1, sub);
    sub.connect(transport().await, "host-chaos-sub", &will("w/cs"), true)
        .await
        .unwrap();
    sub.subscribe("t/chaos").await.unwrap();

    // BumpBuffer is !Send, so publisher and subscriber run as joined futures
    // on the same task instead of spawned tasks.
    let publisher = async {
        core!(s2, b2, publ);
        publ.connect(transport().await, "host-chaos-pub", &will("w/cp"), true)
            .await
            .unwrap();
        for i in 0..N {
            let mut payload = HVec::<u8, 16>::new();
            payload.extend_from_slice(&(i as u32).to_le_bytes()).unwrap();
            publ.publish("t/chaos", &payload).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        publ.disconnect().await;
    };

    let subscriber = async {
        let mut received = Vec::new();
        let mut rng = rand::rng();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while received.len() < N && std::time::Instant::now() < deadline {
            // Randomly cancel the wait mid-flight: this must never corrupt the
            // packet stream (poll_header is the only await crossing the select).
            let cancel_after_ms: u64 = rng.random_range(1..20);
            tokio::select! {
                r = sub.await_event(5) => {
                    if r == Ok(Pending::Message) {
                        while let Some(m) = sub.take_message() {
                            let n = u32::from_le_bytes(m.payload.as_slice().try_into().unwrap());
                            received.push(n);
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(cancel_after_ms)) => {}
            }
        }
        received
    };

    let ((), received) = tokio::join!(publisher, subscriber);

    let expected: Vec<u32> = (0..N as u32).collect();
    assert_eq!(received, expected, "lost or reordered messages");
    sub.disconnect().await;
}
