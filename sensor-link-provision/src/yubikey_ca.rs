//! The CA key on a YubiKey PIV retired slot: PIN handling, CA certificate
//! stored in the same slot, and ECDSA signing for certificate issuance.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use yubikey::{
    YubiKey,
    piv::{AlgorithmId, RetiredSlotId, SlotId, sign_data},
};

use crate::cert::CaSign;

pub struct YubiCa {
    yk: Arc<Mutex<YubiKey>>,
    slot: SlotId,
}

pub struct YubiInfo {
    pub serial: u32,
    pub version: String,
}

/// Opens the (single) connected YubiKey.
pub fn open() -> Result<YubiKey> {
    YubiKey::open().map_err(|e| anyhow!("YubiKey not found ({e}); insert exactly one YubiKey"))
}

pub fn info(yk: &YubiKey) -> YubiInfo {
    YubiInfo {
        serial: yk.serial().0,
        version: yk.version().to_string(),
    }
}

/// Verifies the PIV PIN once. A wrong PIN decrements the retry counter
/// (3 strikes lock the PIV applet until unblocked with the PUK), so the
/// error carries the remaining tries for the operator.
pub fn verify_pin(yk: &mut YubiKey, pin: &str) -> Result<()> {
    if pin.is_empty() {
        bail!("PIN cannot be empty");
    }
    match yk.verify_pin(pin.as_bytes()) {
        Ok(()) => Ok(()),
        Err(yubikey::Error::WrongPin { tries }) => {
            bail!("wrong PIN, {tries} attempt(s) left before the PIV applet locks")
        }
        Err(yubikey::Error::PinLocked) => {
            bail!("PIN is locked; unblock it with the PUK (ykman piv access unblock-pin)")
        }
        Err(e) => Err(anyhow!("PIN verification failed: {e}")),
    }
}

pub fn slot_from_byte(byte: u8) -> Result<SlotId> {
    let retired = RetiredSlotId::try_from(byte)
        .map_err(|_| anyhow!("0x{byte:02x} is not a retired PIV slot"))?;
    Ok(SlotId::Retired(retired))
}

/// The CA certificate stored in `slot`, DER encoded; `None` when the slot has none.
pub fn read_ca_cert(yk: &mut YubiKey, slot: SlotId) -> Result<Option<Vec<u8>>> {
    use x509_cert::der::Encode;
    match yubikey::certificate::Certificate::read(yk, slot) {
        Ok(cert) => Ok(Some(cert.cert.to_der().context("encoding CA certificate")?)),
        Err(yubikey::Error::InvalidObject | yubikey::Error::NotFound) => Ok(None),
        Err(e) => Err(anyhow!("reading certificate from slot: {e}")),
    }
}

impl YubiCa {
    pub fn new(yk: YubiKey, slot: SlotId) -> Self {
        Self {
            yk: Arc::new(Mutex::new(yk)),
            slot,
        }
    }
}

impl CaSign for YubiCa {
    /// PIV ECC signing takes the pre-hashed digest and returns a DER
    /// `ECDSA-Sig-Value`, exactly what rcgen expects for ecdsa-with-SHA256.
    fn sign_der(&self, msg: &[u8]) -> Result<Vec<u8>> {
        let digest = Sha256::digest(msg);
        let mut yk = self
            .yk
            .lock()
            .map_err(|_| anyhow!("YubiKey lock poisoned"))?;
        let sig = sign_data(&mut yk, &digest, AlgorithmId::EccP256, self.slot).map_err(|e| {
            anyhow!("YubiKey signing failed: {e} (touch the key if it blinks; PIN verified?)")
        })?;
        Ok(sig.to_vec())
    }
}
