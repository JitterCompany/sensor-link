//! Processing pipeline of one demo channel
//!
//! Filters and decimates incoming channel data to the 1Hz output rate.

use crate::{
    dsp::{decimator::Decimator, iir},
    logic::{
        demo_processing::{filters, input::SampleRate},
        processing::filter_converge::{ConvergeCounter, ConvergeState},
    },
};

#[allow(unused)]
use crate::utils::float::EmbeddedF32;

// Based on filters::LOWPASS_DECIMATE_100HZ step resonse, see tests
pub const CONVERGENCE_TIME_SEC: f32 = 5.0;

pub const MAX_FILTER_STAGES: usize = 1;

pub struct Pipeline {
    // Counter for filter convergence
    counter: ConvergeCounter,

    // Sample rate of incoming measurements
    input_fs: SampleRate,

    // Sample rate of output measurements
    output_fs: f32,

    /// Low pass filter (anti-aliasing)
    lpf: iir::Filter<MAX_FILTER_STAGES>,

    // Decimator to downsample to f=1Hz
    decimator: Decimator,
}

impl Pipeline {
    pub fn new(fs: SampleRate, output_fs: f32) -> Self {
        let counter = {
            let threshold = (CONVERGENCE_TIME_SEC * fs.value()).ceil() as u32;
            ConvergeCounter::new(threshold)
        };

        // Lowpass filter based on datarate (anti-aliasing)
        let lowpass = match fs {
            SampleRate::FS100 => &filters::LOWPASS_DECIMATE_100HZ,
            SampleRate::FS50 => &filters::LOWPASS_DECIMATE_100HZ, // Note: fs=50 results in this having fco=0.2
            SampleRate::FS10 => &filters::LOWPASS_DECIMATE_10HZ,
            SampleRate::FS4 => &filters::LOWPASS_DECIMATE_4HZ,
        };

        let lpf = iir::Filter::from_coefficents(lowpass);

        Self {
            input_fs: fs,
            output_fs,
            lpf,
            decimator: Self::decimator(fs, output_fs),
            counter,
        }
    }

    fn decimator(fs: SampleRate, output_fs: f32) -> Decimator {
        Decimator::new((fs.value() / output_fs).round() as u32)
    }

    /// Output sample frequency (Hz)
    pub fn output_fs(&self) -> f32 {
        self.output_fs
    }

    /// Change the output rate: only the decimator is replaced, the filter keeps its state
    pub fn set_output_fs(&mut self, output_fs: f32) {
        if output_fs == self.output_fs {
            return;
        }
        self.output_fs = output_fs;
        self.decimator = Self::decimator(self.input_fs, output_fs);
    }

    /// Reset internal state (filters + processing)
    pub fn reset(&mut self) {
        self.counter.reset();
        self.reset_analysis();
    }

    /// Reset analysis state, but not the filter convergence
    fn reset_analysis(&mut self) {
        self.decimator.reset();
    }

    /// Process a slice of samples
    /// Samples are assumed to be taken starting at `time_us` (wall clock time) and at the configured sampling interval
    ///
    /// Returns a (time_us, value) tuple if an output sample is available.
    /// This normally happens at the output rate, see [Self::output_fs].
    /// Note that it takes [CONVERGENCE_TIME_SEC] before the first output sample is available.
    pub fn process(&mut self, time_us: i64, samples: &mut [f32]) -> Option<(i64, f32)> {
        // cannot handle more samples than decimation factor, that would give > 1 output sample
        if samples.len() > self.decimator.decimation_factor() as usize {
            panic!("Too many samples");
        }

        // 1. Apply low pass filter (remove high freq noise)
        self.lpf.apply(samples);

        // 2. Suppress output during filterconvergence
        match self.counter.inc_sample_count(samples.len() as u32) {
            ConvergeState::NoData | ConvergeState::Converging(_) => {
                return None;
            }

            // Filter is stable, reset analysis state for next iteration
            ConvergeState::Converged => {
                self.reset_analysis();
                return None;
            }

            // Active: filter is stable, allow output
            ConvergeState::Active => {}
        }

        // 3. Now we have bandwidth-limited signal, apply decimation
        // Downsample by decimation: every n-th sample is passed through (n=self.decimation_factor)
        let mut output = None;

        let sample_interval_us = self.input_fs.interval_us();
        for (i, sample) in samples.iter().cloned().enumerate() {
            if let Some(out) = self.decimator.decimate_sample(sample) {
                let t_sample = time_us + i as i64 * sample_interval_us;
                output = Some((t_sample, out))
            }
        }

        output
    }
}

#[cfg(test)]
mod test {
    use core::f32;

    use assert_float_eq::*;
    use wavegen::{dc_bias, sine, wf};

    use crate::logic::demo_processing::input;

    use super::*;

    const OUTPUT_FS: f32 = 1.0;

    fn run_pipeline(
        t0: i64,
        fs: SampleRate,
        pipeline: &mut Pipeline,
        mut input_signal: Vec<f32>,
    ) -> (Vec<i64>, Vec<f32>) {
        let mut time_now = t0;

        let mut out_signal: Vec<f32> = Vec::with_capacity(input_signal.len());
        let mut out_time: Vec<i64> = Vec::with_capacity(input_signal.len());
        for chunk in input_signal.chunks_mut(input::N_SAMPLES) {
            if let Some((time, sample)) = pipeline.process(time_now, chunk) {
                out_time.push(time);
                out_signal.push(sample);
            }
            time_now += chunk.len() as i64 * fs.interval_us();
        }
        (out_time, out_signal)
    }

    /// step response should not have significant overshoot
    #[test]
    fn test_step_response() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS100;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let dc_offset = 30.0;
        let input_signal = wf!(f32, fs, dc_bias!(dc_offset));
        let signal_len_s = 30;
        let signal_len = signal_len_s * fs as usize;

        let input_signal_vec: Vec<f32> = input_signal.iter().take(signal_len).collect();

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        pipeline.counter = ConvergeCounter::new(0); // test filter behavior including step response (=during convergence)
        let (out_t, out_sig) = run_pipeline(t0, fs_rate, &mut pipeline, input_signal_vec);

        assert_eq!(out_sig.len(), out_t.len());
        assert_eq!(out_t.len(), signal_len / 100); // downsample 100Hz -> 1Hz
        assert!((out_t[0] - t0).abs() < 1_000_000); // first output sample withing 1000ms (1/1000ms = 1Hz)
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000)); // timestamp deltas all exactly 1/1Hz = 1_000_000 us

        // Step response: overshoot within 1% in first 2 seconds (filter converging)
        let tolerance = 0.01;
        let t_converge = CONVERGENCE_TIME_SEC as usize;
        let max = out_sig[..t_converge]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        log::debug!("converge: max: {}", max);
        assert_float_relative_eq!(dc_offset, max, tolerance);

        // Steady-state amplitude error below -88dB (=1um/25mm)
        let tolerance = 0.00004;
        let max = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::min(a, *b));
        assert_float_relative_eq!(dc_offset, max, tolerance);
        assert_float_relative_eq!(dc_offset, min, tolerance);
    }

    /// step response at fs=10Hz input should not have significant overshoot
    #[test]
    fn test_step_response_10hz() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS10;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let dc_offset = 30.0;
        let input_signal = wf!(f32, fs, dc_bias!(dc_offset));
        let signal_len_s = 30;
        let signal_len = signal_len_s * fs as usize;

        let input_signal_vec: Vec<f32> = input_signal.iter().take(signal_len).collect();

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        pipeline.counter = ConvergeCounter::new(0); // test filter behavior including step response (=during convergence)
        let (out_t, out_sig) = run_pipeline(t0, fs_rate, &mut pipeline, input_signal_vec);

        assert_eq!(out_sig.len(), out_t.len());
        assert_eq!(out_t.len(), signal_len / 10); // downsample 10Hz -> 1Hz
        assert!((out_t[0] - t0).abs() < 1_000_000); // first output sample withing 1000ms (1/1000ms = 1Hz)
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000)); // timestamp deltas all exactly 1/1Hz = 1_000_000 us

        // Step response: overshoot within 1% in first seconds (filter converging)
        let tolerance = 0.01;
        let t_converge = CONVERGENCE_TIME_SEC as usize;
        let max = out_sig[..t_converge]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        log::debug!("converge: max: {}", max);
        assert_float_relative_eq!(dc_offset, max, tolerance);

        // Steady-state amplitude error below -88dB
        let tolerance = 0.00004;
        let max = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::min(a, *b));
        assert_float_relative_eq!(dc_offset, max, tolerance);
        assert_float_relative_eq!(dc_offset, min, tolerance);
    }

    /// step response at fs=4Hz input (the production rate) should not have
    /// significant overshoot
    #[test]
    fn test_step_response_4hz() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS4;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let dc_offset = 30.0;
        let input_signal = wf!(f32, fs, dc_bias!(dc_offset));
        let signal_len_s = 30;
        let signal_len = signal_len_s * fs as usize;

        let input_signal_vec: Vec<f32> = input_signal.iter().take(signal_len).collect();

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        pipeline.counter = ConvergeCounter::new(0); // test filter behavior including step response (=during convergence)
        let (out_t, out_sig) = run_pipeline(t0, fs_rate, &mut pipeline, input_signal_vec);

        assert_eq!(out_sig.len(), out_t.len());
        assert_eq!(out_t.len(), signal_len / 4); // downsample 4Hz -> 1Hz
        assert!((out_t[0] - t0).abs() < 1_000_000); // first output sample withing 1000ms (1/1000ms = 1Hz)
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000)); // timestamp deltas all exactly 1/1Hz = 1_000_000 us

        // Step response: overshoot within 1% in first seconds (filter converging)
        let tolerance = 0.01;
        let t_converge = CONVERGENCE_TIME_SEC as usize;
        let max = out_sig[..t_converge]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        log::debug!("converge: max: {}", max);
        assert_float_relative_eq!(dc_offset, max, tolerance);

        // Steady-state amplitude error below -88dB
        let tolerance = 0.00004;
        let max = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::min(a, *b));
        assert_float_relative_eq!(dc_offset, max, tolerance);
        assert_float_relative_eq!(dc_offset, min, tolerance);
    }

    /// Switching the output rate re-times the output without a convergence gap
    #[test]
    fn test_set_output_fs_keeps_filter_state() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS4;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let dc_offset = 30.0;
        let input_signal = wf!(f32, fs, dc_bias!(dc_offset));

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        let len_1hz = 20 * fs as usize;
        let (out_t, _) = run_pipeline(
            t0,
            fs_rate,
            &mut pipeline,
            input_signal.iter().take(len_1hz).collect(),
        );
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000));

        let new_fs = 1.0 / 60.0;
        pipeline.set_output_fs(new_fs);
        assert_eq!(pipeline.output_fs(), new_fs);

        let t1 = t0 + len_1hz as i64 * fs_rate.interval_us();
        let len_1spm = 180 * fs as usize;
        let (out_t, out_sig) = run_pipeline(
            t1,
            fs_rate,
            &mut pipeline,
            input_signal.iter().take(len_1spm).collect(),
        );
        assert_eq!(out_t.len(), 3);
        assert!(out_t[0] - t1 < 60_000_000); // first sample within one new period
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 60_000_000));
        for sample in out_sig {
            assert_float_relative_eq!(dc_offset, sample, 0.00004);
        }
    }

    /// 0.2 Hz should pass through with minimal distortion
    #[test]
    fn test_simple_sine_100hz_pass_0_2() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS100;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let f1: f32 = 0.2;
        let dc_offset = 30.0;
        let amplitude = 53.2;
        let input_signal = wf!(f32, fs, sine!(f1, amplitude, 0.0_f32), dc_bias!(dc_offset));
        let signal_len_s = 30;
        let signal_len = signal_len_s * fs as usize;

        let input_signal_vec: Vec<f32> = input_signal.iter().take(signal_len).collect();

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        pipeline.counter = ConvergeCounter::new(0); // test filter behavior including step response (during convergence)
        let (out_t, out_sig) = run_pipeline(t0, fs_rate, &mut pipeline, input_signal_vec);

        assert_eq!(out_sig.len(), out_t.len());
        assert_eq!(out_t.len(), signal_len / 100); // downsample 100Hz -> 1Hz
        assert!((out_t[0] - t0).abs() < 1_000_000); // first output sample withing 1000ms (1/1000ms = 1Hz)
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000)); // timestamp deltas all exactly 1/1Hz = 1_000_000 us

        // Amplitude overshoot within 2% (filter converging)
        let tolerance = 0.02;
        let t_converge = CONVERGENCE_TIME_SEC as usize;
        let max = out_sig[..t_converge]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[..t_converge]
            .iter()
            .fold(f32::NAN, |a, b| f32::min(a, *b));
        log::debug!("max: {}, min: {}", max, min);
        assert!(max * (1.0 + tolerance) < dc_offset + amplitude);

        // AC-amplitude is within 15% after filter converging
        let tolerance = 0.15;
        let max = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[t_converge..]
            .iter()
            .fold(f32::NAN, |a, b| f32::min(a, *b));
        assert_float_relative_eq!(amplitude, max - dc_offset, tolerance);
        assert_float_relative_eq!(amplitude, -min + dc_offset, tolerance);
    }

    /// 10 Hz be rejected by anti-alias filter
    #[test]
    fn test_simple_sine_reject_10() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS100;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let f1: f32 = 10.0;
        let amplitude = 53.2;
        let input_signal = wf!(f32, fs, sine!(f1, amplitude, 0.0_f32));
        let signal_len_s = 30;
        let signal_len = signal_len_s * fs as usize;

        let input_signal_vec: Vec<f32> = input_signal.iter().take(signal_len).collect();

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        let (out_t, out_sig) = run_pipeline(t0, fs_rate, &mut pipeline, input_signal_vec);

        assert_eq!(out_sig.len(), out_t.len());
        assert!(out_t.len() >= signal_len / 100 - CONVERGENCE_TIME_SEC as usize); // downsample 100Hz -> 1Hz
        let t_expected = t0 + CONVERGENCE_TIME_SEC as i64 * 1_000_000;
        assert!((out_t[0] - t_expected).abs() < 1_000_000); // first output sample withing 1000ms (1/1000ms = 1Hz)
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000)); // timestamp deltas all exactly 1/1Hz = 1_000_000 us

        // Amplitude is reduced below -50dB after converging
        let tolerance = 0.003;
        let max = out_sig[1..].iter().fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[1..].iter().fold(f32::NAN, |a, b| f32::min(a, *b));
        log::debug!("max: {}, min: {}", max, min);

        assert!(amplitude * tolerance > max);
        assert!(amplitude * tolerance > -min);
    }

    /// 50 Hz be rejected by anti-alias filter
    #[test]
    fn test_simple_sine_reject_50() {
        let _ = simple_logger::init();
        let fs_rate = SampleRate::FS100;
        let fs = fs_rate.value();
        let t0 = 1_000_000_000;

        let f1: f32 = 50.0;
        let amplitude = 53.2;
        let input_signal = wf!(f32, fs, sine!(f1, amplitude, 0.0_f32));
        let signal_len_s = 30;
        let signal_len = signal_len_s * fs as usize;

        let input_signal_vec: Vec<f32> = input_signal.iter().take(signal_len).collect();

        let mut pipeline = Pipeline::new(fs_rate, OUTPUT_FS);
        let (out_t, out_sig) = run_pipeline(t0, fs_rate, &mut pipeline, input_signal_vec);

        assert_eq!(out_sig.len(), out_t.len());
        assert!(out_t.len() >= signal_len / 100 - CONVERGENCE_TIME_SEC as usize); // downsample 100Hz -> 1Hz
        let t_expected = t0 + CONVERGENCE_TIME_SEC as i64 * 1_000_000;
        assert!((out_t[0] - t_expected).abs() < 1_000_000); // first output sample withing 1000ms (1/1000ms = 1Hz)
        assert!(out_t.windows(2).all(|w| w[1] - w[0] == 1_000_000)); // timestamp deltas all exactly 1/1Hz = 1_000_000 us

        // Amplitude is reduced below -88dB after converging
        let tolerance = 0.00004;
        let max = out_sig[1..].iter().fold(f32::NAN, |a, b| f32::max(a, *b));
        let min = out_sig[1..].iter().fold(f32::NAN, |a, b| f32::min(a, *b));
        log::debug!("max: {}, min: {}", max, min);

        assert!(amplitude * tolerance > max);
        assert!(amplitude * tolerance > -min);
    }
}
