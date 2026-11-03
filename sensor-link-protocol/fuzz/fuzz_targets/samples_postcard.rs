#![no_main]

use libfuzzer_sys::fuzz_target;
use sensor_link_protocol::samples::{NChannelSamples, UniformSamples};

fuzz_target!(|data: &[u8]| {
    let Ok(samples) = UniformSamples::<3>::from_topic_data(data) else {
        return;
    };
    // Accepted samples hold no NaN, so a re-encode must decode to the same value.
    let mut buffer = [0u8; 2048];
    let encoded = samples.as_topic_data(&mut buffer).unwrap();
    let decoded = UniformSamples::<3>::from_topic_data(encoded).unwrap();
    assert_eq!(samples, decoded);
    let _ = NChannelSamples::from(samples);
});
