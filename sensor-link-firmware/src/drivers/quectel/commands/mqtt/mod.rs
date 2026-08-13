//! MQTT Commands
//! Quectel_EC2x&EG9x&EM05_MQTT_Application_Note_V1.1

pub mod urc;

use atat::atat_derive::AtatCmd;
use heapless::String;
use sensor_link_protocol::{MAX_ONLINE_PAYLOAD_LEN, MAX_TOPIC_LEN};

use super::{super::types::ContextId, NoResponse};

// 3.2.1 Configure Optional Parameters of MQTT
// The config commands can configure different parameters.
// Depending on the first argument, which selects the param to configure, the rest of the arguments will change.
// Therefore it makes sense to define a seperate struct for each param that we want to configure.

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ConfigRecvMode {
    #[at_arg(position = 0)]
    param: String<9>, // should be "recv/mode",

    #[at_arg(position = 1)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 2)]
    /// Integer type. Configure the MQTT message receiving mode.
    /// 0 MQTT message received from server will be contained in URC.
    /// 1 MQTT message received from server will not be contained in URC.
    pub msg_recv_mode: u8,

    /// 0 Length of MQTT message received from server will not be contained in URC.
    /// 1 Length of MQTT message received from server will be contained in URC.
    pub msg_len_enable: Option<u8>,
}

impl ConfigRecvMode {
    pub fn new(client_idx: ContextId, msg_recv_mode: u8, msg_len_enable: Option<u8>) -> Self {
        Self {
            param: String::try_from("recv/mode").unwrap(),
            client_idx,
            msg_recv_mode,
            msg_len_enable,
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ConfigSSL {
    #[at_arg(position = 0)]
    param: String<3>, // should be "ssl",

    #[at_arg(position = 1)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 2)]
    /// Integer type. Configure the MQTT SSL mode
    /// 0 Use normal TCP connection for MQTT
    /// 1 Use SSL TCP secure connection for MQTT
    pub sslenable: u8,

    /// Integer type. SSL context index. The range is 0-5.
    pub sslctx_id: Option<ContextId>,
}

impl ConfigSSL {
    pub fn new(client_idx: ContextId, sslenable: u8, sslctx_id: Option<ContextId>) -> Self {
        Self {
            param: String::try_from("ssl").unwrap(),
            client_idx,
            sslenable,
            sslctx_id,
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ConfigSession {
    #[at_arg(position = 0)]
    param: String<7>, // should be "session",

    #[at_arg(position = 1)]
    /// MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 2)]
    /// Configure the session type
    /// 0 The server must store the subscriptions of the client after it disconnects.
    /// 1 The server must discard any previously maintained information about the
    /// client and treat the connection as “clean”.
    pub session: u8,
}

impl ConfigSession {
    pub fn new(client_idx: ContextId, clean: bool) -> Self {
        Self {
            param: String::try_from("session").unwrap(),
            client_idx,
            session: if clean { 1 } else { 0 },
        }
    }
}

#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTCFG", NoResponse, timeout_ms = 300, termination = "\r")]
pub struct ConfigWill {
    #[at_arg(position = 0)]
    param: String<4>, // should be "will",

    #[at_arg(position = 1)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 2)]
    /// Configure the Will flag
    /// 0 Ignore the Will flag configuration
    /// 1 Require the Will flag configuration
    pub will_fg: u8,

    #[at_arg(position = 3)]
    /// Configure the Will QoS
    /// 0, 1 or 2
    pub will_qos: u8,

    #[at_arg(position = 4)]
    /// The Will retain flag is only used on PUBLISH messages.
    /// 0 When a client sends a PUBLISH message to a server, the server will not hold
    /// on to the message after it has been delivered to the current subscribers
    /// 1 When a client sends a PUBLISH message to a server, the server should hold
    /// on to the message after it has been delivered to the current subscribers
    pub will_retain: u8,

    #[at_arg(position = 5)]
    /// The Will topic
    pub will_topic: String<MAX_TOPIC_LEN>,

    #[at_arg(position = 6)]
    /// The Will message
    pub will_msg: String<MAX_ONLINE_PAYLOAD_LEN>,
}

impl ConfigWill {
    pub fn new(
        client_idx: ContextId,
        will_fg: u8,
        will_qos: u8,
        will_retain: u8,
        will_topic: String<MAX_TOPIC_LEN>,
        will_msg: String<MAX_ONLINE_PAYLOAD_LEN>,
    ) -> Self {
        Self {
            param: String::try_from("will").unwrap(),
            client_idx,
            will_fg,
            will_qos,
            will_retain,
            will_topic,
            will_msg,
        }
    }
}

/// 3.2.2 Open a Network for MQTT Client
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTOPEN", NoResponse, timeout_ms = 120000, termination = "\r")]
pub struct Open {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 1)]
    /// The address of the server.
    /// It could be an IP address or a domain name.
    /// The maximum size is 100 bytes.
    pub host_name: String<30>,

    #[at_arg(position = 2)]
    /// The port of the server. The range is 1-65535.
    pub port: u16,
}

/// 3.2.3 Close a Network for MQTT Client
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTCLOSE", NoResponse, timeout_ms = 31000, termination = "\r")]
pub struct Close {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,
}

/// 3.2.4 Connect a Client to MQTT Server
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTCONN", NoResponse, timeout_ms = 5000, termination = "\r")]
pub struct Connect {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 1)]
    /// The client identifier string.
    pub mqtt_client_id: String<16>,
}

/// 3.2.5 Disconnect from MQTT Server
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTDISC", NoResponse, termination = "\r")]
pub struct Disconnect {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,
}

/// 3.2.6 Subscribe to a topic
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTSUB", NoResponse, termination = "\r")]
pub struct Subscribe {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 1)]
    /// Message identifier of packet. The range is 1-65535.
    pub msg_id: u16,

    #[at_arg(position = 2)]
    /// Topic that the client wants to subscribe to or unsubscribe from
    pub topic: String<MAX_TOPIC_LEN>,

    #[at_arg(position = 3)]
    /// The QoS level at which the client wants to publish the messages.
    /// 0, 1 or 2
    pub qos: u8,
}

/// 3.2.7 UnSubscribe from a topic
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTUNS", NoResponse, termination = "\r")]
pub struct Unsubscribe {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 1)]
    /// Message identifier of packet. The range is 1-65535.
    pub msg_id: u16,

    #[at_arg(position = 2)]
    /// Topic that the client wants to subscribe to or unsubscribe from
    pub topic: String<MAX_TOPIC_LEN>,
}

/// 3.2.9 Receive message from slot 0-4
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTRECV", urc::MQTTReceive, termination = "\r")]
pub struct Receive {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 1)]
    /// Message identifier of packet. The range is 0-4.
    pub recv_id: Option<u8>,
}

/// 3.2.8 Publish a message on a topic
#[derive(Debug, Clone, AtatCmd)]
#[at_cmd("+QMTPUBEX", NoResponse, termination = "\r")]
pub struct PublishPrepare {
    #[at_arg(position = 0)]
    /// Integer type. MQTT client identifier. The range is 0-5
    pub client_idx: ContextId,

    #[at_arg(position = 1)]
    /// Message identifier of packet. The range is 1-65535.
    pub msg_id: u16,

    #[at_arg(position = 2)]
    /// The QoS level at which the client wants to publish the messages.
    /// 0, 1 or 2
    pub qos: u8,

    #[at_arg(position = 3)]
    /// Whether or not the server will retain the message after it has been delivered to the current subscribers.
    pub retain: u8,

    #[at_arg(position = 4)]
    /// Topic that the client wants to subscribe to or unsubscribe from
    pub topic: String<MAX_TOPIC_LEN>,

    #[at_arg(position = 5)]
    pub msg_len: u16,
}
