mod sniff;

use std::{
    io::{stdout, Write},
    path::PathBuf,
    process,
    time::Duration,
};

use clap::Parser;
use rumqttc::{
    AsyncClient, Event, EventLoop, MqttOptions, Outgoing, Packet, QoS, TlsConfiguration, Transport,
};
use sensor_link_protocol::{
    cmd::{Cmd, CommandPayload},
    fwupdate::{FWAnnounce, FWUpdateURL},
    time::Timestamp,
    TopicToDevice,
};
use simplelog::*;
use tokio::sync::mpsc;

/// How long to wait for the broker to acknowledge a publish.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay before the event loop retries a failed connection.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Sensor Link console
///
/// Publishes commands to a device's MQTT topics, read line by line from stdin,
/// and prints what the device publishes.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// ID of the device to publish to.
    #[arg(short, long)]
    device_id: String,

    /// MQTT broker hostname.
    #[arg(long, env = "MQTT_BROKER_HOST_NAME", default_value = "localhost")]
    broker_host: String,

    /// MQTT broker port.
    #[arg(long, env = "MQTT_BROKER_PORT", default_value_t = 1883)]
    broker_port: u16,

    /// Path to the CA certificate. Enables TLS.
    #[arg(long, requires_all = ["client_cert", "client_key"])]
    cacert: Option<PathBuf>,

    /// Path to the client certificate (requires --cacert).
    #[arg(long, requires = "cacert")]
    client_cert: Option<PathBuf>,

    /// Path to the client certificate's private key (requires --cacert).
    #[arg(long, requires = "cacert")]
    client_key: Option<PathBuf>,

    /// Print received payloads as-is instead of decoding them.
    #[arg(long, default_value_t = false)]
    raw: bool,

    /// Log level.
    #[arg(short, long, default_value = "info")]
    log_level: String,
}

/// Parse a log level, falling back to [`LevelFilter::Off`].
fn level_filter(flag: &str, name: &str) -> LevelFilter {
    match name.to_lowercase().as_str() {
        "off" => LevelFilter::Off,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        other => {
            eprintln!(
                "Warning: '{flag}' must be one of off, error, warn, info, debug, trace \
                 (got '{other}'); logging is off."
            );
            LevelFilter::Off
        }
    }
}

/// Read a line from stdin, or `None` once stdin is closed.
async fn read_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line).unwrap() {
            0 => None,
            _ => Some(line),
        }
    })
    .await
    .unwrap()
}

fn read_file(flag: &str, path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|err| {
        eprintln!("Error: failed to read {flag} file {path:?}: {err}");
        process::exit(1);
    })
}

/// A message to publish: topic and JSON payload.
struct Message {
    topic: String,
    payload: String,
}

impl Message {
    fn new(topic: TopicToDevice, device_id: &str, payload: String) -> Message {
        let topic = topic
            .to_topic_string(device_id)
            .expect("Device ID was validated at startup");
        Message {
            topic: sensor_link_protocol::to_string(topic),
            payload,
        }
    }

    fn command(device_id: &str, cmd: Cmd) -> Message {
        let payload = serde_json::to_string(&CommandPayload { cmd }).unwrap();
        Message::new(TopicToDevice::Command, device_id, payload)
    }

    fn time(device_id: &str) -> Message {
        let payload = serde_json::to_string(&Timestamp {
            time: chrono::Utc::now().timestamp_micros(),
        })
        .unwrap();
        Message::new(TopicToDevice::Time, device_id, payload)
    }

    fn fw_update(device_id: &str, url: &str) -> Result<Message, String> {
        let url = FWUpdateURL::try_from(url)
            .map_err(|_| format!("URL is longer than {} bytes", FWUpdateURL::new().capacity()))?;
        // The device doesn't act on a scheduled time, so it's never sent.
        let payload = serde_json::to_string(&FWAnnounce {
            url,
            timestamp: None,
        })
        .unwrap();
        Ok(Message::new(
            TopicToDevice::FWUpdateAnnounce,
            device_id,
            payload,
        ))
    }
}

fn prompt(device_id: &str) {
    print!("{device_id}> ");
    stdout().flush().unwrap();
}

/// Rumqttc event loop: prints what the device publishes, forwards publish acks
/// and ends after a disconnect.
async fn run_event_loop(
    mut event_loop: EventLoop,
    client: AsyncClient,
    device_id: String,
    raw: bool,
    ack_tx: mpsc::Sender<()>,
) {
    let filter = format!("f/{device_id}/#");
    loop {
        match event_loop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                log::info!(target: "mqtt", "Connected, subscribing to {filter}");
                // The session is clean, so subscribe again on every connect.
                // Not `subscribe().await`: its request is queued for this very
                // loop, which can't drain the queue while waiting on it.
                if let Err(err) = client.try_subscribe(&filter, QoS::AtLeastOnce) {
                    log::error!(target: "mqtt", "Subscribe failed: {err}");
                }
            }
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                println!();
                println!(
                    "{}",
                    sniff::format_message(&publish.topic, &publish.payload, raw)
                );
                println!();
                prompt(&device_id);
            }
            Ok(Event::Incoming(Packet::PubAck(_))) => {
                ack_tx.try_send(()).ok();
            }
            Ok(Event::Outgoing(Outgoing::Disconnect)) => {
                return;
            }
            Ok(event) => {
                log::trace!(target: "mqtt", "{event:?}");
            }
            Err(err) => {
                log::error!(target: "mqtt", "Connection error: {err}");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

/// Publish a message and wait for the broker to acknowledge it.
async fn publish(client: &AsyncClient, ack_rx: &mut mpsc::Receiver<()>, message: Message) {
    // Only one publish is in flight at a time, so any ack left over belongs to
    // an earlier publish that timed out.
    while ack_rx.try_recv().is_ok() {}

    log::info!(target: "main", "Publishing to {}: {}", message.topic, message.payload);
    if let Err(err) = client
        .publish(message.topic, QoS::AtLeastOnce, false, message.payload)
        .await
    {
        log::error!(target: "main", "Publish failed: {err}");
        return;
    }

    match tokio::time::timeout(ACK_TIMEOUT, ack_rx.recv()).await {
        Ok(Some(())) => log::info!(target: "main", "Acknowledged by broker"),
        _ => log::warn!(
            target: "main",
            "Not acknowledged by broker within {}s",
            ACK_TIMEOUT.as_secs()
        ),
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let log_level = level_filter("--log-level", &args.log_level);
    let log_cfg = ConfigBuilder::new()
        .set_target_level(log_level)
        .set_thread_level(LevelFilter::Off)
        .set_location_level(LevelFilter::Off)
        .build();
    let _ = SimpleLogger::init(log_level, log_cfg);

    if let Err(err) = TopicToDevice::Command.to_topic_string(&args.device_id) {
        eprintln!("Error: invalid device ID '{}': {err:?}", args.device_id);
        process::exit(1);
    }

    // A unique client ID, so this tool never takes over the session of another
    // client (such as the server) connected to the same broker.
    let client_id = format!("sensor-link-console-{}", process::id());
    let mut mqttoptions = MqttOptions::new(client_id, &args.broker_host, args.broker_port);
    // Room for large sensor data messages from the device.
    mqttoptions.set_max_packet_size(15_000_000, 100_000);

    let transport = match (&args.cacert, &args.client_cert, &args.client_key) {
        (Some(ca), Some(cert), Some(key)) => {
            log::info!(target: "mqtt", "Using TLS with client cert {cert:?} and key {key:?}");
            Transport::Tls(TlsConfiguration::Simple {
                ca: read_file("--cacert", ca),
                alpn: None,
                client_auth: Some((
                    read_file("--client-cert", cert),
                    read_file("--client-key", key),
                )),
            })
        }
        _ => {
            log::warn!(target: "mqtt", "Note: Not using TLS");
            Transport::Tcp
        }
    };
    mqttoptions.set_transport(transport);

    log::info!(
        target: "mqtt",
        "Connecting to broker: {}:{}",
        args.broker_host,
        args.broker_port
    );
    let (client, event_loop) = AsyncClient::new(mqttoptions, 10);
    let (ack_tx, mut ack_rx) = mpsc::channel(10);
    let event_loop_handle = tokio::spawn(run_event_loop(
        event_loop,
        client.clone(),
        args.device_id.clone(),
        args.raw,
        ack_tx,
    ));

    let device_id = args.device_id.as_str();
    loop {
        println!();
        prompt(device_id);
        let Some(line) = read_line().await else {
            // Stdin closed: commands were piped in and all have been sent.
            break;
        };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let message = match parts.as_slice() {
            [] => continue,
            ["start"] => Message::command(device_id, Cmd::Start),
            ["stop"] => Message::command(device_id, Cmd::Stop),
            ["blink"] => Message::command(device_id, Cmd::Blink),
            ["reboot"] => Message::command(device_id, Cmd::Reboot),
            ["time"] => Message::time(device_id),
            ["fwupdate", url] => match Message::fw_update(device_id, url) {
                Ok(message) => message,
                Err(err) => {
                    println!("Error: {err}");
                    continue;
                }
            },
            ["q"] => {
                println!("Exit now");
                break;
            }
            other => {
                println!(
                    "Error, didn't understand '{other:?}'. \
                     Commands: start, stop, blink, reboot, time, fwupdate <url>, q"
                );
                continue;
            }
        };
        publish(&client, &mut ack_rx, message).await;
    }

    // Disconnect cleanly, but don't hang on a broker that isn't there.
    client.disconnect().await.ok();
    tokio::time::timeout(Duration::from_secs(1), event_loop_handle)
        .await
        .ok();
}
