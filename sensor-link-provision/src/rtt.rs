//! Post-flash boot check: read RTT channel 0 and look for the UID after the
//! most recent boot banner (RAM keeps old RTT data across resets, so only
//! lines after the banner count).

use std::time::{Duration, Instant};

use anyhow::Result;
use probe_rs::{
    Session,
    rtt::{Rtt, ScanRegion},
};

pub struct Capture {
    pub matched: bool,
    pub log: String,
}

/// True when `uid` occurs (case-insensitively) after the last `banner` line.
pub fn uid_seen(log: &str, banner: &str, uid: &str) -> bool {
    let uid = uid.to_lowercase();
    let mut found = false;
    for line in log.lines() {
        if line.contains(banner) {
            found = false;
        }
        if line.to_lowercase().contains(&uid) {
            found = true;
        }
    }
    found
}

/// Attach to the RTT block at `address` and collect channel 0 until the UID
/// shows up or `timeout` passes. Attach is retried while the firmware is
/// still initialising the control block after reset.
pub fn wait_for_uid(
    session: &mut Session,
    address: u64,
    banner: &str,
    uid: &str,
    timeout: Duration,
) -> Result<Capture> {
    let deadline = Instant::now() + timeout;
    let region = ScanRegion::Exact(address);
    let mut log = String::new();
    let mut pending = Vec::new();
    let mut rtt: Option<Rtt> = None;
    let mut buf = [0u8; 4096];

    while Instant::now() < deadline {
        let mut core = session.core(0)?;
        if rtt.is_none() {
            match Rtt::attach_region(&mut core, &region) {
                Ok(r) => rtt = Some(r),
                Err(e) => {
                    log::debug!("rtt attach: {e}");
                    drop(core);
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            }
        }
        let read = match rtt
            .as_mut()
            .and_then(|r| r.up_channel(0))
            .map(|ch| ch.read(&mut core, &mut buf))
        {
            Some(Ok(n)) => n,
            Some(Err(e)) => {
                log::debug!("rtt read: {e}; re-attaching");
                rtt = None;
                0
            }
            None => {
                log::debug!("rtt: no up channel 0; re-attaching");
                rtt = None;
                0
            }
        };
        drop(core);
        if read > 0 {
            pending.extend_from_slice(&buf[..read]);
            log.push_str(&String::from_utf8_lossy(&pending));
            pending.clear();
            if uid_seen(&log, banner, uid) {
                return Ok(Capture { matched: true, log });
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(Capture {
        matched: false,
        log,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_on_banner() {
        let banner = "# Starting";
        assert!(!uid_seen(
            "uid abc123def\n# Starting fw\nhello\n",
            banner,
            "ABC123DEF"
        ));
        assert!(uid_seen(
            "# Starting fw\nUID: abc123def\n",
            banner,
            "ABC123DEF"
        ));
        assert!(uid_seen(
            "stale ABC123DEF\n# Starting a\n# Starting b\nid=ABC123DEF\n",
            banner,
            "ABC123DEF"
        ));
        assert!(!uid_seen("", banner, "X"));
    }
}
