//! Transport-generic MQTT session core.
//!
//! Wraps [`rust_mqtt`] into the shape the [`crate::mqtt::MqttClient`] trait
//! needs: QoS-1 publish/subscribe that await their acks, a cancel-safe
//! [`MqttCore::await_event`], and keepalive pings driven from the same wait
//! loop. Generic over any `embedded_io_async` (0.7) `Read + Write` transport,
//! so it runs identically over a TLS connection on target and a TCP stream in
//! host tests.

use core::num::NonZero;

use heapless::Deque;
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::event::Event as MqttEvent;
use rust_mqtt::client::options::{
    ConnectOptions, DisconnectOptions, PublicationOptions, SubscriptionOptions, TopicReference,
    WillOptions,
};
use rust_mqtt::client::{Client, MqttError};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::types::{MqttString, QoS, TopicFilter, TopicName};

use crate::monotonic_time::{self, FutureTimeout};
use crate::mqtt::{Message, Will};
use sensor_link_protocol::{MAX_MESSAGE_LEN, MAX_TOPIC_LEN};

/// Storage for received-packet fields; construct with `ReceiveBuffer::new`
/// over a `[u8; RECEIVE_BUF_LEN]`.
pub use rust_mqtt::buffer::BumpBuffer as ReceiveBuffer;

/// In-flight subscribe/unsubscribe slots.
const MAX_SUBSCRIBES: usize = 8;
/// Incoming QoS 1/2 publications the broker may have in flight.
const RECEIVE_MAXIMUM: usize = 4;
/// Outgoing QoS 1 publications in flight (the trait publishes sequentially).
const SEND_MAXIMUM: usize = 2;
const MAX_SUBSCRIPTION_IDENTIFIERS: usize = 1;

/// Backing storage for dynamically sized fields of received packets.
/// Reset after every consumed packet, so it only needs to hold one packet's
/// topic + payload + properties.
pub const RECEIVE_BUF_LEN: usize = 4096;

/// Received messages that arrived while waiting for an ack are parked here
/// until `take_message` picks them up.
const MESSAGE_QUEUE_LEN: usize = 2;

const KEEPALIVE_S: u16 = 60;
/// Ping when this fraction of the keepalive interval has passed without
/// outgoing traffic (3/4 of [`KEEPALIVE_S`]).
const PING_AFTER_S: u64 = (KEEPALIVE_S as u64 * 3) / 4;
/// Every ack the broker owes us must arrive within this window.
const ACK_TIMEOUT_MS: u32 = 30_000;

type MqttClient<'c, T> = Client<
    'c,
    T,
    BumpBuffer<'c>,
    MAX_SUBSCRIBES,
    RECEIVE_MAXIMUM,
    SEND_MAXIMUM,
    MAX_SUBSCRIPTION_IDENTIFIERS,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreError {
    /// Transport or protocol failure: the connection is unusable.
    ConnectionLost,
    /// The broker rejected an operation (bad reason code).
    Rejected,
    /// An expected ack did not arrive in time.
    AckTimeout,
    /// A received message did not fit the trait's topic/payload bounds.
    Oversize,
}

/// What `await_event` resolved to; consumed by the driver's
/// `handle_response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    Message,
    ConnectionLost,
}

pub struct MqttCore<'c, T: embedded_io_async_07::Read + embedded_io_async_07::Write> {
    client: MqttClient<'c, T>,
    /// Owned copies of received PUBLISH packets, oldest first.
    messages: Deque<Message, MESSAGE_QUEUE_LEN>,
    /// Set when the connection failed; `take_pending` reports it exactly once.
    lost: bool,
    last_tx: monotonic_time::MonotonicInstant,
}

impl<'c, T: embedded_io_async_07::Read + embedded_io_async_07::Write> MqttCore<'c, T> {
    pub fn new(receive_buf: &'c mut BumpBuffer<'c>) -> Self {
        Self {
            client: Client::new(receive_buf),
            messages: Deque::new(),
            lost: false,
            last_tx: monotonic_time::now(),
        }
    }

    /// Opens the MQTT session on an established transport.
    pub async fn connect(
        &mut self,
        net: T,
        client_id: &str,
        will: &Will,
        clean_start: bool,
    ) -> Result<(), CoreError> {
        let options = ConnectOptions {
            clean_start,
            keep_alive: KeepAlive::Seconds(NonZero::new(KEEPALIVE_S).unwrap()),
            will: Some(WillOptions {
                will_qos: QoS::AtLeastOnce,
                will_retain: true,
                will_topic: topic_name(will.topic.as_str())?,
                will_delay_interval: 0,
                payload_format_indicator: None,
                message_expiry_interval: None,
                content_type: None,
                response_topic: None,
                correlation_data: None,
                will_message: will
                    .payload
                    .as_bytes()
                    .try_into()
                    .map_err(|_| CoreError::Rejected)?,
            }),
            ..Default::default()
        };
        let id: MqttString = client_id.try_into().map_err(|_| CoreError::Rejected)?;
        self.client
            .connect(net, &options, Some(id))
            .await
            .map_err(|_| CoreError::ConnectionLost)?;
        self.lost = false;
        self.mark_tx();
        Ok(())
    }

    /// QoS-1 subscribe; waits for the matching SUBACK.
    pub async fn subscribe(&mut self, topic: &str) -> Result<(), CoreError> {
        let options = SubscriptionOptions {
            qos: QoS::AtLeastOnce,
            ..Default::default()
        };
        let pid = self
            .client
            .subscribe(topic_filter(topic)?, options)
            .await
            .map_err(|e| self.on_send_error(e))?;
        self.mark_tx();
        self.wait_for_ack(AckKind::Suback(pid)).await
    }

    /// Unsubscribe; waits for the matching UNSUBACK.
    pub async fn unsubscribe(&mut self, topic: &str) -> Result<(), CoreError> {
        let pid = self
            .client
            .unsubscribe(topic_filter(topic)?)
            .await
            .map_err(|e| self.on_send_error(e))?;
        self.mark_tx();
        self.wait_for_ack(AckKind::Unsuback(pid)).await
    }

    /// QoS-1 publish; waits for the PUBACK.
    pub async fn publish(&mut self, topic: &str, payload: &[u8]) -> Result<(), CoreError> {
        let topic = topic_name(topic)?;
        let options = PublicationOptions::new(TopicReference::Name(topic)).qos(QoS::AtLeastOnce);
        let payload = Bytes::from(payload);
        let pid = self
            .client
            .publish(&options, payload)
            .await
            .map_err(|e| self.on_send_error(e))?
            .expect("QoS 1 publish always has a packet identifier");
        self.mark_tx();
        self.wait_for_ack(AckKind::Puback(pid)).await
    }

    /// Cancel-safe wait for something `handle_response`-worthy.
    ///
    /// Returns `Ok(pending)` when a message arrived or the connection died
    /// (the caller then calls [`Self::take_pending`]-driven handling), and
    /// `Err(())` on a clean timeout with no activity. Keepalive pings are sent
    /// from inside this loop.
    pub async fn await_event(&mut self, timeout_s: u32) -> Result<Pending, ()> {
        if let Some(p) = self.peek_pending() {
            return Ok(p);
        }
        let deadline = monotonic_time::now().add_micros(u64::from(timeout_s) * 1_000_000);
        loop {
            // Wait at most until either the user timeout or the next
            // keepalive ping is due.
            let now = monotonic_time::now();
            if deadline.micros_since(&now) == 0 {
                return Err(());
            }
            let until_deadline_ms = deadline.micros_since(&now) / 1000;
            let until_ping_ms = (PING_AFTER_S * 1000)
                .saturating_sub(self.last_tx.elapsed_us() / 1000)
                .max(1);
            let wait_ms = until_deadline_ms.min(until_ping_ms).min(u32::MAX as u64) as u32;

            match self.client.poll_header().with_timeout_ms(wait_ms).await {
                Some(Ok(header)) => {
                    // The header is in; the body follows immediately (it is
                    // typically already buffered in the TLS record). This part
                    // is not cancel-safe, but it is also not awaited across
                    // the select in the network task: only `poll_header` is.
                    match self.consume_body(header).await {
                        Ok(_) => {
                            if let Some(p) = self.peek_pending() {
                                return Ok(p);
                            }
                        }
                        Err(_) => return Ok(Pending::ConnectionLost),
                    }
                }
                Some(Err(_)) => {
                    self.lost = true;
                    return Ok(Pending::ConnectionLost);
                }
                None => {
                    // Timer fired: ping if due, otherwise the user timeout hit.
                    if self.last_tx.elapsed_us() / 1_000_000 >= PING_AFTER_S {
                        if self.client.ping().await.is_err() {
                            self.lost = true;
                            return Ok(Pending::ConnectionLost);
                        }
                        self.mark_tx();
                    } else {
                        return Err(());
                    }
                }
            }
        }
    }

    /// Returns what `await_event` found, consuming a queued message if any.
    pub fn take_message(&mut self) -> Option<Message> {
        self.messages.pop_front()
    }

    /// True when the connection has failed and the failure has not been
    /// reported yet. Reading it clears the flag.
    pub fn take_lost(&mut self) -> bool {
        core::mem::take(&mut self.lost)
    }

    pub async fn disconnect(&mut self) {
        let _ = self
            .client
            .disconnect(&DisconnectOptions::default())
            .with_timeout_ms(2000)
            .await;
    }

    fn peek_pending(&self) -> Option<Pending> {
        if !self.messages.is_empty() {
            Some(Pending::Message)
        } else if self.lost {
            Some(Pending::ConnectionLost)
        } else {
            None
        }
    }

    fn mark_tx(&mut self) {
        self.last_tx = monotonic_time::now();
    }

    fn on_send_error(&mut self, _e: MqttError) -> CoreError {
        self.lost = true;
        CoreError::ConnectionLost
    }

    /// Receives and dispatches one packet body after `poll_header`.
    async fn consume_body(
        &mut self,
        header: rust_mqtt::header::FixedHeader,
    ) -> Result<AckSeen, CoreError> {
        let seen = match self.client.poll_body(header).await {
            Ok(MqttEvent::Publish(publish)) => {
                let mut message = Message {
                    topic: heapless::String::new(),
                    payload: heapless::Vec::new(),
                };
                let topic_ok = message.topic.push_str(publish.topic.as_ref().as_str()).is_ok();
                let payload_ok = message
                    .payload
                    .extend_from_slice(publish.message.as_ref())
                    .is_ok();
                if !(topic_ok && payload_ok) {
                    log::warn!(
                        "dropping oversized publish ({} topic / {} payload bytes)",
                        publish.topic.as_ref().as_str().len(),
                        publish.message.as_ref().len()
                    );
                } else if self.messages.push_back(message).is_err() {
                    log::warn!("dropping publish: message queue full");
                }
                AckSeen::None
            }
            Ok(MqttEvent::Suback(ack)) => AckSeen::Suback(ack.packet_identifier, ack.reason_code),
            Ok(MqttEvent::Unsuback(ack)) => {
                AckSeen::Unsuback(ack.packet_identifier, ack.reason_code)
            }
            Ok(MqttEvent::PublishAcknowledged(ack)) => AckSeen::Puback(ack.packet_identifier),
            Ok(MqttEvent::PublishRejected(rej)) => AckSeen::Rejected(rej.packet_identifier),
            Ok(_) => AckSeen::None,
            Err(_) => {
                self.lost = true;
                return Err(CoreError::ConnectionLost);
            }
        };
        // All borrowed packet data has been copied out above, so the receive
        // buffer can be recycled for the next packet.
        unsafe { self.client.buffer_mut().reset() };
        Ok(seen)
    }

    /// Pumps the connection until the given ack arrives. Messages received in
    /// the meantime are queued for `take_message`.
    async fn wait_for_ack(&mut self, ack: AckKind) -> Result<(), CoreError> {
        let deadline = monotonic_time::now().add_micros(u64::from(ACK_TIMEOUT_MS) * 1000);
        loop {
            let now = monotonic_time::now();
            let remaining_ms = deadline.micros_since(&now) / 1000;
            if remaining_ms == 0 {
                return Err(CoreError::AckTimeout);
            }
            let header = match self
                .client
                .poll_header()
                .with_timeout_ms(remaining_ms as u32)
                .await
            {
                None => return Err(CoreError::AckTimeout),
                Some(Err(_)) => {
                    self.lost = true;
                    return Err(CoreError::ConnectionLost);
                }
                Some(Ok(h)) => h,
            };
            match (ack, self.consume_body(header).await?) {
                (AckKind::Suback(want), AckSeen::Suback(got, reason))
                | (AckKind::Unsuback(want), AckSeen::Unsuback(got, reason)) => {
                    if got == want {
                        return if reason.is_erroneous() {
                            Err(CoreError::Rejected)
                        } else {
                            Ok(())
                        };
                    }
                }
                (AckKind::Puback(want), AckSeen::Puback(got)) if got == want => return Ok(()),
                (_, AckSeen::Rejected(_)) => return Err(CoreError::Rejected),
                _ => {}
            }
        }
    }
}

fn topic_name(topic: &str) -> Result<TopicName<'_>, CoreError> {
    let s: MqttString = topic.try_into().map_err(|_| CoreError::Rejected)?;
    TopicName::new(s).ok_or(CoreError::Rejected)
}

fn topic_filter(topic: &str) -> Result<TopicFilter<'_>, CoreError> {
    let s: MqttString = topic.try_into().map_err(|_| CoreError::Rejected)?;
    TopicFilter::new(s).ok_or(CoreError::Rejected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckKind {
    Suback(rust_mqtt::types::PacketIdentifier),
    Unsuback(rust_mqtt::types::PacketIdentifier),
    Puback(rust_mqtt::types::PacketIdentifier),
}

enum AckSeen {
    None,
    Suback(rust_mqtt::types::PacketIdentifier, rust_mqtt::types::ReasonCode),
    Unsuback(rust_mqtt::types::PacketIdentifier, rust_mqtt::types::ReasonCode),
    Puback(rust_mqtt::types::PacketIdentifier),
    Rejected(rust_mqtt::types::PacketIdentifier),
}

/// Compile-time guarantees that the trait's bounds fit the queue types.
const _: () = {
    assert!(MAX_TOPIC_LEN <= RECEIVE_BUF_LEN);
    assert!(MAX_MESSAGE_LEN <= RECEIVE_BUF_LEN);
};
