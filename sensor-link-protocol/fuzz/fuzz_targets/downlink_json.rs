#![no_main]

use libfuzzer_sys::fuzz_target;
use sensor_link_protocol::{
    cmd::CommandPayload,
    fwupdate::{FWAnnounce, FWChunk},
    parse_json_payload, parse_topic_to_device,
    server_status::ServerStatus,
    time::Timestamp,
};

// Every decoder the firmware runs on a payload received from the broker.
fuzz_target!(|data: &[u8]| {
    if let Ok(topic) = core::str::from_utf8(data) {
        let _ = parse_topic_to_device(topic);
    }
    let _ = ServerStatus::from_slice(data);
    let _ = parse_json_payload::<CommandPayload>(data);
    let _ = parse_json_payload::<Timestamp>(data);
    let _ = parse_json_payload::<FWAnnounce>(data);
    let _ = parse_json_payload::<FWChunk>(data);
});
