//! Structured RTT output.
//!
//! Every result a tool might want is emitted as a tagged, whitespace-separated
//! line, so a host-side script can parse the whole run without knowing what any
//! individual step measures. The grammar is documented in
//! `docs/factory-test.md`; in short, after the logger's `[factory:*] - ` prefix:
//!
//! ```text
//! STEP  <code> <name> START
//! STEP  <code> <name> PASS|FAIL <ms>
//! MEAS  <code> <key> <value> <unit> <lo> <hi> PASS|FAIL|INFO
//! CHECK <code> <key> PASS|FAIL
//! INFO  <code> <key> <free text>
//! ```
//!
//! Absent fields are `-`. Every `MEAS` value is an integer, in milli-units (mV,
//! mA, mC) or a plain count, so a tool never has to parse a float. The log level
//! is not part of the grammar: failures are logged at error level so they stand
//! out to a human, but the tag is what identifies a line.
//!
//! The grammar is the house factory-test format, shared with our other factory
//! test firmwares, so one host-side parser serves all of them.
//!
//! The step code is taken from the currently running step rather than passed in,
//! so call sites stay short.

use super::current_code;

/// Start-of-step marker
pub fn step_start(code: u8, name: &str) {
    log::info!(target: "factory", "STEP {code} {name} START");
}

/// End-of-step verdict, with how long the step took
pub fn step_end(code: u8, name: &str, ok: bool, elapsed_ms: u64) {
    if ok {
        log::info!(target: "factory", "STEP {code} {name} PASS {elapsed_ms}");
    } else {
        log::error!(target: "factory", "STEP {code} {name} FAIL {elapsed_ms}");
    }
}

/// A measurement with limits. Returns whether it is inside them, so callers can
/// fold it straight into their result.
pub fn meas(key: &str, value: i64, unit: &str, lo: i64, hi: i64) -> bool {
    let ok = (lo..=hi).contains(&value);
    let code = current_code();
    if ok {
        log::info!(target: "factory", "MEAS {code} {key} {value} {unit} {lo} {hi} PASS");
    } else {
        log::error!(target: "factory", "MEAS {code} {key} {value} {unit} {lo} {hi} FAIL");
    }
    ok
}

/// A measurement with no pass/fail criterion, recorded for later analysis
pub fn meas_info(key: &str, value: i64, unit: &str) {
    let code = current_code();
    log::info!(target: "factory", "MEAS {code} {key} {value} {unit} - - INFO");
}

/// A pass/fail with no associated number. Returns `ok` so callers can chain.
pub fn check(key: &str, ok: bool) -> bool {
    let code = current_code();
    if ok {
        log::info!(target: "factory", "CHECK {code} {key} PASS");
    } else {
        log::error!(target: "factory", "CHECK {code} {key} FAIL");
    }
    ok
}

/// A named non-numeric attribute: a model string, an id, a state name
pub fn info(key: &str, text: core::fmt::Arguments) {
    let code = current_code();
    log::info!(target: "factory", "INFO {code} {key} {text}");
}

/// Free-form diagnostic detail attached to a failure. Tools ignore these; they
/// exist so a human reading the log knows what went wrong.
pub fn note(text: core::fmt::Arguments) {
    log::error!(target: "factory", "  {text}");
}
