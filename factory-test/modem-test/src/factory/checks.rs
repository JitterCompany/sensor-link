//! The jig's self-test: steps 1-4.
//!
//! Each check reports its own lines and returns whether it passed. The windows
//! are the constants below, each with where its number came from; see
//! `docs/factory-test.md` for the measurements behind them.

use core::{fmt::Write as _, ops::RangeInclusive};

use hardware::{
    Adc, GpioPowerSwitch, Pin, PinMode, PinN, Port, PowerSwitch as _, PullupConfig, Straps,
    TriggeredInputPin,
};
use sensor_link_firmware::{
    drivers::{
        boot_stats,
        bq25672::{BatteryCharger, BQ25672},
    },
    heapless::{String, Vec},
    monotonic_time::delay_ms,
};

/// The concrete types this board uses. Spelled out once here rather than
/// carried as generics: this is one board's factory test, not a library.
pub type TimeoutProvider = sensor_link_firmware::monotonic_time::Time;
pub type Charger = BQ25672<hardware::I2C, Pin, hardware::Trigger, TimeoutProvider>;
pub type Rail5V5 = GpioPowerSwitch<Pin, TriggeredInputPin>;

use super::report;

// ----------------------------------------------------------- test windows ---

/// Expected system clock. Every timing in this test assumes it.
pub const SYSCLK_HZ: u32 = 72_000_000;

/// VSYS, the charger's output that feeds both DC-DCs.
///
/// The charger driver programs a 6250 mV minimum system voltage, chosen so the
/// 5V5 buck downstream stays in spec, and a healthy board honours it: 6553 mV
/// measured with no battery fitted. A board reading below the floor the driver
/// asked for is not regulating, which is what a failed one looked like
/// (4364 mV, with 3V3 collapsed to 2 V behind it).
const SYSTEM_MV: RangeInclusive<i32> = 6_000..=13_000;

/// PCB temperature (MCP9701A), read through the internal ADC.
///
/// Measured 22.45 C on a known-good v0 board at room temperature
/// (2026-09-15). The window is room temperature plus the board's own rise and
/// the sensor's tolerance; it is not a calibrated measurement, because the
/// conversion in `bsp/src/adc.rs` assumes a fixed 3.3 V reference.
const PCB_TEMP_MC: RangeInclusive<i32> = 5_000..=45_000;

/// Peak-to-peak spread of the temperature over a burst.
///
/// 77 mC on a known-good board, against 3900 mC on one whose 3V3 rail had
/// collapsed to 2 V: a sensitive supply-health check rather than ADC noise.
/// 500 mC leaves room without losing that.
const PCB_TEMP_SPREAD_MC: i32 = 500;

/// The analog supply, measured against the internal reference.
///
/// The headline rail check: everything else this ADC reads is relative to it,
/// and a board whose 3V3 has sagged still runs, so nothing else notices. 3333 mV
/// measured on a known-good board (probe VTref independently read 3348 mV);
/// 2029 mV on a faulty one. The window is the SiC473's tolerance plus the
/// reference calibration error, which is wide enough to pass any healthy board
/// and nowhere near wide enough to pass a sick one.
const VDDA_MV: RangeInclusive<i32> = 3_150..=3_450;

/// The minimum VSYS at which the 5V5 rail can be tested at all.
///
/// U2 is a buck and cannot output 5.5 V from a lower input, so testing it below
/// this would fail a good board for an upstream reason. A healthy board sits
/// above it comfortably (6553 mV) with no battery fitted, so in practice this
/// only suppresses the check on a board that has already failed `system_mv`.
const RAIL_5V5_MIN_VSYS_MV: i32 = 6_000;

/// Samples per measurement burst, and the capacity of a [`Burst`]
pub const BURST: usize = 8;

/// Settle time between samples of an open-drain power-good line: a good rail
/// leaves the node high-Z, so the weak internal pull-up has to charge it.
const PGOOD_SETTLE_MS: u32 = 2;
/// How many times the 3V3 power-good line is sampled. Several, because it is
/// reported rather than gated and the spread is the interesting part.
const PGOOD_SAMPLES: usize = 5;

/// How long the 5V5 rail is held on to prove it and its power-good line. The
/// SiC473 asserts PG ~16 ms after enable (measured, see `SIC473_PG_TIMEOUT_MS`).
const RAIL_5V5_ON_MS: u32 = 200;

// ------------------------------------------------------------- 3V3 PG pin ---

/// Sample the 3V3 rail's power-good line: `power_good_3v3 = "pd3"`, U3's PG.
///
/// An open-drain node held only by the MCU's internal pull-up. That is ample: a
/// known-good board reads 5/5 high, and it flapped only on a board whose 3V3
/// had collapsed to 2 V, which is the fault it is for.
async fn pwr_3v3_good_samples() -> Vec<bool, PGOOD_SAMPLES> {
    let mut pin = Pin::new(Port::D, PinN::P3, PinMode::Input);
    pin.set_pullup(PullupConfig::Pullup);

    let mut samples = Vec::new();
    for _ in 0..PGOOD_SAMPLES {
        delay_ms(PGOOD_SETTLE_MS).await;
        let _ = samples.push(pin.is_high());
    }
    samples
}

// ------------------------------------------------------------ burst helper --

/// A burst of samples of one quantity.
///
/// Every measurement is taken several times: a wandering reading means a noisy
/// supply or a bad joint, which a single sample would hide.
#[derive(Default)]
pub struct Burst(Vec<i32, BURST>);

impl Burst {
    pub fn push(&mut self, value: i32) {
        // Full means the caller asked for more than BURST samples, which is a
        // bug in the caller, not something a board can cause.
        let _ = self.0.push(value);
    }

    pub fn mean(&self) -> Option<i32> {
        let sum: i32 = self.0.iter().sum();
        (!self.0.is_empty()).then(|| sum / self.0.len() as i32)
    }

    pub fn spread(&self) -> i32 {
        match (self.0.iter().min(), self.0.iter().max()) {
            (Some(min), Some(max)) => max - min,
            _ => 0,
        }
    }

    /// Report the mean against a window, and return whether it is inside.
    pub fn gate(&self, key: &str, unit: &str, window: RangeInclusive<i32>) -> bool {
        let Some(mean) = self.mean() else {
            report::note(format_args!("{key}: no samples"));
            return report::check(key, false);
        };
        report::meas(
            key,
            mean.into(),
            unit,
            (*window.start()).into(),
            (*window.end()).into(),
        )
    }

    /// Report min, mean and max with no pass/fail criterion
    pub fn record(&self, key: &str, unit: &str) {
        let Some(mean) = self.mean() else { return };
        report::meas_info(key, mean.into(), unit);
        for (suffix, value) in [("_min", self.0.iter().min()), ("_max", self.0.iter().max())] {
            if let Some(&value) = value {
                let mut name = String::<32>::new();
                let _ = write!(name, "{key}{suffix}");
                report::meas_info(&name, value.into(), unit);
            }
        }
    }
}

// ------------------------------------------------------------- step 1 boot --

/// Proves the MCU is the part we think it is, running at the clock everything
/// else assumes, on a board this firmware has been validated against.
pub async fn boot(straps: Straps, sysclk_hz: u32, stats: &boot_stats::Stats) -> bool {
    let uid = hardware::device_uid();
    report::info(
        "uid",
        format_args!("{:08X}-{:08X}-{:08X}", uid[0], uid[1], uid[2]),
    );
    report::info(
        "board_version",
        format_args!("{}", straps.board_version().name()),
    );
    report::info("hw_straps", format_args!("{straps}"));

    report::info("boot_reason", format_args!("{:?}", stats.boot_reason));
    report::meas_info("boot_total", stats.boot_total as i64, "count");
    report::meas_info("panic_total", stats.panic_total as i64, "count");
    report::meas_info("fault_total", stats.fault_total as i64, "count");

    let mut ok = report::meas(
        "sysclk",
        sysclk_hz as i64,
        "Hz",
        SYSCLK_HZ as i64,
        SYSCLK_HZ as i64,
    );

    ok &= report::check("hw_straps", straps.is_supported());

    ok
}

// ---------------------------------------------------------- step 2 charger --

/// Proves I2C3 is alive, U1 is a BQ25672 configured for this board, and it has
/// no latched faults.
///
/// `configure()` verifies the part id and checks the cell count against the PROG
/// resistor, so a mis-stuffed charger fails here rather than misbehaving later.
/// It also disables charging, which matters for every current measurement that
/// follows: charge current flows through IBUS and would swamp a module's draw.
pub async fn charger(charger: &mut Charger) -> bool {
    let configured = match charger.configure().await {
        Ok(()) => true,
        Err(e) => {
            report::note(format_args!("charger configure failed: {e:?}"));
            false
        }
    };
    let mut ok = report::check("charger_configure", configured);

    if configured {
        match charger.charger_status().await {
            Ok(status) => {
                report::info(
                    "source",
                    format_args!(
                        "adapter={} usb={} battery={} status={:?}",
                        status.adapter_present,
                        status.usb_present,
                        status.battery_present,
                        status.source_status
                    ),
                );
                report::info("charging", format_args!("{:?}", status.charging));
                ok &= report::check("charger_power_good", status.power_good);
            }
            Err(e) => {
                report::note(format_args!("charger status failed: {e:?}"));
                ok &= report::check("charger_status", false);
            }
        }

        match charger.faults().await {
            Ok(None) => {
                ok &= report::check("charger_faults_clear", true);
            }
            Ok(Some(faults)) => {
                report::note(format_args!("charger faults: {faults:?}"));
                ok &= report::check("charger_faults_clear", false);
            }
            Err(e) => {
                report::note(format_args!("charger fault read failed: {e:?}"));
                ok &= report::check("charger_faults_clear", false);
            }
        }
    }

    ok
}

// ------------------------------------------------------------ step 3 rails --

/// Proves both power-good lines and the charger's own rail measurements.
///
/// Returns the idle IBUS baseline that the modem steps measure against, which
/// only exists when the rails passed.
///
/// The 5V5 rail is switched on only for as long as it takes to prove U2 and its
/// power-good line, then switched off again so the extension FFC is dead for the
/// rest of the run.
pub async fn rails(charger: &mut Charger, rail_5v5: &mut Rail5V5) -> Option<i32> {
    let pgood_3v3 = pwr_3v3_good_samples().await;
    let high = pgood_3v3.iter().filter(|&&high| high).count();
    report::info(
        "pgood_3v3_samples",
        format_args!("{high}/{}", pgood_3v3.len()),
    );
    let mut ok = report::check("pgood_3v3", high == pgood_3v3.len());

    let mut system = Burst::default();
    let mut ibus = Burst::default();
    let mut adapter = Burst::default();
    let mut battery = Burst::default();

    let mut reads = 0;
    for _ in 0..BURST {
        match charger.measure_power().await {
            Ok(m) => {
                system.push(m.system_mv as i32);
                ibus.push(m.current_bus_ma as i32);
                adapter.push(m.adapter_mv as i32);
                battery.push(m.battery_mv as i32);
                reads += 1;
            }
            Err(e) => report::note(format_args!("measure_power failed: {e:?}")),
        }
    }
    ok &= report::check("charger_adc", reads > 0);

    ok &= system.gate("system_mv", "mV", SYSTEM_MV);

    // 5V5 only after VSYS is known: testing it below the buck's input threshold
    // would fail a perfectly good board for a missing battery.
    match system.mean() {
        Some(vsys) if vsys >= RAIL_5V5_MIN_VSYS_MV => {
            let enabled = match rail_5v5.enable().await {
                Ok(()) => true,
                Err(e) => {
                    report::note(format_args!("5V5 enable failed: {e:?}"));
                    false
                }
            };
            ok &= report::check("pgood_5v5", enabled);
            delay_ms(RAIL_5V5_ON_MS).await;
            rail_5v5.disable();
        }
        Some(vsys) => {
            report::info(
                "pgood_5v5",
                format_args!("not tested: VSYS {vsys} mV is below the {RAIL_5V5_MIN_VSYS_MV} mV a buck needs -- fit a battery"),
            );
        }
        None => {
            ok &= report::check("pgood_5v5", false);
        }
    }
    // Informational: which source feeds the bench is the operator's choice, and
    // a battery need not be fitted at all.
    adapter.record("adapter_mv", "mV");
    battery.record("battery_mv", "mV");
    ibus.record("ibus_idle_ma", "mA");

    ibus.mean().filter(|_| ok)
}

// ------------------------------------------------------ step 4 temperature --

/// Proves ADC1, the MCP9701A, and the 3V3 rail as the MCU itself sees it.
///
/// `vdda` comes from the internal reference against its factory calibration, so
/// it is an absolute measurement of the analog supply rather than one relative
/// to it -- the only way this board can check its own 3V3 without external
/// equipment.
pub async fn temperature(adc: &mut Adc<TimeoutProvider>) -> bool {
    let mut temp = Burst::default();
    let mut vdda = Burst::default();
    let mut vrefint = Burst::default();
    let mut cal = 0u16;
    for _ in 0..BURST {
        match adc.measure().await {
            Ok(m) => {
                temp.push(m.board_temp_millicelsius);
                vdda.push(m.vdda_millivolts.into());
                vrefint.push(m.vrefint_raw.into());
                cal = m.vrefint_cal;
            }
            Err(e) => report::note(format_args!("adc measure failed: {e:?}")),
        }
    }
    let mut ok = vdda.gate("vdda_mv", "mV", VDDA_MV);
    vrefint.record("vrefint_raw", "count");
    report::meas_info("vrefint_cal", cal.into(), "count");

    ok &= report::meas(
        "pcb_temp_spread",
        temp.spread().into(),
        "mC",
        0,
        PCB_TEMP_SPREAD_MC.into(),
    );
    ok &= temp.gate("pcb_temp", "mC", PCB_TEMP_MC);
    ok
}
