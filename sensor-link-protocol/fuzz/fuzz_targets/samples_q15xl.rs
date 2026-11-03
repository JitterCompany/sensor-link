#![no_main]

use libfuzzer_sys::fuzz_target;
use sensor_link_protocol::samples::{
    deserialize::{parse_q15xl_to_nchannel, parse_q15xl_to_nchannel_allow_nan},
    q15xl::{Uniform, Q15XL},
};

fuzz_target!(|data: &[u8]| {
    let _ = parse_q15xl_to_nchannel::<3>(data);
    let _ = parse_q15xl_to_nchannel_allow_nan::<3>(data);

    let Ok(samples) = Uniform::<Q15XL<3, 100>>::from_topic_data(data) else {
        return;
    };
    let mut buffer = [0u8; Uniform::<Q15XL<3, 100>>::MAX_SERIALIZED_SIZE];
    let encoded = samples.as_topic_data(&mut buffer).unwrap();
    let decoded = Uniform::<Q15XL<3, 100>>::from_topic_data(encoded).unwrap();
    assert_eq!(samples, decoded);
});
