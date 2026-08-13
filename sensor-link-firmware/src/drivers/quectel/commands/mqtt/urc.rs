//! MQTT URCs these can be found in
//! Quectel_EC2x&EG9x&EM05_MQTT_Application_Note_V1.1

use atat::{atat_derive::AtatResp, serde_at::de::length_delimited::LengthDelimited};
use heapless::String;
use num_enum::TryFromPrimitive;

use crate::{drivers::quectel::commands::URCParse, mqtt::Message};
use sensor_link_protocol::{MAX_MESSAGE_LEN, MAX_TOPIC_LEN};

/// 3.2.2 Open result URC
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTOpen {
    #[allow(dead_code)]
    pub client_idx: u8,

    pub result: i8,
}

/// Result codes for the MQTTOpen URC
/// 3.2.2
#[repr(i8)]
#[derive(Debug, TryFromPrimitive)]
pub enum MQTTOpenResult {
    /// Failed to open network
    FailedToOpen = -1,
    /// Network opened successfully
    Success = 0,
    /// Wrong parameter
    WrongParameter = 1,
    /// MQTT identifier is occupied
    MqttIdentifierOccupied = 2,
    /// Failed to activate PDP
    FailedToActivatePDP = 3,
    /// Failed to parse domain name
    FailedToParseDomainName = 4,
    /// Network connection error
    NetworkConnectionError = 5,
    /// Unexpected result, check specs.
    Unknown = 99,
}

impl Default for MQTTOpenResult {
    fn default() -> Self {
        Self::Unknown
    }
}

impl URCParse<MQTTOpenResult> for MQTTOpen {
    type Res = i8;

    fn val(&self) -> i8 {
        self.result
    }
}

/// 3.2.4 Connect result URC
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTConnect {
    #[allow(dead_code)]
    pub client_idx: u8,

    #[allow(dead_code)]
    pub result: u8,

    pub ret_code: u8,
}

/// Result codes for the MQTTConnect URC
#[repr(u8)]
#[derive(Debug, TryFromPrimitive)]
pub enum MQTTConnectResult {
    /// Connection Accepted
    Accepted = 0,
    /// Connection Refused: Unacceptable Protocol Version
    ProtocolRefused = 1,
    /// Connection Refused: Identifier Rejected
    IdentifierRefused = 2,
    /// Connection Refused: Server Unavailable
    ServerUnavailable = 3,
    /// Connection Refused: Bad User Name or Password
    BadUserNameOrPassword = 4,
    /// Connection Refused: Not Authorized
    NotAuthorized = 5,
    /// Unexpected result
    Unknown = 99,
}

impl Default for MQTTConnectResult {
    fn default() -> Self {
        Self::Unknown
    }
}

impl URCParse<MQTTConnectResult> for MQTTConnect {
    type Res = u8;

    fn val(&self) -> u8 {
        self.ret_code
    }
}

/// 3.2.5 Disconnect result URC
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTDisconnect {
    #[allow(dead_code)]
    pub client_idx: u8,

    pub result: i8,
}

/// Result codes for the MQTT URCs that only respond with two options:
/// - succes or fail.
#[repr(i8)]
#[derive(Debug, TryFromPrimitive)]
pub enum MQTTResult {
    Success = 0,
    Failed = -1,
}

impl Default for MQTTResult {
    fn default() -> Self {
        Self::Failed
    }
}

impl URCParse<MQTTResult> for MQTTDisconnect {
    type Res = i8;

    fn val(&self) -> i8 {
        self.result
    }
}

/// 3.2.3 Close result URC
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTClose {
    #[allow(dead_code)]
    pub client_idx: u8,

    pub result: i8,
}

impl URCParse<MQTTResult> for MQTTClose {
    type Res = i8;

    fn val(&self) -> i8 {
        self.result
    }
}

/// Result codes for URCs that may resend packets
#[repr(u8)]
#[derive(Debug, TryFromPrimitive)]
pub enum MQTTPacketResult {
    /// Sent packet successfully and received ACK from server
    Success = 0,
    /// Packet retransmission
    Retransmission = 1,
    /// Failed to send packet
    Failed = 2,
    /// Unexpecteed result: check specs.
    Unknown = 99,
}

impl Default for MQTTPacketResult {
    fn default() -> Self {
        Self::Unknown
    }
}

/// 3.2.6 Subscribe result URC
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTSubscribe {
    #[allow(dead_code)]
    pub client_idx: u8,

    pub msg_id: u16,
    pub result: u8,

    #[allow(dead_code)]
    pub value: u8,
}

impl URCParse<MQTTPacketResult> for MQTTSubscribe {
    type Res = u8;

    fn val(&self) -> u8 {
        self.result
    }
}

/// 3.2.7 Unsubscribe result URC
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTUnsubscribe {
    #[allow(dead_code)]
    pub client_idx: u8,

    pub msg_id: u16,
    /// Integer type. Result of the command execution
    /// 0 Sent packet successfully and received ACK from server
    /// 1 Packet retransmission
    /// 2 Failed to send packet
    pub result: u8,

    #[allow(dead_code)]
    pub value: Option<u8>,
}

impl URCParse<MQTTPacketResult> for MQTTUnsubscribe {
    type Res = u8;

    fn val(&self) -> u8 {
        self.result
    }
}

/// 4.2 The URC begins with “+QMTRECV:”. It is mainly used to notify the host
/// to read the received MQTT packet data that is reported from MQTT server.
/// This can also be the response to tha AT+QMTRECV command.
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTReceive {
    #[at_arg(position = 0)]
    pub client_idx: u8,
    /// Message identifier of packet. The range is 0-65535. It will be 0 only when <qos>=0.
    #[at_arg(position = 1)]
    pub msg_id: u16,
    /// The topic on which the message was received from MQTT server.
    #[at_arg(position = 2)]
    pub topic: String<MAX_TOPIC_LEN>,
    #[at_arg(position = 3)]
    pub payload: LengthDelimited<MAX_MESSAGE_LEN>,
}

/// 4.2 The URC begins with “+QMTRECV:”. It is mainly used to notify the host
/// to read the received MQTT packet data that is reported from MQTT server.
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTReceiveWithoutData {
    #[allow(dead_code)]
    #[at_arg(position = 0)]
    pub client_idx: u8,

    /// Indicates the slot the message was received in.
    /// The range is 0-4.
    #[at_arg(position = 1)]
    pub recv_id: u8,
}

#[derive(Clone, Debug, AtatResp)]
pub struct MQTTPublish {
    #[allow(dead_code)]
    pub client_idx: u8,
    pub msg_id: u16,

    /// 0=success, 1=retransmission, 2=failed
    pub result: u8,

    // retransmission count (only present if result==1)
    #[allow(dead_code)]
    pub value: Option<u8>,
}

impl URCParse<MQTTPacketResult> for MQTTPublish {
    type Res = u8;

    fn val(&self) -> Self::Res {
        self.result
    }
}

impl From<MQTTReceive> for Message {
    fn from(value: MQTTReceive) -> Self {
        Self {
            topic: value.topic,
            payload: value.payload.bytes.into_iter().map(|x| x).collect(),
        }
    }
}

pub trait MqttAck {
    fn parse_packet_result(
        &self,
        expected_msg_id: u16,
    ) -> Result<MQTTPacketResult, MQTTPacketResult>;
}

impl MqttAck for MQTTSubscribe {
    fn parse_packet_result(
        &self,
        expected_msg_id: u16,
    ) -> Result<MQTTPacketResult, MQTTPacketResult> {
        let result = self.parse();
        if expected_msg_id == self.msg_id {
            Ok(result)
        } else {
            Err(result)
        }
    }
}
impl MqttAck for MQTTUnsubscribe {
    fn parse_packet_result(
        &self,
        expected_msg_id: u16,
    ) -> Result<MQTTPacketResult, MQTTPacketResult> {
        let result = self.parse();
        if expected_msg_id == self.msg_id {
            Ok(result)
        } else {
            Err(result)
        }
    }
}
impl MqttAck for MQTTPublish {
    fn parse_packet_result(
        &self,
        expected_msg_id: u16,
    ) -> Result<MQTTPacketResult, MQTTPacketResult> {
        let result = self.parse();
        if expected_msg_id == self.msg_id {
            Ok(result)
        } else {
            Err(result)
        }
    }
}

/// 4.1 The URC begins with “+QMTSTAT:”.
/// It will be reported when there is a change in the state of MQTT link layer.
#[derive(Clone, Debug, AtatResp)]
pub struct MQTTStatus {
    #[allow(dead_code)]
    pub client_idx: u8,
    pub err_code: u8,
}

/// See 4.1
#[repr(u8)]
#[derive(Debug, TryFromPrimitive)]
pub enum MQTTStatusResult {
    /// Connection is closed or reset by peer.
    ConnectionClosed = 1,

    /// Sending PINGREQ packet timed out or failed.
    PingreqTimeout = 2,

    /// Sending CONNECT packet timed out or failed.
    ConnectTimeout = 3,

    /// Receiving CONNECK packet timed out or failed.
    ConneckTimout = 4,

    /// The client sends DISCONNECT packet to sever and
    /// the server is initiative to close MQTT connection.
    /// NB: This is a normal process.
    CloseAfterDisconnect = 5,

    /// The client initiated to close MQTT connection
    /// due to packet sending failure all the time.
    ClosedDueToPacketFailure = 6,

    /// The link is not alive or the server is unavailable.
    ServerUnavailable = 7,

    /// Unexpecteed result: check specs.
    Unknown = 99,
}

impl Default for MQTTStatusResult {
    fn default() -> Self {
        Self::Unknown
    }
}

impl URCParse<MQTTStatusResult> for MQTTStatus {
    type Res = u8;

    fn val(&self) -> u8 {
        self.err_code
    }
}
