//! Formatting of the messages a device publishes.

use chrono::{DateTime, Utc};
use sensor_link_protocol::{
    device_log::LogMessage, parse_topic_from_device, server::parse_uniform_samples_allow_nan,
    TopicFromDevice,
};

/// Binary payloads are shown up to this many bytes.
const MAX_HEX_BYTES: usize = 64;

/// Q15XL header: timestamp (8) + sample rate (4) + number of samples (2).
const Q15XL_HEADER_LEN: usize = 14;

/// The highest channel count [`format_samples`] tries to decode.
const MAX_CHANNELS: usize = 8;

/// Format a received message as a header line and an indented body.
///
/// With `raw`, the payload is shown as received rather than decoded.
pub fn format_message(topic: &str, payload: &[u8], raw: bool) -> String {
    // UTC, like the timestamps of this tool's own log lines.
    let time = Utc::now().format("%H:%M:%S%.3f");
    let (name, body) = match parse_topic_from_device(topic) {
        Ok(parts) if !raw => (format!("{:?}", parts.topic), decode(parts.topic, payload)),
        _ => (topic.to_string(), raw_payload(payload)),
    };

    let mut output = format!("[{time}] {name}");
    for line in body.lines() {
        output.push_str("\n  ");
        output.push_str(line);
    }
    output
}

fn decode(topic: TopicFromDevice, payload: &[u8]) -> String {
    match topic {
        TopicFromDevice::Log => match serde_json::from_slice::<LogMessage>(payload) {
            Ok(log) => format!(
                "{} {} {}: {}",
                format_ms(log.ts),
                log.level.as_str().to_uppercase(),
                log.target,
                log.msg
            ),
            Err(err) => format!("(parse error: {err}) {}", raw_payload(payload)),
        },
        TopicFromDevice::BenchmarkData => format_samples(payload)
            .unwrap_or_else(|err| format!("(parse error: {err}) {}", raw_payload(payload))),
        // Everything else is JSON, of which products define parts (events,
        // device info, status), so it is shown as JSON rather than parsed into
        // the protocol's types.
        _ => pretty_json(payload),
    }
}

/// Summarize Q15XL uniform samples.
///
/// The payload doesn't say how many channels it holds, so that follows from its
/// length: each channel takes an exponent byte plus two bytes per sample.
fn format_samples(payload: &[u8]) -> Result<String, String> {
    let header = payload
        .get(..Q15XL_HEADER_LEN)
        .ok_or("payload shorter than its header")?;
    let n_samples = u16::from_le_bytes([header[12], header[13]]) as usize;
    let data_len = payload.len() - Q15XL_HEADER_LEN;
    let bytes_per_ch = 1 + 2 * n_samples;
    if n_samples == 0 || !data_len.is_multiple_of(bytes_per_ch) {
        return Err(format!(
            "{data_len} data bytes don't hold whole channels of {n_samples} samples"
        ));
    }

    match data_len / bytes_per_ch {
        1 => samples_summary::<1>(payload),
        2 => samples_summary::<2>(payload),
        3 => samples_summary::<3>(payload),
        4 => samples_summary::<4>(payload),
        5 => samples_summary::<5>(payload),
        6 => samples_summary::<6>(payload),
        7 => samples_summary::<7>(payload),
        8 => samples_summary::<8>(payload),
        n => Err(format!("{n} channels, at most {MAX_CHANNELS} supported")),
    }
}

fn samples_summary<const N_CH: usize>(payload: &[u8]) -> Result<String, String> {
    let samples = parse_uniform_samples_allow_nan::<N_CH>(payload).map_err(|e| format!("{e:?}"))?;
    let (first, last) = match (samples.t.first(), samples.t.last()) {
        (Some(first), Some(last)) => (*first, *last),
        _ => return Ok("no samples".to_string()),
    };

    let mut output = format!(
        "{} samples × {N_CH} channels @ {} Hz, {} … {}",
        samples.t.len(),
        samples.fs,
        format_us(first),
        format_us(last),
    );
    for (i, ch) in samples.ch.iter().enumerate() {
        let min = ch.iter().copied().fold(f32::INFINITY, f32::min);
        let max = ch.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mean = ch.iter().sum::<f32>() / ch.len() as f32;
        output.push_str(&format!(
            "\nch{i}: min {min:.4}, max {max:.4}, mean {mean:.4}"
        ));
    }
    Ok(output)
}

fn format_ms(ms: i64) -> String {
    format_us(ms.saturating_mul(1000))
}

fn format_us(us: i64) -> String {
    match DateTime::from_timestamp_micros(us) {
        Some(time) => time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        None => format!("{us} µs"),
    }
}

fn pretty_json(payload: &[u8]) -> String {
    match serde_json::from_slice::<serde_json::Value>(payload) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap(),
        Err(err) => format!("(JSON parse error: {err}) {}", raw_payload(payload)),
    }
}

fn raw_payload(payload: &[u8]) -> String {
    match std::str::from_utf8(payload) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let hex: String = payload
                .iter()
                .take(MAX_HEX_BYTES)
                .map(|b| format!("{b:02x}"))
                .collect();
            let ellipsis = if payload.len() > MAX_HEX_BYTES {
                "…"
            } else {
                ""
            };
            format!("(binary, {} bytes) 0x{hex}{ellipsis}", payload.len())
        }
    }
}
