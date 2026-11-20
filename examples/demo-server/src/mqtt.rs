//! MQTT client task, modeled on Jitter's sensor-mqtt dual-task pattern:
//! one supervised task owns the AsyncClient, a second owns EventLoop::poll()
//! (they must be separate tasks to avoid a rumqttc deadlock, see
//! https://rumqtt.bytebeam.io/docs/rumqttc/FAQs).
//!
//! Publishes retained "online"/"offline" to `server/status` (LastWill covers
//! ungraceful disconnects) and reports connection state on a watch channel.

mod codec;
pub mod control_out;
pub mod processing;

use std::sync::Arc;

use sensor_link_mqtt::{MqttConfig, MqttHooks, MqttTaskParams, TlsConfig};
use sensor_link_server_core::store_traits::DeviceStore;
use task_supervisor::{Handle, PanicCallback};
use tokio::sync::{mpsc, watch, Mutex};

use crate::{
    mqtt::{codec::DemoTopicCodec, control_out::DemoControlOut, processing::DemoProcessingIn},
    AppEventStore, DataType,
};

pub type ControlMessageIn = sensor_link_mqtt::ControlMessageIn<
    demo_protocol::DeviceType,
    demo_protocol::status::DeviceStatus,
    demo_protocol::event::Event,
>;
pub type ControlMessageOut = sensor_link_mqtt::ControlMessageOut<DemoControlOut>;
pub type ProcessingMessage = sensor_link_mqtt::ProcessingMessage<DataType, DemoProcessingIn>;

/// Reads MQTT_BROKER_HOST_NAME / MQTT_BROKER_PORT and, when all of
/// MQTT_CA_CERT_PATH / MQTT_CLIENT_CERT_PATH / MQTT_CLIENT_KEY_PATH are
/// set, the certificate files for mTLS. TLS is a config-only difference.
fn build_config_from_env() -> anyhow::Result<MqttConfig> {
    let tls = match (
        std::env::var("MQTT_CA_CERT_PATH"),
        std::env::var("MQTT_CLIENT_CERT_PATH"),
        std::env::var("MQTT_CLIENT_KEY_PATH"),
    ) {
        (Ok(ca), Ok(cert), Ok(key)) => Some(TlsConfig {
            ca_cert: std::fs::read(ca)?,
            client_cert: std::fs::read(cert)?,
            client_key: std::fs::read(key)?,
        }),
        _ => None,
    };
    Ok(MqttConfig {
        hostname: crate::get_mqtt_broker_host_name(),
        port: crate::get_mqtt_broker_port(),
        tls,
    })
}

pub fn start_task<DS>(
    status_tx: watch::Sender<bool>,
    data_tx: mpsc::Sender<ProcessingMessage>,
    device_tx: mpsc::Sender<ControlMessageIn>,
    rx: mpsc::Receiver<ControlMessageOut>,
    db: DS,
    on_task_panic: PanicCallback,
) -> anyhow::Result<Handle>
where
    DS: AppEventStore + DeviceStore + Clone + Send + 'static,
{
    Ok(sensor_link_mqtt::start_task::<DemoTopicCodec, DS>(
        build_config_from_env()?,
        MqttTaskParams {
            data_tx,
            device_tx,
            msg_out: Arc::new(Mutex::new(rx)),
            db,
            on_task_panic,
            additional_topics: vec![],
            hooks: MqttHooks {
                connection_status_tx: Some(status_tx),
                ..Default::default()
            },
        },
    ))
}
