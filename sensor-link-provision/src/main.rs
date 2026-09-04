#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod artifacts;
mod cert;
mod config_bin;
mod flash;
mod log_csv;
mod profile;
mod rtt;
mod sound;
mod validate;
mod worker;
mod yubikey_ca;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--selftest-sign") => selftest_sign(&args[1..]),
        Some("--flash-test") => flash_test(&args[1..]),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("--dev-ca-key" | "--dev-ca-cert") => {
            let key = arg(&args, "--dev-ca-key").context("--dev-ca-key PEM is required")?;
            let cert = arg(&args, "--dev-ca-cert").context("--dev-ca-cert PEM is required")?;
            run_gui(Some(worker::DevCa {
                key: key.into(),
                cert: cert.into(),
            }))
        }
        Some(other) => bail!("unknown option {other}\n{USAGE}"),
        None => run_gui(None),
    }
}

const USAGE: &str = "sensor-link-provision [--dev-ca-key KEY.pem --dev-ca-cert CA.pem]
sensor-link-provision --selftest-sign --zip Z [--ca-cert PEM]
sensor-link-provision --flash-test --zip Z [--variant N] [--uid UID]
Without options the GUI starts with the CA on a YubiKey. --dev-ca-key/--dev-ca-cert start the GUI
with a development CA from files instead (no YubiKey; shown prominently in the GUI).
The subcommands exercise the hardware paths from a terminal:
  --selftest-sign   sign a test certificate with the YubiKey CA (asks for the PIN) and verify it
  --flash-test      flash bootloader + firmware + a test config to a connected board and watch RTT";

fn app_icon() -> Option<eframe::egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../assets/jitter-icon.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = img.dimensions();
    Some(eframe::egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

fn run_gui(dev_ca: Option<worker::DevCa>) -> Result<()> {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([960.0, 720.0])
        .with_title("sensor-link provisioning");
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "sensor-link-provision",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, dev_ca)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI: {e}"))
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn selftest_sign(args: &[String]) -> Result<()> {
    let zip = PathBuf::from(arg(args, "--zip").context("--zip is required")?);
    let artifacts = artifacts::Artifacts::load(&zip)?;
    let identity = &artifacts.profile.identity;
    let slot = yubikey_ca::slot_from_byte(identity.piv_slot()?)?;
    let mut yk = yubikey_ca::open()?;
    let info = yubikey_ca::info(&yk);
    println!("YubiKey S/N {} firmware {}", info.serial, info.version);
    let pin = rpassword_prompt("YubiKey PIV PIN: ")?;
    yubikey_ca::verify_pin(&mut yk, &pin)?;
    let ca_der = match yubikey_ca::read_ca_cert(&mut yk, slot)? {
        Some(der) => {
            println!("CA certificate read from slot {}", identity.ca_piv_slot);
            der
        }
        None => {
            let path =
                arg(args, "--ca-cert").context("slot holds no certificate; pass --ca-cert PEM")?;
            pem::parse(std::fs::read(&path)?)
                .map_err(|e| anyhow::anyhow!("{path}: {e}"))?
                .into_contents()
        }
    };
    let ca = cert::Ca::new(ca_der, Box::new(yubikey_ca::YubiCa::new(yk, slot)))?;
    println!("CA subject: {}", ca.subject());
    let key = cert::DeviceKey::generate()?;
    let issued = cert::issue(identity, "SELFTEST1", &key, &ca)?;
    println!(
        "Issued and verified. serial={} sha256={} pem={} bytes",
        issued.serial_hex,
        issued.sha256_hex,
        issued.pem.len()
    );
    print!("{}", issued.pem);
    Ok(())
}

fn flash_test(args: &[String]) -> Result<()> {
    let zip = PathBuf::from(arg(args, "--zip").context("--zip is required")?);
    let variant: usize = arg(args, "--variant")
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(0);
    let uid = arg(args, "--uid").unwrap_or_else(|| "FLASHTEST".into());
    let artifacts = artifacts::Artifacts::load(&zip)?;
    let profile = &artifacts.profile;
    let v = profile
        .variants
        .get(variant)
        .context("variant index out of range")?;
    println!("Probes: {:?}", flash::list_probes());

    // Throwaway CA and device identity: enough to exercise flash + boot check.
    let ca_key = cert::DeviceKey::generate()?;
    let ca_signing =
        p256::ecdsa::SigningKey::from(&p256::SecretKey::from_sec1_pem(&ca_key.sec1_pem()?)?);
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "flash-test CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_cert = params.self_signed(&SelfSigner(
        ca_signing.clone(),
        rcgen::SubjectPublicKeyInfo::from_der(&ca_key.spki_der()?)?,
    ))?;
    let ca = cert::Ca::new(
        ca_cert.der().to_vec(),
        Box::new(cert::SoftwareSigner(ca_signing)),
    )?;
    let key = cert::DeviceKey::generate()?;
    let issued = cert::issue(&profile.identity, &uid, &key, &ca)?;
    let config = config_bin::build(
        &uid,
        v.device_type,
        issued.pem.as_bytes(),
        key.sec1_pem()?.as_bytes(),
    )?;

    let t = &profile.target;
    println!("Attaching to {} at {} kHz", t.chip, t.swd_speed_khz);
    let mut session = flash::attach(&t.chip, t.swd_speed_khz)?;
    println!("Flashing {}", artifacts.bootloader.name);
    flash::flash_elf(&mut session, &artifacts.bootloader.path, |f| {
        print!("\r  {:>3.0}%", f * 100.0);
        let _ = std::io::Write::flush(&mut std::io::stdout());
    })?;
    println!();
    println!("Flashing {}", artifacts.firmwares[variant].name);
    flash::flash_elf(&mut session, &artifacts.firmwares[variant].path, |f| {
        print!("\r  {:>3.0}%", f * 100.0);
        let _ = std::io::Write::flush(&mut std::io::stdout());
    })?;
    println!();
    println!(
        "Writing config ({} bytes) at {:#x}",
        config.len(),
        t.config_flash_start
    );
    flash::write_region(
        &mut session,
        t.config_flash_start,
        t.config_flash_end,
        &config,
        |_| {},
    )?;
    println!(
        "Reset + RTT at {:#x}, waiting {} s for '{uid}'",
        t.rtt_address, t.rtt_timeout_s
    );
    flash::reset(&mut session)?;
    let cap = rtt::wait_for_uid(
        &mut session,
        t.rtt_address,
        &t.boot_banner,
        &uid,
        Duration::from_secs(t.rtt_timeout_s),
        |_log| {},
    )?;
    println!(
        "--- RTT ---\n{}\n--- {} ---",
        cap.log,
        if cap.matched {
            "UID SEEN"
        } else {
            "UID NOT SEEN"
        }
    );
    Ok(())
}

struct SelfSigner(p256::ecdsa::SigningKey, rcgen::SubjectPublicKeyInfo);
impl rcgen::PublicKeyData for SelfSigner {
    fn der_bytes(&self) -> &[u8] {
        self.1.der_bytes()
    }
    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        self.1.algorithm()
    }
}
impl rcgen::SigningKey for SelfSigner {
    fn sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, rcgen::Error> {
        cert::CaSign::sign_der(&cert::SoftwareSigner(self.0.clone()), msg)
            .map_err(|_| rcgen::Error::RemoteKeyError)
    }
}

fn rpassword_prompt(prompt: &str) -> Result<String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_owned())
}
