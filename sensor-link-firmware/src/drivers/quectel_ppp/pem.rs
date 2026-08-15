//! Minimal no_std PEM→DER decoding for the provisioned credentials.
//!
//! The flash config and the compiled-in CA hold PEM text (see
//! `crate::utils::x509`); embedded-tls wants raw DER. Only what the driver
//! needs is supported: a single block per input, labels for certificates and
//! EC/PKCS#8 private keys.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Largest DER object we ever decode (cert ≤ 1024 B PEM in flash → DER is
/// smaller; CA certs are of the same order).
pub const MAX_DER_LEN: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PemError {
    /// No `-----BEGIN …-----` / `-----END …-----` pair found.
    MissingArmor,
    /// The armor label is not one the driver understands.
    UnknownLabel,
    /// Invalid base64 between the armor lines.
    Base64,
    /// Decoded DER does not fit the output buffer.
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PemLabel {
    Certificate,
    /// `EC PRIVATE KEY` — SEC1 ECPrivateKey.
    Sec1Key,
    /// `PRIVATE KEY` — PKCS#8 PrivateKeyInfo.
    Pkcs8Key,
}

/// Decodes the first PEM block in `input` into `out`, returning the label and
/// the DER length.
pub fn decode(input: &[u8], out: &mut [u8]) -> Result<(PemLabel, usize), PemError> {
    let text = core::str::from_utf8(input).map_err(|_| PemError::MissingArmor)?;

    let begin = text.find("-----BEGIN ").ok_or(PemError::MissingArmor)?;
    let after_begin = &text[begin + "-----BEGIN ".len()..];
    let label_end = after_begin.find("-----").ok_or(PemError::MissingArmor)?;
    let label = match &after_begin[..label_end] {
        "CERTIFICATE" => PemLabel::Certificate,
        "EC PRIVATE KEY" => PemLabel::Sec1Key,
        "PRIVATE KEY" => PemLabel::Pkcs8Key,
        _ => return Err(PemError::UnknownLabel),
    };

    let body = &after_begin[label_end + "-----".len()..];
    let end = body.find("-----END ").ok_or(PemError::MissingArmor)?;
    let body = &body[..end];

    // Strip whitespace/newlines into a bounded scratch, then decode.
    let mut compact = [0u8; MAX_DER_LEN.div_ceil(3) * 4 + 4];
    let mut n = 0;
    for b in body.bytes() {
        if !b.is_ascii_whitespace() {
            if n == compact.len() {
                return Err(PemError::TooLarge);
            }
            compact[n] = b;
            n += 1;
        }
    }

    let len = STANDARD
        .decode_slice(&compact[..n], out)
        .map_err(|e| match e {
            base64::DecodeSliceError::OutputSliceTooSmall => PemError::TooLarge,
            base64::DecodeSliceError::DecodeError(_) => PemError::Base64,
        })?;
    Ok((label, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Throwaway self-signed P-256 certificate + keys (openssl, no real system).
    const CERT: &str = "-----BEGIN CERTIFICATE-----\n\
MIIBeDCCAR+gAwIBAgIUM6V9/zaAyCmkKX+bJ5B36labTm0wCgYIKoZIzj0EAwIw\n\
EjEQMA4GA1UEAwwHdGVzdC1jYTAeFw0yNjA4MTUxMjE4NTFaFw0yNjA5MTQxMjE4\n\
NTFaMBIxEDAOBgNVBAMMB3Rlc3QtY2EwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNC\n\
AASkRIl0N9W4HCSjlUu3rAkdJ98Sp/1yWjZBkziOvu7iXm3wKLpeCh7pNxbv4tCt\n\
p+Yo9WS1W3qWnyGgr3wlOCbAo1MwUTAdBgNVHQ4EFgQUuEEl9LnVMamMi4sMIJqR\n\
I9IgRVUwHwYDVR0jBBgwFoAUuEEl9LnVMamMi4sMIJqRI9IgRVUwDwYDVR0TAQH/\n\
BAUwAwEB/zAKBggqhkjOPQQDAgNHADBEAiAqaXq80FNFTSWg0/xMMcBWb+sAfzD4\n\
QOesM//rt6AFzgIgRr86OOaLSSqijSDWzxpoyDuYCsImGcFLHjhibMdj7uw=\n\
-----END CERTIFICATE-----\n";

    #[test]
    fn decodes_certificate() {
        let mut out = [0u8; MAX_DER_LEN];
        let (label, len) = decode(CERT.as_bytes(), &mut out).unwrap();
        assert_eq!(label, PemLabel::Certificate);
        // DER SEQUENCE header.
        assert_eq!(out[0], 0x30);
        assert!(len > 300);
    }

    #[test]
    fn detects_key_labels() {
        let sec1 = "-----BEGIN EC PRIVATE KEY-----\nMAA=\n-----END EC PRIVATE KEY-----\n";
        let mut out = [0u8; 16];
        let (label, len) = decode(sec1.as_bytes(), &mut out).unwrap();
        assert_eq!((label, len), (PemLabel::Sec1Key, 2));

        let pkcs8 = "-----BEGIN PRIVATE KEY-----\nMAA=\n-----END PRIVATE KEY-----\n";
        let (label, _) = decode(pkcs8.as_bytes(), &mut out).unwrap();
        assert_eq!(label, PemLabel::Pkcs8Key);
    }

    #[test]
    fn rejects_garbage() {
        let mut out = [0u8; MAX_DER_LEN];
        assert_eq!(decode(b"hello", &mut out), Err(PemError::MissingArmor));
        assert_eq!(
            decode(
                b"-----BEGIN SOMETHING-----\nMAA=\n-----END SOMETHING-----",
                &mut out
            ),
            Err(PemError::UnknownLabel)
        );
        assert_eq!(
            decode(
                b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----",
                &mut out
            ),
            Err(PemError::Base64)
        );
    }

    #[test]
    fn output_too_small() {
        let mut out = [0u8; 4];
        assert_eq!(decode(CERT.as_bytes(), &mut out), Err(PemError::TooLarge));
    }
}
