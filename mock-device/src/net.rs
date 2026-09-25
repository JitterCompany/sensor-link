use std::{str::FromStr, time::Duration};
use tokio::sync::mpsc;

use rumqttc::{
    AsyncClient,
    Event::{Incoming, Outgoing},
    EventLoop, LastWill, MqttOptions, Outgoing as OutgoingPacket,
    Packet::{ConnAck, Disconnect, PubAck, PubComp, Publish, SubAck, UnsubAck},
    QoS, TlsConfiguration, Transport,
};

use sensor_link_firmware::{
    heapless,
    mqtt::{self, FileError, Will},
    sensor_link_protocol::{
        info::VersionString, Error, MAX_FILE_CHUNK_LEN, MAX_TOPIC_LEN, MAX_UID_LEN,
    },
    utils::x509,
};

use crate::{with_timeout, SensorArgs};

const CONFIGURED_QOS: QoS = QoS::ExactlyOnce;

enum InternalMessage {
    Error,
    /// Boxed: an inline `mqtt::Message` carries topic and payload buffers that
    /// dwarf the `Error` variant, and every channel slot would pay for them.
    Message(Box<mqtt::Message>),
}

enum InternalAck {
    Ack,
    Error,
}

pub struct Mqtt {
    pub client_id: String,
    client: Option<AsyncClient>,
    options: MqttOptions,
    tx: mpsc::Sender<InternalMessage>,
    rx: mpsc::Receiver<InternalMessage>,
    ack_tx: mpsc::Sender<InternalAck>,
    ack_rx: mpsc::Receiver<InternalAck>,
    /// Message to be processed.
    message: Option<mqtt::Message>,
}

/// Returns a unique identifier for the device: `mock_{:04}`,
/// where `{:04}` means zero padded 4 digits.
fn uid(instance_no: usize) -> String {
    let uid = format!("mock_{instance_no:04}");

    assert!(uid.len() <= MAX_UID_LEN);
    uid
}

// MQTT driver implementation
impl Mqtt {
    pub fn new(args: SensorArgs) -> Mqtt {
        let (host, port) = (args.broker_host.clone(), args.broker_port);

        log::info!(target: "mqtt", "Connecting to broker: {host}:{port}");

        let uid = uid(args.instance_no);
        let mut mqttoptions = MqttOptions::new(
            &uid, // will be overruled by cert if using TLS
            host, port,
        );

        // These options are not (yet) enabled in firmware
        // mqttoptions.set_clean_session(false); // Ensure we will receive message from when we were offline
        // mqttoptions.set_keep_alive(Duration::from_secs(5));

        mqttoptions.set_max_packet_size(2048, 1548);

        let (transport, client_id) = if args.use_tls {
            log::info!(target: "mqtt", "Using TLS");

            let ca_path = args.cacert.as_ref().expect("Missing --cacert argument");
            let (_, ca) =
                x509::parse_cert(&ca_path.to_path_buf()).expect("Failed to parse CA Cert");

            let (cert_path, key_path) = if let Some(certdir) = args.cert_dir {
                // For sensor index 1
                // We expect a mock_0001/mock_0001.cert and mock_0001/mock_0001.key
                let cert_path = certdir.join(&uid).join(format!("{uid}.cert"));
                let key_path = certdir.join(&uid).join(format!("{uid}.key"));

                log::info!(target: "mqtt", "Using client cert: {cert_path:?} and key: {key_path:?}");

                (cert_path, key_path)
            } else {
                panic!("Missing cert_dir");
            };

            let (common_name, client_cert) =
                x509::parse_cert(&cert_path).expect("Failed to parse Client Cert");
            let client_key = x509::parse_key(&key_path).expect("Failed to parse Client Key");

            (
                Transport::Tls(TlsConfiguration::Simple {
                    ca: ca.into(),
                    alpn: None,
                    client_auth: Some((client_cert.into(), client_key.into())),
                }),
                common_name,
            )
        } else {
            log::warn!("Note: Not using TLS");
            (Transport::Tcp, uid)
        };
        mqttoptions.set_transport(transport);

        let (tx, rx) = mpsc::channel(10);

        // Internal Channel to receive acks in functions
        let (ack_tx, ack_rx) = mpsc::channel(10);

        // We store the mqtt options so that we can reuse them for reconnects.
        Mqtt {
            options: mqttoptions,
            client: None,
            client_id,
            tx,
            rx,
            ack_tx,
            ack_rx,
            message: None,
        }
    }
}

/// Rumqttc event loop
async fn run_event_loop(
    mut event_loop: EventLoop,
    tx: mpsc::Sender<InternalMessage>,
    ack_tx: mpsc::Sender<InternalAck>,
) {
    loop {
        let poll_res = match event_loop.poll().await {
            Err(err) => {
                log::error!(target: "mqtt", "Error: {:?}", err);
                let _ = tx.try_send(InternalMessage::Error);
                let _ = ack_tx.try_send(InternalAck::Error);
                break;
            }
            Ok(poll_res) => poll_res,
        };
        match poll_res {
            Incoming(ConnAck(_)) => {}
            Incoming(Publish(p)) => {
                log::info!(target: "mqtt", "Received message on: {:?}", p.topic);
                let Ok(topic) = p.topic.parse() else {
                    continue;
                };
                let Ok(payload) = heapless::Vec::from_slice(&p.payload) else {
                    continue;
                };
                tx.send(InternalMessage::Message(Box::new(mqtt::Message {
                    topic,
                    payload,
                })))
                .await
                .expect("Send message on internal channel");
            }
            Incoming(PubAck(_)) | Incoming(PubComp(_)) => {
                // Publish acknowledged
                // PubAck for QoS 1
                // PubComp for QoS 2
                let _ = ack_tx.send(InternalAck::Ack).await;
            }
            Incoming(SubAck(_)) | Incoming(UnsubAck(_)) => {
                let _ = ack_tx.send(InternalAck::Ack).await;
            }
            Outgoing(OutgoingPacket::Disconnect) | Incoming(Disconnect) => {
                log::debug!("event loop received disconnect");
                break;
            }
            Incoming(mess) => {
                log::debug!(target: "mqtt", "Unhandled incoming message: {:?}", mess);
            }
            Outgoing(mess) => {
                log::debug!(target: "mqtt", "Unhandled outgoing message: {:?}", mess);
            }
        }
    }
}

impl mqtt::MqttClient for Mqtt {
    type ClientError = rumqttc::ClientError;
    type PollError = ();

    async fn connect(
        &mut self,
        _client_id: &str,
        will: Will,
    ) -> Result<(), Error<Self::ClientError>> {
        // Flush internal events, acks included: an `InternalAck::Error` left
        // over from the previous connection's event loop would otherwise fail
        // the first subscribe or publish of this one.
        while self.rx.try_recv().is_ok() {}
        while self.ack_rx.try_recv().is_ok() {}

        let lastwill = LastWill::new(
            will.topic.as_str(),
            will.payload.as_str(),
            QoS::AtLeastOnce,
            true,
        );
        self.options.set_last_will(lastwill);

        let (client, eventloop) = AsyncClient::new(self.options.clone(), 10);
        self.client = Some(client);

        let tx = self.tx.clone();
        let ack_tx = self.ack_tx.clone();

        tokio::spawn(async move {
            run_event_loop(eventloop, tx, ack_tx).await;
        });

        log::info!(target: "mqtt", "init done");

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        log::info!(target: "mqtt", "Disconnecting..");
        self.client
            .as_mut()
            .expect("No client to disconnect")
            .disconnect()
            .await
            .ok();
        Ok(())
    }

    async fn publish_raw(
        &mut self,
        topic_name: heapless::String<{ MAX_TOPIC_LEN }>,
        message: &[u8],
    ) -> Result<(), Error<Self::ClientError>> {
        log::debug!(target: "mqtt", "publishing to {topic_name}");
        match self
            .client
            .as_mut()
            .expect("Client isn't present, please connect first")
            .publish(topic_name.to_string(), CONFIGURED_QOS, false, message)
            .await
        {
            Ok(_) => match CONFIGURED_QOS {
                QoS::AtLeastOnce | QoS::ExactlyOnce => match self.ack_rx.recv().await {
                    Some(InternalAck::Ack) => Ok(()),
                    Some(InternalAck::Error) => {
                        log::error!(target: "mqtt", "Publish failed (error)");
                        Err(Error::MQTT("Publish failed"))
                    }
                    None => {
                        log::error!(target: "mqtt", "Publish failed (none)");
                        Err(Error::MQTT("Publish failed"))
                    }
                },
                QoS::AtMostOnce => Ok(()),
            },
            Err(err) => Err(Error::Client(err)),
        }
    }

    async fn subscribe(
        &mut self,
        topic_name: heapless::String<{ MAX_TOPIC_LEN }>,
    ) -> Result<(), Error<Self::ClientError>> {
        log::debug!(target: "mqtt", "subscribing to {}", topic_name);
        match self
            .client
            .as_mut()
            .expect("Client isn't present, please connect first")
            .subscribe(topic_name.to_string(), CONFIGURED_QOS)
            .await
        {
            Ok(_) => match CONFIGURED_QOS {
                QoS::AtLeastOnce | QoS::ExactlyOnce => match self.ack_rx.recv().await {
                    Some(InternalAck::Ack) => {
                        log::info!(target: "mqtt", "subscribed to {}", topic_name);
                        Ok(())
                    }
                    _ => Err(Error::MQTT("Subscribe failed")),
                },
                QoS::AtMostOnce => Ok(()),
            },
            Err(err) => Err(Error::Client(err)),
        }
    }

    async fn reconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        // Reconnect not required for Mock Sensor.
        Ok(())
    }

    async fn unsubscribe(
        &mut self,
        topic_name: heapless::String<{ MAX_TOPIC_LEN }>,
    ) -> Result<(), Error<Self::ClientError>> {
        log::info!(target: "mqtt", "unsubscribing from {}", topic_name);
        match self
            .client
            .as_mut()
            .expect("Client isn't present, please connect first")
            .unsubscribe(topic_name.to_string())
            .await
        {
            Ok(_) => match CONFIGURED_QOS {
                QoS::AtLeastOnce | QoS::ExactlyOnce => match self.ack_rx.recv().await {
                    Some(InternalAck::Ack) => Ok(()),
                    _ => Err(Error::MQTT("Publish failed")),
                },
                QoS::AtMostOnce => Ok(()),
            },
            Err(err) => Err(Error::Client(err)),
        }
    }

    async fn download_file(&mut self, _url: &str) -> Result<(), Error<Self::ClientError>> {
        log::warn!(target: "mqtt", "download_file not implemented");
        Ok(())
    }

    async fn read_file_chunk(
        &mut self,
        _chunk_size: usize,
    ) -> Result<heapless::Vec<u8, MAX_FILE_CHUNK_LEN>, FileError> {
        log::warn!(target: "mqtt", "read_file_chunk not implemented");
        Err(FileError::FileNotFound)
    }

    async fn await_response(&mut self, timeout_s: u32) -> Result<(), Self::PollError> {
        match with_timeout(self.rx.recv(), Duration::from_secs(timeout_s as u64)).await {
            Some(Some(InternalMessage::Message(m))) => {
                self.message = Some(*m);
                Ok(())
            }
            _ => Err(()),
        }
    }

    async fn handle_response(&mut self) -> Result<mqtt::Event, ()> {
        if let Some(m) = self.message.take() {
            Ok(mqtt::Event::ReceivedMessage(m))
        } else {
            Err(())
        }
    }

    async fn signal_quality(&mut self) -> Option<i16> {
        Some(100)
    }

    async fn modem_model(&mut self) -> VersionString {
        VersionString::from_str("Mock Modem").unwrap()
    }

    async fn modem_fw_version(&mut self) -> VersionString {
        VersionString::from_str("1.0.0").unwrap()
    }
}
