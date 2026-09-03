//! `provision.toml`: the project profile shipped inside the CI artifact zip.
//! Everything project-specific lives here; the binary has no built-in targets.

use std::fmt;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub project: Project,
    pub variants: Vec<Variant>,
    pub artifacts: Artifacts,
    pub target: Target,
    pub identity: Identity,
    #[serde(default)]
    pub session: Session,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
}

/// One flashable device variant: a firmware image plus the device-type byte
/// written into the config (must match what the firmware expects, or the
/// bootloader refuses to boot).
#[derive(Debug, Clone, Deserialize)]
pub struct Variant {
    pub name: String,
    pub device_type: u8,
    /// Glob matched against file names in the zip (ELF, no extension).
    pub firmware: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifacts {
    /// Glob matched against file names in the zip (ELF, no extension).
    pub bootloader: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    /// probe-rs chip name, e.g. `STM32L4R5ZITxP`.
    pub chip: String,
    #[serde(default = "default_swd_speed")]
    pub swd_speed_khz: u32,
    pub config_flash_start: u64,
    pub config_flash_end: u64,
    /// Fixed RAM address of the SEGGER RTT control block.
    pub rtt_address: u64,
    /// Line prefix printed by the firmware at boot; the UID must appear after
    /// the most recent occurrence.
    #[serde(default = "default_boot_banner")]
    pub boot_banner: String,
    #[serde(default = "default_rtt_timeout")]
    pub rtt_timeout_s: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Identity {
    pub uid_length: usize,
    pub cert_subject: CertSubject,
    pub cert_validity_days: u32,
    /// PIV retired slot holding the CA key: `R1`..`R20` or hex `82`..`95`.
    pub ca_piv_slot: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CertSubject {
    #[serde(rename = "OU")]
    pub ou: String,
    #[serde(rename = "O")]
    pub o: String,
    #[serde(rename = "C")]
    pub c: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Session {
    /// Default CSV log path; `~` expands to the home directory.
    #[serde(default)]
    pub default_log: Option<String>,
    /// Reminder shown when the operator ends the session.
    #[serde(default)]
    pub exit_note: Option<String>,
}

fn default_swd_speed() -> u32 {
    4000
}
fn default_boot_banner() -> String {
    "# Starting".into()
}
fn default_rtt_timeout() -> u64 {
    10
}

impl Profile {
    pub fn parse(toml_str: &str) -> Result<Self> {
        let profile: Profile = toml::from_str(toml_str).context("invalid provision.toml")?;
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<()> {
        if self.variants.is_empty() {
            bail!("provision.toml: at least one [[variants]] entry is required");
        }
        let t = &self.target;
        if t.config_flash_start == 0 || t.config_flash_end <= t.config_flash_start {
            bail!("provision.toml: invalid config flash range");
        }
        if t.rtt_address == 0 {
            bail!("provision.toml: rtt_address must be set");
        }
        if self.identity.uid_length == 0 {
            bail!("provision.toml: uid_length must be > 0");
        }
        if self.identity.cert_validity_days == 0 {
            bail!("provision.toml: cert_validity_days must be > 0");
        }
        self.identity.piv_slot()?;
        Ok(())
    }

    pub fn default_log_path(&self) -> Option<std::path::PathBuf> {
        let raw = self.session.default_log.as_deref()?;
        Some(expand_home(raw))
    }
}

pub fn expand_home(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return std::path::PathBuf::from(home).join(rest);
    }
    std::path::PathBuf::from(path)
}

impl Identity {
    /// Slot byte (`0x82..=0x95`) for `ca_piv_slot`.
    pub fn piv_slot(&self) -> Result<u8> {
        let s = self.ca_piv_slot.trim();
        let byte = if let Some(n) = s.strip_prefix(['R', 'r']) {
            let n: u8 = n.parse().context("ca_piv_slot: expected R1..R20")?;
            if !(1..=20).contains(&n) {
                bail!("ca_piv_slot: retired slots are R1..R20");
            }
            0x82 + n - 1
        } else {
            u8::from_str_radix(s, 16).context("ca_piv_slot: expected R<n> or hex 82..95")?
        };
        if !(0x82..=0x95).contains(&byte) {
            bail!("ca_piv_slot: retired slots are 0x82..0x95");
        }
        Ok(byte)
    }
}

impl fmt::Display for CertSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OU={}, O={}, C={}", self.ou, self.o, self.c)
    }
}

#[cfg(test)]
pub(crate) const EXAMPLE_PROFILE: &str = r#"
[project]
name = "BTB Zonneboiler"

[[variants]]
name = "Zonneboiler"
device_type = 1
firmware = "zonneboiler-*"

[artifacts]
bootloader = "bootloader-*"

[target]
chip = "STM32L4R5ZITxP"
config_flash_start = 0x081FE000
config_flash_end = 0x08200000
rtt_address = 0x2009FF00

[identity]
uid_length = 9
cert_subject = { OU = "Devices", O = "BTB Energy", C = "NL" }
cert_validity_days = 9650
ca_piv_slot = "R1"

[session]
default_log = "~/zonneboiler-provisioning/issuance.csv"
exit_note = "Enter the SIM ICCID of each new device in the dashboard."
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let p = Profile::parse(EXAMPLE_PROFILE).unwrap();
        assert_eq!(p.target.config_flash_start, 0x081FE000);
        assert_eq!(p.target.swd_speed_khz, 4000);
        assert_eq!(p.identity.piv_slot().unwrap(), 0x82);
        assert_eq!(p.variants[0].device_type, 1);
    }

    #[test]
    fn slot_forms() {
        let mut p = Profile::parse(EXAMPLE_PROFILE).unwrap();
        p.identity.ca_piv_slot = "95".into();
        assert_eq!(p.identity.piv_slot().unwrap(), 0x95);
        p.identity.ca_piv_slot = "R21".into();
        assert!(p.identity.piv_slot().is_err());
        p.identity.ca_piv_slot = "9c".into();
        assert!(p.identity.piv_slot().is_err());
    }

    #[test]
    fn frogwatch_style_multi_variant() {
        let toml = r#"
[project]
name = "Frogwatch"
[[variants]]
name = "Vibration"
device_type = 0
firmware = "vibration-sensor-v*"
[[variants]]
name = "Vibration Pro (A352)"
device_type = 4
firmware = "vibration-sensor-a352-*"
[[variants]]
name = "Fissure hub"
device_type = 2
firmware = "fissure-hub-*"
[artifacts]
bootloader = "bootloader-*"
[target]
chip = "STM32L4R7ZITx"
config_flash_start = 0x081FE000
config_flash_end = 0x08200000
rtt_address = 0x2009FF00
[identity]
uid_length = 9
cert_subject = { OU = "Devices", O = "Jitter", C = "NL" }
cert_validity_days = 10950
ca_piv_slot = "82"
"#;
        let p = Profile::parse(toml).unwrap();
        assert_eq!(p.variants.len(), 3);
        assert!(p.session.default_log.is_none());
    }
}
