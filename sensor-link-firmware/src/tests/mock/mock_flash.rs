pub use crate::storage::backend::{InMemoryFlash as MockFlash, MockError};

const DUMMY_TOTAL_SIZE: usize = 4 * 1024 * 1024;

pub fn new<const ERASE_SIZE: usize>() -> MockFlash<ERASE_SIZE> {
    MockFlash::new(DUMMY_TOTAL_SIZE)
}
