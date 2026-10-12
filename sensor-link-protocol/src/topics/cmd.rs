use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

/// Command variants that can be send over the cmd topic
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Cmd {
    Start,
    Stop,
    Blink,
    Reboot,
    /// Enter diagnostic mode for `timeout_s` seconds: the device stays online
    /// and publishes its own log records on
    /// [TopicFromDevice::Log](crate::TopicFromDevice::Log).
    ///
    /// On the wire: `{"cmd":{"diagnostics_on":{"timeout_s":600}}}`. The device
    /// caps the timeout at [MAX_DIAGNOSTICS_TIMEOUT_S](crate::MAX_DIAGNOSTICS_TIMEOUT_S).
    /// Sending it again while in diagnostic mode restarts the timeout.
    DiagnosticsOn {
        timeout_s: u32,
    },
    /// Leave diagnostic mode before its timeout expires.
    DiagnosticsOff,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub struct CommandPayload {
    pub cmd: Cmd,
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for CommandPayload {}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::topics::parse_json_payload;
    use heapless::String;

    #[test]
    fn json_decode_command() {
        let s: heapless::String<100> = String::try_from("{\"cmd\": \"blink\"}").unwrap();
        dbg!(&s);
        println!("payload: {:?}", s.as_bytes());

        let cmd = parse_json_payload::<CommandPayload>(s.as_bytes());

        assert!(cmd.is_some());
    }

    /// The commands on the wire, in both directions.
    #[test]
    fn json_commands() {
        let commands = [
            (Cmd::Start, r#""start""#),
            (Cmd::Stop, r#""stop""#),
            (Cmd::Blink, r#""blink""#),
            (Cmd::Reboot, r#""reboot""#),
            (
                Cmd::DiagnosticsOn { timeout_s: 600 },
                r#"{"diagnostics_on":{"timeout_s":600}}"#,
            ),
            (Cmd::DiagnosticsOff, r#""diagnostics_off""#),
        ];
        for (cmd, wire) in commands {
            let json: heapless::String<100> =
                serde_json_core::to_string(&CommandPayload { cmd: cmd.clone() }).unwrap();
            assert_eq!(json, format!("{{\"cmd\":{wire}}}").as_str());

            let decoded = parse_json_payload::<CommandPayload>(json.as_bytes());
            assert_eq!(decoded.map(|c| c.cmd), Some(cmd));
        }
    }

    /// The timeout is what bounds diagnostic mode, so it cannot be left out.
    #[test]
    fn json_decode_diagnostics_on_requires_timeout() {
        for payload in [
            r#"{"cmd":"diagnostics_on"}"#,
            r#"{"cmd":{"diagnostics_on":{}}}"#,
        ] {
            assert!(parse_json_payload::<CommandPayload>(payload.as_bytes()).is_none());
        }
    }

    #[test]
    fn json_decode_empty_payload() {
        let s: heapless::String<100> = String::try_from("").unwrap();
        dbg!(&s);
        println!("payload: {:?}", s.as_bytes());

        let cmd = parse_json_payload::<CommandPayload>(s.as_bytes());

        assert!(cmd.is_none());
    }
    #[test]
    fn json_decode_payload_len_1() {
        let s: heapless::String<100> = String::try_from(" ").unwrap();
        dbg!(&s);
        println!("payload: {:?}", s.as_bytes());

        let cmd = parse_json_payload::<CommandPayload>(s.as_bytes());

        assert!(cmd.is_none());
    }

    #[test]
    fn json_decode_payload_empty_json_object() {
        let s: heapless::String<100> = String::try_from("{}").unwrap();
        dbg!(&s);
        println!("payload: {:?}", s.as_bytes());

        let cmd = parse_json_payload::<CommandPayload>(s.as_bytes());

        assert!(cmd.is_none());
    }
}
