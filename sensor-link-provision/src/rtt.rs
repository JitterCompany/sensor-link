//! Post-flash boot checks: read RTT channel 0 and look for the UID and other
//! markers after the most recent boot banner (RAM keeps old RTT data across
//! resets, so only lines after the banner count).

use std::time::{Duration, Instant};

use anyhow::Result;
use probe_rs::{
    Session,
    rtt::{Rtt, ScanRegion},
};

/// True when `needle` occurs (case-insensitively) after the last `banner` line.
pub fn seen_after_banner(log: &str, banner: &str, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    let mut found = false;
    for line in log.lines() {
        if line.contains(banner) {
            found = false;
        }
        if line.to_lowercase().contains(&needle) {
            found = true;
        }
    }
    found
}

/// RTT channel 0 of a freshly reset device. The log accumulates across
/// [`wait_for`](Self::wait_for) calls, so later checks see earlier output.
pub struct BootLog {
    region: ScanRegion,
    banner: String,
    rtt: Option<Rtt>,
    pub log: String,
}

impl BootLog {
    pub fn new(address: u64, banner: &str) -> Self {
        BootLog {
            region: ScanRegion::Exact(address),
            banner: banner.to_owned(),
            rtt: None,
            log: String::new(),
        }
    }

    /// Collect channel 0 until `needle` shows up after the boot banner or
    /// `timeout` passes. Attach is retried while the firmware is still
    /// initialising the control block after reset.
    pub fn wait_for(
        &mut self,
        session: &mut Session,
        needle: &str,
        timeout: Duration,
        mut on_update: impl FnMut(&str),
    ) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline {
            if seen_after_banner(&self.log, &self.banner, needle) {
                return Ok(true);
            }
            let mut core = session.core(0)?;
            if self.rtt.is_none() {
                match Rtt::attach_region(&mut core, &self.region) {
                    Ok(r) => self.rtt = Some(r),
                    Err(e) => {
                        log::debug!("rtt attach: {e}");
                        drop(core);
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                }
            }
            let read = match self
                .rtt
                .as_mut()
                .and_then(|r| r.up_channel(0))
                .map(|ch| ch.read(&mut core, &mut buf))
            {
                Some(Ok(n)) => n,
                Some(Err(e)) => {
                    log::debug!("rtt read: {e}; re-attaching");
                    self.rtt = None;
                    0
                }
                None => {
                    log::debug!("rtt: no up channel 0; re-attaching");
                    self.rtt = None;
                    0
                }
            };
            drop(core);
            if read > 0 {
                self.log.push_str(&String::from_utf8_lossy(&buf[..read]));
                on_update(&self.log);
            } else {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(seen_after_banner(&self.log, &self.banner, needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchored_on_banner() {
        let banner = "# Starting";
        assert!(!seen_after_banner(
            "uid abc123def\n# Starting fw\nhello\n",
            banner,
            "ABC123DEF"
        ));
        assert!(seen_after_banner(
            "# Starting fw\nUID: abc123def\n",
            banner,
            "ABC123DEF"
        ));
        assert!(seen_after_banner(
            "stale ABC123DEF\n# Starting a\n# Starting b\nid=ABC123DEF\n",
            banner,
            "ABC123DEF"
        ));
        assert!(!seen_after_banner("", banner, "X"));
    }
}
