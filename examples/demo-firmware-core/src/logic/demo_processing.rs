//! Processing task of the demo device: filters and decimates the sampled
//! channels to the configured data rate.

mod filters;
mod pipeline;

use demo_protocol::settings::DataRate;

#[allow(unused)]
use crate::utils::float::EmbeddedF32;

use crate::{
    logic::{
        demo_processing::{
            input::{SampleRate, DEMO_N_CH},
            pipeline::Pipeline,
        },
        dispatch::demo_buffer::{DemoResults, SampleData},
        processing::fs_estimate,
        settings::Settings,
        signal::Signal,
        FiniteStream, LatestValueReceiveChannel, ProcessingOutputT, ReceiveChannel, SendChannel,
    },
    monotonic_time,
    sync::session_channel::traits::SessionReceiver,
};

use futures::FutureExt as _;

/// Input data for the processing task
pub mod input {
    pub use demo_protocol::{DemoChannel, NUM_CH_DEMO as DEMO_N_CH};

    /// Number of samples per channel in one batch
    pub const N_SAMPLES: usize = 4;

    // One batch is one second: the pipeline requires a batch no longer than
    // its decimation factor, which is the input rate at the 1 SPS output.
    const _: () = assert!(N_SAMPLES as u32 == super::INPUT_FS.value() as u32);

    /// One batch of samples
    pub struct Samples {
        /// Timestamp of the first sample in the set.
        pub t: i64,

        /// Samples per channel, in [`DemoChannel`] order and in the unit of the
        /// data stream; None = channel fault
        pub ch: [Option<[f32; N_SAMPLES]>; DEMO_N_CH],
    }

    #[derive(Debug, Clone, Copy)]
    pub enum SampleRate {
        FS100 = 100,
        FS50 = 50,
        FS10 = 10,
        FS4 = 4,
    }

    impl SampleRate {
        /// Sample frequency value in Hz
        pub const fn value(&self) -> f32 {
            match self {
                SampleRate::FS100 => 100.0,
                SampleRate::FS50 => 50.0,
                SampleRate::FS10 => 10.0,
                SampleRate::FS4 => 4.0,
            }
        }

        /// Sample interval in microseconds
        pub const fn interval_us(&self) -> i64 {
            (1.0e6_f32 / self.value()) as i64
        }
    }
}

/// Rate of the incoming sample stream
pub const INPUT_FS: SampleRate = SampleRate::FS4;

/// Task will start data_in session within this time
pub const TASK_INIT_TIMEOUT_MS: u32 = 1_000;

/// Output sample rate (Hz) for a configured data rate
pub fn output_fs(data_rate: &DataRate) -> f32 {
    match data_rate {
        DataRate::_1SPS => 1.0,
        DataRate::_1SPM => 1.0 / 60.0,
        DataRate::_6SPH => 6.0 / 3600.0,
        DataRate::_2SPH => 2.0 / 3600.0,
        DataRate::_1SPH => 1.0 / 3600.0,
    }
}

#[inline(never)]
pub async fn task<
    DQI: SessionReceiver<input::Samples>,
    DQO: SendChannel<ProcessingOutputT<DemoResults>>,
    SI: LatestValueReceiveChannel<Settings>,
    SQO: SendChannel<Signal>,
>(
    data_in: &mut DQI,
    data_out: &mut DQO,
    settings: Settings,
    settings_in: &mut SI,
    _signal_out: &mut SQO,
) {
    log::info!(target: "Processing","Starting task");

    let mut data_rate = settings.data_rate;

    let mut pipelines: [Pipeline; DEMO_N_CH] =
        core::array::from_fn(|_| Pipeline::new(INPUT_FS, output_fs(&data_rate)));

    // Check fs is within 1% of expected (what is the spec?)
    let mut fs_estimate = fs_estimate::FSEstimate::new(INPUT_FS.value(), INPUT_FS.value() * 0.01);

    // Start receive session
    if let Ok(mut data_in) = data_in.start() {
        loop {
            match data_in.recv().await {
                // End of stream: stop processing task
                Ok(FiniteStream::End) => {
                    break;
                }

                // Reset is unused: the measurement task never sends it.
                Ok(FiniteStream::Reset) => {
                    log::warn!(target: "Processing", "Unexpected Reset signal, ignoring");
                }

                // Data samples to process
                Ok(FiniteStream::Data(mut raw_samples)) => {
                    // New settings: change the output interval if required
                    if let Some(Ok(new_settings)) = settings_in.recv().now_or_never() {
                        if new_settings.data_rate != data_rate {
                            log::info!(target: "Processing", "Data rate: {:?} -> {:?}", data_rate, new_settings.data_rate);
                            data_rate = new_settings.data_rate;
                            let fs = output_fs(&data_rate);
                            for pipeline in pipelines.iter_mut() {
                                pipeline.set_output_fs(fs);
                            }
                        }
                    }

                    if let fs_estimate::FSIntegrity::Outofbounds(fs_est) =
                        fs_estimate.check(input::N_SAMPLES, monotonic_time::now())
                    {
                        log::warn!(target: "Processing","FS estimate invalid: {fs_est}");
                    }

                    let t_start = raw_samples.t;

                    // All channels share the same timebase, so their output samples are combined
                    // into a single result. Channels without data stay NaN.
                    let mut t_output = None;
                    let mut output = [f32::NAN; DEMO_N_CH];

                    let channels = raw_samples.ch.iter_mut().enumerate();

                    for (ch, data) in channels {
                        let Some(data) = data.as_mut() else {
                            pipelines[ch].reset();
                            continue;
                        };

                        if let Some((t_sample, sample)) = pipelines[ch].process(t_start, data) {
                            t_output = Some(t_sample);
                            output[ch] = sample;
                        }
                    }

                    // No output sample available (yet): pipelines decimate to a lower output rate
                    let Some(t_output) = t_output else {
                        continue;
                    };

                    let result = DemoResults::Data(SampleData::from_slices(
                        t_output,
                        pipelines[0].output_fs(),
                        core::array::from_fn(|ch| core::slice::from_ref(&output[ch])),
                    ));

                    if data_out
                        .send(ProcessingOutputT::Data(result))
                        .await
                        .is_err()
                    {
                        log::error!(target: "Processing","Failed to send data");
                    }
                }

                Err(error) => {
                    log::error!(target: "Processing", "task failed: {:?}", error);
                    break;
                }
            }
        }
    }

    log::info!(target: "Processing","Stopping task");
}

#[cfg(test)]
mod test {
    use super::SampleRate;

    const fn validate_fs(fs: SampleRate) {
        assert!(1.0 == fs.value() as f64 * fs.interval_us() as f64 / 1e6);
    }

    #[test]
    fn test_sample_rate() {
        validate_fs(SampleRate::FS100);
        validate_fs(SampleRate::FS50);
        validate_fs(SampleRate::FS10);
        validate_fs(SampleRate::FS4);
    }
}
