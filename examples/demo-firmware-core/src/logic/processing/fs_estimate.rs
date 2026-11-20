use crate::monotonic_time;
#[allow(unused)]
use crate::utils::float::EmbeddedF32;

pub enum FSIntegrity {
    Converging,
    Ok,
    Outofbounds(f32),
}
pub struct FSEstimate<INST: monotonic_time::traits::Instant> {
    start_time: Option<INST>,
    total_samples: usize,
    target_fs: f32,
    max_error: f32,
}

impl<INST: monotonic_time::traits::Instant> FSEstimate<INST> {
    /// Returns a new FSEstimate instance.
    ///
    /// # Arguments
    ///
    /// * `target_fs` -  The target frequency in Hz
    /// * `max_error` - The maximum allowed error in Hz
    pub fn new(target_fs: f32, max_error: f32) -> Self {
        Self {
            start_time: None,
            total_samples: 0,
            target_fs,
            max_error,
        }
    }

    pub fn reset(&mut self) {
        self.start_time = None;
        self.total_samples = 0;
    }

    /// Check if the target frequency is met.
    ///
    /// This should be called every time new samples arrive.
    /// Gives a result every 60 seconds.
    ///
    /// # Returns
    ///
    /// - `None` if there is not enough data yet to calculate the estimate.
    /// - `Some(true)` if the target frequency is met.
    /// - `Some(false)` if the calculated frequency is too far off.
    pub fn check(&mut self, num_samples: usize, time_us: INST) -> FSIntegrity {
        match &self.start_time {
            None => {
                self.start_time = Some(time_us);
            }
            Some(start_time) => {
                self.total_samples += num_samples;

                if self.total_samples >= (60.0 * self.target_fs) as usize {
                    let elapsed_s = time_us.micros_since(start_time) as f32 / 1_000_000.0;
                    let fs_estimate = self.total_samples as f32 / elapsed_s;
                    self.reset();
                    if (fs_estimate - self.target_fs).abs() < self.max_error {
                        return FSIntegrity::Ok;
                    } else {
                        return FSIntegrity::Outofbounds(fs_estimate);
                    }
                }
            }
        }
        FSIntegrity::Converging
    }
}

#[cfg(test)]
mod test {

    use crate::monotonic_time::traits::Instant;
    use sensor_link_firmware::tests::mock::mock_timeout::MockInstant;

    use super::*;

    #[test]
    // Simulate exact timing and expect correct estimates for fs=1000
    fn test_estimate_1000hz_ok() {
        // Repeat test with some inconvenient batch sizes
        for batch_size in [10, 20, 25, 33, 89, 100] {
            test_estimate_ok(1000.0, batch_size);
        }
    }
    #[test]
    // Simulate exact timing and expect correct estimates for odd fs=1234.5
    fn test_estimate_1234_5hz_ok() {
        for batch_size in [10, 20, 25, 33, 89, 100] {
            test_estimate_ok(1234.5, batch_size);
        }
    }

    fn test_estimate_ok(fs: f32, sample_batch_size: usize) {
        let max_err = 1.0;

        let mut fse = FSEstimate::new(fs, max_err);

        const TEST_DURATION_US: i64 = 10_000 * 1_000_000;
        let iterations: usize =
            (fs * TEST_DURATION_US as f32 / sample_batch_size as f32 / 1e6) as usize;

        let t0 = MockInstant::now();

        for i in 0..iterations {
            let sample = i * sample_batch_size;
            let t_us = (sample as f32 * 1e6 / fs) as u64;

            match fse.check(sample_batch_size, t0.add_micros(t_us)) {
                // Should eventually converge in a reasonable amount of time
                FSIntegrity::Converging => {
                    assert!(i < iterations / 2)

                    // Should eventually (but not immediately) know integrity is OK
                }
                FSIntegrity::Ok => {
                    assert!(i != 0);
                    break;
                }

                // Should not trigger out-of-bounds as we give exactly the expected amount of samples
                FSIntegrity::Outofbounds(estimate) => panic!(
                    "i={i}, fs={fs}, batch={sample_batch_size}, t={:.6}: estimate={estimate}",
                    t_us as f32 / 1e6
                ),
            }
        }
    }
}
