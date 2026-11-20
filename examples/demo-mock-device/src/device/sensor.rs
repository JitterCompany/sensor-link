//! Simulated demo channels
//!

use crate::signal_gen::*;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use demo_firmware_core::logic::demo_processing::{
    input::{DemoChannel, Samples, DEMO_N_CH, N_SAMPLES},
    INPUT_FS,
};
use rand::Rng;
use std::fmt::Debug;

/// One simulated signal per demo channel, with noise
#[derive(Debug)]
pub struct SignalGenerator {
    /// Sample rate in Hz
    sample_rate: f64,

    /// One signal per channel, in [`DemoChannel`] order
    signals: [Box<dyn Signal>; DEMO_N_CH],

    /// Peak noise per channel, in the unit of the channel
    noise: [f64; DEMO_N_CH],
}

impl SignalGenerator {
    /// Values in the range a healthy SL23-modem-tester reports on a bench
    /// supply: 25 C board, 12 V adapter, tens of mA, 6.55 V system rail.
    pub fn new() -> Self {
        let signal = |offset: f64, amplitude: f64, period_s: f64| {
            Box::new(Offset::new(
                offset,
                SineSignal {
                    amplitude,
                    frequency: 1.0 / period_s,
                    phase: 0.0,
                },
            )) as Box<dyn Signal>
        };
        let signals = DemoChannel::ALL.map(|ch| match ch {
            DemoChannel::BoardTemperature => signal(25.0, 2.0, 3600.0),
            DemoChannel::Vbus => signal(12.0, 0.05, 600.0),
            DemoChannel::Ibus => signal(0.05, 0.02, 300.0),
            DemoChannel::Vsys => signal(6.55, 0.01, 600.0),
        });

        Self {
            sample_rate: f64::from(INPUT_FS.value()),
            signals,
            noise: [0.05, 0.01, 0.002, 0.005],
        }
    }

    /// Generate one batch of samples, starting at `timestamp`
    pub fn generate(&mut self, timestamp: DateTime<Utc>) -> Samples {
        let period = ChronoDuration::microseconds(1_000_000 / self.sample_rate as i64);

        let mut result = Samples {
            t: timestamp.timestamp_micros(),
            ch: [None; DEMO_N_CH],
        };

        for (ch, samples) in result.ch.iter_mut().enumerate() {
            *samples = Some(core::array::from_fn(|i| {
                let t_secs = timestamp_secs(timestamp + period * i as i32);
                Self::add_noise(self.signals[ch].compute(t_secs), self.noise[ch])
            }));
        }

        result
    }

    /// Add noise to a value (noise_amp is peak amplitude)
    fn add_noise(value: f64, noise_amp: f64) -> f32 {
        if noise_amp > 0.0 {
            (value + rand::thread_rng().gen_range(-noise_amp..noise_amp)) as f32
        } else {
            value as f32
        }
    }
}

/// Duration of one batch of [`N_SAMPLES`] samples
pub fn batch_duration() -> std::time::Duration {
    std::time::Duration::from_micros(N_SAMPLES as u64 * INPUT_FS.interval_us() as u64)
}

fn timestamp_secs(t: DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_micros() as f64 * 1e-6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_are_one_second_of_plausible_values() {
        assert_eq!(batch_duration(), std::time::Duration::from_secs(1));

        let samples = SignalGenerator::new().generate(Utc::now());
        let temp = samples.ch[DemoChannel::BoardTemperature.index()].unwrap();
        assert!(temp.iter().all(|t| (20.0..30.0).contains(t)));
        let vsys = samples.ch[DemoChannel::Vsys.index()].unwrap();
        assert!(vsys.iter().all(|v| (6.4..6.7).contains(v)));
    }
}
