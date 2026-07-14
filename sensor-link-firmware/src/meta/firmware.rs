//! Provides access to metadata about the current application firmware.
//!
//! The firmware-image validator and signer are generic over an
//! [`ApplicationHeader`] implementation. A V2 header ([`v2::ApplicationHeader`])
//! is provided here; a consumer that needs a different on-wire header format
//! (e.g. one that also accepts older header revisions) implements
//! [`ApplicationHeader`] for its own type and drives the same [`Validator`].

use num_enum::TryFromPrimitive;

use crate::utils::crypto;

pub mod v2;
pub use v2::ApplicationHeader as ApplicationHeaderV2;

/// Abstracts the device-type discriminator stored in a firmware header, so the
/// header/validator stay agnostic of any concrete device-type enum.
pub trait DeviceType: Copy + Clone + core::fmt::Debug + PartialEq {
    /// Serialize to its single wire byte.
    fn to_byte(self) -> u8;
    /// Deserialize from a wire byte. `None` => unknown/invalid discriminant.
    fn try_from_byte(byte: u8) -> Option<Self>
    where
        Self: Sized;
}

/// The firmware-header operations [`Validator`] and [`sign`] depend on.
///
/// Implementing this for a concrete header type lets the same validator logic
/// drive different on-wire header formats.
pub trait ApplicationHeader: Sized + Clone {
    type DeviceType: DeviceType;
    /// Buffer able to hold exactly [`Self::BINARY_SIZE`] bytes (e.g. `[u8; BINARY_SIZE]`).
    type RawBuffer: AsRef<[u8]> + AsMut<[u8]>;

    const BINARY_SIZE: usize;
    const BINARY_SIZE_RAW: usize;
    const BINARY_SIZE_AUTH: usize;

    /// A zeroed [`Self::RawBuffer`].
    fn new_raw_buffer() -> Self::RawBuffer;

    fn try_from_bytes(bytes: &[u8]) -> Result<Self, ApplicationHeaderError>;
    fn as_bytes(&self, bytes: &mut [u8]) -> Result<(), ApplicationHeaderError>;
    fn authenticated_fields_as_bytes(&self, bytes: &mut [u8])
        -> Result<(), ApplicationHeaderError>;
    fn write_signature(&mut self, application_total_length: usize, signature: crypto::Signature);

    fn security(&self) -> &Security;
    fn device_type(&self) -> Self::DeviceType;
    fn anti_downgrade_version(&self) -> u32;

    /// Total length of the firmware image (including this header), if known.
    ///
    /// Derived from the [`Security`] level: images without integrity/signature
    /// protection ([`Security::None`]) do not record a length, so `None` is returned.
    fn length(&self) -> Option<usize> {
        match self.security() {
            Security::None => None,
            Security::IntegrityOnly(len, _) | Security::Signed(len, _) => Some(*len),
        }
    }
}

/// Largest `BINARY_SIZE_AUTH` any header implementation is expected to use.
/// Sized generously to avoid a generic const in an array length.
const MAX_AUTH_FIELD_BYTES: usize = 16;

#[derive(TryFromPrimitive, Clone, Copy, Debug, PartialEq, PartialOrd)]
#[repr(u8)]
/// Security level: how much confidence do we have about the validity of
/// the application firmware?
pub enum SecurityLevel {
    /// No Security: only suitable for local flashing (dev / R&D)
    None = 0,

    /// Integrity only (hash): no validation that this official firmware
    IntegrityOnly = 1,

    /// Signed: Authenticity + Integrity protected (proof this is official firmware)
    Signed = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Security {
    None,
    IntegrityOnly(usize, crypto::Hash),
    Signed(usize, crypto::Signature),
}

impl Security {
    pub fn level(&self) -> SecurityLevel {
        match self {
            Security::None => SecurityLevel::None,
            Security::IntegrityOnly(_, _) => SecurityLevel::IntegrityOnly,
            Security::Signed(_, _) => SecurityLevel::Signed,
        }
    }
}

/// Errors that may occur while serializing/deserializing an [ApplicationHeader]
#[derive(Debug, Clone, PartialEq)]
pub enum ApplicationHeaderError {
    TooFewBytes,
    InvalidHeader,
    InvalidSecurityLevel,
    InvalidSecurityTag,
    InvalidLength,
    InvalidDeviceType,
}

/// Possible reasons why a signature has failed. See [sign()](fn@sign)
#[derive(Debug, Clone)]
pub enum SignError {
    /// Not a valid firmware image that can be signed
    InvalidImage(ApplicationHeaderError),

    /// Signing crypto itself failed
    Signing,
}

/// Possible reasons why a firmware fails validation. See [Validator](struct@Validator)
#[derive(Debug, Clone)]
pub enum ValidationError {
    // More data expected
    Incomplete,

    // Firmware metadata invalid
    Invalid(ApplicationHeaderError),

    // Security level (see argument) is not sufficient
    Security(SecurityLevel),

    // Downgrade to this version (see argument) is not allowed
    Downgrade(u32),

    // Could not verify integrity (hash mismatch)
    Integrity,

    // Could not verify signature (signature invalid)
    Signature,
}

enum ValidatorState<H: ApplicationHeader> {
    Init(H::RawBuffer),
    NeedMoreData(H, crypto::Hasher),
    Complete(H, crypto::Hash),
    Fail(ApplicationHeaderError),
}

impl<H: ApplicationHeader> Default for ValidatorState<H> {
    fn default() -> Self {
        ValidatorState::Init(H::new_raw_buffer())
    }
}

/// Validator: used to validate a firmware image
///
/// The validator does not require the whole image to be in memory at once.
/// It is generic over the firmware header format ([`ApplicationHeader`]).
pub struct Validator<H: ApplicationHeader> {
    // Settings
    signing_key: crypto::PubKey,
    min_security: SecurityLevel,
    device_type: H::DeviceType,

    // State
    state: ValidatorState<H>,
    n_bytes: usize,
}

impl<H: ApplicationHeader> Validator<H> {
    /// Create a firmware validator instance
    pub fn new(
        signing_key: crypto::PubKey,
        min_security: SecurityLevel,
        device_type: H::DeviceType,
    ) -> Self {
        Self {
            signing_key,
            min_security,
            device_type,

            state: ValidatorState::default(),
            n_bytes: 0,
        }
    }

    /// Reset all internal state. Prepares for a new validation
    pub fn reset(&mut self) {
        *self = Self::new(
            self.signing_key.clone(),
            self.min_security,
            self.device_type,
        );
    }

    /// Update the validator by feeding in the next slice of bytes
    ///
    /// The slice of bytes should represent (part of) a firmware image.
    /// As the firmware may not fit in RAM at once, it can be passed in
    /// successive slices.
    ///
    /// Trailing data is ignored, so if the length of the firmware is not known
    /// you can just keep sending data and periodically check the validation result.
    ///
    /// See [allow_update_from()](Self::allow_update_from()) and [verify()](Self::verify())
    ///
    /// Returns true if complete (more data may be added but it will be ignored after that)
    pub fn update(&mut self, mut application_bytes: &[u8]) -> bool {
        let result = loop {
            let bytes_processed = match &mut self.state {
                // buffer bytes into slice untill it can be parsed into a ApplicationHeader
                ValidatorState::Init(buffer) => {
                    let slice = buffer.as_mut();
                    let remaining = H::BINARY_SIZE - self.n_bytes;
                    let bytes_processed = remaining.min(application_bytes.len());
                    slice[self.n_bytes..self.n_bytes + bytes_processed]
                        .copy_from_slice(&application_bytes[..bytes_processed]);
                    if bytes_processed == remaining {
                        self.n_bytes = 0;
                        let fw_img = H::try_from_bytes(buffer.as_ref());
                        self.state = match fw_img {
                            Ok(meta) => ValidatorState::NeedMoreData(meta, crypto::Hasher::new()),
                            Err(error) => ValidatorState::Fail(error),
                        }
                    }
                    bytes_processed
                }

                // process bytes untill the firmware can be verified
                ValidatorState::NeedMoreData(meta, hasher) => {
                    let n_bytes_required = match meta.security() {
                        // No security: skip ahead to complete (length is unknown)
                        Security::None => {
                            self.state = ValidatorState::Complete(meta.clone(), hasher.hash());
                            return true;
                        }

                        // Security levels requiring a length
                        Security::IntegrityOnly(length, _hash) => *length,
                        Security::Signed(length, _signature) => *length,
                    };

                    // Length includes metadata so should not be shorter than just the metadata
                    if self.n_bytes > n_bytes_required {
                        self.state = ValidatorState::Fail(ApplicationHeaderError::InvalidLength);
                        return true;
                    } else {
                        let remaining = n_bytes_required - self.n_bytes;
                        let bytes_processed = remaining.min(application_bytes.len());
                        if self.n_bytes == H::BINARY_SIZE {
                            let mut header_bytes = [0u8; MAX_AUTH_FIELD_BYTES];
                            let header_bytes = &mut header_bytes[..H::BINARY_SIZE_AUTH];
                            meta.authenticated_fields_as_bytes(header_bytes).unwrap();
                            hasher.update(header_bytes)
                        }
                        hasher.update(&application_bytes[..bytes_processed]);

                        if bytes_processed == remaining {
                            self.state = ValidatorState::Complete(meta.clone(), hasher.hash());
                        }
                        bytes_processed
                    }
                }
                ValidatorState::Complete(_, _) => return true,
                ValidatorState::Fail(_) => return true,
            };
            self.n_bytes += bytes_processed;
            application_bytes = &application_bytes[bytes_processed..];

            if bytes_processed == 0 || application_bytes.len() == 0 {
                break false;
            }
        };
        result
    }

    /// Validate the new application firmware
    ///
    /// Validates the application.
    /// If validation succeeds, the metadata header
    /// (which includes the actual security level) is returned.
    /// Returns an error if the application is invalid or security level is not met.
    ///
    /// If `ValidationError::Incomplete` is returned, more data is expected.
    /// Try again after adding more data via [update()](Self::update())
    pub fn verify(&self) -> Result<H, ValidationError> {
        self.validate_update(None)
    }

    /// Validate the new application firmware and determine if upgrade is allowed
    /// If validation succeeds and the upgrade is allowed, the metadata header
    /// (which includes the actual security level) is returned.
    /// Returns an error if the application is invalid or security level is not met.
    ///
    /// If `ValidationError::Incomplete` is returned, more data is expected.
    /// Try again after adding more data via [update()](Self::update())
    pub fn allow_update_from(&self, existing_application: &H) -> Result<H, ValidationError> {
        self.validate_update(Some(existing_application))
    }

    fn validate_update(&self, existing_application: Option<&H>) -> Result<H, ValidationError> {
        match &self.state {
            ValidatorState::Init(_) | ValidatorState::NeedMoreData(_, _) => {
                Err(ValidationError::Incomplete)
            }
            ValidatorState::Fail(meta_error) => Err(ValidationError::Invalid(meta_error.clone())),
            ValidatorState::Complete(meta, hash) => {
                let level = meta.security().level();

                let min_version = existing_application
                    .map(|existing| existing.anti_downgrade_version())
                    .unwrap_or(0);
                let version = meta.anti_downgrade_version();

                if meta.device_type() != self.device_type {
                    return Err(ValidationError::Invalid(
                        ApplicationHeaderError::InvalidDeviceType,
                    ));
                }

                if level < self.min_security {
                    Err(ValidationError::Security(level))
                } else if version < min_version {
                    Err(ValidationError::Downgrade(version))
                } else {
                    match meta.security() {
                        Security::None => Ok(meta.clone()),
                        Security::IntegrityOnly(_len, ref_hash) => {
                            if ref_hash == hash {
                                Ok(meta.clone())
                            } else {
                                Err(ValidationError::Integrity)
                            }
                        }
                        Security::Signed(_len, ref_signature) => {
                            if self.signing_key.verify(hash, ref_signature).is_ok() {
                                Ok(meta.clone())
                            } else {
                                Err(ValidationError::Signature)
                            }
                        }
                    }
                }
            }
        }
    }

    /// Hash over all the application data
    ///
    /// If `ValidationError::Incomplete` is returned, more data is expected.
    /// Try again after adding more data via [update()](Self::update())
    pub fn data_hash(&self) -> Result<crypto::Hash, ValidationError> {
        match &self.state {
            ValidatorState::Init(_) | ValidatorState::NeedMoreData(_, _) => {
                Err(ValidationError::Incomplete)
            }
            ValidatorState::Fail(meta_error) => Err(ValidationError::Invalid(meta_error.clone())),
            ValidatorState::Complete(_meta, hash) => Ok(hash.clone()),
        }
    }
}

/// Sign a firmware image
///
/// The complete firmware has to be passed in as a slice of bytes.
/// This keeps the API simple but is not suitable for use in firmware.
///
/// If succesfull, the resulting application should pass validation via [Validator](struct@Validator).
pub fn sign<H: ApplicationHeader>(
    signing_key: &crypto::PrivKey,
    binary: &mut [u8],
) -> Result<(), SignError> {
    let mut header = H::try_from_bytes(binary).map_err(SignError::InvalidImage)?;

    // Sign the binary, skipping the unsigned part of the header
    let signature = signing_key
        .sign(&binary[H::BINARY_SIZE_RAW..])
        .map_err(|_| SignError::Signing)?;

    header.write_signature(binary.len(), signature);
    header
        .as_bytes(&mut binary[..H::BINARY_SIZE])
        .map_err(SignError::InvalidImage)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Header = v2::ApplicationHeader<MockDeviceType>;

    #[derive(Copy, Clone, Debug, PartialEq)]
    enum MockDeviceType {
        A = 0,
        B = 1,
    }

    impl DeviceType for MockDeviceType {
        fn to_byte(self) -> u8 {
            self as u8
        }
        fn try_from_byte(byte: u8) -> Option<Self> {
            match byte {
                0 => Some(Self::A),
                1 => Some(Self::B),
                _ => None,
            }
        }
    }

    /// A throwaway public key; only used where validation never reaches signature
    /// verification (e.g. `None`/`IntegrityOnly` images).
    fn dummy_pubkey() -> crypto::PubKey {
        // Uncompressed SEC1 point of the P256 generator: 0x04 || Gx || Gy.
        const GENERATOR: [u8; 65] = [
            0x04, 0x6b, 0x17, 0xd1, 0xf2, 0xe1, 0x2c, 0x42, 0x47, 0xf8, 0xbc, 0xe6, 0xe5, 0x63,
            0xa4, 0x40, 0xf2, 0x77, 0x03, 0x7d, 0x81, 0x2d, 0xeb, 0x33, 0xa0, 0xf4, 0xa1, 0x39,
            0x45, 0xd8, 0x98, 0xc2, 0x96, 0x4f, 0xe3, 0x42, 0xe2, 0xfe, 0x1a, 0x7f, 0x9b, 0x8e,
            0xe7, 0xeb, 0x4a, 0x7c, 0x0f, 0x9e, 0x16, 0x2b, 0xce, 0x33, 0x57, 0x6b, 0x31, 0x5e,
            0xce, 0xcb, 0xb6, 0x40, 0x68, 0x37, 0xbf, 0x51, 0xf5,
        ];
        crypto::PubKey::try_from_bytes(&GENERATOR).unwrap()
    }

    #[test]
    fn test_invalid_header() {
        let bytes = [0x11; Header::BINARY_SIZE];
        assert_eq!(
            Header::try_from_bytes(&bytes).unwrap_err(),
            ApplicationHeaderError::InvalidHeader
        );
    }

    #[test]
    fn test_header_serialize_deserialize_loopback() {
        let ref_hash = crypto::Hasher::new().hash();
        let mut header = Header::placeholder(MockDeviceType::B);
        header.security = Security::IntegrityOnly(123, ref_hash.clone());
        header.anti_downgrade_version = 456;

        let mut bytes = [0x11; Header::BINARY_SIZE];
        header.as_bytes(&mut bytes).unwrap();
        let parsed = Header::try_from_bytes(&bytes).unwrap();

        assert_eq!(parsed.security_level(), SecurityLevel::IntegrityOnly);
        assert_eq!(parsed.anti_downgrade_version(), 456);
        assert_eq!(parsed.device_type, MockDeviceType::B);
        match parsed.security {
            Security::IntegrityOnly(length, hash) => {
                assert_eq!(length, 123);
                assert_eq!(hash, ref_hash);
            }
            _ => panic!("Unexpected security level!"),
        }
    }

    #[test]
    fn test_validate_none_security() {
        let header = Header::placeholder(MockDeviceType::A);
        let mut binary = [0u8; Header::BINARY_SIZE + 12];
        header.as_bytes(&mut binary).unwrap();
        binary[Header::BINARY_SIZE..].copy_from_slice(b"Hello World.");

        let mut validator: Validator<Header> =
            Validator::new(dummy_pubkey(), SecurityLevel::None, MockDeviceType::A);
        validator.update(&binary);
        assert!(validator.verify().is_ok());
    }

    #[test]
    fn test_validate_integrity_only() {
        // Build a placeholder image, compute the hash over (auth fields || data),
        // then mark it IntegrityOnly and validate.
        let data = b"Hello World.";
        let mut header = Header::placeholder(MockDeviceType::A);
        let total_len = Header::BINARY_SIZE + data.len();

        // Compute reference hash exactly like the validator does.
        let mut hasher = crypto::Hasher::new();
        let mut auth = [0u8; Header::BINARY_SIZE_AUTH];
        header.authenticated_fields_as_bytes(&mut auth).unwrap();
        hasher.update(&auth);
        hasher.update(data);
        let hash = hasher.hash();

        header.security = Security::IntegrityOnly(total_len, hash);

        let mut binary = [0u8; Header::BINARY_SIZE + 12];
        header.as_bytes(&mut binary).unwrap();
        binary[Header::BINARY_SIZE..].copy_from_slice(data);

        let mut validator: Validator<Header> = Validator::new(
            dummy_pubkey(),
            SecurityLevel::IntegrityOnly,
            MockDeviceType::A,
        );
        validator.update(&binary);
        assert!(validator.verify().is_ok());
    }

    #[test]
    fn test_validate_wrong_device_type_rejected() {
        let data = b"Hello World.";
        let mut header = Header::placeholder(MockDeviceType::A);
        let total_len = Header::BINARY_SIZE + data.len();
        let mut hasher = crypto::Hasher::new();
        let mut auth = [0u8; Header::BINARY_SIZE_AUTH];
        header.authenticated_fields_as_bytes(&mut auth).unwrap();
        hasher.update(&auth);
        hasher.update(data);
        header.security = Security::IntegrityOnly(total_len, hasher.hash());

        let mut binary = [0u8; Header::BINARY_SIZE + 12];
        header.as_bytes(&mut binary).unwrap();
        binary[Header::BINARY_SIZE..].copy_from_slice(data);

        // Validator expects device type B, image is for A.
        let mut validator: Validator<Header> = Validator::new(
            dummy_pubkey(),
            SecurityLevel::IntegrityOnly,
            MockDeviceType::B,
        );
        validator.update(&binary);
        assert!(matches!(
            validator.verify(),
            Err(ValidationError::Invalid(
                ApplicationHeaderError::InvalidDeviceType
            ))
        ));
    }
}
