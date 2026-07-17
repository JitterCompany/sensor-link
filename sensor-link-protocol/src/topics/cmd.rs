use super::TopicPayloadSerialize;
use crate::MAX_MESSAGE_LEN;
use serde::{Deserialize, Serialize};

/// Command variants that can be send over the cmd topic
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Cmd {
    Start,
    Stop,
    Blink,
    Reboot,
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
