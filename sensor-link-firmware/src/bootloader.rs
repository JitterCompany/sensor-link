pub mod common_rtt_logger;
pub mod common_time;

use log;

use crate::{
    meta::firmware::{ApplicationHeader, SecurityLevel, ValidationError, Validator},
    storage::flash_db::{File as FileObject, FileStore, ObjectExt},
    utils::crypto::PubKey,
};

/// Read access to the application image currently installed in internal flash.
///
/// Abstracts the concrete internal-flash driver so the update logic below stays
/// generic over the hardware; the concrete type is supplied by the app module.
pub trait AppFlashRead {
    /// The installed application image (metadata header + code).
    fn application_total(&self) -> &[u8];
}

/// Erase + rewrite access to the application region of internal flash.
pub trait AppFlashWrite: AppFlashRead {
    type EraseError;
    type WriteError;
    type Writer<'a>: AppFlashWriter<WriteError = Self::WriteError>
    where
        Self: 'a;

    /// Erase the application region, returning a writer for the replacement image.
    ///
    /// # Safety
    /// Must only be called from the bootloader, never from the running application.
    unsafe fn erase_application(&mut self) -> Result<Self::Writer<'_>, Self::EraseError>;
}

/// Sink for streaming a replacement application image into the erased region.
pub trait AppFlashWriter {
    type WriteError;
    fn append(&mut self, data: &[u8]) -> Result<usize, Self::WriteError>;
    fn finalize(self) -> Result<usize, Self::WriteError>;
}

pub enum AppStatus<H: ApplicationHeader> {
    /// Firmware already matches the update: skip update
    UpToDate(H),

    /// Firmware cannot be updated: skip update
    UpdateRejected(H, ValidationError),

    /// Valid update is found (different from the existing firmware)
    ShouldUpdate(H),

    /// No firmware is available at all: cannot update or boot
    Bricked(ValidationError, ValidationError),
}

// Read existing firmware (from internal flash) into Validator.
fn validate_existing_firmware<H, IF>(
    internal_flash: &IF,
    device_type: H::DeviceType,
    pubkey: &PubKey,
) -> Validator<H>
where
    H: ApplicationHeader,
    IF: AppFlashRead,
{
    let mut validator = Validator::new(pubkey.clone(), SecurityLevel::None, device_type);
    validator.update(internal_flash.application_total());
    validator
}

// Read new candidate firmware (from external flash) into Validator.
// Depending on firmware size this may take a couple of seconds, so only call when needed
async fn validate_new_firmware<H, S, F, const BLOCK_SIZE: usize, const CHUNK_SIZE: usize>(
    store: &mut S,
    fw_file: F,
    device_type: H::DeviceType,
    pubkey: &PubKey,
) -> Validator<H>
where
    H: ApplicationHeader,
    F: FileObject<BLOCK_SIZE>,
    S: FileStore<BLOCK_SIZE, F>,
{
    let mut validator = Validator::new(pubkey.clone(), SecurityLevel::Signed, device_type);
    match store.file_handle(fw_file).await {
        Ok(file_handle) => {
            let mut bytes_read = 0;
            let mut complete = false;
            for frag in 0..fw_file.fragment_count() as u32 {
                let mut buffer = [0; CHUNK_SIZE];
                match store
                    .read_file_fragment(&file_handle, frag, &mut buffer)
                    .await
                {
                    Ok(len) => {
                        bytes_read += len;
                        if validator.update(&buffer[..len]) {
                            complete = true;
                            break;
                        }
                    }
                    Err(err) => {
                        log::debug!(
                            "Update candidate: fragment {frag} not readable ({err:?}) after {bytes_read} bytes"
                        );
                        break;
                    }
                }
            }
            log::debug!("Update candidate: read {bytes_read} bytes, complete: {complete}");
        }
        Err(err) => log::debug!("Update candidate: no readable firmware file ({err:?})"),
    }
    validator
}

pub async fn validate_update<H, S, F, IF, const BLOCK_SIZE: usize, const CHUNK_SIZE: usize>(
    internal_flash: &IF,
    store: &mut S,
    fw_file: F,
    device_type: H::DeviceType,
    pubkey: &PubKey,
) -> AppStatus<H>
where
    H: ApplicationHeader + PartialEq,
    F: FileObject<BLOCK_SIZE>,
    S: FileStore<BLOCK_SIZE, F>,
    IF: AppFlashRead,
{
    // Read first chunk of firmware file, parsed as header
    let mut buffer = [0; CHUNK_SIZE];
    let mut new_unvalidated_header = None;
    if let Ok(file_handle) = store.file_handle(fw_file).await {
        if let Ok(len) = store.read_file_fragment(&file_handle, 0, &mut buffer).await {
            new_unvalidated_header = H::try_from_bytes(&buffer[..len]).ok();
        }
    }
    match &new_unvalidated_header {
        Some(header) => log::debug!(
            "Update candidate header: {:?} bytes, min version {}",
            header.length(),
            header.anti_downgrade_version()
        ),
        None => log::debug!("Update candidate: no valid header"),
    }

    // validate existing firmware
    let old_validator = validate_existing_firmware::<H, IF>(internal_flash, device_type, pubkey);
    match old_validator.verify() {
        // Existing firmware valid
        Ok(existing_header) => {
            log::debug!("Existing firmware is valid...");

            // If the existing firmware is valid and its header (which includes embedded security level + signature/hash)
            // is the same, updating is pointless. This is a big speedup as we can minimize reading external flash
            if new_unvalidated_header.map_or(false, |new| new == existing_header) {
                log::debug!("Existing firmware header matches available update");
                AppStatus::UpToDate(existing_header)
            } else {
                // Read the new candidate firmware from external flash
                let new_validator = validate_new_firmware::<H, S, F, BLOCK_SIZE, CHUNK_SIZE>(
                    store,
                    fw_file,
                    device_type,
                    pubkey,
                )
                .await;

                // This check is not really necessary. But may give a small speedup in case only the
                // header/security level is different (e.g. if we downgrade from signature to hash in internal
                // flash we could only check the hash on startup and boot faster. but this is not implemented yet)
                if match (new_validator.data_hash(), old_validator.data_hash()) {
                    (Ok(new_hash), Ok(old_hash)) => new_hash == old_hash,
                    _ => false,
                } {
                    log::debug!("Existing firmware hash matches available update");
                    AppStatus::UpToDate(existing_header)

                // Different version: validate update
                } else {
                    match new_validator.allow_update_from(&existing_header) {
                        Ok(new_header) => AppStatus::ShouldUpdate(new_header),
                        Err(error) => AppStatus::UpdateRejected(existing_header, error),
                    }
                }
            }
        }

        // Existing firmware invalid: verify new update candidate
        Err(existing_error) => {
            log::debug!("Existing firmware invalid {existing_error:?}");

            // Read the new candidate firmware from external flash
            let new_validator = validate_new_firmware::<H, S, F, BLOCK_SIZE, CHUNK_SIZE>(
                store,
                fw_file,
                device_type,
                pubkey,
            )
            .await;
            match new_validator.verify() {
                Ok(new_header) => AppStatus::ShouldUpdate(new_header),
                Err(new_error) => AppStatus::Bricked(existing_error, new_error),
            }
        }
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum UpdateError<E, W> {
    Erase(E),
    Write(W),
    Validate(ValidationError),
}
pub async fn apply_update<H, S, F, IF, const BLOCK_SIZE: usize, const CHUNK_SIZE: usize>(
    internal_flash: &mut IF,
    store: &mut S,
    fw_file: F,
    device_type: H::DeviceType,
    pubkey: &PubKey,
) -> Result<H, UpdateError<IF::EraseError, IF::WriteError>>
where
    H: ApplicationHeader,
    F: FileObject<BLOCK_SIZE>,
    S: FileStore<BLOCK_SIZE, F>,
    IF: AppFlashWrite,
{
    // Erase old firmware & copy new firmware from store
    {
        log::debug!("Erasing old application...");
        let mut flash_writer = unsafe { internal_flash.erase_application() }
            .map_err(|erase_error| UpdateError::Erase(erase_error))?;

        // Copy firmware file to internal flash
        log::debug!("Writing new application...");
        if let Ok(file_handle) = store.file_handle(fw_file).await {
            for frag in 0..fw_file.fragment_count() as u32 {
                let mut buffer = [0; CHUNK_SIZE];
                if let Ok(len) = store
                    .read_file_fragment(&file_handle, frag, &mut buffer)
                    .await
                {
                    flash_writer
                        .append(&buffer[..len])
                        .map_err(|w_error| UpdateError::Write(w_error))?;
                }
            }
        }
        flash_writer
            .finalize()
            .map_err(|w_error| UpdateError::Write(w_error))?;
    }

    // Verify internal firmware after applying
    log::info!("Update complete. Verifying...");
    let header = validate_existing_firmware::<H, IF>(internal_flash, device_type, pubkey)
        .verify()
        .map_err(|error| UpdateError::Validate(error))?;
    Ok(header)
}
