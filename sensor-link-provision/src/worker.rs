//! Background thread that owns the hardware (probe, YubiKey) and runs the
//! session and per-device flows. The GUI only exchanges messages with it.

use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use eframe::egui;

use crate::{
    artifacts::Artifacts,
    cert::{self, Ca, DeviceKey},
    config_bin, flash, log_csv, rtt, yubikey_ca,
};

pub const STEPS: [&str; 8] = [
    "Sign device certificate",
    "Build config",
    "Connect to probe",
    "Flash bootloader",
    "Flash firmware",
    "Write config",
    "Append to log",
    "Verify boot (RTT)",
];
const STEP_LOG: usize = 6;
const STEP_RTT: usize = 7;

/// Development-only CA from files (`--dev-ca-key/--dev-ca-cert`); replaces
/// the YubiKey entirely.
#[derive(Debug, Clone)]
pub struct DevCa {
    pub key: PathBuf,
    pub cert: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub zip: PathBuf,
    pub variant: usize,
    pub log: PathBuf,
    pub pin: String,
    /// Used when the PIV slot holds no CA certificate.
    pub ca_cert_file: Option<PathBuf>,
    pub dev_ca: Option<DevCa>,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub project: String,
    pub variant: String,
    pub device_type: u8,
    pub bootloader: String,
    pub firmware: String,
    pub ca_subject: String,
    pub yubikey: String,
    pub dev_ca: bool,
    pub probe: String,
    pub log: PathBuf,
    pub uid_min: usize,
    pub uid_max: usize,
    pub exit_note: Option<String>,
}

pub enum Command {
    /// Enumerate probes (must run off the UI thread: USB/HID enumeration
    /// re-enters the macOS event loop).
    ListProbes,
    StartSession(SessionConfig),
    Provision {
        uid: String,
        icc: String,
        reprovision: bool,
    },
    /// Re-run the step that failed.
    Retry,
    /// Give up on the current device.
    Skip,
}

#[derive(Debug, Clone)]
pub enum StepState {
    Pending,
    Running,
    Done,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Flashed and the firmware reported the UID over RTT.
    Ok,
    /// Flashed and logged, but the boot log did not confirm the UID.
    Unverified,
    /// Not provisioned; safe to retry the same UID.
    Fail,
}

pub enum Event {
    Probes(Vec<String>),
    /// Accumulated RTT boot log for the device being verified.
    Rtt(String),
    /// Flashing progress (0.0..=1.0) for a step.
    StepProgress {
        index: usize,
        fraction: f32,
    },
    SessionReady(Box<SessionInfo>),
    SessionFailed(String),
    Step {
        index: usize,
        state: StepState,
    },
    DeviceFinished {
        uid: String,
        outcome: Outcome,
        reprovision: bool,
        rtt_log: String,
        error: Option<String>,
    },
}

pub fn spawn(commands: Receiver<Command>, events: Sender<Event>, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("provision-worker".into())
        .spawn(move || {
            Worker {
                commands,
                events,
                ctx,
                session: None,
            }
            .run()
        })
        .expect("spawning worker thread");
}

struct Session {
    artifacts: Artifacts,
    variant: usize,
    log: PathBuf,
    ca: Ca,
}

struct Worker {
    commands: Receiver<Command>,
    events: Sender<Event>,
    ctx: egui::Context,
    session: Option<Session>,
}

/// The operator chose Skip after a failed step.
struct Skipped;

impl Worker {
    fn send(&self, ev: Event) {
        let _ = self.events.send(ev);
        self.ctx.request_repaint();
    }

    fn run(mut self) {
        while let Ok(cmd) = self.commands.recv() {
            match cmd {
                Command::ListProbes => {
                    let probes = flash::list_probes();
                    self.send(Event::Probes(probes));
                }
                Command::StartSession(cfg) => match self.start_session(&cfg) {
                    Ok(info) => self.send(Event::SessionReady(Box::new(info))),
                    Err(e) => self.send(Event::SessionFailed(format!("{e:#}"))),
                },
                Command::Provision {
                    uid,
                    icc,
                    reprovision,
                } => self.provision(&uid, &icc, reprovision),
                Command::Retry | Command::Skip => {}
            }
        }
    }

    fn start_session(&mut self, cfg: &SessionConfig) -> Result<SessionInfo> {
        let artifacts = Artifacts::load(&cfg.zip)?;
        let profile = &artifacts.profile;
        let variant = profile
            .variants
            .get(cfg.variant)
            .context("variant index out of range")?
            .clone();
        let (ca, yubikey) = match &cfg.dev_ca {
            Some(dev) => (
                cert::load_dev_ca(&dev.key, &dev.cert)?,
                format!("none, development CA from {}", dev.key.display()),
            ),
            None => {
                let slot = yubikey_ca::slot_from_byte(profile.identity.piv_slot()?)?;
                let mut yk = yubikey_ca::open()?;
                let yk_info = yubikey_ca::info(&yk);
                yubikey_ca::verify_pin(&mut yk, &cfg.pin)?;
                let ca_der = match yubikey_ca::read_ca_cert(&mut yk, slot)? {
                    Some(der) => der,
                    None => {
                        let path = cfg.ca_cert_file.as_ref().context(
                            "the PIV slot holds no CA certificate; select the CA certificate file (PEM) in the setup screen",
                        )?;
                        let text = std::fs::read(path)
                            .with_context(|| format!("reading {}", path.display()))?;
                        let pem = pem::parse(&text).map_err(|e| {
                            anyhow!("{} is not a PEM certificate: {e}", path.display())
                        })?;
                        pem.into_contents()
                    }
                };
                (
                    Ca::new(ca_der, Box::new(yubikey_ca::YubiCa::new(yk, slot)))?,
                    format!("S/N {} (firmware {})", yk_info.serial, yk_info.version),
                )
            }
        };

        // Prove the slot key matches the CA certificate before touching any device.
        let probe_key = DeviceKey::generate()?;
        cert::issue(&profile.identity, "SELFTEST", &probe_key, &ca)
            .context("CA self-test failed: the CA key does not match the CA certificate")?;

        log_csv::init(&cfg.log)?;
        let probe = flash::find_probe()
            .map(|p| p.identifier)
            .unwrap_or_else(|e| format!("not found ({e})"));

        let info = SessionInfo {
            project: profile.project.name.clone(),
            variant: variant.name.clone(),
            device_type: variant.device_type,
            bootloader: artifacts.bootloader.name.clone(),
            firmware: artifacts.firmwares[cfg.variant].name.clone(),
            ca_subject: ca.subject(),
            yubikey,
            dev_ca: cfg.dev_ca.is_some(),
            probe,
            log: cfg.log.clone(),
            uid_min: profile.identity.uid_min,
            uid_max: profile.identity.uid_max,
            exit_note: profile.session.exit_note.clone(),
        };
        self.session = Some(Session {
            artifacts,
            variant: cfg.variant,
            log: cfg.log.clone(),
            ca,
        });
        Ok(info)
    }

    /// Run one step, letting the operator retry it on failure.
    fn step<T>(&self, index: usize, mut f: impl FnMut() -> Result<T>) -> Result<T, Skipped> {
        loop {
            self.send(Event::Step {
                index,
                state: StepState::Running,
            });
            match f() {
                Ok(v) => {
                    self.send(Event::Step {
                        index,
                        state: StepState::Done,
                    });
                    return Ok(v);
                }
                Err(e) => {
                    self.send(Event::Step {
                        index,
                        state: StepState::Failed(format!("{e:#}")),
                    });
                    loop {
                        match self.commands.recv() {
                            Ok(Command::Retry) => break,
                            Ok(Command::Skip) | Err(_) => return Err(Skipped),
                            Ok(_) => {}
                        }
                    }
                }
            }
        }
    }

    fn provision(&mut self, uid: &str, icc: &str, reprovision: bool) {
        for index in 0..STEPS.len() {
            self.send(Event::Step {
                index,
                state: StepState::Pending,
            });
        }
        let Some(session) = self.session.as_ref() else {
            self.send(Event::SessionFailed("no active session".into()));
            return;
        };
        let profile = &session.artifacts.profile;
        let variant = &profile.variants[session.variant];
        let target = &profile.target;

        let ptx = self.events.clone();
        let pctx = self.ctx.clone();
        let progress = |index: usize, fraction: f32| {
            let _ = ptx.send(Event::StepProgress { index, fraction });
            pctx.request_repaint();
        };

        let mut rtt_log = String::new();
        let mut last_error = None;
        let outcome = (|| -> Result<Outcome, Skipped> {
            let (issued, key_pem) = self.step(0, || {
                let key = DeviceKey::generate()?;
                let issued = cert::issue(&profile.identity, uid, &key, &session.ca)?;
                Ok((issued, key.sec1_pem()?))
            })?;
            let config = self.step(1, || {
                config_bin::build(
                    uid,
                    variant.device_type,
                    issued.pem.as_bytes(),
                    key_pem.as_bytes(),
                )
            })?;
            let mut probe = self.step(2, || flash::attach(&target.chip, target.swd_speed_khz))?;
            self.step(3, || {
                flash::flash_elf(&mut probe, &session.artifacts.bootloader.path, |f| {
                    progress(3, f)
                })
            })?;
            self.step(4, || {
                flash::flash_elf(
                    &mut probe,
                    &session.artifacts.firmwares[session.variant].path,
                    |f| progress(4, f),
                )
            })?;
            self.step(5, || {
                flash::write_region(
                    &mut probe,
                    target.config_flash_start,
                    target.config_flash_end,
                    &config,
                    |f| progress(5, f),
                )
            })?;
            self.step(STEP_LOG, || {
                log_csv::append(
                    &session.log,
                    &log_csv::Row {
                        uid,
                        icc,
                        cert_serial: &issued.serial_hex,
                        cert_sha256: &issued.sha256_hex,
                    },
                )
            })?;
            // From here the device is provisioned; the boot check is advisory.
            self.send(Event::Step {
                index: STEP_RTT,
                state: StepState::Running,
            });
            let tx = self.events.clone();
            let ctx = self.ctx.clone();
            let check = flash::reset(&mut probe).and_then(|()| {
                rtt::wait_for_uid(
                    &mut probe,
                    target.rtt_address,
                    &target.boot_banner,
                    uid,
                    Duration::from_secs(target.rtt_timeout_s),
                    |log| {
                        let _ = tx.send(Event::Rtt(log.to_owned()));
                        ctx.request_repaint();
                    },
                )
            });
            drop(probe);
            match check {
                Ok(cap) if cap.matched => {
                    rtt_log = cap.log;
                    self.send(Event::Step {
                        index: STEP_RTT,
                        state: StepState::Done,
                    });
                    Ok(Outcome::Ok)
                }
                Ok(cap) => {
                    rtt_log = cap.log;
                    let msg = format!(
                        "UID not seen in the boot log within {} s",
                        target.rtt_timeout_s
                    );
                    self.send(Event::Step {
                        index: STEP_RTT,
                        state: StepState::Failed(msg.clone()),
                    });
                    last_error = Some(msg);
                    Ok(Outcome::Unverified)
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    self.send(Event::Step {
                        index: STEP_RTT,
                        state: StepState::Failed(msg.clone()),
                    });
                    last_error = Some(msg);
                    Ok(Outcome::Unverified)
                }
            }
        })()
        .unwrap_or_else(|Skipped| {
            last_error = Some("skipped by operator".into());
            Outcome::Fail
        });

        self.send(Event::DeviceFinished {
            uid: uid.to_owned(),
            outcome,
            reprovision,
            rtt_log,
            error: last_error,
        });
    }
}
