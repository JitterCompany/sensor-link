//! RTT-aware sleep gate. Keeps the MCU awake while a debug probe is
//! attached and recent log writes have fully fit into the RTT buffer,
//! so the host can drain the channel without racing WFI. Falls back to
//! `hardware::sleep()` when the host stops draining or no probe is present.

use core::sync::atomic::{AtomicU32, Ordering};

use demo_firmware_core::{log, monotonic_time::MonotonicInstant};
use hardware::{cortex_m, SleepLevel};

/// Time without a fully-fitting log write before the gate gives up and
/// sleeps. Tuned for reconnect tolerance over power: a host hiccup
/// (RTT Viewer restart, JLinkRTTLogger relaunch) within this window is
/// absorbed without losing the awake state.
///
/// Cost of an unclean host disconnect: stay awake for
/// `buffer_fill_time + RTT_STALE_US` before the gate notices. With heavy
/// logging that's ~30 s; with a quiet device it stretches longer.
const RTT_STALE_US: u64 = 30_000_000;
const HEARTBEAT_THRESHOLD_US: u64 = RTT_STALE_US * 7 / 10;

/// Monotonic milliseconds-since-init mod 2^32 of the most recent
/// fully-fitting RTT log write. `0` means "never written"; wraparound
/// happens after 49 days of uptime, far past any session, so wrapping
/// subtract is exact within any practical window.
static LAST_WRITE_MS: AtomicU32 = AtomicU32::new(0);

static AWAKE_COUNT: AtomicU32 = AtomicU32::new(0);
static SLEPT_COUNT: AtomicU32 = AtomicU32::new(0);
static HEARTBEAT_COUNT: AtomicU32 = AtomicU32::new(0);

fn now_ms() -> u32 {
    (MonotonicInstant::now().micros_since_init() / 1_000) as u32
}

fn last_write_age_us() -> Option<u64> {
    let last = LAST_WRITE_MS.load(Ordering::Relaxed);
    if last == 0 {
        return None;
    }
    let now = now_ms();
    Some(now.wrapping_sub(last) as u64 * 1_000)
}

fn is_attached() -> bool {
    cortex_m::peripheral::DCB::is_debugger_attached()
}

/// Idle calls this after each log-channel write whose return value equals
/// the full message length. Partial/trimmed writes intentionally do NOT
/// refresh: a trimmed write means the host stopped draining, which is
/// exactly the failure mode the gate must detect.
pub fn note_rtt_write() {
    let prev = LAST_WRITE_MS.load(Ordering::Relaxed);
    let now = now_ms();
    // Clamp 0 → 1 so the sentinel cannot collide with a real timestamp
    // taken in the first millisecond of uptime.
    LAST_WRITE_MS.store(now.max(1), Ordering::Relaxed);

    if prev == 0 {
        log::info!(target: "rtt-hb", "connected");
    } else {
        let gap_us = now.wrapping_sub(prev) as u64 * 1_000;
        if gap_us > RTT_STALE_US {
            log::info!(target: "rtt-hb", "reconnected after {}s", gap_us / 1_000_000);
        }
    }
}

/// Emit a single stat line. Intended as the handler for the `rtt` RTT
/// input command.
pub fn log_stats() {
    log::info!(
        target: "rtt",
        "attached={} age_us={:?} awake={} slept={} hb={}",
        is_attached() as u8,
        last_write_age_us(),
        AWAKE_COUNT.load(Ordering::Relaxed),
        SLEPT_COUNT.load(Ordering::Relaxed),
        HEARTBEAT_COUNT.load(Ordering::Relaxed),
    );
}

/// Emit a low-rate heartbeat when the staleness timer is approaching its
/// deadline, then sleep the MCU unless a probe is attached AND the last
/// fully-fitting log write was within `RTT_STALE_US`. Returns `true`
/// when WFI was skipped (gate kept the MCU awake), `false` when sleep
/// ran.
pub fn sleep_unless_debugger_reading() -> bool {
    // trying to sleep while debugger may be attached?
    if is_attached() && hardware::current_sleep_level() != SleepLevel::None {
        if let Some(age) = last_write_age_us() {
            if age <= RTT_STALE_US {
                // approaching the stale deadline: emit a heartbeat to probe
                // the host and (if it fits) extend the awake window
                if age > HEARTBEAT_THRESHOLD_US {
                    HEARTBEAT_COUNT.fetch_add(1, Ordering::Relaxed);
                    log::info!(target: "rtt-hb", "{}s", age / 1_000_000);
                }
                AWAKE_COUNT.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            // (host stopped reading long ago, sleep)
        }
    }

    // sleep
    SLEPT_COUNT.fetch_add(1, Ordering::Relaxed);
    hardware::sleep();
    false
}
