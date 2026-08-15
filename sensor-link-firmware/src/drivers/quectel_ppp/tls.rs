//! TLS material and crypto provider for the PPP driver.
//!
//! Mutual TLS 1.3 with embedded-tls: the provisioned PEM credentials are
//! decoded to DER once at driver construction, the server certificate is
//! verified against the pinned project CA (rustpki), and the client
//! authenticates with its P-256 key — which never leaves the MCU.

use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, SignatureScheme, TlsClock,
    TlsError, TlsVerifier,
};
use p256::ecdsa::signature::SignerMut;
use p256::ecdsa::SigningKey;
use p256::SecretKey;

use super::pem::{self, PemError, PemLabel, MAX_DER_LEN};
use crate::drivers::time;

/// Incoming TLS records can be up to 2^14 + overhead; the broker will not
/// honor a smaller fragment length, so the full size is required.
pub const TLS_RX_BUF_LEN: usize = 16_640;
/// Outgoing records: writes are streamed, so this only bounds the record
/// size, not the payload size.
pub const TLS_TX_BUF_LEN: usize = 4096;

/// Buffer inside the verifier holding the server's certificate message.
const SERVER_CERT_SIZE: usize = 2048;

pub type CipherSuite = Aes128GcmSha256;
pub type Verifier<'a> = CertVerifier<'a, CipherSuite, WallClock, SERVER_CERT_SIZE>;

/// Provisioned credentials, decoded from PEM to DER once at startup.
pub struct TlsMaterial {
    ca: [u8; MAX_DER_LEN],
    ca_len: usize,
    cert: [u8; MAX_DER_LEN],
    cert_len: usize,
    key: [u8; MAX_DER_LEN],
    key_len: usize,
    key_label: PemLabel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialError {
    Ca(PemError),
    Cert(PemError),
    Key(PemError),
    /// The key PEM did not contain a usable P-256 private key.
    BadKey,
}

impl TlsMaterial {
    pub fn from_pem(ca: &[u8], cert: &[u8], key: &[u8]) -> Result<Self, MaterialError> {
        let mut material = TlsMaterial {
            ca: [0; MAX_DER_LEN],
            ca_len: 0,
            cert: [0; MAX_DER_LEN],
            cert_len: 0,
            key: [0; MAX_DER_LEN],
            key_len: 0,
            key_label: PemLabel::Sec1Key,
        };
        let (label, len) = pem::decode(ca, &mut material.ca).map_err(MaterialError::Ca)?;
        if label != PemLabel::Certificate {
            return Err(MaterialError::Ca(PemError::UnknownLabel));
        }
        material.ca_len = len;

        let (label, len) = pem::decode(cert, &mut material.cert).map_err(MaterialError::Cert)?;
        if label != PemLabel::Certificate {
            return Err(MaterialError::Cert(PemError::UnknownLabel));
        }
        material.cert_len = len;

        let (label, len) = pem::decode(key, &mut material.key).map_err(MaterialError::Key)?;
        material.key_len = len;
        material.key_label = label;

        // Fail at construction, not at handshake time.
        material.signing_key().map_err(|_| MaterialError::BadKey)?;
        Ok(material)
    }

    pub fn ca_der(&self) -> &[u8] {
        &self.ca[..self.ca_len]
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert[..self.cert_len]
    }

    fn signing_key(&self) -> Result<SigningKey, TlsError> {
        let der = &self.key[..self.key_len];
        let secret = match self.key_label {
            PemLabel::Sec1Key => SecretKey::from_sec1_der(der).ok(),
            PemLabel::Pkcs8Key => {
                use p256::pkcs8::DecodePrivateKey;
                SecretKey::from_pkcs8_der(der).ok()
            }
            PemLabel::Certificate => None,
        }
        .ok_or(TlsError::InvalidPrivateKey)?;
        Ok(SigningKey::from(&secret))
    }
}

/// Certificate-validity clock backed by the device wall clock (RTC/CombinedTime).
///
/// Returns `None` until the wall clock has been initialized and reports a
/// plausible time, which skips the validity-window check: on a factory-fresh
/// device time only arrives (over MQTT) after the first TLS handshake.
pub struct WallClock;

/// 2020-01-01 UTC in seconds; anything earlier is an uninitialized clock.
const PLAUSIBLE_EPOCH_S: i64 = 1_577_836_800;

impl TlsClock for WallClock {
    fn now() -> Option<u64> {
        match time::timestamp_us() {
            Ok(us) if us / 1_000_000 >= PLAUSIBLE_EPOCH_S => Some((us / 1_000_000) as u64),
            _ => None,
        }
    }
}

/// [`embedded_tls::config::CryptoProvider`] wiring RNG, pinned-CA verification
/// and client-certificate signing together.
pub struct BtbCryptoProvider<'a, RNG> {
    rng: RNG,
    verifier: Verifier<'a>,
    material: &'a TlsMaterial,
}

impl<'a, RNG: CryptoRngCore> BtbCryptoProvider<'a, RNG> {
    pub fn new(material: &'a TlsMaterial, rng: RNG) -> Self {
        Self {
            rng,
            verifier: CertVerifier::new(Certificate::X509(material.ca_der())),
            material,
        }
    }
}

impl<RNG: CryptoRngCore> CryptoProvider for BtbCryptoProvider<'_, RNG> {
    type CipherSuite = CipherSuite;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(
        &mut self,
    ) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }

    fn signer(
        &mut self,
    ) -> Result<(impl SignerMut<Self::Signature>, SignatureScheme), TlsError> {
        let key = self.material.signing_key()?;
        Ok((key, SignatureScheme::EcdsaSecp256r1Sha256))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        Some(Certificate::X509(self.material.cert_der()))
    }
}
