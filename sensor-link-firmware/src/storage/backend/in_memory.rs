use embedded_storage::nor_flash::{NorFlashError, NorFlashErrorKind};

pub struct InMemoryFlash<const ERASE_SIZE: usize> {
    // Mock memory
    pub memory: Vec<u8>,

    // Error injection
    countdown_erase_error: Option<(u32, MockError)>,
    countdown_write_error: Option<(u32, MockError)>,
    countdown_read_error: Option<(u32, MockError)>,

    // Stat counters: incremented on each (attempted) action
    pub read_count: u64,
    pub write_count: u64,
    pub erase_count: u64,
}

impl<const ERASE_SIZE: usize> InMemoryFlash<ERASE_SIZE> {
    pub fn new(total_size: usize) -> Self {
        Self {
            memory: vec![0xFF; total_size],
            countdown_erase_error: None,
            countdown_read_error: None,
            countdown_write_error: None,

            read_count: 0,
            write_count: 0,
            erase_count: 0,
        }
    }

    /// Reset the stat counters
    pub fn reset_stats(&mut self) {
        self.read_count = 0;
        self.write_count = 0;
        self.erase_count = 0;
    }
}

// test-only implementation
#[cfg(test)]
impl<const ERASE_SIZE: usize> InMemoryFlash<ERASE_SIZE> {
    /// Mock: cause the nth next read to fail with given error
    pub fn trigger_read_error_after(&mut self, nth_op: u32, error: MockError) {
        self.countdown_read_error = Some((nth_op, error))
    }

    /// Mock: cause the nth next write to fail with given error
    pub fn trigger_write_error_after(&mut self, nth_op: u32, error: MockError) {
        self.countdown_write_error = Some((nth_op, error))
    }

    /// Mock: cause the nth next erase to fail with given error
    pub fn trigger_erase_error_after(&mut self, nth_op: u32, error: MockError) {
        self.countdown_erase_error = Some((nth_op, error))
    }
}

#[derive(Debug, Clone)]
pub struct MockError(pub NorFlashErrorKind);
impl NorFlashError for MockError {
    fn kind(&self) -> NorFlashErrorKind {
        self.0
    }
}

impl<const ERASE_SIZE: usize> embedded_storage::nor_flash::ErrorType for InMemoryFlash<ERASE_SIZE> {
    type Error = MockError;
}

fn trigger_error_on_countdown(countdown: &mut Option<(u32, MockError)>) -> Result<(), MockError> {
    match countdown {
        // Countdown finished: trigger error (once)
        Some((0, error)) => {
            let error = error.clone();
            *countdown = None;
            return Err(error);
        }

        // Advance countdown
        Some((n, _)) => {
            *n -= 1;
        }
        _ => {}
    }

    Ok(())
}

impl<const ERASE_SIZE: usize> embedded_storage_async::nor_flash::ReadNorFlash
    for InMemoryFlash<ERASE_SIZE>
{
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.read_count += 1;
        trigger_error_on_countdown(&mut self.countdown_read_error)?;

        let offset = offset as usize;
        let end = offset.saturating_add(bytes.len());
        if end > self.memory.len() {
            log::error!(target: "InMemoryFlash", "read out of bounds: {}-byte read at offset {offset}, capacity {}", bytes.len(), self.memory.len());
            return Err(MockError(NorFlashErrorKind::OutOfBounds));
        }

        bytes.copy_from_slice(&self.memory[offset..end]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.memory.len()
    }
}

impl<const ERASE_SIZE: usize> embedded_storage_async::nor_flash::NorFlash
    for InMemoryFlash<ERASE_SIZE>
{
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.erase_count += 1;
        let result = trigger_error_on_countdown(&mut self.countdown_erase_error);

        // if an error is injected, corrupt all to-be-written data by XOR.
        // this simulates the erase failing halfway and leaving the flash area in undetermined state (0x5A)
        // also this means that writes to this area will fail (as the bytes are not 0xFF) untill next erase
        let corruption = match result {
            Ok(_) => 0,
            Err(_) => 0xA5,
        };

        let erase_size = Self::ERASE_SIZE as u32;
        if from % erase_size != 0 || to % erase_size != 0 {
            return Err(MockError(NorFlashErrorKind::NotAligned));
        }

        if from.max(to) > self.memory.len() as u32 {
            log::error!(target: "InMemoryFlash", "erase out of bounds: erase from {from} to {to}, capacity {}", self.memory.len());
            return Err(MockError(NorFlashErrorKind::OutOfBounds));
        }

        let from = from as usize;
        let to = to as usize;
        self.memory[from..to].fill(0xFF ^ corruption);
        result
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.write_count += 1;
        let result = trigger_error_on_countdown(&mut self.countdown_write_error);

        // if an error is injected, corrupt all to-be-written data by XOR
        // this simulates the write failing halfway and leaving the flash area in undetermined state (0xA5)
        let corruption = match result {
            Ok(_) => 0,
            Err(_) => 0xA5,
        };

        let offset = offset as usize;

        let end = offset.saturating_add(bytes.len());
        if end > self.memory.len() {
            log::error!(target: "InMemoryFlash", "write out of bounds: {}-byte write at offset {offset}, capacity {}", bytes.len(), self.memory.len());
            return Err(MockError(NorFlashErrorKind::OutOfBounds));
        }

        // strict simulation: assume only 0xFF bytes can be written
        // (in reality most flash will allow any 1 bits to be written to 0)
        let dst = self.memory[offset..end].iter_mut();
        for (to, from) in dst.zip(bytes.into_iter()) {
            if *to == 0xFF {
                *to = *from ^ corruption;
            }
        }
        result
    }
}
