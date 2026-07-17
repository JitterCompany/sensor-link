use core::i32;

use serde::{Deserialize, Serialize};

/// Network time syncronization lower limit (microsecond): updates at a faster interval are ignored
pub const NETWORK_TIME_UPDATE_INTERVAL_MIN_US: i64 = 60_000_000;

/// Network time syncronization upper limit derives from this accuracy threshold.
/// Once this accuracy is met, a new drift measurement is started
pub const NETWORK_UNCERTAINTY_THRESHOLD_MIN_PPB: u32 = 1_000;

/// Initial estimate is always assumed to have this minimum uncertainty
pub const ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB: u32 = 100;

/// Maximum drift correction error (ppb): adjustments with larger error are clamped or ignored
pub const CORRECTION_ERROR_MAX_PPB: i32 = 500_000_000;

const _: () = assert!(CORRECTION_ERROR_MAX_PPB > 0);
pub const CORRECTION_ERROR_MAX_PPB_U32: u32 = CORRECTION_ERROR_MAX_PPB as u32;

#[derive(Clone)]
pub struct TempErrorEstimate {
    pub error_ppb: i32,
    pub uncertainty_ppb: u32,
}

impl TempErrorEstimate {
    pub fn zero() -> Self {
        Self {
            error_ppb: 0,
            uncertainty_ppb: 0,
        }
    }
}

/// Drift-only error estimate (normalized with respect to temperature)
#[derive(Clone)]
pub struct DriftError(pub ErrorEstimate);

/// Total drift error estimate (including temperature error)
#[derive(Clone, Debug)]
pub struct TotalDriftError {
    pub error_ppb: i32,
    pub uncertainty_ppb: u32,
}

#[derive(Clone, Debug)]
pub struct ErrorEstimate {
    pub error_ppb: i32,
    pub uncertainty_ppb: u32,
}

impl ErrorEstimate {
    /// Perfectly certain zero error
    pub fn zero() -> Self {
        Self {
            error_ppb: 0,
            uncertainty_ppb: 0,
        }
    }

    /// Unknown estimate: maximum uncertainty around zero
    pub fn unknown() -> Self {
        Self {
            error_ppb: 0,
            uncertainty_ppb: CORRECTION_ERROR_MAX_PPB_U32,
        }
    }
}

impl DriftError {
    /// Average two estimates, exponentially weighed based on uncertainty
    pub fn weighed_average(&self, other: DriftError) -> DriftError {
        // exponential averaging of uncertainties
        let frac = {
            let self_sq = self.0.uncertainty_ppb as f32 * self.0.uncertainty_ppb as f32;
            let other_sq = other.0.uncertainty_ppb as f32 * other.0.uncertainty_ppb as f32;
            other_sq / (self_sq + other_sq)
        };

        let other_frac = 1.0 - frac;

        let uncertainty_ppb = ((frac * self.0.uncertainty_ppb as f32)
            + (other_frac * other.0.uncertainty_ppb as f32)) as u32;

        let error_ppb =
            ((frac * self.0.error_ppb as f32) + (other_frac * other.0.error_ppb as f32)) as i32;
        Self(ErrorEstimate {
            error_ppb: error_ppb.clamp(-CORRECTION_ERROR_MAX_PPB, CORRECTION_ERROR_MAX_PPB),
            uncertainty_ppb: uncertainty_ppb.min(CORRECTION_ERROR_MAX_PPB_U32),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct RawMono(i64);

#[derive(Clone, Copy, PartialEq, Debug)]
struct TempCorrectedMono(i64);

#[derive(Clone, Copy, PartialEq, Debug)]
struct DriftCorrectedMono(i64);

// TODO implement support for offset adjustment. See issue #427.
pub struct ClockErrorEstimate {
    drift: ClockDriftEstimate,
}

/// Clock calibration data: may be persisted across reboots
/// to quickly settle the clock error estimate
#[derive(Clone, Serialize, Deserialize)]
pub struct ClockCalibration {
    drift_error_ppb: i32,
    drift_uncertainty_ppb: u32,
}

impl ClockCalibration {
    /// Check if two ClockCalibration instances are significantly (> [ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB] different)
    pub fn is_significantly_different(&self, other: Self) -> bool {
        self.drift_uncertainty_ppb
            .abs_diff(other.drift_uncertainty_ppb)
            > ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB
            || self
                .drift_error_ppb
                .saturating_sub(other.drift_error_ppb)
                .abs() as u32
                > ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB
    }
}

impl From<ClockCalibration> for DriftError {
    fn from(cal: ClockCalibration) -> Self {
        DriftError(ErrorEstimate {
            error_ppb: cal.drift_error_ppb,
            uncertainty_ppb: cal.drift_uncertainty_ppb,
        })
    }
}

impl From<DriftError> for ClockCalibration {
    fn from(drift: DriftError) -> Self {
        ClockCalibration {
            drift_error_ppb: drift.0.error_ppb,
            drift_uncertainty_ppb: drift.0.uncertainty_ppb,
        }
    }
}

impl ClockErrorEstimate {
    /// Create a ClockErrorEstimate from ClockCalibration or DriftError
    pub fn new(initial_calibration: impl Into<ClockCalibration>) -> Self {
        let cal: ClockCalibration = initial_calibration.into();
        Self {
            drift: ClockDriftEstimate::new(cal.into()),
        }
    }

    pub fn calibration(&self) -> ClockCalibration {
        self.drift.temp_normalized_estimate().into()
    }

    /// Update clock drift estimate based on latest temperature correction
    ///
    ///
    /// Note: for accurate results, the resulting estimate must be applied ASAP to adjust the clock.
    /// Any latency before applying will result in a small error depending on how much the estimate changes.
    ///
    #[must_use]
    pub fn update_temp_correction(
        &mut self,
        timestamp: Timestamp,
        temp_adjust: TempErrorEstimate,
    ) -> TotalDriftError {
        let (t_raw, t_temp) = self.convert_to_temp_corrected(timestamp);
        self.drift
            .update_temp_correction(t_raw, t_temp, temp_adjust)
    }

    /// Update clock drift estimate based on comparison with a reference clock
    ///
    /// Returns an error if the update did not result in a new drift estimate.
    ///
    /// Note: if the update succeeds, the caller should update its clock with
    /// the result from [Self::drift_estimate()] ASAP.
    pub fn update_network_time(
        &mut self,
        timestamp: Timestamp,
        network_time: NetworkTime,
    ) -> Result<(), NetworkEstError> {
        // get temp-normalized and drift-corrected time
        let (t_temp, t_drift) = self.convert_to_drift_corrected(timestamp);

        self.drift
            .update_network_time(t_temp, t_drift, network_time)
    }

    /// Estimate the total drift error including temperature effects
    pub fn drift_estimate(&self) -> TotalDriftError {
        self.drift.estimate()
    }

    /// Convert a timestamp to raw + temp-corrected monotonic units for internal calculations
    fn convert_to_temp_corrected(&mut self, timestamp: Timestamp) -> (RawMono, TempCorrectedMono) {
        match timestamp {
            // Apply temperature correction
            Timestamp::RawMono(raw) => {
                let raw = RawMono(raw);
                (raw, self.drift.convert_raw_to_temp_corrected(raw))
            }

            // TempCorrected is already what we want
            Timestamp::TempCorrectedMono(temp_corr) => {
                let temp_corr = TempCorrectedMono(temp_corr);
                (
                    self.drift.convert_temp_corrected_to_raw(temp_corr),
                    temp_corr,
                )
            }

            // Reverse drift correction to get the TempCorrectedMono timestamp
            Timestamp::DriftCorrectedMono(drift_corr) => {
                let drift_corr = DriftCorrectedMono(drift_corr);
                let temp_corr = self
                    .drift
                    .convert_drift_corrected_to_temp_corrected(drift_corr);
                (
                    self.drift.convert_temp_corrected_to_raw(temp_corr),
                    temp_corr,
                )
            }

            // Reverse offset AND drift correction to get the TempCorrectedMono timestamp
            Timestamp::OffsetAdjusted(time_offset_adj) => {
                // TODO implement #427
                // for now assume offsetadjusted==driftcorrected
                let drift_corr = DriftCorrectedMono(time_offset_adj);

                let temp_corr = self
                    .drift
                    .convert_drift_corrected_to_temp_corrected(drift_corr);
                (
                    self.drift.convert_temp_corrected_to_raw(temp_corr),
                    temp_corr,
                )
            }
        }
    }

    fn convert_to_drift_corrected(
        &mut self,
        timestamp: Timestamp,
    ) -> (TempCorrectedMono, DriftCorrectedMono) {
        match timestamp {
            // Apply temperature AND drift correction
            Timestamp::RawMono(raw) => {
                let raw = RawMono(raw);
                let temp_corr = self.drift.convert_raw_to_temp_corrected(raw);
                (
                    temp_corr,
                    self.drift
                        .convert_temp_corrected_to_drift_corrected(temp_corr),
                )
            }

            // Apply drift correction
            Timestamp::TempCorrectedMono(temp_corr) => {
                let temp_corr = TempCorrectedMono(temp_corr);
                (
                    temp_corr,
                    self.drift
                        .convert_temp_corrected_to_drift_corrected(temp_corr),
                )
            }

            // Reverse drift correction to get the TempCorrectedMono timestamp
            Timestamp::DriftCorrectedMono(drift_corr) => {
                let drift_corr = DriftCorrectedMono(drift_corr);
                let temp_corr = self
                    .drift
                    .convert_drift_corrected_to_temp_corrected(drift_corr);
                (temp_corr, drift_corr)
            }

            // Reverse offset AND drift correction to get the TempCorrectedMono timestamp
            Timestamp::OffsetAdjusted(time_offset_adj) => {
                // TODO implement #427
                // for now assume offsetadjusted==driftcorrected
                let drift_corr = DriftCorrectedMono(time_offset_adj);

                let temp_corr = self
                    .drift
                    .convert_drift_corrected_to_temp_corrected(drift_corr);
                (temp_corr, drift_corr)
            }
        }
    }
}

/// reference state for network-based drift estimation
struct NetworkRef {
    /// Network time this estimate is based on
    t_network: NetworkTime,

    /// Local time when this NetworkEstimate was made
    t_local: TempCorrectedMono,

    /// Drift error estimate `estimate_pre_temp` as it was at `t_local`
    initial_estimate: DriftError,

    /// Maximum uncertainty due to temperature since `t_local`.
    max_temp_uncertainty_ppb: u32,
}

struct ClockDriftEstimate {
    /// Drift estimate (excluding temperature effects)
    estimate_pre_temp: DriftError,

    /// Timestamps when pre_temp was last updated (or first accessed)
    estimate_at: Option<(TempCorrectedMono, DriftCorrectedMono)>,

    /// Last known temperature correction with timestamps
    temp: Option<(RawMono, TempCorrectedMono, TempErrorEstimate)>,

    /// last known server time used for drift estimation
    network: Option<NetworkRef>,
}

#[derive(Debug, PartialEq)]
pub enum NetworkEstError {
    /// No previous network update to use as reference
    NoPreviousData,

    /// Previous network update was too recent for acurate drift estimate
    PreviousDataTooRecent,

    /// Previous network update too recent relative to its latency
    UncertaintyTooLarge,

    /// Estimate could be generated but would not improve the current drift estimate
    UncertaintyNotImprovedYet,

    /// Unreasonably large drift error between local and network time. Estimator will reset
    ErrorTooLarge,
}

impl ClockDriftEstimate {
    pub fn new(mut initial_drift_estimate: DriftError) -> Self {
        // start with some initial uncertainty (if there is truly no uncertianty, why do drift estimates?)
        if initial_drift_estimate.0.uncertainty_ppb < ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB {
            initial_drift_estimate.0.uncertainty_ppb = ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB;
        }
        log::info!(target: "core::time_adjust", "initial: {:?}", initial_drift_estimate.0);
        Self {
            estimate_pre_temp: initial_drift_estimate,
            estimate_at: None,
            temp: None,
            network: None,
        }
    }

    /// Update clock drift estimate based on latest temperature correction
    ///
    ///
    /// Note: for accurate results, the resulting estimate must be applied ASAP to adjust the clock.
    /// Any latency before applying will result in a small error depending on how much the estimate changes.
    ///
    #[must_use]
    pub fn update_temp_correction(
        &mut self,
        t_raw: RawMono,
        t_temp: TempCorrectedMono,
        temp_adjust: TempErrorEstimate,
    ) -> TotalDriftError {
        // Keep track of worst-case uncertainty during the network interval due to:
        // - uncertainty of the temp_adjust itself
        // - uncertainty due to difference with previous temp adjust (minimize by updating often)
        if let Some(network) = &mut self.network {
            if t_temp.0 > network.t_local.0 {
                let uncertainty = self
                    .temp
                    .as_ref()
                    .map(|(_, _, prev)| {
                        let prev_min = prev.error_ppb - prev.uncertainty_ppb as i32;
                        let prev_max = prev.error_ppb + prev.uncertainty_ppb as i32;
                        let new_min = temp_adjust.error_ppb - temp_adjust.uncertainty_ppb as i32;
                        let new_max = temp_adjust.error_ppb + temp_adjust.uncertainty_ppb as i32;

                        let err_min = prev_min.min(new_min);
                        let err_max = prev_max.max(new_max);
                        let worst_case_uncertainty = (err_max - err_min).abs() as u32;

                        worst_case_uncertainty
                    })
                    .unwrap_or(temp_adjust.uncertainty_ppb);
                if uncertainty > network.max_temp_uncertainty_ppb {
                    network.max_temp_uncertainty_ppb = uncertainty;
                }
            }
        }

        self.temp = Some((t_raw, t_temp, temp_adjust));

        self.estimate()
    }

    /// Update clock drift estimate based on comparison with a reference clock
    ///
    /// Returns an error if the update did not result in a new drift estimate.
    ///
    /// Note: if the update succeeds, the caller should update its clock with
    /// the result from [Self::estimate()] ASAP.
    pub fn update_network_time(
        &mut self,
        t_temp: TempCorrectedMono,
        t_drift: DriftCorrectedMono,
        network_time: NetworkTime,
    ) -> Result<(), NetworkEstError> {
        let Some(prev_network) = &self.network else {
            self.network = Some(NetworkRef {
                t_network: network_time,
                t_local: t_temp,
                initial_estimate: self.estimate_pre_temp.clone(),
                max_temp_uncertainty_ppb: self
                    .temp
                    .as_ref()
                    .map(|(_, _, est)| est.uncertainty_ppb)
                    .unwrap_or(0),
            });
            return Err(NetworkEstError::NoPreviousData);
        };
        let prev_time: &TempCorrectedMono = &prev_network.t_local;

        let local_dt = t_temp.0.saturating_sub(prev_time.0);
        let network_dt = network_time
            .timestamp_server_us
            .saturating_sub(prev_network.t_network.timestamp_server_us);

        // Worst-case: one of the latencies was actually zero (negative is impossible!), the other was the maximum estimated value.
        // This implies the maximum latency is the longest of the two (not the sum).
        let max_latency_error: u32 = network_time
            .latency_estimate_us
            .max(prev_network.t_network.latency_estimate_us);

        // Only continue if dt is positive. Shorter than 1 minute has very little chance to improve the clock
        let network_dt_minimum = network_dt.saturating_sub(i64::from(max_latency_error));
        if network_dt_minimum.min(local_dt) <= NETWORK_TIME_UPDATE_INTERVAL_MIN_US {
            return Err(NetworkEstError::PreviousDataTooRecent);
        }

        // calculate absolute uncertainty (>=0 per definition)
        let uncertainty_ppb = (i64::from(max_latency_error) * 1_000_000_000) / network_dt_minimum;
        if uncertainty_ppb >= i64::from(CORRECTION_ERROR_MAX_PPB) {
            return Err(NetworkEstError::UncertaintyTooLarge);
        }
        let uncertainty_ppb = uncertainty_ppb as u32;

        let drift = local_dt - network_dt;
        let error_ppb = (drift.saturating_mul(1_000_000_000)) / network_dt;

        // Network time drift is extreme (i32::MAX micros is > half an hour!).
        // Reset state so we can recover later, assuming there was a discontinuity in time
        if drift >= i64::from(i32::MAX) || error_ppb >= i64::from(CORRECTION_ERROR_MAX_PPB) {
            self.network = Some(NetworkRef {
                t_network: network_time,
                t_local: t_temp,
                initial_estimate: self.estimate_pre_temp.clone(),
                max_temp_uncertainty_ppb: prev_network.max_temp_uncertainty_ppb,
            });
            return Err(NetworkEstError::ErrorTooLarge);
        }

        let network_estimate = DriftError(ErrorEstimate {
            error_ppb: error_ppb as i32,
            uncertainty_ppb: uncertainty_ppb + prev_network.max_temp_uncertainty_ppb,
        });
        self.estimate_pre_temp = prev_network
            .initial_estimate
            .weighed_average(network_estimate);
        self.estimate_at = Some((t_temp, t_drift));

        // network-related part of uncertainty below threshold: reset to start a new network interval:
        // - next measurement may be in a more temperature-stable period which can increase the drift estimate
        // - NETWORK_UNCERTAINTY_THRESHOLD_MIN_PPB forces eventual restarts to correct for slow changes in drift
        if uncertainty_ppb
            < prev_network
                .max_temp_uncertainty_ppb
                .min(NETWORK_UNCERTAINTY_THRESHOLD_MIN_PPB)
        {
            self.network = Some(NetworkRef {
                t_network: network_time,
                t_local: t_temp,
                initial_estimate: self.estimate_pre_temp.clone(),
                max_temp_uncertainty_ppb: prev_network.max_temp_uncertainty_ppb,
            });
        }

        Ok(())
    }

    /// Estimate the total drift error including temperature effects.
    ///
    /// Use this estimate to correct the clock frequency
    /// For example: an estimated error of +10ppm means the clock runs
    /// 10ppm too fast and must run about 10ppm slower.
    pub fn estimate(&self) -> TotalDriftError {
        let temp_error = self
            .temp
            .as_ref()
            .map(|(_, _, estimate)| estimate.clone())
            .unwrap_or(TempErrorEstimate::zero());
        let drift_error = &self.estimate_pre_temp.0;

        TotalDriftError {
            error_ppb: (temp_error.error_ppb + drift_error.error_ppb)
                .clamp(-CORRECTION_ERROR_MAX_PPB, CORRECTION_ERROR_MAX_PPB),
            uncertainty_ppb: (temp_error.uncertainty_ppb + drift_error.uncertainty_ppb)
                .min(CORRECTION_ERROR_MAX_PPB_U32),
        }
    }

    /// Estimate, normalized with temperature
    ///
    /// Intended use: store this estimate for later re-use (e.g. after a reboot).
    /// For use in calibrating the local clock, see [Self::estimate()] instead.
    pub fn temp_normalized_estimate(&self) -> DriftError {
        self.estimate_pre_temp.clone()
    }

    /// correct with last known temperature correction
    /// this assumes the temperature was the same at the given timestamp.
    /// (for timestamps very far from the last correction this wont be accurate).
    fn convert_raw_to_temp_corrected(&self, raw: RawMono) -> TempCorrectedMono {
        let Some(time_correction) = &self.temp else {
            return TempCorrectedMono(raw.0);
        };

        let t_raw_offset: RawMono = time_correction.0;
        let slope_error_ppb = time_correction.2.error_ppb;

        let delta_raw = raw.0 - t_raw_offset.0;

        let delta_corr = reverse_slope_convert(delta_raw, -slope_error_ppb);

        let t_corr_offset: TempCorrectedMono = time_correction.1;
        TempCorrectedMono(t_corr_offset.0 + delta_corr)
    }

    fn convert_temp_corrected_to_raw(&self, temp_corr: TempCorrectedMono) -> RawMono {
        let Some(time_correction) = &self.temp else {
            return RawMono(temp_corr.0);
        };

        let t_corr_offset: TempCorrectedMono = time_correction.1;
        let slope_error_ppb = time_correction.2.error_ppb;

        let delta_corr = temp_corr.0 - t_corr_offset.0;
        let delta_raw = forward_slope_convert(delta_corr, slope_error_ppb);

        let t_raw_offset: RawMono = time_correction.0;
        RawMono(t_raw_offset.0 + delta_raw)
    }

    fn convert_temp_corrected_to_drift_corrected(
        &mut self,
        temp_corr: TempCorrectedMono,
    ) -> DriftCorrectedMono {
        let Some(corr_at) = &self.estimate_at else {
            let drift_corr = DriftCorrectedMono(temp_corr.0);
            self.estimate_at = Some((temp_corr, drift_corr));
            return drift_corr;
        };
        let temp_corr_offset: TempCorrectedMono = corr_at.0;
        let drift_corr_offset: DriftCorrectedMono = corr_at.1;

        let error_ppb = self.estimate_pre_temp.0.error_ppb;
        let delta_drift = reverse_slope_convert(temp_corr.0 - temp_corr_offset.0, -error_ppb);

        DriftCorrectedMono(drift_corr_offset.0 + delta_drift)
    }

    fn convert_drift_corrected_to_temp_corrected(
        &mut self,
        drift_corr: DriftCorrectedMono,
    ) -> TempCorrectedMono {
        let Some(corr_at) = &self.estimate_at else {
            let temp_corr = TempCorrectedMono(drift_corr.0);
            self.estimate_at = Some((temp_corr, drift_corr));
            return temp_corr;
        };
        let temp_corr_offset: TempCorrectedMono = corr_at.0;
        let drift_corr_offset: DriftCorrectedMono = corr_at.1;

        let error_ppb = self.estimate_pre_temp.0.error_ppb;
        let delta_temp = forward_slope_convert(drift_corr.0 - drift_corr_offset.0, error_ppb);
        TempCorrectedMono(temp_corr_offset.0 + delta_temp)
    }
}

/// Slope correct: correct dt by multiplying it by (1+correction_ppb)
///
/// For example: correction 1_000 ppb results in dt * (1.0+1.0e-6)
fn forward_slope_convert(dt: i64, correction_ppb: i32) -> i64 {
    dt + slope_correct(dt, correction_ppb)
}

fn slope_correct(dt: i64, correction_ppb: i32) -> i64 {
    // 'small' delta: this will usually be the case assuming temp correction is recent.
    // This gives an exact answer and cannot overflow (i32 * i32 -> i64)
    if dt.saturating_abs() < i64::from(i32::MAX) {
        (dt * i64::from(correction_ppb)) / 1_000_000_000

    // large delta: floating point approximation
    } else {
        (dt as f32 * (correction_ppb as f32 / 1_000_000_000.0)) as i64
    }
}

#[cfg(test)]
static REV_SLOPE_PERFCOUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Reverse slope correct: correct dt by dividing it by (1-correction_ppb)
///
/// For example: correction 1_000 ppb results in dt / (1.0-1.0e-6)
fn reverse_slope_convert(mut dt: i64, correction_ppb: i32) -> i64 {
    {
        // approximate 1/(1-error) by multiplying by (1+error).
        // For small errors this is correct, for larger values a few
        // iterations are needed to converge (typ 2-3 for < 100ppm)

        // Iteration limit in case extreme corrections are requested.
        // With 16 iterations, even 50% correction over 30 minutes is still within 8ppm
        const MAX_ITERATIONS: usize = 16;

        let mut delta_adj = dt;
        for _i in 0..MAX_ITERATIONS {
            #[cfg(test)]
            REV_SLOPE_PERFCOUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

            delta_adj = slope_correct(delta_adj, correction_ppb);
            dt += delta_adj;
            if delta_adj == 0 {
                break;
            }
        }
        dt
    }
}

#[cfg(test)]
mod test {

    use core::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn test_slope_convert_small_adjustment() {
        REV_SLOPE_PERFCOUNT.store(0, Ordering::Relaxed);

        // note: 1800_000_000 chosen because it is slightly below i32::MAX
        let reverse = reverse_slope_convert(1800_000_000, 100_000);
        let forward = forward_slope_convert(1800_000_000, 100_000);

        assert_eq!(forward, 1800_180_000); // 1800e6 * (1+100ppm)
        assert_eq!(reverse, 1800_180_018); // 1800e6 / (1-100ppm)

        assert!(REV_SLOPE_PERFCOUNT.load(Ordering::Relaxed) <= 3);
    }

    #[test]
    fn test_slope_convert_small_adjustment_negative() {
        REV_SLOPE_PERFCOUNT.store(0, Ordering::Relaxed);

        // note: 1800_000_000 chosen because it is slightly below i32::MAX
        let reverse = reverse_slope_convert(1800_000_000, -100_000);
        let forward = forward_slope_convert(1800_000_000, -100_000);

        assert_eq!(forward, 1799_820_000); // 1800e6 * (1-100ppm)
        assert_eq!(reverse, 1799_820_018); // 1800e6 / (1+100ppm)

        assert!(REV_SLOPE_PERFCOUNT.load(Ordering::Relaxed) <= 3);
    }

    #[test]
    fn test_slope_convert_extreme_adjustment() {
        REV_SLOPE_PERFCOUNT.store(0, Ordering::Relaxed);

        // adjust this test + assertion if CORRECTION_ERROR_MAX_PPB changes
        assert_eq!(CORRECTION_ERROR_MAX_PPB, 500_000_000);

        let reverse = reverse_slope_convert(1800_000_000, 500_000_000);
        let forward = forward_slope_convert(1800_000_000, 500_000_000);

        assert_eq!(forward, 2700_000_000); // 1800e6 * (1+0.5m)
        assert!((reverse - 3600_000_000).abs() < 28_800); // 36e9 / (1-0.5) with < 8ppm error due to MAX_ITERATIONS

        assert_eq!(REV_SLOPE_PERFCOUNT.load(Ordering::Relaxed), 16);
    }

    #[test]
    fn test_slope_convert_small_adjustment_long_time() {
        REV_SLOPE_PERFCOUNT.store(0, Ordering::Relaxed);

        // note: 86_400_000_000 chosen because it is >> i32::MAX
        let reverse = reverse_slope_convert(86_400_000_000, 100_000);
        let forward = forward_slope_convert(86_400_000_000, 100_000);

        assert_eq!(forward, 86_408_640_000); // 86.4e9 * (1+100ppm)
        assert_eq!(reverse, 86_408_640_864); // 86.4e9 / (1-100ppm)

        assert!(REV_SLOPE_PERFCOUNT.load(Ordering::Relaxed) <= 3);
    }

    /// smoke test: temp compensation error=0 should have no impact
    #[test]
    fn test_temp_compensation_zero() {
        let mut drift_est = ClockErrorEstimate::new(DriftError(ErrorEstimate {
            error_ppb: 120_000,
            uncertainty_ppb: 40_000,
        }));

        let raw_t0: i64 = 314_000_000_000;
        let raw_t1 = raw_t0 + 3600_000_000;
        let temp_adjust = super::TempErrorEstimate::zero();
        let err_est = drift_est.update_temp_correction(Timestamp::RawMono(raw_t0), temp_adjust);
        // total error = initial (+0 for temp error)
        assert_eq!(err_est.error_ppb, 120_000);
        assert_eq!(err_est.uncertainty_ppb, 40_000);

        assert_eq!(
            TempCorrectedMono(raw_t0),
            drift_est
                .drift
                .convert_raw_to_temp_corrected(RawMono(raw_t0))
        );
        assert_eq!(
            TempCorrectedMono(raw_t1),
            drift_est
                .drift
                .convert_raw_to_temp_corrected(RawMono(raw_t1))
        );
        assert_eq!(
            RawMono(raw_t0),
            drift_est
                .drift
                .convert_temp_corrected_to_raw(TempCorrectedMono(raw_t0))
        );
        assert_eq!(
            RawMono(raw_t1),
            drift_est
                .drift
                .convert_temp_corrected_to_raw(TempCorrectedMono(raw_t1))
        );
    }

    /// temp compensation only, with a recent compensation
    #[test]
    fn test_temp_compensation_recent() {
        let mut drift_est = ClockErrorEstimate::new(DriftError(ErrorEstimate {
            error_ppb: 120_000,
            uncertainty_ppb: 40_000,
        }));

        let raw_t0: i64 = 314_000_000_000;
        let raw_t1 = raw_t0 + 1_800_000_000;
        let temp_adjust = super::TempErrorEstimate {
            error_ppb: 100_000,
            uncertainty_ppb: 5_000,
        };
        let err_est = drift_est.update_temp_correction(Timestamp::RawMono(raw_t0), temp_adjust);
        // total error = initial + temp error
        assert_eq!(err_est.error_ppb, 220_000);
        assert_eq!(err_est.uncertainty_ppb, 45_000);

        assert_eq!(
            TempCorrectedMono(raw_t0),
            drift_est
                .drift
                .convert_raw_to_temp_corrected(RawMono(raw_t0))
        );

        // temp error = +100ppm -> clock correction by -100ppm = 1/(1+100ppm) = -179_982ms
        assert_eq!(
            TempCorrectedMono(raw_t1 - 179_982),
            drift_est
                .drift
                .convert_raw_to_temp_corrected(RawMono(raw_t1))
        );
        assert_eq!(
            RawMono(raw_t0),
            drift_est
                .drift
                .convert_temp_corrected_to_raw(TempCorrectedMono(raw_t0))
        );
        assert_eq!(
            RawMono(raw_t1),
            drift_est
                .drift
                .convert_temp_corrected_to_raw(TempCorrectedMono(raw_t1 - 179_982))
        );
    }

    /// temp compensation only, with a recent compensation
    #[test]
    fn test_temp_compensation_recent_negative_initial() {
        let mut drift_est = ClockErrorEstimate::new(DriftError(ErrorEstimate {
            error_ppb: -120_000,
            uncertainty_ppb: 40_000,
        }));

        let raw_t0: i64 = 314_000_000_000;
        let temp_adjust = super::TempErrorEstimate {
            error_ppb: 10_000,
            uncertainty_ppb: 5_000,
        };
        let err_est = drift_est.update_temp_correction(Timestamp::RawMono(raw_t0), temp_adjust);
        // total error = initial + temp error
        assert_eq!(err_est.error_ppb, -110_000);
        assert_eq!(err_est.uncertainty_ppb, 45_000);
    }

    #[test]
    fn test_min_uncertainty() {
        let drift_est = ClockErrorEstimate::new(DriftError(ErrorEstimate {
            error_ppb: 0,
            uncertainty_ppb: 0,
        }));
        assert_eq!(
            ESTIMATE_MIN_INITIAL_UNCERTAINTY_PPB,
            drift_est.drift_estimate().uncertainty_ppb
        );
    }

    /// network compensation only
    #[test]
    fn test_network_compensation_only() {
        let mut drift_est = ClockErrorEstimate::new(DriftError(ErrorEstimate::unknown()));

        let server_t0: i64 = 1_234_000_000_000;
        let local_t0: i64 = 314_000_000_000;
        let network_e0 = NetworkTime {
            timestamp_server_us: server_t0,
            latency_estimate_us: 100_000,
        };

        // started with maximum uncertainty
        assert_eq!(
            CORRECTION_ERROR_MAX_PPB_U32,
            drift_est.drift_estimate().uncertainty_ppb
        );

        // first update: no previous data can exist
        assert_eq!(
            NetworkEstError::NoPreviousData,
            drift_est
                .update_network_time(Timestamp::TempCorrectedMono(local_t0), network_e0.clone())
                .unwrap_err()
        );

        // almost immediately retry: too recent
        assert_eq!(
            NetworkEstError::PreviousDataTooRecent,
            drift_est
                .update_network_time(Timestamp::TempCorrectedMono(local_t0 + 100), network_e0)
                .unwrap_err()
        );

        // after half hour: estimate updated
        {
            let server_dt: i64 = 1_800_000_000;
            let local_dt: i64 = 1_800_146_221; //+81.234ppm
            let network_e1 = NetworkTime {
                timestamp_server_us: server_t0 + server_dt + 100_000,
                latency_estimate_us: 100_000,
            };

            drift_est
                .update_network_time(
                    Timestamp::TempCorrectedMono(local_t0 + local_dt),
                    network_e1,
                )
                .unwrap();

            // estimate should quickly converge and must not deviate further
            // than its own estimated uncertainty
            let est = drift_est.drift_estimate();
            assert!(est.uncertainty_ppb < 300_000);
            assert!((est.error_ppb - 81_234).abs() < est.uncertainty_ppb as i32);
        }

        // after 4 hours: estimate updated again
        {
            let server_dt: i64 = 14_400_000_000;
            let local_dt: i64 = 14_401_169_768; //+81.234ppm

            let network_e1 = NetworkTime {
                timestamp_server_us: server_t0 + server_dt,
                latency_estimate_us: 100_000,
            };

            drift_est
                .update_network_time(
                    Timestamp::TempCorrectedMono(local_t0 + local_dt),
                    network_e1,
                )
                .unwrap();

            // estimate should be fully settled by now (13ppm << 500_000 ppm initial error)
            let est = drift_est.drift_estimate();
            assert!(est.uncertainty_ppb < 13_900); // 200ms / 14400 seconds = 13.888 ppm

            // estimate should almost be perfect as the network timestamps in this test are noise-free
            assert!((est.error_ppb - 81_234).abs() < 10);
        }
    }

    /// network compensation only
    #[test]
    fn test_network_compensation_neg_with_temp() {
        let mut drift_est = ClockErrorEstimate::new(DriftError(ErrorEstimate::unknown()));

        let server_t0: i64 = 1_234_000_000_000;
        let local_t0: i64 = 314_000_000_000;
        let network_e0 = NetworkTime {
            timestamp_server_us: server_t0,
            latency_estimate_us: 100_000,
        };
        // very small error, should have almost no influence
        let _ = drift_est.update_temp_correction(
            Timestamp::TempCorrectedMono(local_t0),
            TempErrorEstimate {
                error_ppb: -400,
                uncertainty_ppb: 100,
            },
        );

        // started with maximum uncertainty
        assert_eq!(
            CORRECTION_ERROR_MAX_PPB_U32,
            drift_est.drift_estimate().uncertainty_ppb
        );

        // first update: no previous data can exist
        assert_eq!(
            NetworkEstError::NoPreviousData,
            drift_est
                .update_network_time(Timestamp::TempCorrectedMono(local_t0), network_e0.clone())
                .unwrap_err()
        );

        // almost immediately retry: too recent
        assert_eq!(
            NetworkEstError::PreviousDataTooRecent,
            drift_est
                .update_network_time(Timestamp::TempCorrectedMono(local_t0 + 100), network_e0)
                .unwrap_err()
        );

        // after 4 hours: estimate updated again
        {
            let server_dt: i64 = 14_400_000_000;
            let local_dt: i64 = 14_398_830_232; //-81.234ppm

            let network_e1 = NetworkTime {
                timestamp_server_us: server_t0 + server_dt,
                latency_estimate_us: 100_000,
            };

            drift_est
                .update_network_time(
                    Timestamp::TempCorrectedMono(local_t0 + local_dt),
                    network_e1,
                )
                .unwrap();

            // estimate should be fully settled by now (13ppm << 500_000 ppm initial error)
            let est = drift_est.drift_estimate();
            assert!(est.uncertainty_ppb < 13_900 + 2 * 100); // 200ms / 14400 seconds = 13.888 ppm, 2*100ppb temp error (both effect on current estimate and during past network drift comp)

            // estimate should be almost be perfectly equal to temp error (400ppb) as the network timestamps in this test are noise-free
            assert!((est.error_ppb - -81_234).abs() < 10 + 400);
        }
    }
}

/// Timestamp conversion helper: wraps a timer with metadata showing which corrections have been applied
#[derive(Debug)]
pub enum Timestamp {
    /// Raw monotonic time (e.g. crystal output) without any corrections
    RawMono(i64),

    /// Temperature-corrected monotonic time
    TempCorrectedMono(i64),

    /// Drift-corrected monotonic time (includes temp correction)
    DriftCorrectedMono(i64),

    /// Offset-adjusted wall-clock time (includes temp and drift correction)
    OffsetAdjusted(i64),
}

#[derive(Debug, Clone)]
pub struct NetworkTime {
    pub timestamp_server_us: i64,
    pub latency_estimate_us: u32,
}
