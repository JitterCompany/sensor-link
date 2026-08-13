//! AT Commands for Quectel modems (EC2x/EG915N)
//!
//! Commands are structured according to the quectel command manuals.
//! Each module defines commands, responses and URCs.

pub mod file;
pub mod general;
pub mod http;
pub mod mqtt;
pub mod packetdomain;
pub mod ssl;
pub mod tcpip;

use core::fmt::Debug;

use atat::atat_derive::{AtatResp, AtatUrc};

#[derive(Debug, Clone, AtatResp)]
pub struct NoResponse;

#[derive(Clone, Debug)]
pub enum Urc {
    ModemReady,

    ModemOff,

    MQTTOpen(mqtt::urc::MQTTOpen),

    MQTTClose(mqtt::urc::MQTTClose),

    MQTTConnect(mqtt::urc::MQTTConnect),

    MQTTDisconnect(mqtt::urc::MQTTDisconnect),

    MQTTSubscribe(mqtt::urc::MQTTSubscribe),

    MQTTUnsubscribe(mqtt::urc::MQTTUnsubscribe),

    MQTTPublish(mqtt::urc::MQTTPublish),

    MQTTStatus(mqtt::urc::MQTTStatus),

    QIURC(tcpip::urc::QIURC),

    // This one is disabled, because atat cannot see the
    // difference between +CREG urc and the +CREG response
    CREG(general::urc::CREG),

    // This one is disabled, because atat cannot see the
    // difference between +CEREG urc and the +CEREG response
    EREG(general::urc::CREG),

    MQTTReceive(mqtt::urc::MQTTReceiveWithoutData),

    HTTPGet(http::responses::GetResponse),
    HTTPReadFile(http::responses::ReadFileResponse),
}

/// This enum only is used for detecting the prefix via atat::Parser.
/// Urc structs are parsed via a custom parser.
#[derive(Clone, AtatUrc, Debug)]
#[allow(dead_code)]
enum CustomUrc {
    #[at_urc("+DUMMY")]
    MQTTReceive(mqtt::urc::MQTTReceive),
}

#[derive(Clone, AtatUrc, Debug)]
enum DefaultUrc {
    #[at_urc("RDY")]
    ModemReady,

    #[at_urc("POWERED DOWN")]
    ModemOff,

    #[at_urc("+QMTOPEN")]
    MQTTOpen(mqtt::urc::MQTTOpen),

    #[at_urc("+QMTCLOSE")]
    MQTTClose(mqtt::urc::MQTTClose),

    #[at_urc("+QMTCONN")]
    MQTTConnect(mqtt::urc::MQTTConnect),

    #[at_urc("+QMTDISC")]
    MQTTDisconnect(mqtt::urc::MQTTDisconnect),

    #[at_urc("+QMTSUB")]
    MQTTSubscribe(mqtt::urc::MQTTSubscribe),

    #[at_urc("+QMTUNS")]
    MQTTUnsubscribe(mqtt::urc::MQTTUnsubscribe),

    #[at_urc("+QMTPUBEX")]
    MQTTPublish(mqtt::urc::MQTTPublish),

    #[at_urc("+QMTRECV")]
    MQTTReceive(mqtt::urc::MQTTReceiveWithoutData),

    #[at_urc("+QMTSTAT")]
    MQTTStatus(mqtt::urc::MQTTStatus),

    #[at_urc("+QIURC")]
    QIURC(tcpip::urc::QIURC),

    // This one is disabled, because atat cannot see the
    // difference between +CREG urc and the +CREG response
    // See https://github.com/BlackbirdHQ/atat/issues/179 for workaround.
    #[at_urc("+CREG_DISABLED")]
    CREG(general::urc::CREG),

    // This one is disabled, because atat cannot see the
    // difference between +CEREG urc and the +CEREG response
    #[at_urc("+CEREG_DISABLED")]
    EREG(general::urc::CREG),

    #[at_urc("+QHTTPGET")]
    HTTPGet(http::responses::GetResponse),

    #[at_urc("+QHTTPREADFILE")]
    HTTPReadFile(http::responses::ReadFileResponse),
}

impl atat::AtatUrc for Urc {
    type Response = Urc;

    fn parse(resp: &[u8]) -> Option<Self::Response> {
        if let Some(urc) = <DefaultUrc as atat::AtatUrc>::parse(resp) {
            Some(match urc {
                DefaultUrc::ModemReady => Urc::ModemReady,
                DefaultUrc::ModemOff => Urc::ModemOff,
                DefaultUrc::MQTTOpen(urc) => Urc::MQTTOpen(urc),
                DefaultUrc::MQTTClose(urc) => Urc::MQTTClose(urc),
                DefaultUrc::MQTTConnect(urc) => Urc::MQTTConnect(urc),
                DefaultUrc::MQTTDisconnect(urc) => Urc::MQTTDisconnect(urc),
                DefaultUrc::MQTTSubscribe(urc) => Urc::MQTTSubscribe(urc),
                DefaultUrc::MQTTUnsubscribe(urc) => Urc::MQTTUnsubscribe(urc),
                DefaultUrc::MQTTPublish(urc) => Urc::MQTTPublish(urc),
                DefaultUrc::MQTTStatus(urc) => Urc::MQTTStatus(urc),
                DefaultUrc::QIURC(urc) => Urc::QIURC(urc),
                DefaultUrc::CREG(urc) => Urc::CREG(urc),
                DefaultUrc::EREG(urc) => Urc::EREG(urc),
                DefaultUrc::MQTTReceive(urc) => Urc::MQTTReceive(urc),
                DefaultUrc::HTTPGet(urc) => Urc::HTTPGet(urc),
                DefaultUrc::HTTPReadFile(urc) => Urc::HTTPReadFile(urc),
            })
        } else {
            None
        }
    }
}

impl atat::Parser for Urc {
    fn parse(buf: &[u8]) -> Result<(&[u8], usize), atat::digest::ParseError> {
        <DefaultUrc as atat::Parser>::parse(buf)
            .or_else(|_| <CustomUrc as atat::Parser>::parse(buf))
    }
}

pub trait URCParse<T: Debug + Default> {
    type Res: core::convert::TryInto<T>;
    fn val(&self) -> Self::Res;

    fn parse(&self) -> T {
        self.val().try_into().unwrap_or_default()
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum UrcVariant {
    ModemReady,
    ModemOff,
    MQTTOpen,
    MQTTClose,
    MQTTConnect,
    MQTTDisconnect,
    MQTTSubscribe,
    MQTTUnsubscribe,
    MQTTReceive,
    MQTTPublish,
    MQTTStatus,
    QIURC,
    CREG,
    EREG,
    HTTPGet,
    HTTPReadFile,
}

impl UrcVariant {
    /// Timeout in ms: how long to wait for this URC to occur
    pub fn timeout(&self) -> u32 {
        match self {
            UrcVariant::MQTTOpen => 120_000,
            UrcVariant::MQTTClose => 31_000,
            UrcVariant::MQTTConnect => 5_000,
            UrcVariant::MQTTDisconnect => 30_000,
            UrcVariant::MQTTSubscribe => 15_000,
            UrcVariant::MQTTUnsubscribe => 15_000,
            UrcVariant::MQTTReceive => 0,
            UrcVariant::MQTTPublish => 15_000,

            // Received if modem is booted. Normally within 15 seconds after power-on, max timeout not specified by Quectel..
            UrcVariant::ModemReady => 20_000,

            // Received when modem shuts down (AT+QPOWD)
            UrcVariant::ModemOff => 65_000,

            // Is send when status changes, so timeout not relevant.
            UrcVariant::MQTTStatus => 0,

            UrcVariant::QIURC => 0,
            // Wait for 90 / 60 seconds according to TCP flow chart.
            UrcVariant::CREG => 90_000,
            UrcVariant::EREG => 60_000,
            UrcVariant::HTTPGet => 60_000,      // todo
            UrcVariant::HTTPReadFile => 60_000, // todo
        }
    }
}

impl Urc {
    pub fn variant(&self) -> UrcVariant {
        match self {
            Urc::ModemReady => UrcVariant::ModemReady,
            Urc::ModemOff => UrcVariant::ModemOff,
            Urc::MQTTClose(_) => UrcVariant::MQTTClose,
            Urc::MQTTConnect(_) => UrcVariant::MQTTConnect,
            Urc::MQTTOpen(_) => UrcVariant::MQTTOpen,
            Urc::MQTTDisconnect(_) => UrcVariant::MQTTDisconnect,
            Urc::MQTTSubscribe(_) => UrcVariant::MQTTSubscribe,
            Urc::MQTTUnsubscribe(_) => UrcVariant::MQTTUnsubscribe,
            Urc::MQTTReceive(_) => UrcVariant::MQTTReceive,
            Urc::MQTTPublish(_) => UrcVariant::MQTTPublish,
            Urc::MQTTStatus(_) => UrcVariant::MQTTStatus,
            Urc::QIURC(_) => UrcVariant::QIURC,
            Urc::CREG(_) => UrcVariant::CREG,
            Urc::EREG(_) => UrcVariant::EREG,
            Urc::HTTPGet(_) => UrcVariant::HTTPGet,
            Urc::HTTPReadFile(_) => UrcVariant::HTTPReadFile,
        }
    }
}
#[allow(unused)]
#[cfg(test)]
mod test {

    use super::*;
    use crate::{drivers::quectel::ContextId, utils::LossyStr};
    use atat::{AtatCmd, DefaultDigester, DigestResult, Digester};

    #[test]
    fn test_urc_parsing() {
        let mut digester = DefaultDigester::<Urc>::new();

        let input = String::try_from("\r\n+QMTRECV: 1,0\r\n").unwrap();
        let res = digester.digest(input.as_bytes());
        match res {
            (DigestResult::Urc(urc_line), _) => {
                if let Some(urc) = <Urc as atat::AtatUrc>::parse(urc_line) {
                    println!("urc: {urc:?}");
                } else {
                    println!("Parsing URC FAILED: {:?}", LossyStr(urc_line));
                    panic!("parsing failed");
                }
            }
            _ => {
                panic!("unexpected result");
            }
        }
    }

    #[test]
    fn test_urc_with_read_command() {
        simple_logger::init().ok();
        let mut digester = DefaultDigester::<Urc>::new();

        let input = String::from(
            "\r\n+QMTRECV: 1,0\r\n+QMTRECV: 1,0,\"t/dev/commands\",16,\"{\"cmd\": \"blink\"}\"\r\n\r\nOK\r\n",
        );
        let res = digester.digest(input.as_bytes());
        let swallowed = match res {
            (DigestResult::Urc(urc_line), swallowed) => {
                if let Some(urc) = <Urc as atat::AtatUrc>::parse(urc_line) {
                    println!("urc: {urc:?}");
                } else {
                    println!("Parsing URC FAILED: {:?}", LossyStr(urc_line));
                    panic!("parsing failed");
                }
                swallowed
            }
            _ => {
                panic!("unexpected result");
            }
        };
        println!("Swallowed: {}", swallowed);
        println!("rest: {:?}", input.split_at(swallowed).1);

        let rest = String::from(
            "+QMTRECV: 1,0,\"t/dev/commands\",16,\"{\"cmd\": \"blink\"}\"\r\n\r\nOK\r\n",
        );

        let res = digester.digest(rest.as_bytes());

        match res {
            (DigestResult::Response(bytes), swallowed) => {
                println!("rest recognized as Response");
            }
            res => {
                panic!("unexpected result: {res:?}");
            }
        };
    }

    #[test]
    fn test_cmd_qmtpubex() {
        use atat::AtatLen;

        let publish_cmd = mqtt::PublishPrepare {
            client_idx: ContextId(1),
            msg_id: 65535,
            qos: 2,
            retain: 0,
            topic: heapless::String::try_from("my_topic_is_40_characters_long1234567890").unwrap(),
            msg_len: 65535,
        };
        let mut buff = [0; 128];
        let len = publish_cmd.write(&mut buff);
        let serialized = core::str::from_utf8(&buff[..len]).unwrap();
        assert_eq!(
            serialized,
            "AT+QMTPUBEX=1,65535,2,0,\"my_topic_is_40_characters_long1234567890\",65535\r"
        );
        assert_eq!(serialized.len(), 73);
        assert_eq!(mqtt::PublishPrepare::LEN, 66);
        assert!(mqtt::PublishPrepare::MAX_LEN > serialized.len());
    }
}
