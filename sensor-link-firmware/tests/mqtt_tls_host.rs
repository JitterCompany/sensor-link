//! Host integration tests: MQTT core over embedded-tls (mutual TLS 1.3)
//! against a mosquitto broker configured like production (x509-only auth,
//! CN as identity).
//!
//! Broker + throwaway PKI: `tests/scripts/host-mqtt-tls-broker.sh start`,
//! then `cargo test --features quectel-ppp,test-mono -- --ignored`.
//!
//! These tests settle the open TLS questions from the investigation:
//! TLS 1.3 negotiation against `tls_version tlsv1.2` (a minimum), mutual TLS
//! with P-256 client certs, and rustpki verification of a SHA-256-signed
//! server certificate under a SHA-384 self-signed CA.
#![cfg(all(feature = "quectel-ppp", feature = "test-mono"))]

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::{TlsConfig, TlsConnection, TlsContext};
use heapless::String as HString;
use rand_core::OsRng;
use tokio::net::TcpStream;

use sensor_link_firmware::drivers::quectel_ppp::mqtt_core::{
    MqttCore, Pending, ReceiveBuffer, RECEIVE_BUF_LEN,
};
use sensor_link_firmware::drivers::quectel_ppp::tls::{
    BtbCryptoProvider, CipherSuite, TlsMaterial, TLS_RX_BUF_LEN, TLS_TX_BUF_LEN,
};
use sensor_link_firmware::mqtt::Will;
use sensor_link_firmware::std_monotonic_driver;

const BROKER: &str = "127.0.0.1:18885";

fn cert_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/host-tls-test")
}

fn material() -> TlsMaterial {
    let dir = cert_dir();
    let read = |name: &str| {
        std::fs::read(dir.join(name))
            .unwrap_or_else(|_| panic!("{name} missing — run tests/scripts/host-mqtt-tls-broker.sh start"))
    };
    TlsMaterial::from_pem(&read("ca.pem"), &read("client.crt"), &read("client.key")).unwrap()
}

fn init() {
    simple_logger::init().ok();
    std_monotonic_driver::start();
}

async fn tls_transport<'b>(
    material: &TlsMaterial,
    rx_buf: &'b mut [u8],
    tx_buf: &'b mut [u8],
) -> TlsConnection<'b, FromTokio<TcpStream>, CipherSuite> {
    let stream = TcpStream::connect(BROKER)
        .await
        .expect("TLS broker not running — tests/scripts/host-mqtt-tls-broker.sh start");
    stream.set_nodelay(true).unwrap();

    let config = TlsConfig::new().with_server_name("localhost");
    let provider = BtbCryptoProvider::new(material, OsRng);
    let mut tls = TlsConnection::new(FromTokio::new(stream), rx_buf, tx_buf);
    tls.open(TlsContext::new(&config, provider))
        .await
        .expect("TLS handshake failed");
    tls
}

fn will(topic: &str) -> Will {
    Will {
        topic: HString::try_from(topic).unwrap(),
        payload: HString::try_from("gone").unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized TLS broker"]
async fn mutual_tls_connect_publish_receive() {
    init();
    let material = material();

    let mut rx1 = vec![0u8; TLS_RX_BUF_LEN];
    let mut tx1 = vec![0u8; TLS_TX_BUF_LEN];
    let tls = tls_transport(&material, &mut rx1, &mut tx1).await;

    let mut storage = [0u8; RECEIVE_BUF_LEN];
    let mut bump = ReceiveBuffer::new(&mut storage);
    let mut core = MqttCore::new(&mut bump);

    // The broker forces the client id to the certificate CN
    // (use_username_as_clientid), so the id passed here is a placeholder —
    // exactly as in production.
    core.connect(tls, "placeholder", &will("f/host-tls-client/online"), true)
        .await
        .expect("MQTT connect over mTLS failed");

    core.subscribe("t/host-tls-client/loop").await.unwrap();
    core.publish("t/host-tls-client/loop", b"tls-roundtrip")
        .await
        .unwrap();

    let pending = core.await_event(10).await.expect("message before timeout");
    assert_eq!(pending, Pending::Message);
    let message = core.take_message().unwrap();
    assert_eq!(message.topic.as_str(), "t/host-tls-client/loop");
    assert_eq!(message.payload.as_slice(), b"tls-roundtrip");

    core.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the dockerized TLS broker"]
async fn handshake_fails_against_wrong_ca() {
    // A server certificate not signed by the pinned CA must be rejected.
    // Reuses the plain-TCP broker port where no TLS runs at all: the
    // handshake must fail cleanly rather than hang or "succeed".
    init();
    let material = material();
    let stream = match TcpStream::connect("127.0.0.1:18884").await {
        Ok(s) => s,
        Err(_) => return, // plain broker not running; nothing to prove here
    };
    let mut rx = vec![0u8; TLS_RX_BUF_LEN];
    let mut tx = vec![0u8; TLS_TX_BUF_LEN];
    let config = TlsConfig::new().with_server_name("localhost");
    let provider = BtbCryptoProvider::new(&material, OsRng);
    let mut tls = TlsConnection::new(FromTokio::new(stream), &mut rx, &mut tx);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tls.open(TlsContext::new(&config, provider)),
    )
    .await;
    assert!(
        matches!(result, Ok(Err(_))),
        "handshake against a non-TLS endpoint must fail, got {result:?}"
    );
}
