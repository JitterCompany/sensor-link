//! Device identity: a fresh ECDSA P-256 key pair plus an X.509 client
//! certificate signed by the project CA. The signing primitive is abstracted
//! so the CA key can live on a YubiKey (production) or in memory (tests).

use anyhow::{Context, Result, anyhow, bail};
use p256::{
    ecdsa::{Signature, VerifyingKey, signature::Verifier},
    pkcs8::{EncodePublicKey, LineEnding},
};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyUsagePurpose, PublicKeyData, SerialNumber, SignatureAlgorithm, SigningKey,
    SubjectPublicKeyInfo,
};
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::profile::Identity;

/// Signs `msg` (the raw TBS DER) with the CA key; returns a DER ECDSA signature.
pub trait CaSign: Send {
    fn sign_der(&self, msg: &[u8]) -> Result<Vec<u8>>;
}

/// A CA: its certificate and a way to sign with its key.
pub struct Ca {
    pub cert_der: Vec<u8>,
    spki: SubjectPublicKeyInfo,
    signer: Box<dyn CaSign>,
    /// rcgen's error type cannot carry a message; the signer's error is kept here.
    last_error: std::sync::Mutex<Option<String>>,
}

impl Ca {
    pub fn new(cert_der: Vec<u8>, signer: Box<dyn CaSign>) -> Result<Self> {
        let (_, x509) = X509Certificate::from_der(&cert_der).context("parsing CA certificate")?;
        let spki = SubjectPublicKeyInfo::from_der(x509.public_key().raw)
            .map_err(|e| anyhow!("CA public key: {e}"))?;
        Ok(Self {
            cert_der,
            spki,
            signer,
            last_error: std::sync::Mutex::new(None),
        })
    }

    pub fn subject(&self) -> String {
        X509Certificate::from_der(&self.cert_der)
            .map(|(_, c)| c.subject().to_string())
            .unwrap_or_default()
    }
}

impl PublicKeyData for Ca {
    fn der_bytes(&self) -> &[u8] {
        self.spki.der_bytes()
    }
    fn algorithm(&self) -> &'static SignatureAlgorithm {
        self.spki.algorithm()
    }
}

impl SigningKey for Ca {
    fn sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, rcgen::Error> {
        self.signer.sign_der(msg).map_err(|e| {
            *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{e:#}"));
            rcgen::Error::RemoteKeyError
        })
    }
}

/// Fresh device key pair. The secret only ever lives in memory.
pub struct DeviceKey {
    secret: p256::SecretKey,
}

impl DeviceKey {
    pub fn generate() -> Result<Self> {
        for _ in 0..8 {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|e| anyhow!("random: {e}"))?;
            if let Ok(secret) = p256::SecretKey::from_slice(&bytes) {
                return Ok(Self { secret });
            }
        }
        bail!("could not generate a P-256 key")
    }

    /// SEC1 `EC PRIVATE KEY` PEM, the form the firmware parses.
    pub fn sec1_pem(&self) -> Result<String> {
        Ok(self
            .secret
            .to_sec1_pem(LineEnding::LF)
            .map_err(|e| anyhow!("key PEM: {e}"))?
            .to_string())
    }

    pub fn spki_der(&self) -> Result<Vec<u8>> {
        Ok(self
            .secret
            .public_key()
            .to_public_key_der()
            .map_err(|e| anyhow!("public key DER: {e}"))?
            .into_vec())
    }
}

pub struct IssuedCert {
    pub pem: String,
    /// Uppercase hex, as `openssl x509 -serial` prints it.
    pub serial_hex: String,
    /// SHA-256 over the DER, lowercase hex.
    pub sha256_hex: String,
}

/// Issue a client certificate for `uid` (the CN) signed by `ca`.
/// Extensions match the project's openssl `v3_client` profile.
pub fn issue(identity: &Identity, uid: &str, key: &DeviceKey, ca: &Ca) -> Result<IssuedCert> {
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| anyhow!("{e}"))?;
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::OrganizationalUnitName,
        identity.cert_subject.ou.as_str(),
    );
    dn.push(DnType::OrganizationName, identity.cert_subject.o.as_str());
    dn.push(DnType::CountryName, identity.cert_subject.c.as_str());
    dn.push(DnType::CommonName, uid);
    params.distinguished_name = dn;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::ContentCommitment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.use_authority_key_identifier_extension = true;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::days(i64::from(identity.cert_validity_days));
    let mut serial = [0u8; 16];
    getrandom::fill(&mut serial).map_err(|e| anyhow!("random: {e}"))?;
    params.serial_number = Some(SerialNumber::from_slice(&serial));

    let spki_der = key.spki_der()?;
    let device_pub =
        SubjectPublicKeyInfo::from_der(&spki_der).map_err(|e| anyhow!("device public key: {e}"))?;
    let issuer = Issuer::from_ca_cert_der(&ca.cert_der.clone().into(), ca)
        .map_err(|e| anyhow!("CA certificate: {e}"))?;
    let cert = params.signed_by(&device_pub, &issuer).map_err(|e| {
        let detail = ca
            .last_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        match detail {
            Some(d) => anyhow!("signing certificate: {d}"),
            None => anyhow!("signing certificate: {e}"),
        }
    })?;

    let der = cert.der().to_vec();
    verify_chain(&der, &ca.cert_der)?;
    let serial_hex = hex::encode_upper(serial).trim_start_matches('0').to_owned();
    Ok(IssuedCert {
        pem: cert.pem(),
        sha256_hex: hex::encode(Sha256::digest(&der)),
        serial_hex: if serial_hex.is_empty() {
            "0".into()
        } else {
            serial_hex
        },
    })
}

/// Check that `cert_der` is signed by the key in `ca_der` (ECDSA P-256 / SHA-256),
/// the equivalent of `openssl verify -CAfile ca.pem cert.pem`.
pub fn verify_chain(cert_der: &[u8], ca_der: &[u8]) -> Result<()> {
    let (_, cert) = X509Certificate::from_der(cert_der).context("parsing issued certificate")?;
    let (_, ca) = X509Certificate::from_der(ca_der).context("parsing CA certificate")?;
    if cert.issuer() != ca.subject() {
        bail!(
            "certificate issuer '{}' does not match CA subject '{}'",
            cert.issuer(),
            ca.subject()
        );
    }
    let ca_key = VerifyingKey::from_sec1_bytes(&ca.public_key().subject_public_key.data)
        .map_err(|e| anyhow!("CA public key is not P-256: {e}"))?;
    let sig = Signature::from_der(&cert.signature_value.data)
        .map_err(|e| anyhow!("signature encoding: {e}"))?;
    ca_key
        .verify(cert.tbs_certificate.as_ref(), &sig)
        .map_err(|_| anyhow!("certificate signature does not verify against the CA"))?;
    Ok(())
}

/// In-memory CA signer (tests, development CA).
pub struct SoftwareSigner(pub p256::ecdsa::SigningKey);

/// A CA whose private key is a PEM file on disk. Development only: the key
/// is copyable, so production signing stays on the YubiKey.
pub fn load_dev_ca(key_path: &std::path::Path, cert_path: &std::path::Path) -> Result<Ca> {
    use p256::pkcs8::DecodePrivateKey;
    let key_text = std::fs::read_to_string(key_path)
        .with_context(|| format!("reading {}", key_path.display()))?;
    let secret = p256::SecretKey::from_sec1_pem(&key_text)
        .or_else(|_| p256::SecretKey::from_pkcs8_pem(&key_text))
        .map_err(|e| anyhow!("{} is not a P-256 private key PEM: {e}", key_path.display()))?;
    let cert_text =
        std::fs::read(cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
    let cert_der = pem::parse(&cert_text)
        .map_err(|e| anyhow!("{} is not a PEM certificate: {e}", cert_path.display()))?
        .into_contents();
    Ca::new(
        cert_der,
        Box::new(SoftwareSigner(p256::ecdsa::SigningKey::from(&secret))),
    )
}

impl CaSign for SoftwareSigner {
    fn sign_der(&self, msg: &[u8]) -> Result<Vec<u8>> {
        use p256::ecdsa::signature::Signer;
        let sig: Signature = self.0.sign(msg);
        Ok(sig.to_der().as_bytes().to_vec())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::profile::{EXAMPLE_PROFILE, Profile};

    /// Self-signed P-256 CA built with rcgen through the same `Ca` path.
    pub(crate) fn test_ca() -> Ca {
        let key = DeviceKey::generate().unwrap();
        let signing = p256::ecdsa::SigningKey::from(&key.secret);
        let spki = SubjectPublicKeyInfo::from_der(&key.spki_der().unwrap()).unwrap();
        struct Tmp(SoftwareSigner, SubjectPublicKeyInfo);
        impl PublicKeyData for Tmp {
            fn der_bytes(&self) -> &[u8] {
                self.1.der_bytes()
            }
            fn algorithm(&self) -> &'static SignatureAlgorithm {
                self.1.algorithm()
            }
        }
        impl SigningKey for Tmp {
            fn sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, rcgen::Error> {
                self.0.sign_der(msg).map_err(|e| {
                    log::error!("CA signing: {e:#}");
                    rcgen::Error::RemoteKeyError
                })
            }
        }
        let tmp = Tmp(SoftwareSigner(signing.clone()), spki);
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::OrganizationName, "Test");
        dn.push(DnType::CommonName, "test_ca");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let cert = params.self_signed(&tmp).unwrap();
        Ca::new(cert.der().to_vec(), Box::new(SoftwareSigner(signing))).unwrap()
    }

    #[test]
    fn issues_verifiable_client_cert() {
        let profile = Profile::parse(EXAMPLE_PROFILE).unwrap();
        let ca = test_ca();
        let key = DeviceKey::generate().unwrap();
        let issued = issue(&profile.identity, "ABC123DEF", &key, &ca).unwrap();

        let der = pem::parse(&issued.pem).unwrap().into_contents();
        let (_, x) = X509Certificate::from_der(&der).unwrap();
        assert_eq!(
            x.subject().to_string(),
            "OU=Devices, O=BTB Energy, C=NL, CN=ABC123DEF"
        );
        assert_eq!(x.issuer().to_string(), "O=Test, CN=test_ca");
        let bc = x.basic_constraints().unwrap().unwrap();
        assert!(!bc.value.ca);
        let ku = x.key_usage().unwrap().unwrap().value;
        assert!(ku.digital_signature() && ku.non_repudiation() && !ku.key_encipherment());
        let eku = x.extended_key_usage().unwrap().unwrap().value;
        assert!(eku.client_auth && !eku.server_auth);
        let aki = x
            .get_extension_unique(&x509_parser::oid_registry::OID_X509_EXT_AUTHORITY_KEY_IDENTIFIER)
            .unwrap();
        assert!(aki.is_some(), "authorityKeyIdentifier missing");
        let days =
            (x.validity().not_after.timestamp() - x.validity().not_before.timestamp()) / 86400;
        assert_eq!(days, 9650);
        assert_eq!(
            issued.serial_hex,
            x.raw_serial_as_string()
                .replace(':', "")
                .to_uppercase()
                .trim_start_matches('0')
        );

        assert!(
            issued.pem.len() <= 1024,
            "PEM is {} bytes",
            issued.pem.len()
        );
        let key_pem = key.sec1_pem().unwrap();
        assert!(key_pem.starts_with("-----BEGIN EC PRIVATE KEY-----"));
        assert!(key_pem.len() <= 512);
        // The config image accepts it.
        crate::config_bin::build("ABC123DEF", 1, issued.pem.as_bytes(), key_pem.as_bytes())
            .unwrap();
    }

    #[test]
    fn dev_ca_from_files() {
        let profile = Profile::parse(EXAMPLE_PROFILE).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let key = DeviceKey::generate().unwrap();
        let signing = p256::ecdsa::SigningKey::from(&key.secret);
        let ca = {
            // Self-signed CA cert for that key, written out as files.
            let spki = SubjectPublicKeyInfo::from_der(&key.spki_der().unwrap()).unwrap();
            let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
            params.distinguished_name.push(DnType::CommonName, "dev_ca");
            params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let ca = Ca::new(vec![0x30, 0x00], Box::new(SoftwareSigner(signing.clone())));
            assert!(ca.is_err(), "garbage DER must be rejected");
            struct Tmp(SoftwareSigner, SubjectPublicKeyInfo);
            impl PublicKeyData for Tmp {
                fn der_bytes(&self) -> &[u8] {
                    self.1.der_bytes()
                }
                fn algorithm(&self) -> &'static SignatureAlgorithm {
                    self.1.algorithm()
                }
            }
            impl SigningKey for Tmp {
                fn sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, rcgen::Error> {
                    self.0
                        .sign_der(msg)
                        .map_err(|_| rcgen::Error::RemoteKeyError)
                }
            }
            params
                .self_signed(&Tmp(SoftwareSigner(signing), spki))
                .unwrap()
        };
        let key_path = dir.path().join("ca.key");
        let cert_path = dir.path().join("ca.pem");
        std::fs::write(&key_path, key.sec1_pem().unwrap()).unwrap();
        std::fs::write(&cert_path, ca.pem()).unwrap();

        let dev = load_dev_ca(&key_path, &cert_path).unwrap();
        let device = DeviceKey::generate().unwrap();
        let issued = issue(&profile.identity, "DEV000001", &device, &dev).unwrap();
        let der = pem::parse(&issued.pem).unwrap().into_contents();
        verify_chain(&der, ca.der()).unwrap();
        assert!(load_dev_ca(&cert_path, &cert_path).is_err());
    }

    #[test]
    fn rejects_wrong_ca() {
        let profile = Profile::parse(EXAMPLE_PROFILE).unwrap();
        let ca = test_ca();
        let other = test_ca();
        let key = DeviceKey::generate().unwrap();
        let issued = issue(&profile.identity, "ABC123DEF", &key, &ca).unwrap();
        let der = pem::parse(&issued.pem).unwrap().into_contents();
        assert!(verify_chain(&der, &other.cert_der).is_err());
        assert!(verify_chain(&der, &ca.cert_der).is_ok());
    }
}
