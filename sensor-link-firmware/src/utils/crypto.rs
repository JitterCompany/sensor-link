//! Cryptography utility functions
//!
//! Simplified crypto API containing just what is needed for the firmware.
//!

use p256::{
    ecdsa::{signature::hazmat::PrehashVerifier, SigningKey, VerifyingKey},
    EncodedPoint,
};
use sha2::{Digest, Sha256};

#[cfg(feature = "alloc")]
use {
    core::str::FromStr,
    p256::{elliptic_curve, NistP256},
};

#[derive(Debug, Clone)]
/// Hasher: used to create a [Hash](struct@Hash)
pub struct Hasher {
    sha: Sha256,
}

/// Hash: verifies the integrity of data
///
/// Compare to a known-good hash (`==`) to check for errors.
/// Authenticity is not guaranteed (since everyone could have made the hash)
///
/// A hasher can be created via a [Hasher](struct@Hasher) or deserialized via [try_from_bytes()](Hash::try_from_bytes())
#[derive(Debug, Clone, PartialEq)]
pub struct Hash {
    bytes: [u8; Hash::BINARY_SIZE],
}

/// Signature: verifies the integrity + authenticity of data
///
/// Can be verified against a trusted [PubKey](struct@PubKey) to prove the data
/// has not changed since the related [PrivKey](struct@PrivKey) signed it
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    signature: p256::ecdsa::Signature,
}

/// *Public* key
#[derive(Clone)]
pub struct PubKey {
    key: VerifyingKey,
}

/// Error in the deserialization of a public key
#[derive(Debug)]
pub enum PubKeyError {
    /// Header '-----BEGIN PUBLIC KEY-----' not found
    UnexpectedFormat,

    /// Failed to parse the public key (not P256?)
    Invalid,
}

/// *Private* key
///
/// Does not derive debug or copy on purpose as this key is meant to be secret
pub struct PrivKey {
    key: SigningKey,
}

/// Error in the deserialization of a private key
#[derive(Debug)]
pub enum PrivKeyError {
    /// Header '-----BEGIN EC PRIVATE KEY-----' not found
    UnexpectedFormat,

    /// Failed to parse the private key (not P256?)
    Invalid,
}

impl PrivKey {
    /// Expects a private key in SEC1 PEM format
    ///
    /// This format can be recognized by the following header:
    /// ```text
    /// -----BEGIN EC PRIVATE KEY-----
    /// ```
    #[cfg(any(feature = "alloc", doc))]
    pub fn try_from_pem(pem_string: &str) -> Result<Self, PrivKeyError> {
        // Search for header. This allows for compatibility with keys that have other sections
        // such as EC PARAMS
        let offset = pem_string
            .find("-----BEGIN EC PRIVATE KEY-----")
            .ok_or(PrivKeyError::UnexpectedFormat)?;
        let pem_string = &pem_string[offset..];

        // elleptic_curve library can import PEM (with feature flag 'pem' enabled)
        let secret_key = elliptic_curve::SecretKey::<NistP256>::from_sec1_pem(pem_string)
            .map_err(|_| PrivKeyError::Invalid)?;

        Ok(Self {
            key: secret_key.into(),
        })
    }

    /// Sign the given data
    ///
    /// If succesfull, this creates a Signature that proves the authenticity of the data.
    /// Anyone with the matching public key can verify the validity of this Signature.
    pub fn sign(&self, data: &[u8]) -> Result<Signature, ()> {
        let (signature, _recid) = self.key.sign_recoverable(data).map_err(|_| ())?;

        Ok(Signature { signature })
    }
}

impl Signature {
    pub const BINARY_SIZE: usize = 64;

    /// Serialize into a byte array
    pub fn to_bytes(&self) -> [u8; Self::BINARY_SIZE] {
        self.signature.to_bytes().into()
    }

    /// Try to deserialize the first Self::BINARY_SIZE bytes of a slice to a signature
    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() < Self::BINARY_SIZE {
            return Err(());
        }
        let bytes = &bytes[..Self::BINARY_SIZE];
        let signature = p256::ecdsa::Signature::from_slice(bytes).map_err(|_| ())?;

        Ok(Self { signature })
    }
}

impl Hash {
    pub const BINARY_SIZE: usize = 32;

    /// Serialize to array of bytes
    pub fn to_bytes(&self) -> [u8; Self::BINARY_SIZE] {
        self.bytes.into()
    }

    /// Try to deserialize the first Self::BINARY_SIZE bytes of a slice to a hash
    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        if bytes.len() < Self::BINARY_SIZE {
            return Err(());
        }
        let bytes = &bytes[..Self::BINARY_SIZE];
        Ok(Self {
            bytes: bytes.try_into().map_err(|_| ())?,
        })
    }
}

impl Hasher {
    /// Initialize a new hasher
    pub fn new() -> Self {
        Self { sha: Sha256::new() }
    }

    /// Update the hasher state with the given data.
    ///
    /// Not all data to be hashed has to be passed in at once.
    /// This method can be called repeatedly with successive slices
    /// and will keep updating the internal hash result.
    pub fn update(&mut self, bytes_to_hash: &[u8]) {
        self.sha.update(bytes_to_hash);
    }

    /// Hash value over all data that was passed to
    /// [update()](Hasher::update()) so far.
    pub fn hash(&self) -> Hash {
        let hash = self.sha.clone();
        let bytes = hash.finalize().into();
        Hash { bytes }
    }
}

impl PubKey {
    /// Expects a private key in X.509 SPKI PEM format
    ///
    /// This format can be recognized by the following header:
    /// ```text
    /// -----BEGIN PUBLIC KEY-----
    /// ```
    #[cfg(any(feature = "alloc", doc))]
    pub fn try_from_pem(pem_string: &str) -> Result<Self, PubKeyError> {
        // Search for header. This allows for compatibility with bundled keys that have other sections
        let offset = pem_string
            .find("-----BEGIN PUBLIC KEY-----")
            .ok_or(PubKeyError::UnexpectedFormat)?;
        let pem_string = &pem_string[offset..];

        let public_key = elliptic_curve::PublicKey::<NistP256>::from_str(pem_string)
            .map_err(|_| PubKeyError::Invalid)?;
        Ok(Self {
            key: public_key.into(),
        })
    }

    pub const BINARY_SIZE: usize = 1 + 64;

    /// Expects a slice of at least Self::BINARY_SIZE bytes
    ///
    /// **NB**: this expects the key in binary format as generated by [as_bytes()](method@Self::to_bytes()).
    ///
    /// See [try_from_pem()](method@Self::try_from_pem) to import from 'standard' PEM format
    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, ()> {
        let point = EncodedPoint::from_bytes(bytes).map_err(|_| ())?;
        let key = VerifyingKey::from_encoded_point(&point).map_err(|_| ())?;

        Ok(Self { key })
    }

    /// Serialize to array of bytes
    ///
    /// The key can later be deserialized via try_from_bytes()
    pub fn to_bytes(&self) -> [u8; Self::BINARY_SIZE] {
        let sec1_point_uncompressed = self.key.to_encoded_point(false);
        let encoded_slice = sec1_point_uncompressed.as_bytes();
        assert!(encoded_slice.len() <= Self::BINARY_SIZE);

        let mut result = [0; Self::BINARY_SIZE];
        result[..encoded_slice.len()].copy_from_slice(encoded_slice);
        result
    }

    /// Verify a signature against this public key.
    ///
    /// To verify a signature over arbitrary data, perform these steps:
    /// 1. create a [Hasher](struct@Hasher)
    /// 2. use the hasher to createa a [Hash](struct@Hash) over all data that is supposed to be signed
    /// 3. use [PubKey::verify()] to verify the signature
    ///
    /// A valid signature means that the [PrivKey] matching this Pubkey has
    /// signed exactly the same data, proving its authenticity.
    pub fn verify(&self, hash: &Hash, signature: &Signature) -> Result<(), ()> {
        // This assertion is hardcoded on purpose.
        // P256 key needs exactly 32 bytes hash output to be secure
        assert!(hash.bytes.len() == 32);
        let key: &VerifyingKey = &self.key;
        key.verify_prehash(&hash.bytes, &signature.signature)
            .map_err(|_| ())
    }
}
