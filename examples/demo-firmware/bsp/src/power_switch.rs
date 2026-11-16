//! Power-switch driver: enable/disable a power rail with a power-good signal.
//!
//! Couples an `enable` output pin with a `power_good` triggered-input pin so that
//! `enable()` returns only after the rail has actually come up (or times out).
//! Implements [`PowerSwitch`], defined here.
//!
//! PG polarity: **active-high is assumed** (rail good → line high). Internal
//! pull behaviour is selectable via [`Config::pg_pulls`].

use embedded_hal::digital::{InputPin, OutputPin};
use sensor_link_firmware::{
    monotonic_time::{delay_ms, FutureTimeout, Timeout},
    traits::Trigger as TriggerTrait,
};

use crate::gpio::{Pullup, PullupConfig};

/// Controls a switchable power rail.
///
/// `enable()` is async: it may wait for a power-good signal and a settling delay,
/// and may time out if the rail never asserts.
/// `disable()` is sync so it remains usable from `Drop` impls.
///
/// Invariant: after `enable()` returns, the rail is either `Ok(on)` or `Err(off)` — never
/// a half-state. On any failure path the implementation de-asserts the rail before returning.
pub trait PowerSwitch {
    /// Enable the rail and wait until it is good-to-use.
    async fn enable(&mut self) -> Result<(), PowerSwitchError>;

    /// Disable the rail. Synchronous — safe to call from `Drop`.
    fn disable(&mut self);

    /// True if the rail is currently enabled (tracked in software).
    fn is_enabled(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PowerSwitchError {
    /// Power-good was not asserted within the implementation-defined timeout.
    Timeout,
}

/// Timing configuration for a [`GpioPowerSwitch`].
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Delay added after power-good asserts, before `enable()` returns `Ok`.
    /// Covers regulator-internal settling / downstream LDO stabilisation that
    /// happens after the PG pin has gone high.
    pub settling_delay_ms: u32,

    /// Maximum time to wait for PG to assert after `enable()` drives the
    /// enable pin high. If PG never asserts within this, `enable()` returns
    /// `Err(Timeout)` and the rail is powered off.
    pub timeout_ms: u32,

    /// Minimum off-time after `disable()` before the next `enable()` is
    /// allowed to re-assert. Covers rail-capacitor discharge; without this,
    /// an immediate on→off→on may skip the actual discharge.
    pub min_off_ms: u32,

    /// Internal pull behaviour for the PG pin.
    pub pg_pulls: PgPullStrategy,
}

/// PG-pin pull strategy. Selects whether `GpioPowerSwitch` applies internal
/// pulls on each rail-state transition.
#[derive(Debug, Clone, Copy)]
pub enum PgPullStrategy {
    /// Internal pullup while on, pulldown while off.
    Dynamic,
}

impl PgPullStrategy {
    fn off(self) -> PullupConfig {
        match self {
            Self::Dynamic => PullupConfig::Pulldown,
        }
    }
    fn on(self) -> PullupConfig {
        match self {
            Self::Dynamic => PullupConfig::Pullup,
        }
    }
}

/// Couples an enable pin with a power-good triggered-input pin.
///
/// `enable()` drives `enable` high, waits for `power_good` to read high (via edge
/// notifications), then adds `settling_delay_ms` before returning. If `power_good`
/// does not assert within `timeout_ms`, the rail is automatically torn back down
/// and `Err(Timeout)` is returned — the switch is guaranteed off after a failed
/// enable.
///
/// `disable()` drives `enable` low and arms a `min_off_ms` deadline so that a
/// subsequent `enable()` waits for the rail to actually discharge before
/// re-asserting.
pub struct GpioPowerSwitch<EN, PG> {
    enable: EN,
    power_good: PG,
    config: Config,
    enabled: bool,
    off_timeout: Timeout,
}

impl<EN, PG> GpioPowerSwitch<EN, PG>
where
    EN: OutputPin,
    PG: InputPin + TriggerTrait + Pullup,
{
    /// Create a new `GpioPowerSwitch`.
    ///
    /// The caller must have constructed `enable` as an output pin (initial state
    /// low) and `power_good` as an edge-triggered input. The returned switch
    /// drives `enable` low and sets the PG pullup to the off-state.
    pub fn new(mut enable: EN, mut power_good: PG, config: Config) -> Self {
        let _ = enable.set_low();
        power_good.set_pullup(config.pg_pulls.off());
        Self {
            enable,
            power_good,
            config,
            enabled: false,
            off_timeout: Timeout::new(),
        }
    }

    fn power_off(&mut self) {
        let _ = self.enable.set_low();
        self.power_good.set_pullup(self.config.pg_pulls.off());
        self.off_timeout.set_ms(self.config.min_off_ms);
        self.enabled = false;
    }

    async fn await_power_good(&mut self) -> Result<(), PowerSwitchError> {
        let pg = &mut self.power_good;
        let timeout_ms = self.config.timeout_ms;
        let wait_fut = async {
            loop {
                // is_high() drains the trigger ready-flag so a subsequent
                // wait_untill_any_edge() is race-free against an edge that
                // landed during the read.
                if pg.is_high().unwrap_or(false) {
                    return;
                }
                pg.wait_untill_any_edge().await;
            }
        };
        wait_fut
            .with_timeout_ms(timeout_ms)
            .await
            .ok_or(PowerSwitchError::Timeout)
    }
}

impl<EN, PG> PowerSwitch for GpioPowerSwitch<EN, PG>
where
    EN: OutputPin,
    PG: InputPin + TriggerTrait + Pullup,
{
    async fn enable(&mut self) -> Result<(), PowerSwitchError> {
        if self.enabled {
            return Ok(());
        }

        // Ensure the rail has actually discharged if we were recently disabled.
        self.off_timeout.wait().await;

        // Flip PG pullup before driving enable high so the open-drain PG reads
        // correctly as soon as the rail asserts.
        self.power_good.set_pullup(self.config.pg_pulls.on());
        let _ = self.enable.set_high();

        if let Err(e) = self.await_power_good().await {
            self.power_off();
            return Err(e);
        }

        delay_ms(self.config.settling_delay_ms).await;
        self.enabled = true;
        Ok(())
    }

    fn disable(&mut self) {
        self.power_off();
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }
}
