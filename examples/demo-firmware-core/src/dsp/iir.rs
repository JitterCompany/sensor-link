//! IIR Filter module for no-std environments.

use biquad::{Coefficients, DirectForm2Transposed, *};
use heapless::Vec;

pub struct Filter<const N: usize> {
    stages: Vec<DirectForm2Transposed<f32>, N>,
}

impl<const N: usize> Filter<N> {
    pub fn from_coefficents(coefficients: &[Coefficients<f32>]) -> Self {
        let stages = coefficients
            .iter()
            .map(|coeffs| DirectForm2Transposed::<f32>::new(*coeffs))
            .collect();

        Self { stages }
    }

    #[cfg(test)]
    /// Creates an N stages all pass filter.
    pub fn all_pass() -> Self {
        let all_pass_coeffs = Coefficients::<f32> {
            a1: 0.,
            a2: 0.,
            b0: 1.,
            b1: 0.,
            b2: 0.,
        };
        // Create vector of N and fill all of them with all_pass_coeffs
        let mut stages = Vec::<DirectForm2Transposed<f32>, N>::new();
        for _ in 0..N {
            stages
                .push(DirectForm2Transposed::<f32>::new(all_pass_coeffs))
                .unwrap();
        }
        Self { stages }
    }

    /// Runs signal through filter but only returns final filtered value.
    /// This does not change the input signal
    pub fn run_get_last(&mut self, signal: &[f32]) -> Option<f32> {
        if signal.is_empty() {
            return None;
        }
        let mut res: f32 = 0.0;
        for sample in signal {
            for stage in self.stages.iter_mut() {
                res = stage.run(*sample);
            }
        }
        Some(res)
    }

    /// Applies the filter to the signal _in place_.
    pub fn apply(&mut self, signal: &mut [f32]) {
        for sample in signal {
            for stage in self.stages.iter_mut() {
                *sample = stage.run(*sample);
            }
        }
    }

    pub fn run(&mut self, mut input_vec: Vec<f32, N>) -> Vec<f32, N> {
        let mut output_vec: Vec<f32, N> = Vec::<f32, N>::new();
        for i in 0..self.stages.len() {
            output_vec.clear();
            for elem in input_vec {
                let output_elem = self.stages[i].run(elem);
                output_vec.push(output_elem).ok();
            }
            input_vec = output_vec.clone();
        }
        output_vec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavegen::{dc_bias, sine, wf};

    /*
    Test_1: DC signal should not pass through high pass filter
    input_signal: DC signal
    output_signal: no signal
    Max aplittude to test: 100
    */
    #[test]
    fn filtering_dc_using_biquad_iir_with_own_filter_coefficents() {
        let input_signal = wf!(f32, 200.0_f32, dc_bias!(100.0_f32));
        let expected_output_signal = wf!(f32, 200.0_f32, dc_bias!(0.0_f32));

        const COEFFS_INT: Coefficients<f32> = Coefficients {
            a1: -0.998_586_8,
            a2: 0.0,
            b0: 0.000_499_646_76,
            b1: 0.000_499_646_76,
            b2: 0.0,
        };

        let mut filter = Filter::from_coefficents(&[COEFFS_INT]);

        let input_signal_vec = input_signal.iter().take(10).collect::<Vec<_, 10>>();
        let expected_output_signal_vec = expected_output_signal
            .iter()
            .take(10)
            .collect::<Vec<_, 10>>();

        let output_vec = filter.run(input_signal_vec);

        let output_signal_vec_max = output_vec.into_iter().reduce(f32::max).unwrap().floor();
        let expected_output_signal_vec_max = expected_output_signal_vec
            .into_iter()
            .reduce(f32::max)
            .unwrap()
            .floor();
        assert_eq!(expected_output_signal_vec_max, output_signal_vec_max);
    }

    /*
     Test_2 High Pass filter doesn't let frequency below fs pass
    Input: two sine wave and a DC
    Output: SIne wave with higher fequency and higher aplitude
    */
    #[test]
    fn filtering_certain_sin_waves_using_highpass_biquad_iir() {
        let input_signal = wf!(
            f32,
            200.0_f32,
            sine!(50.0_f32, 10.0_f32, 0.0_f32),
            sine!(5.0_f32, 5.0_f32, 0.0_f32),
            dc_bias!(7.0_f32)
        );
        let expected_output_signal = wf!(f32, 200.0_f32, sine!(50.0_f32, 9.0_f32, 0.0_f32)); // (+/- 10% amplitude)

        let fs = 200.hz();
        let f0 = 20.hz();

        let coeffs2 =
            Coefficients::<f32>::from_params(Type::HighPass, fs, f0, Q_BUTTERWORTH_F32).unwrap();
        let mut filter = Filter::from_coefficents(&[coeffs2]);
        let input_signal_vec = input_signal.iter().take(200).collect::<Vec<_, 200>>();

        let expected_output_signal_vec = expected_output_signal
            .iter()
            .take(200)
            .collect::<Vec<_, 200>>();

        let mut output_vec = filter.run(input_signal_vec);
        output_vec = Vec::from_slice(&output_vec.as_slice()[output_vec.len() - 40..]).unwrap();

        let output_signal_vec_max = output_vec.into_iter().reduce(f32::max).unwrap().floor();
        let expected_output_signal_vec_max = expected_output_signal_vec
            .into_iter()
            .reduce(f32::max)
            .unwrap()
            .floor();
        assert_eq!(expected_output_signal_vec_max, output_signal_vec_max);
    }
}
