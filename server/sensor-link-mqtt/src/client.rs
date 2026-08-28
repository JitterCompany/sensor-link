use chrono::Utc;
use sensor_link_protocol::{
    event::{Event, EventPayload},
    parse_system_topic, parse_topic_from_device,
    server::{parse_device_info_v2, parse_device_info_v3, parse_online},
    server_status::ServerStatus,
    sms::{self, format_request, SMSResult},
    time::Timestamp,
    SystemTopic, TopicFromDevice, TopicParseError, TopicParts, TopicToDevice,
};
use serde::de::DeserializeOwned;
use tokio::sync::{
    mpsc,
    watch::{self, Receiver},
    Mutex,
};

use std::{marker::PhantomData, sync::Arc, time::Duration};

use rumqttc::{
    AsyncClient, EventLoop, LastWill, MqttOptions, Packet, Publish, QoS, TlsConfiguration,
    Transport,
};
use sensor_link_server_core::{
    event::SendStatus,
    sensor_server_log::SensorServerLog,
    store_traits::{DeviceStore, EventStore},
};
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};

pub type ConnectionErrorCallback = Arc<dyn Fn(&rumqttc::ConnectionError) + Send + Sync + 'static>;

/// Optional observability hooks for the MQTT client task.
///
/// Bundles the callbacks and watchers an embedder may want to attach to the
/// client. Adding a new hook here does not change [`start_task`]'s signature,
/// and — because the struct derives [`Default`] — callers that don't need a
/// given hook (or any) don't have to be updated when new fields are added.
#[derive(Default, Clone)]
pub struct MqttHooks {
    /// Invoked when the event loop hits a connection error. Deduplicated: only
    /// the first error of a disconnect streak is reported.
    pub on_connection_error: Option<ConnectionErrorCallback>,
    /// Receives the client's connection status whenever it changes (`true` when
    /// connected, `false` when disconnected), so external code can observe
    /// connectivity via a [`watch::Receiver`].
    pub connection_status_tx: Option<watch::Sender<bool>>,
}

use crate::{
    metrics, ControlMessageIn, ControlMessageOut, DeviceControlIn, DeviceControlOut, MqttDataType,
    ParsedMqttIn, ParsedMqttInFor, ProcessingIn, ProcessingMessage, SystemMessageIn, TopicCodec,
};

pub fn print_error(component: &str, message: &str, error: &str) {
    tracing::error!("[{component}] {message}: {error}");
}

pub struct TlsConfig {
    pub ca_cert: Vec<u8>,
    pub client_cert: Vec<u8>,
    pub client_key: Vec<u8>,
}

pub struct MqttConfig {
    pub hostname: String,
    pub port: u16,
    pub tls: Option<TlsConfig>,
}

const MQTT_TOPICS: [&str; 8] = [
    "f/#",
    "qmonixx/+/strain_bulk/+",
    "qmonixx/+/temperature_bulk/+",
    "qmonixx/+/strain/#",
    "qmonixx/+/temperature/#",
    "qmonixx/+/s/#",
    "qmonixx/+/T/#",
    "sms/resp",
];

async fn mqtt_subscribe(client: &AsyncClient) -> Result<(), Vec<rumqttc::ClientError>> {
    let mut errors = Vec::new();
    for topic in MQTT_TOPICS {
        if let Err(err) = client.subscribe(topic, QoS::AtLeastOnce).await {
            errors.push(err);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn start_task<C, DS>(
    config: MqttConfig,
    data_tx: mpsc::Sender<ProcessingMessage<C::DT, C::P>>,
    device_tx: mpsc::Sender<ControlMessageIn<C::D, C::S, C::EV>>,
    rx: mpsc::Receiver<ControlMessageOut<C::ControlOut>>,
    db: DS,
    on_task_panic: PanicCallback,
    hooks: MqttHooks,
) -> Handle
where
    C: TopicCodec,
    DS: EventStore + DeviceStore + Clone + Send + 'static,
{
    let port = config.port;
    let mut mqttoptions = MqttOptions::new(
        "rust-mqtt-client", // may be ignored and replaced with username depending on broker config
        &config.hostname,
        port,
    );
    mqttoptions.set_clean_session(false); // Ensures we will receive messages from when we were offline
    mqttoptions.set_keep_alive(Duration::from_secs(5));

    mqttoptions.set_max_packet_size(15_000_000, 100_000);
    tracing::info!("Connecting to broker: {mqttoptions:?}");

    let transport = match config.tls {
        Some(tls) => Transport::Tls(TlsConfiguration::Simple {
            ca: tls.ca_cert,
            alpn: None,
            client_auth: Some((tls.client_cert, tls.client_key)),
        }),
        None => {
            tracing::warn!("MQTT: not using TLS");
            Transport::Tcp
        }
    };
    mqttoptions.set_transport(transport);

    tracing::info!("Connecting to MQTT broker: {:?}", mqttoptions);

    let task_function = mqtt_task::<DS, C>;
    let rx = Arc::new(Mutex::new(rx));
    let on_panic_clone = on_task_panic.clone();
    Handle::new(
        move |shutdown_rx| {
            task_function(
                shutdown_rx,
                MqttTaskParams {
                    mqttoptions: mqttoptions.clone(),
                    data_tx: data_tx.clone(),
                    device_tx: device_tx.clone(),
                    msg_out: rx.clone(),
                    db: db.clone(),
                    on_task_panic: on_panic_clone.clone(),
                    hooks: hooks.clone(),
                },
            )
        },
        get_crate_relative_function_path(task_function),
        on_task_panic,
    )
}

/// Dependencies for [`mqtt_task`], bundled to keep its argument list manageable.
struct MqttTaskParams<DS, C: TopicCodec> {
    mqttoptions: MqttOptions,
    data_tx: mpsc::Sender<ProcessingMessage<C::DT, C::P>>,
    device_tx: mpsc::Sender<ControlMessageIn<C::D, C::S, C::EV>>,
    msg_out: Arc<Mutex<mpsc::Receiver<ControlMessageOut<C::ControlOut>>>>,
    db: DS,
    on_task_panic: PanicCallback,
    hooks: MqttHooks,
}

async fn mqtt_task<DS, C: TopicCodec>(mut shutdown_rx: Receiver<()>, params: MqttTaskParams<DS, C>)
where
    DS: EventStore + DeviceStore + Clone + Send + 'static,
{
    let MqttTaskParams {
        mut mqttoptions,
        data_tx,
        device_tx,
        msg_out,
        db,
        on_task_panic,
        hooks,
    } = params;

    let mut msg_out = msg_out
        .try_lock()
        .expect("Message out receiver seems to be locked by another task then MQTT task");

    // Will is sent when the clients disconnects ungracefully.
    // During a graceful shutdown, the server will send the will manually.
    // The message is retained so that sensors will also receive it when they connect after the server goes offline.
    let lastwill = LastWill::new(
        SystemTopic::ServerStatus.as_str(),
        ServerStatus::Offline.serialize(),
        QoS::AtLeastOnce,
        true,
    );
    mqttoptions.set_last_will(lastwill);

    // Capacity of channel between client and eventloop
    const CAPACITY: usize = 128;
    let (client, eventloop) = AsyncClient::new(mqttoptions, CAPACITY);

    let (connect_tx, mut connect_rx) = watch::channel(());

    // Start event loop task
    // Note that EventLoop *must* run in a different task than client to avoid deadlocks.
    // See https://rumqtt.bytebeam.io/docs/rumqttc/FAQs#program-is-stuck-forever-on-publish--subscribe
    let event_loop_handle = {
        let in_handler = InHandler::<DS, C> {
            data_tx,
            device_tx,
            db: db.clone(),
        };
        let eventloop = Arc::new(Mutex::new(eventloop));
        let connect_tx = Arc::new(Mutex::new(connect_tx));
        let task_func = mqtt_event_loop_task;
        Handle::new(
            move |shutdown_rx| {
                task_func(
                    shutdown_rx,
                    connect_tx.clone(),
                    eventloop.clone(),
                    in_handler.clone(),
                    hooks.clone(),
                )
            },
            "sensor_mqtt::client::mqtt_event_loop_task",
            on_task_panic,
        )
    };

    let errors = mqtt_subscribe(&client).await;
    if let Err(errors) = errors {
        print_error(
            "MQTT client",
            "CRITICAL ERROR: Failed to subscribe to topics",
            &format!("{:?}", errors),
        );
        let _ = client.disconnect().await;
        return;
    }

    if let Err(err) = publish_server_status(&client, ServerStatus::Ok).await {
        print_error(
            "MQTT client",
            "CRITICAL ERROR: Failed to publish ServerStatus::Ok",
            &err.to_string(),
        );
        let _ = client.disconnect().await;
        return;
    }

    loop {
        tokio::select! {

            _ = shutdown_rx.changed() => {
                break;
            }

            _ = connect_rx.changed() => {
                tracing::debug!("mqtt_task: connected(?), subscribing...");
                if let Err(errors) = mqtt_subscribe(&client).await {
                    print_error(
                        "MQTT client",
                        "CRITICAL ERROR: Failed to (re)-subscribe to topics",
                        &format!("{:?}", errors),
                    );
                    let _ = client.disconnect().await;
                    break;
                }
            }

            Some(message) = msg_out.recv() => {
                let device_id = message.device_id;
                match C::encode_control_out(&device_id, message.payload) {
                    Ok((topic, payload)) => {
                        try_publish_to_device(&db, &client, device_id, topic, payload).await;
                    }
                    Err(err) => {
                        print_error("MQTT client", "while encoding outgoing message", &err);
                        insert_sensor_server_log(&db, device_id, "error".to_string(), &err, "".to_string()).await;
                    }
                }
            }

        }
    }

    if let Err(err) = publish_server_status(&client, ServerStatus::Offline).await {
        print_error(
            "MQTT client",
            "Failed to publish ServerStatus::Offline",
            &err.to_string(),
        );
    }

    // try to disconnect
    let _ = client.disconnect().await;

    // give event loop some time to process the disconnect
    let _ = event_loop_handle
        .shutdown_with_timeout(Duration::from_secs(3))
        .await;
}

async fn publish_server_status(
    client: &AsyncClient,
    status: ServerStatus,
) -> Result<(), rumqttc::ClientError> {
    client
        .publish(
            SystemTopic::ServerStatus.as_str(),
            QoS::AtLeastOnce,
            true,
            status.serialize(),
        )
        .await
}
#[derive(Debug)]
pub struct InHandlerError {
    error: String,
    device_id: String,
    topic: String,
    payload_len: usize,
}
/// Handles incoming MQTT messages
pub trait IncomingHandler: Clone {
    async fn process(&mut self, incoming: Publish) -> Result<(), InHandlerError>;
}

// Abstract over implementation details that are not relevant to
// the event loop in an attempt to make spawning less ugly
struct InHandler<DB, C: TopicCodec>
where
    DB: EventStore + DeviceStore + Clone,
{
    data_tx: mpsc::Sender<ProcessingMessage<C::DT, C::P>>,
    device_tx: mpsc::Sender<ControlMessageIn<C::D, C::S, C::EV>>,
    db: DB,
}
impl<DB, C: TopicCodec> Clone for InHandler<DB, C>
where
    DB: EventStore + DeviceStore + Clone,
{
    fn clone(&self) -> Self {
        Self {
            data_tx: self.data_tx.clone(),
            device_tx: self.device_tx.clone(),
            db: self.db.clone(),
        }
    }
}

impl<DB, C: TopicCodec> IncomingHandler for InHandler<DB, C>
where
    DB: EventStore + DeviceStore + Clone,
{
    async fn process(&mut self, incoming: Publish) -> Result<(), InHandlerError> {
        process_mqtt_topic::<DB, C>(incoming, &self.data_tx, &self.device_tx, &self.db).await
    }
}

async fn mqtt_event_loop_task(
    mut shutdown_rx: Receiver<()>,
    connect_tx: Arc<Mutex<watch::Sender<()>>>,
    eventloop: Arc<Mutex<EventLoop>>,
    mut incoming_handler: impl IncomingHandler,
    hooks: MqttHooks,
) {
    let mut eventloop = eventloop
        .try_lock()
        .expect("event loop seems to be locked by another task then MQTT task");

    let connect_tx = connect_tx
        .try_lock()
        .expect("connection tx wignal seems to be locked by another task??");

    let mut error_detected = false;
    let mut shutdown_requested = false;
    loop {
        // await change on shutdown_rx only if not already requested
        // this prevents a busy loop as shutdown_rx keeps being complete forever
        let shutdown = async {
            if shutdown_requested {
                std::future::pending::<()>().await;
            } else {
                let _ = shutdown_rx.changed().await;
            };
        };

        tokio::select! {

            _ = shutdown => {
                shutdown_requested = true;

                // Error already set, awaiting succesful disconnect is probably a waste of time?
                if error_detected {
                    break;

                // MQTT task should call client.disconnect(), which should eventually trigger eventloop.poll() to result in a connection error
                }
            }

            event = eventloop.poll() => {

                match event {
                    Ok(rumqttc::Event::Incoming(Packet::ConnAck(_))) => {
                        tracing::info!("Connected to MQTT broker");
                        error_detected = false;

                        if let Some(status_tx) = &hooks.connection_status_tx {
                            let _ = status_tx.send(true);
                        }

                        // send a signal to the mqtt_task that a new connection is made
                        let _ = connect_tx.send(());
                    }
                    Ok(rumqttc::Event::Incoming(Packet::Publish(publish))) => {
                        error_detected = false;
                        if let Err(err) = incoming_handler.process(publish)
                            .await {
                                tracing::error!(topic = %err.topic, payload_len = %err.payload_len, device_id = %err.device_id, "Failed to process MQTT message: {:?}", err.error)
                            }
                    }
                    Err(err) => {
                        // Upon shutdown, we expect a disconnect error
                        if shutdown_requested {
                            tracing::debug!("Exit event loop (got {err:?} after shutdown request)");
                            break;
                        }
                        if !error_detected {
                            if let Some(on_connection_error) = &hooks.on_connection_error {
                                on_connection_error(&err);
                            }
                            if let Some(status_tx) = &hooks.connection_status_tx {
                                let _ = status_tx.send(false);
                            }
                            print_error("MQTT client", "while polling", &err.to_string());
                        }
                        // Reduce CPU load while MQTT broker is down
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        error_detected = true;
                    }
                    _other => {
                        error_detected = false
                    },
                }
            }
        }
    }

    // Notify that we're disconnecting
    if let Some(status_tx) = &hooks.connection_status_tx {
        let _ = status_tx.send(false);
    }

    tracing::info!("Exit MQTT event loop task");
}

async fn process_mqtt_topic<DS, C: TopicCodec>(
    publish: Publish,
    data_tx: &mpsc::Sender<ProcessingMessage<C::DT, C::P>>,
    device_tx: &mpsc::Sender<ControlMessageIn<C::D, C::S, C::EV>>,
    db: &DS,
) -> Result<(), InHandlerError>
where
    DS: EventStore + DeviceStore,
{
    tracing::debug!(
        "Topic: {}, payload: {:?} bytes",
        publish.topic,
        publish.payload.len()
    );

    let topic_name = publish
        .topic // Split and take last part {prefix}/{uid}/{topic}
        .splitn(3, '/')
        .nth(2)
        .unwrap_or(&publish.topic);

    metrics::increment_counter(
        "mqtt",
        "mqtt_msg",
        1u64,
        &[
            ("topic", topic_name),
            ("payload_len", &publish.payload.len().to_string()),
        ],
    );

    let parsed_msg = get_mqtt_message::<C>(&publish)?;
    match parsed_msg {
        ParsedMqttIn::Data(msg) => {
            if data_tx.send(msg).await.is_err() {
                tracing::error!("Error sending message to processing task");
            }
        }
        ParsedMqttIn::Control(msg) => {
            match &msg.payload {
                DeviceControlIn::DeviceInfo(_) | DeviceControlIn::DeviceStatus(_) => {
                    insert_sensor_server_log(
                        db,
                        msg.device_id.clone(),
                        "info".to_string(),
                        &publish.topic,
                        String::from_utf8_lossy(&publish.payload).to_string(),
                    )
                    .await;
                }
                DeviceControlIn::DeviceOnline(online) => {
                    let log_msg = match online.since() {
                        Some(_) => "Online",
                        None if online.ungraceful_offline() => "Offline (ungraceful)",
                        None => "Offline",
                    };
                    insert_sensor_server_log(
                        db,
                        msg.device_id.clone(),
                        "info".to_string(),
                        log_msg,
                        "".to_string(),
                    )
                    .await;
                }
                DeviceControlIn::Event(event) => {
                    if let Some(log_type) = C::event_log_type(&event.event) {
                        insert_sensor_server_log(
                            db,
                            msg.device_id.clone(),
                            log_type.to_string(),
                            &publish.topic,
                            String::from_utf8_lossy(&publish.payload).to_string(),
                        )
                        .await;
                    }
                }
                DeviceControlIn::FirmwareUpdateStatus(_) => {
                    insert_sensor_server_log(
                        db,
                        msg.device_id.clone(),
                        "info".to_string(),
                        &publish.topic,
                        String::from_utf8_lossy(&publish.payload).to_string(),
                    )
                    .await;
                }
            }

            if device_tx.send(msg).await.is_err() {
                tracing::error!("Error sending message to device task");
            }
        }
        ParsedMqttIn::System(SystemMessageIn::SMSResponse(sms_response)) => {
            if sms_response.result == SMSResult::Success {
                if let Err(err) = db.event_mark_sent(&sms_response.id, SendStatus::Sent).await {
                    tracing::error!("Failed to mark event as sent: {}", err);
                }
            }
            return Ok(());
        }
    }
    Ok(())
}

fn get_mqtt_message<C: TopicCodec>(
    publish: &Publish,
) -> Result<ParsedMqttInFor<C>, InHandlerError> {
    let full_topic = &publish.topic;
    if let Some(system_topic) = parse_system_topic(full_topic) {
        match system_topic {
            SystemTopic::SMSResponse => {
                let response = sms::parse_response(&publish.payload).ok_or(InHandlerError {
                    error: "Failed to parse SMS response".to_string(),
                    device_id: "SMS Gateway".to_string(),
                    topic: publish.topic.clone(),
                    payload_len: publish.payload.len(),
                })?;
                return Ok(ParsedMqttIn::System(SystemMessageIn::SMSResponse(response)));
            }
            _ => {
                return Err(InHandlerError {
                    error: "Unexpected system topic".to_string(),
                    device_id: "".to_string(),
                    topic: publish.topic.clone(),
                    payload_len: publish.payload.len(),
                })
            }
        }
    }

    let parts = C::parse_topic_from_device(full_topic).map_err(|err| InHandlerError {
        error: format!("Parse topic: {err:?}"),
        device_id: "?".to_string(),
        topic: full_topic.to_string(),
        payload_len: publish.payload.len(),
    })?;

    let device_id = parts.device_id.clone();
    let topic = parts.topic;
    C::parse_payload(&publish.payload, parts).map_err(|err| InHandlerError {
        error: format!("Parse payload: {err:?}"),
        device_id,
        topic: format!("{:?}", topic),
        payload_len: publish.payload.len(),
    })
}

async fn try_publish_to_device(
    db: &impl DeviceStore,
    client: &AsyncClient,
    device_id: String,
    topic: String,
    payload: String,
) {
    let mut log_type = "info".to_string();
    let payload_for_log = payload.clone();

    if let Err(err) = client
        .publish(topic.as_str(), QoS::AtLeastOnce, false, payload)
        .await
    {
        print_error("MQTT client", "while publishing", &err.to_string());
        log_type = "error".to_string();
    }
    insert_sensor_server_log(db, device_id, log_type, topic.as_str(), payload_for_log).await;
}

async fn insert_sensor_server_log(
    db: &impl DeviceStore,
    sensor_id: String,
    log_type: String,
    log_msg: &str,
    payload_for_log: String,
) {
    let _ = db
        .insert_sensor_server_log(SensorServerLog {
            id: None,
            timestamp: Utc::now(),
            sensor_id,
            group_id: None,
            user_id: "auto".to_string(),
            duration: 0,
            _type: log_type,
            header: log_msg.into(),
            body: vec![format!("Payload: {payload_for_log}")],
        })
        .await;
}

/// [TopicCodec] for the topic/payload vocabulary common to every
/// Jitter-manufactured device ([sensor_link_protocol::TopicFromDevice] /
/// [sensor_link_protocol::TopicToDevice]).
///
/// It is generic over the device-info (`D`), status (`S`) and event (`EV`)
/// payload types so that manufacturer-specific codecs can reuse it (by
/// specializing those type parameters) to handle their shared topics.
pub struct JitterTopicCodec<DT, D, S, EV = Event>(CodecMarker<DT, D, S, EV>);

/// Zero-sized marker carrying a codec's type parameters without owning them.
///
/// Uses `fn() -> (…)` so the marker is covariant and imposes no auto-trait or
/// drop obligations on `DT`/`D`/`S`/`EV`.
type CodecMarker<DT, D, S, EV> = PhantomData<fn() -> (DT, D, S, EV)>;

impl<DT, D, S, EV> TopicCodec for JitterTopicCodec<DT, D, S, EV>
where
    DT: MqttDataType,
    D: Send + DeserializeOwned + 'static,
    S: Send + DeserializeOwned + 'static,
    EV: Send + DeserializeOwned + 'static,
{
    type DT = DT;
    type D = D;
    type S = S;
    type EV = EV;
    type P = ProcessingIn;
    type TopicFromDevice = TopicFromDevice;
    type ControlOut = DeviceControlOut;

    fn parse_topic_from_device(
        topic: &str,
    ) -> Result<TopicParts<Self::TopicFromDevice>, TopicParseError> {
        parse_topic_from_device(topic)
    }

    fn parse_payload(
        payload: &[u8],
        parts: TopicParts<Self::TopicFromDevice>,
    ) -> Result<ParsedMqttIn<DT, D, S, EV, ProcessingIn>, String> {
        let TopicParts {
            device_id, topic, ..
        } = parts;
        match topic {
            TopicFromDevice::Online => {
                let online =
                    parse_online(payload).map_err(|err| format!("Parse Online: {err:?}"))?;
                Ok(ParsedMqttIn::Control(ControlMessageIn {
                    device_id,
                    payload: DeviceControlIn::DeviceOnline(online),
                }))
            }
            TopicFromDevice::DeviceInfoV2 => {
                let device_info = parse_device_info_v2(payload)
                    .map_err(|err| format!("Parse Device Info v2: {err:?}"))?;
                Ok(ParsedMqttIn::Control(ControlMessageIn {
                    device_id,
                    payload: DeviceControlIn::DeviceInfo(device_info.into()),
                }))
            }
            TopicFromDevice::DeviceInfoV3 => {
                let device_info = parse_device_info_v3(payload)
                    .map_err(|err| format!("Parse Device Info v3: {err:?}"))?;
                Ok(ParsedMqttIn::Control(ControlMessageIn {
                    device_id,
                    payload: DeviceControlIn::DeviceInfo(device_info),
                }))
            }
            TopicFromDevice::Status => {
                let status = serde_json::from_slice(payload)
                    .map_err(|err| format!("Parse Device Status: {err:?}"))?;
                Ok(ParsedMqttIn::Control(ControlMessageIn {
                    device_id,
                    payload: DeviceControlIn::DeviceStatus(status),
                }))
            }
            TopicFromDevice::Event => {
                let event_payload: EventPayload<EV> = serde_json::from_slice(payload)
                    .map_err(|err| format!("Parse Event: {err:?}"))?;
                Ok(ParsedMqttIn::Control(ControlMessageIn {
                    device_id,
                    payload: DeviceControlIn::Event(event_payload),
                }))
            }
            TopicFromDevice::FWStatus => {
                let fw_status = serde_json::from_slice(payload)
                    .map_err(|err| format!("Parse FW update status: {err:?}"))?;
                Ok(ParsedMqttIn::Control(ControlMessageIn {
                    device_id,
                    payload: DeviceControlIn::FirmwareUpdateStatus(fw_status),
                }))
            }
            other => Err(format!("Unsupported topic: {other:?}")),
        }
    }

    fn encode_control_out(
        device_id: &str,
        out: DeviceControlOut,
    ) -> Result<(String, String), String> {
        match out {
            DeviceControlOut::DeviceCommand(data) => {
                let payload = serde_json::to_string(&data)
                    .map_err(|err| format!("Serialize DeviceCommand: {err}"))?;
                let topic = TopicToDevice::Command
                    .to_topic_string(device_id)
                    .map_err(|err| format!("Encode topic: {err:?}"))?;
                Ok((sensor_link_protocol::to_string(topic), payload))
            }
            DeviceControlOut::FWUpdateAnnounce(data) => {
                let payload = serde_json::to_string(&data)
                    .map_err(|err| format!("Serialize FWUpdateAnnounce: {err}"))?;
                let topic = TopicToDevice::FWUpdateAnnounce
                    .to_topic_string(device_id)
                    .map_err(|err| format!("Encode topic: {err:?}"))?;
                Ok((sensor_link_protocol::to_string(topic), payload))
            }
            DeviceControlOut::Time => {
                let payload = Timestamp {
                    time: Utc::now().timestamp_micros(),
                };
                let payload = serde_json::to_string(&payload)
                    .map_err(|err| format!("Serialize Time: {err}"))?;
                let topic = TopicToDevice::Time
                    .to_topic_string(device_id)
                    .map_err(|err| format!("Encode topic: {err:?}"))?;
                Ok((sensor_link_protocol::to_string(topic), payload))
            }
            DeviceControlOut::SMSRequest(data) => {
                let payload =
                    format_request(&data).map_err(|_| "Serialize SMSRequest".to_string())?;
                Ok((SystemTopic::SMSRequest.as_str().to_string(), payload))
            }
        }
    }
}
