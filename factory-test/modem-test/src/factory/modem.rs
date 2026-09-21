//! The modem steps: is there a module in the slot, does it talk, and is its SIM
//! readable.
//!
//! Driven straight over the UART rather than through the Quectel driver. The
//! driver's `initialize()` renegotiates the baud rate and persists it with
//! `AT&W`, autodetects an APN and provisions certificates on first boot — none
//! of which belongs in an incoming-goods check, and a half-finished baud
//! negotiation is indistinguishable from a dead module. Raw exchanges also mean
//! any command can be sent without modelling it first.
//!
//! **Pass is defined by communication, never by current alone**: the module
//! answers, its model is one we support, the SIM reads READY and its ICCID comes
//! back. Current is what picks the fault code when communication fails, so the
//! operator learns *how* a module is broken rather than only that it is.

use embedded_io_async::{Read as _, Write as _};
use hardware::{Pin, UART};
use sensor_link_firmware::{
    drivers::bq25672::BatteryCharger as _,
    monotonic_time::{delay_ms, now, FutureTimeout as _, MonotonicInstant},
    traits::{BaudRateControl as _, Suspend as _},
};

use super::{
    checks::{Burst, Charger, BURST},
    report,
    result::{self, ModemFault},
};

// ------------------------------------------------------------- constants ----

/// Models this firmware accepts, matched case-insensitively against `AT+CGMM`.
///
/// `MiniPcie` in the modem driver declares `MODEL_PREFIX = "ec2"` for the same
/// carrier, so this is the same acceptance rule stated where the test can see
/// it. A module outside the list fails loudly rather than passing silently.
const SUPPORTED_MODELS: &[&str] = &["ec21", "ec25"];

/// The module's power-on default. A factory-fresh module answers here.
const BAUD_DEFAULT: u32 = 115_200;
/// What a module that has already been through provisioning answers at, since
/// the driver persists its negotiated rate with `AT&W`.
const BAUD_PROVISIONED: u32 = 3_000_000;

/// How long a module gets to boot far enough to answer `AT`, measured from when
/// its rail came up.
const BOOT_TIMEOUT_MS: u32 = 15_000;
/// Gap between `AT` probes while waiting for it
const POLL_MS: u32 = 250;
/// An ordinary command's reply
const AT_TIMEOUT_MS: u32 = 1_000;
/// `AT+CFUN` reconfigures the radio and answers more slowly than a query
const CFUN_TIMEOUT_MS: u32 = 5_000;
/// `AT+QPOWD` acknowledges, then powers the module down in its own time. The
/// acknowledgement is all this waits for; the rail is cut regardless.
const POWERDOWN_TIMEOUT_MS: u32 = 3_000;
/// SIM initialisation finishes well after the module starts answering AT, so
/// `AT+CPIN?` has to be retried rather than asked once.
const SIM_RETRIES: u32 = 8;
const SIM_RETRY_MS: u32 = 250;

/// Settle time after the rail comes up, before the current is sampled
const RAIL_SETTLE_MS: u32 = 50;
/// From releasing reset to resuming the UART
const RESET_RELEASE_MS: u32 = 2;
/// Given to an acknowledged `AT+QPOWD` before the rail is cut regardless
const POWERDOWN_GRACE_MS: u32 = 300;

/// Above this much current over the idle baseline, a module counts as present.
/// Below it, the slot is empty or the rail did not come up. (A good module: 18 mA.)
const MODULE_PRESENT_MA: i32 = 10;
/// Above this much over the baseline, a module draws far more than it should:
/// the known defect class, modules that get hot. A good module draws 18 mA, a
/// bad one 141 mA; the gate sits well clear of the good one. Measurements are in
/// `docs/factory-test.md`.
const MODULE_OVERCURRENT_MA: i32 = 80;

// ----------------------------------------------------------------- io ------

/// The modem's UART and its two control lines.
pub struct ModemIo {
    pub uart: UART,
    /// `cellular_pwr_en`: the U6 load switch feeding 3V3_PCIE
    pub enable: Pin,
    /// `cellular_reset_n`: PERST# on the mPCIe slot, high to release
    pub reset: Pin,
}

impl ModemIo {
    /// Raise the module's rail and release it from reset.
    ///
    /// The pin sequence only; whether anything answers is step 6's business.
    pub async fn power_on(&mut self) -> MonotonicInstant {
        self.enable.set_high();
        self.reset.set_high();
        delay_ms(RESET_RELEASE_MS).await;
        self.uart.resume();
        now()
    }

    /// Cut the rail and stop driving an unpowered module.
    pub fn power_off(&mut self) {
        self.uart.suspend();
        self.reset.set_low();
        self.enable.set_low();
    }

    /// Send a command and return whatever reply arrives before the timeout.
    ///
    /// A short or empty reply is normal: a missing answer is a result for the
    /// caller to report, not an error to propagate.
    async fn exchange<'b>(&mut self, cmd: &[u8], buf: &'b mut [u8], timeout_ms: u32) -> &'b [u8] {
        // Drain anything left over, so a previous command's tail is not read as
        // this one's answer.
        let mut scratch = [0_u8; 64];
        while self
            .uart
            .read(&mut scratch)
            .with_timeout_ms(1)
            .await
            .is_some()
        {}

        if self
            .uart
            .write(cmd)
            .with_timeout_ms(AT_TIMEOUT_MS)
            .await
            .is_none()
        {
            return &buf[..0];
        }

        let mut filled = 0;
        let deadline = now();
        while filled < buf.len() {
            let remaining =
                timeout_ms.saturating_sub((now().micros_since(&deadline) / 1000) as u32);
            if remaining == 0 {
                break;
            }
            match self
                .uart
                .read(&mut buf[filled..])
                .with_timeout_ms(remaining)
                .await
            {
                Some(Ok(0)) | None => break,
                Some(Ok(n)) => {
                    filled += n;
                    // A complete reply ends in a final result code; stop as soon
                    // as one arrives rather than waiting out the timeout.
                    if ends_response(&buf[..filled]) {
                        break;
                    }
                }
                Some(Err(_)) => break,
            }
        }
        &buf[..filled]
    }
}

/// Has a final result code arrived?
fn ends_response(buf: &[u8]) -> bool {
    contains(buf, b"OK\r\n") || contains(buf, b"ERROR")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Case-insensitive search, for matching model strings.
fn contains_ignore_case(haystack: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    haystack
        .windows(needle.len())
        .any(|w| w.iter().zip(needle).all(|(a, b)| a.eq_ignore_ascii_case(b)))
}

/// A reply with the final result code and the framing newlines taken off, but
/// the `+CMD: ` prefix left on, so an error still reads as `+CME ERROR: 13`.
fn trimmed(buf: &[u8]) -> &[u8] {
    let end = buf
        .windows(4)
        .position(|w| w == b"OK\r\n")
        .unwrap_or(buf.len());
    let body = &buf[..end];
    let start = body
        .iter()
        .position(|&b| !matches!(b, b'\r' | b'\n' | b' '))
        .unwrap_or(0);
    let stop = body
        .iter()
        .rposition(|&b| !matches!(b, b'\r' | b'\n' | b' '))
        .map_or(start, |i| i + 1);
    &body[start..stop.max(start)]
}

/// The value a query returned, or `None` when the module answered with an error
/// or nothing at all.
///
/// The `+CMD: ` echo is stripped, which matters for the ICCID -- the field a
/// traceability system keys on should be digits, not `+QCCID: <digits>`. That
/// same stripping is why the `None` case exists: it would otherwise reduce
/// `+CME ERROR: 13` to `13`, and a bare `13` in the `iccid` field is
/// indistinguishable from a card number.
fn value(buf: &[u8]) -> Option<&[u8]> {
    if !contains(buf, b"OK\r\n") {
        return None;
    }
    let body = trimmed(buf);
    Some(match body.iter().position(|&b| b == b':') {
        Some(colon) if body.first() == Some(&b'+') => {
            let value = &body[colon + 1..];
            let skip = value.iter().position(|&b| b != b' ').unwrap_or(0);
            &value[skip..]
        }
        _ => body,
    })
}

/// Logs one queried field, but only when the module actually returned a value.
///
/// A query that errored produces no `INFO` row at all, only a note saying what
/// came instead: see [`value`] for why a field row must never carry anything
/// but the real thing. The machine-readable signal is the `CHECK` row, which
/// fails.
fn report_field(field: &str, value: Option<&[u8]>, reply: &[u8]) {
    match value {
        Some(v) => report::info(field, format_args!("{}", Printable(v))),
        None => report::note(format_args!(
            "{field}: no value, module answered \"{}\"",
            Printable(trimmed(reply))
        )),
    }
}

/// Wrapper so a byte slice can be logged as text without allocating.
struct Printable<'a>(&'a [u8]);

impl core::fmt::Display for Printable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for &b in self.0 {
            // Control characters would make a mess of the log line.
            let c = if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            };
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

// ------------------------------------------------------ step 5 modem_rail --

/// Proves a module is in the slot and drawing current, before anything waits on
/// it to talk: an empty slot is caught in a moment rather than after a 15 s
/// boot timeout. Expects the rail to have just been switched on.
pub async fn rail(charger: &mut Charger, baseline_ma: i32) -> bool {
    delay_ms(RAIL_SETTLE_MS).await;

    // VSYS comes from the same reading, so it costs nothing to record. It is
    // what separates a module drawing too much from one that is pulling the
    // system rail down hard enough for the load switch to fold back -- and that
    // can only be seen while the faulty module is actually in the slot.
    let mut ibus = Burst::default();
    let mut vsys = Burst::default();
    for _ in 0..BURST {
        if let Ok(m) = charger.measure_power().await {
            ibus.push(m.current_bus_ma.into());
            vsys.push(m.system_mv as i32);
        }
    }
    ibus.record("ibus_modem_on_ma", "mA");
    vsys.record("system_mv_modem_on", "mV");

    let Some(delta) = ibus.mean().map(|mean| mean - baseline_ma) else {
        report::note(format_args!("no current readings with the modem rail up"));
        return false;
    };
    let in_band = report::meas(
        "modem_idle_delta_ma",
        delta.into(),
        "mA",
        MODULE_PRESENT_MA.into(),
        MODULE_OVERCURRENT_MA.into(),
    );

    // No current at all is a hard gate: nothing is fitted, so there is nothing
    // to wait fifteen seconds for and nothing to bin.
    if delta < MODULE_PRESENT_MA {
        report::note(format_args!(
            "{delta} mA over baseline: nothing fitted, or the 3V3_PCIE rail did not come up"
        ));
        return report::check("module_present", false);
    }

    // Too much current only arms the fault: the module still gets asked to do
    // its job, and one that does passes. If communication then fails, the armed
    // fault is what says *how* the module is broken.
    if !in_band {
        report::note(format_args!(
            "{delta} mA over baseline is far beyond idle; testing it anyway, the verdict is the modem's to earn"
        ));
        result::arm_fault(ModemFault::Overcurrent);
    }

    report::check("module_present", true)
}

// -------------------------------------------------------- step 6 modem_at --

/// Proves USART3 both ways, that the module boots, and that it is a model we
/// support.
pub async fn at(io: &mut ModemIo, powered_on: &MonotonicInstant) -> bool {
    let mut buf = [0_u8; 256];

    // A factory-fresh module answers at its default rate; one that has been
    // through provisioning persists the negotiated one. Try both rather than
    // making the operator care which.
    let Some(baud) = wait_for_at(io, &mut buf, powered_on).await else {
        report::note(format_args!(
            "no answer to AT within {BOOT_TIMEOUT_MS} ms of power-on, at either baud"
        ));
        return report::check("at_response", false);
    };
    report::meas_info("baud", baud.into(), "bps");
    report::meas_info(
        "boot_time",
        (now().micros_since(powered_on) / 1000) as i64,
        "ms",
    );
    let mut ok = report::check("at_response", true);

    // Echo off, so replies are just the payload.
    io.exchange(b"ATE0\r", &mut buf, AT_TIMEOUT_MS).await;
    // Verbose errors, so a SIM failure says which one it is.
    io.exchange(b"AT+CMEE=1\r", &mut buf, AT_TIMEOUT_MS).await;

    // Airplane mode: RF off, SIM still powered. The factory is not necessarily
    // in a supported region and the SIM need not be on a supported provider, so
    // letting the module hunt for a network would only waste time and put
    // current bursts on the supply. CFUN is volatile, so nothing is left behind.
    let reply = io.exchange(b"AT+CFUN=4\r", &mut buf, CFUN_TIMEOUT_MS).await;
    report::info(
        "rf",
        format_args!(
            "{}",
            if contains(reply, b"OK") {
                "disabled"
            } else {
                "still enabled (AT+CFUN=4 refused)"
            }
        ),
    );

    let reply = io.exchange(b"AT+CGMM\r", &mut buf, AT_TIMEOUT_MS).await;
    report_field("model", value(reply), reply);
    let accepted = SUPPORTED_MODELS
        .iter()
        .any(|model| contains_ignore_case(reply, model));
    if !accepted {
        result::fail(ModemFault::Model);
    }
    ok &= report::check("model", accepted);

    let reply = io.exchange(b"AT+CGMR\r", &mut buf, AT_TIMEOUT_MS).await;
    report_field("modem_firmware", value(reply), reply);

    ok
}

/// Poll `AT` at both plausible baud rates until one answers.
async fn wait_for_at(
    io: &mut ModemIo,
    buf: &mut [u8],
    powered_on: &MonotonicInstant,
) -> Option<u32> {
    while (now().micros_since(powered_on) / 1000) < u64::from(BOOT_TIMEOUT_MS) {
        for baud in [BAUD_DEFAULT, BAUD_PROVISIONED] {
            if io.uart.get_baud_rate() != baud {
                io.uart.set_baud_rate(baud);
            }
            let reply = io.exchange(b"AT\r", buf, POLL_MS).await;
            if contains(reply, b"OK") {
                return Some(baud);
            }
        }
    }
    None
}

// ------------------------------------------------------- step 7 modem_sim --

/// Proves the SIM holder on the module, its ESD network and the module's own SIM
/// interface.
///
/// The holder is on the module (J8 of `custom_ec2x_mini_pcie`), not on this
/// board, so a failure here is always the module or the SIM — never the jig.
pub async fn sim(io: &mut ModemIo) -> bool {
    let mut buf = [0_u8; 256];

    let mut ready = false;
    for attempt in 0..SIM_RETRIES {
        let reply = io.exchange(b"AT+CPIN?\r", &mut buf, AT_TIMEOUT_MS).await;
        if contains(reply, b"READY") {
            report::info("sim", format_args!("ready after {attempt} retries"));
            ready = true;
            break;
        }
        if contains(reply, b"+CME ERROR: 10") {
            // "SIM not inserted". A module with a solder fault on its holder
            // reports this too, so it is a failure either way -- but the log
            // says which error it was.
            report::info("sim", format_args!("not inserted (+CME ERROR: 10)"));
            break;
        }
        report::info(
            "sim_retry",
            format_args!("{attempt}: {}", Printable(trimmed(reply))),
        );
        delay_ms(SIM_RETRY_MS).await;
    }
    let mut ok = report::check("sim_ready", ready);

    // The ICCID proves a real exchange with the card rather than just a powered
    // slot, and records which SIM went through for this board.
    let reply = io.exchange(b"AT+QCCID\r", &mut buf, AT_TIMEOUT_MS).await;
    let iccid = value(reply).filter(|v| v.len() > 8);
    report_field("iccid", iccid, reply);
    ok &= report::check("iccid", iccid.is_some());

    // Informational: identifies the subscription, needs no network.
    let reply = io.exchange(b"AT+CIMI\r", &mut buf, AT_TIMEOUT_MS).await;
    report_field("imsi", value(reply), reply);

    ok
}

// --------------------------------------------------- step 8 modem_current --

/// Running current with the radio off, then a polite power-down.
///
/// Also proves the load switch actually switches: the current has to come back
/// down to the idle baseline once the rail is cut.
pub async fn current(io: &mut ModemIo, charger: &mut Charger, baseline_ma: i32) -> bool {
    let mut running = Burst::default();
    let mut vsys = Burst::default();
    for _ in 0..BURST {
        if let Ok(m) = charger.measure_power().await {
            running.push(m.current_bus_ma.into());
            vsys.push(m.system_mv as i32);
        }
    }
    running.record("ibus_running_ma", "mA");
    vsys.record("system_mv_running", "mV");
    if let Some(mean) = running.mean() {
        report::meas_info("modem_running_delta_ma", (mean - baseline_ma).into(), "mA");
    }

    // Polite power-down first, then cut the supply regardless of what it said.
    let mut buf = [0_u8; 128];

    // Is the session still alive at this point? Distinguishes a module that has
    // stopped talking from one that simply does not accept the command.
    let reply = io.exchange(b"AT\r", &mut buf, AT_TIMEOUT_MS).await;
    let alive = contains(reply, b"OK");
    report::info(
        "session",
        format_args!("{}", if alive { "alive" } else { "not responding" }),
    );

    // Only ask a module that is listening. Asking one that is not costs two
    // full command timeouts, which is enough to blow this step's budget -- and
    // then the step is dropped mid-way, the rail is never cut here, and the
    // rail-off check below is lost. That check is the one thing still worth
    // proving about a module that has stopped talking.
    let mut acknowledged = false;
    if alive {
        // Two spellings, because they are not interchangeable across Quectel
        // firmwares: an EC21EFAR06A03M4G answers ERROR to the parameterised
        // form and accepts the bare one. Trying both keeps this working across
        // module revisions instead of encoding one bench module's quirk.
        for cmd in [b"AT+QPOWD\r".as_slice(), b"AT+QPOWD=1\r".as_slice()] {
            let reply = io.exchange(cmd, &mut buf, POWERDOWN_TIMEOUT_MS).await;
            if contains(reply, b"OK") {
                acknowledged = true;
                break;
            }
        }
    }
    report::info(
        "powerdown",
        format_args!(
            "{}",
            match (alive, acknowledged) {
                (_, true) => "accepted",
                (true, false) => "refused; rail cut anyway, and the current below is the proof",
                (false, false) => "skipped, module not responding; rail cut instead",
            }
        ),
    );
    delay_ms(POWERDOWN_GRACE_MS).await;
    io.power_off();
    delay_ms(RAIL_SETTLE_MS).await;

    let mut off = Burst::default();
    for _ in 0..BURST {
        if let Ok(m) = charger.measure_power().await {
            off.push(m.current_bus_ma.into());
        }
    }
    off.record("ibus_modem_off_ma", "mA");

    match off.mean() {
        Some(mean) => {
            let residual = mean - baseline_ma;
            report::meas_info("modem_off_delta_ma", residual.into(), "mA");
            // The rail is off, so the current must have come back down. If it
            // has not, the load switch is not switching.
            report::check("modem_rail_off", residual < MODULE_PRESENT_MA)
        }
        None => report::check("modem_rail_off", false),
    }
}
