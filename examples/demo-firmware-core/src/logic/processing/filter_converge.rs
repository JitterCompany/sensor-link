#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConvergeState {
    /// No data received yet
    NoData,

    /// Filters are converging: don't send output data yet
    Converging(u32),

    /// Filter has just converged: a good moment to reset/start processing the output
    /// Note: only samples *after* reaching this state should be passed through
    Converged,

    /// Filters are still converged: samples can be passed through processing
    Active,
}

pub struct ConvergeCounter {
    /// Minimum samples to reach convergence (filters settled)
    threshold: u32,
    state: ConvergeState,
}
impl ConvergeCounter {
    /// Create a new ConvergeCounter
    ///
    /// If `threshold` is 0, the counter will always return `Active`.
    pub fn new(threshold: u32) -> Self {
        let state = if threshold == 0 {
            ConvergeState::Active
        } else {
            ConvergeState::NoData
        };
        ConvergeCounter { threshold, state }
    }

    /// Increment sample count and returns the convergence state
    pub fn inc_sample_count(&mut self, n_samples: u32) -> ConvergeState {
        self.state = match self.state {
            ConvergeState::NoData => {
                if n_samples >= self.threshold {
                    ConvergeState::Converged
                } else {
                    ConvergeState::Converging(n_samples)
                }
            }
            ConvergeState::Converging(n) => {
                let n = n.saturating_add(n_samples);
                if n >= self.threshold {
                    ConvergeState::Converged
                } else {
                    ConvergeState::Converging(n)
                }
            }
            ConvergeState::Converged | ConvergeState::Active => ConvergeState::Active,
        };

        self.state
    }

    /// Reset the convergence process, for example after a configuration change or re-start with discontinuous data
    pub fn reset(&mut self) {
        self.state = ConvergeState::NoData;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_nodata_to_converging_to_converged_to_active() {
        let mut c = ConvergeCounter::new(5);
        assert_eq!(c.inc_sample_count(1), ConvergeState::Converging(1));
        assert_eq!(c.inc_sample_count(3), ConvergeState::Converging(4));
        assert_eq!(c.inc_sample_count(1), ConvergeState::Converged);
        assert_eq!(c.inc_sample_count(1), ConvergeState::Active);
        assert_eq!(c.inc_sample_count(10), ConvergeState::Active);
    }

    #[test]
    fn transitions_nodata_to_converged_instantly() {
        let mut c = ConvergeCounter::new(5);
        // +10 samples: the threshold is 5, so we should skip the Converging state
        assert_eq!(c.inc_sample_count(10), ConvergeState::Converged);
        assert_eq!(c.inc_sample_count(1), ConvergeState::Active);
    }

    #[test]
    fn transitions_nodata_to_converging_to_converged_to_active_large_increments() {
        let mut c = ConvergeCounter::new(12);
        assert_eq!(c.inc_sample_count(10), ConvergeState::Converging(10));
        assert_eq!(c.inc_sample_count(10), ConvergeState::Converged);
        assert_eq!(c.inc_sample_count(10), ConvergeState::Active);
        assert_eq!(c.inc_sample_count(10), ConvergeState::Active);
    }

    #[test]
    fn only_reset_returns_to_nodata() {
        let mut c = ConvergeCounter::new(3);
        assert_eq!(c.inc_sample_count(1), ConvergeState::Converging(1));
        assert_eq!(c.inc_sample_count(1), ConvergeState::Converging(2));
        assert_eq!(c.inc_sample_count(1), ConvergeState::Converged);
        assert_eq!(c.inc_sample_count(1), ConvergeState::Active);
        assert_eq!(c.inc_sample_count(100), ConvergeState::Active);
        c.reset();
        assert_eq!(c.inc_sample_count(1), ConvergeState::Converging(1));
    }

    #[test]
    fn saturating_add_still_converges_at_max() {
        let mut c = ConvergeCounter::new(u32::MAX);
        assert_eq!(
            c.inc_sample_count(u32::MAX - 1),
            ConvergeState::Converging(u32::MAX - 1)
        );
        assert_eq!(c.inc_sample_count(u32::MAX), ConvergeState::Converged);
        assert_eq!(c.inc_sample_count(1), ConvergeState::Active);
    }
}
