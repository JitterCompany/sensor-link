//! Device operational settings
//!
//! All settings that are not concerned with the measurement configuration.
//!

use demo_protocol::settings::{DataRate, Settings as ProtocolSettings};

use crate::utils::num::force_clamped;

/// Maximum sync interval setting: higher values are clamped to this limit.
/// The orchestrator's sync timer uses the same bound.
pub const SYNC_INTERVAL_MAX_SEC: i32 = 4 * 3600;

/// minimum sync interval: lower values are converted to sync interval=0 (always online)
pub const SYNC_INTERVAL_ZERO_SEC: i32 = 180;

#[derive(Debug, Clone)]
pub struct Settings {
    pub data_rate: DataRate,
    pub sync_interval_s: i32,
    pub hash: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            data_rate: Default::default(),
            sync_interval_s: 900,
            hash: 0,
        }
    }
}

/// Non-critical issues: settings were adjusted so they can still be used
#[derive(Debug, PartialEq)]
pub enum Warning {
    /// SyncInterval out of range
    SyncIntervalRange,
}

/// Critical issues: settings are not suitable to be used at all
#[derive(Debug, PartialEq)]
pub enum Error {}

pub fn validate(settings: ProtocolSettings) -> Result<(Settings, Option<Warning>), Error> {
    let mut warning = None;
    let mut settings: Settings = settings.into();

    // all values are considered valid for these:
    let _ = &settings.hash;

    log::info!("Sync interval {}", settings.sync_interval_s);
    if force_clamped(&mut settings.sync_interval_s, 0, SYNC_INTERVAL_MAX_SEC) {
        warning = Some(Warning::SyncIntervalRange);
    }
    if settings.sync_interval_s != 0 && settings.sync_interval_s < SYNC_INTERVAL_ZERO_SEC {
        settings.sync_interval_s = 0;
    }

    Ok((settings, warning))
}

#[cfg(test)]
mod test {

    use demo_protocol::settings;

    use super::*;

    fn build_settings(sync_interval_s: i32) -> settings::Settings {
        settings::Settings {
            data_rate: Default::default(),
            sync_interval_s,
            hash: u64::MAX,
        }
    }

    #[test]
    fn test_validate_normal_settings() {
        let (validated, warn) = validate(build_settings(900)).unwrap();
        assert!(warn.is_none());
        assert_eq!(validated.sync_interval_s, 900);
    }

    #[test]
    fn test_validate_clamps_sync_interval() {
        let (validated, warn) = validate(build_settings(i32::MAX)).unwrap();
        assert_eq!(warn, Some(Warning::SyncIntervalRange));
        assert_eq!(validated.sync_interval_s, SYNC_INTERVAL_MAX_SEC);
    }
}
