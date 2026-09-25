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
    /// Start publishing the device's own log records on
    /// [TopicFromDevice::Log](crate::TopicFromDevice::Log).
    DiagnosticsOn,
    /// Stop publishing the device's own log records on
    /// [TopicFromDevice::Log](crate::TopicFromDevice::Log).
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

    /// The command names on the wire, in both directions.
    #[test]
    fn json_command_names() {
        let names = [
            (Cmd::Start, "start"),
            (Cmd::Stop, "stop"),
            (Cmd::Blink, "blink"),
            (Cmd::Reboot, "reboot"),
            (Cmd::DiagnosticsOn, "diagnostics_on"),
            (Cmd::DiagnosticsOff, "diagnostics_off"),
        ];
        for (cmd, name) in names {
            let json: heapless::String<100> =
                serde_json_core::to_string(&CommandPayload { cmd: cmd.clone() }).unwrap();
            assert_eq!(json, format!("{{\"cmd\":\"{name}\"}}").as_str());

            let decoded = parse_json_payload::<CommandPayload>(json.as_bytes());
            assert_eq!(decoded.map(|c| c.cmd), Some(cmd));
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
