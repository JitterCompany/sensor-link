use sensor_link_protocol::samples::NChannelSamples;

pub use sensor_link_protocol::server::{
    parse_device_info_v3, parse_event, parse_online, parse_uniform_samples_allow_nan, Error,
};

use crate::{data::std::Data, DemoChannel as Ch, NUM_CH_DEMO};

/// Parse data from [TopicFromDevice::Data](crate::TopicFromDevice::Data)
#[inline]
pub fn parse_data(bytes: &[u8]) -> Result<Data, Error> {
    let samples: NChannelSamples<NUM_CH_DEMO> = parse_uniform_samples_allow_nan(bytes)?;
    let ch = |c: Ch| samples.ch[c.index()].clone();
    Ok(Data {
        t: samples.t,
        fs: samples.fs,
        board_temperature: ch(Ch::BoardTemperature),
        vbus: ch(Ch::Vbus),
        ibus: ch(Ch::Ibus),
        vsys: ch(Ch::Vsys),
    })
}
