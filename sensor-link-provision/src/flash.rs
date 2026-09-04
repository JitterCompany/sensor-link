//! Probe access and flash programming through probe-rs (native J-Link driver,
//! no SEGGER software needed).

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use probe_rs::{
    Permissions, Session,
    flashing::{
        DownloadOptions, ElfLoader, ElfOptions, FlashLoader, FlashProgress,
        download_file_with_options, erase,
    },
    probe::{DebugProbeInfo, list::Lister},
};

/// All connected probes, as display strings.
pub fn list_probes() -> Vec<String> {
    Lister::new().list_all().iter().map(describe).collect()
}

fn describe(p: &DebugProbeInfo) -> String {
    match &p.serial_number {
        Some(s) => format!("{} (S/N {s})", p.identifier),
        None => p.identifier.clone(),
    }
}

/// The single probe to use. With several connected, a J-Link is preferred;
/// several J-Links is an error (the operator must unplug one).
pub fn find_probe() -> Result<DebugProbeInfo> {
    let all = Lister::new().list_all();
    if all.is_empty() {
        bail!("no debug probe found; connect the J-Link over USB");
    }
    let jlinks: Vec<&DebugProbeInfo> = all
        .iter()
        .filter(|p| p.identifier.to_lowercase().contains("j-link"))
        .collect();
    let candidates = if jlinks.is_empty() {
        all.iter().collect()
    } else {
        jlinks
    };
    match candidates.as_slice() {
        [one] => Ok((*one).clone()),
        many => bail!(
            "{} probes connected, keep only one: {}",
            many.len(),
            many.iter()
                .map(|p| describe(p))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn attach(chip: &str, speed_khz: u32) -> Result<Session> {
    let info = find_probe()?;
    let mut probe = info
        .open()
        .with_context(|| format!("opening {}", describe(&info)))?;
    probe
        .set_speed(speed_khz)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("setting SWD speed to {speed_khz} kHz"))?;
    probe
        .attach(chip, Permissions::new())
        .map_err(anyhow::Error::from)
        .with_context(|| {
            format!("attaching to {chip} (is the board powered and the SWD plug seated?)")
        })
}

/// Flash an ELF; only the sectors it covers are erased.
pub fn flash_elf(session: &mut Session, path: &Path) -> Result<()> {
    let mut options = DownloadOptions::default();
    options.verify = true;
    options.progress = FlashProgress::new(|ev| log::debug!("flash: {ev:?}"));
    download_file_with_options(session, path, ElfLoader(ElfOptions::default()), options)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("flashing {}", path.display()))
}

/// Erase `start..end` and write `data` at `start` (the config region).
pub fn write_region(session: &mut Session, start: u64, end: u64, data: &[u8]) -> Result<()> {
    if data.len() as u64 > end - start {
        bail!(
            "config ({} bytes) does not fit in {:#x}..{:#x}",
            data.len(),
            start,
            end
        );
    }
    let mut progress = FlashProgress::new(|ev| log::debug!("erase: {ev:?}"));
    erase(session, &mut progress, start, end, false)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("erasing {start:#x}..{end:#x}"))?;

    let mut loader = FlashLoader::new(
        session.target().memory_map.clone(),
        session.target().source().clone(),
    );
    loader
        .add_data(start, data)
        .map_err(anyhow::Error::from)
        .context("staging config")?;
    let mut options = DownloadOptions::default();
    options.verify = true;
    options.skip_erase = true;
    options.keep_unwritten_bytes = true;
    options.progress = FlashProgress::new(|ev| log::debug!("write: {ev:?}"));
    loader
        .commit(session, options)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("writing config at {start:#x}"))
}

/// Reset the target and let the firmware run.
///
/// probe-rs `Core::reset()` only issues SYSRESETREQ; it does not touch the
/// reset vector catch (DEMCR.VC_CORERESET). Flashing can leave that catch
/// armed, and the core then halts at the reset vector instead of running, so
/// the RTT boot check sees nothing. `reset_and_halt` sets and then clears the
/// catch itself, so a following `run()` reliably starts the firmware.
pub fn reset(session: &mut Session) -> Result<()> {
    let mut core = session.core(0)?;
    core.reset_and_halt(Duration::from_millis(500))
        .map_err(anyhow::Error::from)
        .context("reset")?;
    core.run()
        .map_err(anyhow::Error::from)
        .context("run after reset")
}
