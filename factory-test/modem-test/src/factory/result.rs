//! Which steps ran, which failed, and what each indicator should show.
//!
//! All of it lives in atomics rather than RTIC resources, because the LED task,
//! the deadline task, the RTT summary and the panic handler all have to read the
//! same source of truth and the panic handler cannot touch RTIC resources.
//!
//! Two numbering schemes live here on purpose, and they are not the same thing:
//!
//! * [`Step`] codes identify a step in the RTT log. They are global, stable and
//!   never reused, so a host-side tool can key measurements on `(code, key)`
//!   across boards and across firmware versions.
//! * Fault codes are what the operator *counts on an LED*. They are scoped to a
//!   [`Zone`], so every zone starts at 2 and no code needs more than a handful of
//!   blinks. Nothing has to be reserved for a test that does not exist yet.

use core::{
    fmt::Write as _,
    sync::atomic::{AtomicU32, AtomicU8, Ordering},
};

use sensor_link_firmware::heapless::String;

/// A test step, in run order. The code identifies the step in the log only --
/// what the operator counts is the zone's fault code, not this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    /// Clock, board straps and version, boot stats, MCU unique ID
    Boot = 1,
    /// BQ25672 answers on I2C3, is the right part, and has no latched faults
    Charger = 2,
    /// Power-good lines, rail voltages, and the idle IBUS baseline
    Rails = 3,
    /// MCP9701A board temperature through the internal ADC
    Temperature = 4,
    /// Modem rail up, and the current says a module is actually in the slot
    ModemRail = 5,
    /// Modem boots and answers AT; model accepted
    ModemAt = 6,
    /// SIM detected and its ICCID readable
    ModemSim = 7,
    /// Running current with RF off, then a polite power-down
    ModemCurrent = 8,
}

impl Step {
    pub const ALL: [Step; 8] = [
        Step::Boot,
        Step::Charger,
        Step::Rails,
        Step::Temperature,
        Step::ModemRail,
        Step::ModemAt,
        Step::ModemSim,
        Step::ModemCurrent,
    ];

    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Bit position in [`PROGRESS`]
    const fn bit(self) -> u32 {
        1 << (self as u8 - 1)
    }

    /// Short slug used in every log line, so a capture can be grepped per step
    pub const fn name(self) -> &'static str {
        match self {
            Step::Boot => "boot",
            Step::Charger => "charger",
            Step::Rails => "rails",
            Step::Temperature => "temperature",
            Step::ModemRail => "modem_rail",
            Step::ModemAt => "modem_at",
            Step::ModemSim => "modem_sim",
            Step::ModemCurrent => "modem_current",
        }
    }

    /// Which indicator reports this step
    pub const fn zone(self) -> Zone {
        match self {
            Step::Boot | Step::Charger | Step::Rails | Step::Temperature => Zone::Board,
            Step::ModemRail | Step::ModemAt | Step::ModemSim | Step::ModemCurrent => Zone::Modem,
        }
    }

    /// How long the step may take before it is cut off and failed
    pub const fn timeout_ms(self) -> u32 {
        match self {
            Step::Boot => 2_000,
            Step::Charger => 3_000,
            Step::Rails => 5_000,
            Step::Temperature => 3_000,
            Step::ModemRail => 5_000,
            // Includes the module's own boot, ~11 s
            Step::ModemAt => 20_000,
            Step::ModemSim => 6_000,
            Step::ModemCurrent => 5_000,
        }
    }

    /// The fault shown when this step fails without naming a more specific one
    pub const fn default_fault(self) -> Fault {
        match self {
            Step::Boot => Fault::Board(BoardFault::Mcu),
            Step::Charger => Fault::Board(BoardFault::Charger),
            Step::Rails => Fault::Board(BoardFault::Rails),
            Step::Temperature => Fault::Board(BoardFault::Temperature),
            Step::ModemRail => Fault::Modem(ModemFault::NoCurrent),
            Step::ModemAt => Fault::Modem(ModemFault::NoResponse),
            Step::ModemSim => Fault::Modem(ModemFault::Sim),
            Step::ModemCurrent => Fault::Modem(ModemFault::Overcurrent),
        }
    }
}

/// One thing under test, with one LED pair of its own.
///
/// [`Zone::Board`] is the test jig itself rather than a device under test: if it
/// fails, no module verdict is worth anything, which is why it gets its own
/// indicator and overrides the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Zone {
    Board = 0,
    Modem = 1,
    // The jig's `Sensor test` LED pair has no zone: this firmware tests modems only.
}

impl Zone {
    pub const ALL: [Zone; 2] = [Zone::Board, Zone::Modem];
    pub const COUNT: usize = Zone::ALL.len();

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn name(self) -> &'static str {
        match self {
            Zone::Board => "board",
            Zone::Modem => "modem",
        }
    }
}

/// The smallest blink code. A single flash is never used: without a reference
/// for the rate it cannot be told apart from a slow steady blink.
pub const MIN_FAULT_CODE: u8 = 2;

/// Faults of the jig itself, blinked on `Status / Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BoardFault {
    /// Wrong sysclk, unexpected board straps, or boot stats showing a fault loop
    Mcu = 2,
    /// Charger not answering on I2C3, wrong part, or latched faults
    Charger = 3,
    /// A rail outside its window, or a power-good line low
    Rails = 4,
    /// Board temperature implausible
    Temperature = 5,
    /// The firmware panicked on the previous run, or the run never finished
    Firmware = 6,
}

/// Faults of the module under test, blinked on `Modem test / Fail`. Ordered by
/// how often we expect them, so the common ones take the fewest flashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModemFault {
    /// Current rose normally, but the module never answered AT
    NoResponse = 2,
    /// SIM not detected, or its ICCID could not be read
    Sim = 3,
    /// Module draws far more than it should -- the known defect class
    Overcurrent = 4,
    /// No current at all: nothing fitted, or the 3V3_PCIE rail failed
    NoCurrent = 5,
    /// AT+CGMM reported a model this firmware does not support
    Model = 6,
}

/// A fault, which also says which zone's LED pair shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Board(BoardFault),
    Modem(ModemFault),
}

impl Fault {
    pub const fn zone(self) -> Zone {
        match self {
            Fault::Board(_) => Zone::Board,
            Fault::Modem(_) => Zone::Modem,
        }
    }

    /// The number of flashes
    pub const fn code(self) -> u8 {
        match self {
            Fault::Board(f) => f as u8,
            Fault::Modem(f) => f as u8,
        }
    }
}

impl From<BoardFault> for Fault {
    fn from(fault: BoardFault) -> Self {
        Fault::Board(fault)
    }
}

impl From<ModemFault> for Fault {
    fn from(fault: ModemFault) -> Self {
        Fault::Modem(fault)
    }
}

/// What one indicator is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ZonePhase {
    /// Not started; the pair is dark
    Idle = 0,
    Running = 1,
    Pass = 2,
    Fail = 3,
}

/// Overall run state, which drives the main indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Running = 0,
    Pass = 1,
    Fail = 2,
}

/// Steps that have reported, and those that reported a failure, as bitmasks
/// over [`Step::bit`].
static REPORTED: AtomicU32 = AtomicU32::new(0);
static FAILED: AtomicU32 = AtomicU32::new(0);
static PHASE: AtomicU8 = AtomicU8::new(Phase::Running as u8);
/// Code of the step currently running, for the panic message
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Per-zone phase and the first fault code recorded for it. First wins: the
/// earliest failure is the one that explains the rest.
static ZONE_PHASE: [AtomicU8; Zone::COUNT] =
    [const { AtomicU8::new(ZonePhase::Idle as u8) }; Zone::COUNT];
static ZONE_FAULT: [AtomicU8; Zone::COUNT] = [const { AtomicU8::new(0) }; Zone::COUNT];

pub fn set_current(step: Step) {
    CURRENT.store(step.code(), Ordering::Relaxed);
}

pub fn current_code() -> u8 {
    CURRENT.load(Ordering::Relaxed)
}

/// Record a step result. A failure marks its zone failed; the fault code itself
/// is set by the check, via [`fail`].
pub fn record(step: Step, ok: bool) {
    REPORTED.fetch_or(step.bit(), Ordering::Relaxed);
    if !ok {
        FAILED.fetch_or(step.bit(), Ordering::Relaxed);
        set_zone_phase(step.zone(), ZonePhase::Fail);
    }
}

/// Fail the fault's zone with it. Keeps the first fault: later failures are
/// usually consequences of the first one.
pub fn fail(fault: impl Into<Fault>) {
    let fault = fault.into();
    arm_fault(fault);
    set_zone_phase(fault.zone(), ZonePhase::Fail);
}

/// Record a fault without failing its zone: evidence of *how* a module is
/// broken that is not, on its own, grounds to reject it. Shown only if a later
/// check fails the zone; first fault wins, as with [`fail`].
pub fn arm_fault(fault: impl Into<Fault>) {
    let fault = fault.into();
    let _ = ZONE_FAULT[fault.zone().index()].compare_exchange(
        0,
        fault.code(),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// The blink code recorded for a zone, if any
pub fn zone_fault(zone: Zone) -> Option<u8> {
    match ZONE_FAULT[zone.index()].load(Ordering::Relaxed) {
        0 => None,
        code => Some(code),
    }
}

pub fn zone_phase(zone: Zone) -> ZonePhase {
    match ZONE_PHASE[zone.index()].load(Ordering::Relaxed) {
        1 => ZonePhase::Running,
        2 => ZonePhase::Pass,
        3 => ZonePhase::Fail,
        _ => ZonePhase::Idle,
    }
}

/// Set a zone's phase. A zone that has already failed stays failed -- the
/// verdict latches, so a later step cannot paint over it.
pub fn set_zone_phase(zone: Zone, phase: ZonePhase) {
    if zone_phase(zone) == ZonePhase::Fail && phase != ZonePhase::Fail {
        return;
    }
    ZONE_PHASE[zone.index()].store(phase as u8, Ordering::Relaxed);
}

/// Has this step reported a result yet?
fn reported(step: Step) -> bool {
    REPORTED.load(Ordering::Relaxed) & step.bit() != 0
}

pub fn phase() -> Phase {
    match PHASE.load(Ordering::Relaxed) {
        1 => Phase::Pass,
        2 => Phase::Fail,
        _ => Phase::Running,
    }
}

pub fn set_phase(phase: Phase) {
    PHASE.store(phase as u8, Ordering::Relaxed);
}

pub fn failed(step: Step) -> bool {
    FAILED.load(Ordering::Relaxed) & step.bit() != 0
}

/// Mark every step that never reported as failed. Used by the deadline task: a
/// hung run must read as a failure, not as an eternal busy blink.
///
/// Records the steps and nothing else. The caller names the cause, and for the
/// deadline that cause is the jig: a run that did not finish says nothing about
/// the module in the slot, and blaming the step that happened to be executing
/// would bin a possibly-good part under whichever code that step defaults to.
pub fn fail_unreported() {
    for step in Step::ALL.into_iter().filter(|&step| !reported(step)) {
        record(step, false);
    }
}

/// End the run: publish the verdict (unless it already ended) and log the summary.
pub fn finish(elapsed_ms: u64) {
    publish_verdict();
    log_summary(elapsed_ms);
}

fn publish_verdict() {
    if phase() != Phase::Running {
        return;
    }
    for zone in Zone::ALL {
        if zone_phase(zone) == ZonePhase::Running {
            set_zone_phase(zone, ZonePhase::Pass);
        }
    }
    set_phase(if anything_failed() {
        Phase::Fail
    } else {
        Phase::Pass
    });
}

/// Whether the run failed: a step failed, or a zone was failed directly with no
/// step to blame (a panic on the previous boot runs no steps at all).
fn anything_failed() -> bool {
    FAILED.load(Ordering::Relaxed) != 0
        || Zone::ALL
            .into_iter()
            .any(|zone| zone_phase(zone) == ZonePhase::Fail)
}

/// Log one line per step plus the single machine-greppable verdict line.
///
/// A host-side gate gets everything it needs from the last line:
/// `FACTORY TEST: PASS` or `FACTORY TEST: FAIL steps 3,6`.
fn log_summary(elapsed_ms: u64) {
    log::info!(target: "factory", "==== FACTORY TEST SUMMARY ({elapsed_ms} ms) ====");
    for step in Step::ALL {
        let verdict = match (reported(step), failed(step)) {
            (false, _) => "NOT RUN",
            (true, true) => "FAIL",
            (true, false) => "PASS",
        };
        log::info!(target: "factory", "  {:>2} {:<14} {verdict}", step.code(), step.name());
    }
    for zone in Zone::ALL {
        if let Some(code) = zone_fault(zone) {
            log::error!(target: "factory", "  zone {:<8} FAIL, blink code {code}", zone.name());
        }
    }

    if !anything_failed() {
        log::info!(target: "factory", "FACTORY TEST: PASS");
        return;
    }

    // One greppable verdict line: "FACTORY TEST: FAIL steps 3,6", or bare when
    // nothing ran far enough to name a step.
    let mut list = String::<48>::new();
    for step in Step::ALL.into_iter().filter(|&step| failed(step)) {
        let separator = if list.is_empty() { "" } else { "," };
        let _ = write!(list, "{separator}{}", step.code());
    }
    if list.is_empty() {
        log::error!(target: "factory", "FACTORY TEST: FAIL");
    } else {
        log::error!(target: "factory", "FACTORY TEST: FAIL steps {list}");
    }
}
