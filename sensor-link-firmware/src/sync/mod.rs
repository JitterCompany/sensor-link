//! Datastructures for synchronization between threads.
//!

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    ptr,
    sync::atomic::{fence, AtomicU32, Ordering},
};

use num_enum::TryFromPrimitive;

mod once_lock;
pub use once_lock::OnceLock;

pub mod reserving_sender;
pub mod session_channel;

use crate::logic::LatestValueSendChannel;

/// Keep track of the internal 'ping pong' buffer and whether or not writing is busy
///
/// NB: the order is important, the state always advances 0,1,2,3,0, etc
#[repr(u32)]
#[derive(TryFromPrimitive, PartialEq, Debug, Clone, Copy)]
enum State {
    WriteIdleRead0 = 0,
    Writing1Read0 = 1,
    WriteIdleRead1 = 2,
    Writing0Read1 = 3,
}

#[repr(usize)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum ReadIndex {
    Zero = 0,
    One = 1,
}

#[repr(usize)]
#[derive(Debug, Clone, Copy)]
enum WriteIndex {
    Zero = 0,
    One = 1,
}

impl State {
    pub fn from_u32(v: u32) -> Self {
        // Note: unwrap can't panic as all 2-bit values are valid
        State::try_from(v & 0b11).unwrap()
    }
    pub fn from_write_index(index: WriteIndex) -> Self {
        match index {
            WriteIndex::Zero => Self::Writing0Read1,
            WriteIndex::One => Self::Writing1Read0,
        }
    }
    pub fn next(self) -> Self {
        Self::from_u32(self as u32 + 1)
    }

    pub fn read_index(self) -> ReadIndex {
        match self {
            State::WriteIdleRead0 | State::Writing1Read0 => ReadIndex::Zero,
            State::WriteIdleRead1 | State::Writing0Read1 => ReadIndex::One,
        }
    }

    pub fn write_index(self) -> Option<WriteIndex> {
        match self {
            State::WriteIdleRead0 | State::WriteIdleRead1 => None,
            State::Writing0Read1 => Some(WriteIndex::Zero),
            State::Writing1Read0 => Some(WriteIndex::One),
        }
    }
}

/// A thread-safe double-buffer. Allows writing while one or more threads are reading.
///
/// It is optimized for a scenario where writes occur (much) less often than reads.
/// Multiple readers can at any time observe the last known message without blocking writes.
///
/// Main features:
/// - multiple concurrent readers allowed at all times (with low overhead)
/// - readers never block writing
/// - writing never blocks reading (only locks other threads from writing at the same time)
/// - readers observe the most recently available data, but old data may be overwritten at any time (this is not a queue)
#[derive(Debug)]
pub struct DoubleBuffer<T> {
    state: AtomicU32,
    data_version: AtomicU32,
    buffer: [UnsafeCell<T>; 2],
}

unsafe impl<T> Sync for DoubleBuffer<T> {}

impl<T> DoubleBuffer<T>
where
    T: Copy,
{
    /// Instantiate a new buffer with given initial value
    ///
    /// Note that the underlying T must implement / derive the `Copy` trait.
    pub const fn new_const(initial_value: T) -> Self {
        Self {
            data_version: AtomicU32::new(0),
            state: AtomicU32::new(State::WriteIdleRead0 as u32),
            buffer: [
                UnsafeCell::new(initial_value),
                UnsafeCell::new(initial_value),
            ],
        }
    }
}

impl<T> DoubleBuffer<T>
where
    T: Clone,
{
    /// Instantiate a new buffer with given initial value
    ///
    /// Note that the underlying T must implement / derive the `Clone` trait.
    pub fn new(initial_value: T) -> Self {
        Self {
            data_version: AtomicU32::new(0),
            state: AtomicU32::new(State::WriteIdleRead0 as u32),
            buffer: [
                UnsafeCell::new(initial_value.clone()),
                UnsafeCell::new(initial_value),
            ],
        }
    }

    /// Version: can be used to quickly check if [Self::read] would yield the same state
    /// as a previous read by comparing the version number.
    pub fn version(&self) -> BufferVersion {
        BufferVersion(self.data_version.load(Ordering::Relaxed))
    }

    /// Check if new data was written since the given [BufferVersion].
    ///
    /// The intended use is to check for potentially new data before copying it via [Self::read].
    /// Note that the only comparison is the given version, so it may return true even though
    /// the last [Self::read] might have already observed the latest data.
    pub fn new_data_available_since(&self, version: &mut BufferVersion) -> bool {
        let latest = self.version();
        let result = version.0 != latest.0;
        *version = latest;
        result
    }

    /// Read the most recently written state
    ///
    /// This function does not block and is safe to call from any context.
    /// Note that the result is returned as a clone, so the underlying type must
    /// implement/derive `Clone`. A clone is usually only done once within this function,
    /// but may be repeated if another thread is writing during the clone.
    pub fn read(&self) -> T {
        loop {
            // Double buffering: check where to read the most recent value
            let state = self.state(Ordering::Acquire);
            let version = self.data_version.load(Ordering::Relaxed);
            let value = &self.buffer[state.read_index() as usize];

            // Copy the value. MaybeUninit because at this point we might have gotten
            // a corrupt value due to a conflicting write
            let value = unsafe { ptr::read_volatile(value.get() as *mut MaybeUninit<T>) };
            fence(Ordering::Acquire);

            // Assuming few writes and lots of reads, this check is most likely true.
            // Ocassionally the loop spins when a write happend while we were reading
            let state_check = self.state(Ordering::Acquire);
            let version_check = self.data_version.load(Ordering::Relaxed);
            if state.read_index() == state_check.read_index() && version == version_check {
                // Safe because:
                // - value is always initialized
                // - version number has not changed: no writes occurred (assuming exactly 2**32 writes in the time this loop spins is impossible)
                // - read index has not changed: no write took effect during the copy
                return unsafe { value.assume_init() };
            }
        }
    }

    /// Try to update the value as observed via `read()`
    ///
    /// This function returns a `WriteLockGuard` which behaves as a mutable reference
    /// to the underlying type T. As soon as this guard object is dropped, all readers
    /// will observe the updated version.
    ///
    /// Note that only one writer can exist at the same time: this function returns
    /// `None` if another writer is still active (`WriteLockGuard` not dropped yet)
    pub fn try_write(&self) -> Option<WriteLockGuard<'_, T>> {
        // Try to enter Writing1Read or Writing0Read1 state.
        // If both fail, it means that another writer is likely still active
        let state = State::from_u32(
            self.state
                .compare_exchange(
                    State::WriteIdleRead0 as u32,
                    State::Writing1Read0 as u32,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .or_else(|_| {
                    self.state.compare_exchange(
                        State::WriteIdleRead1 as u32,
                        State::Writing0Read1 as u32,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                })
                .ok()?,
        )
        .next();

        // This always succeeds (but the compiler cant prove it)
        let write_index = state.write_index()?;

        let mut writer = WriteLockGuard {
            lock: self,
            write_index,
        };

        // Initialize the to-be-overwritten value with the last
        // known value
        // even if the writer is not (re-)writing all data
        *writer.deref_mut() = self.read();
        Some(writer)
    }

    fn finish_writing(&self, write_index: WriteIndex) {
        let new_state = State::from_write_index(write_index).next();
        self.data_version.fetch_add(1, Ordering::Relaxed);
        self.state.store(new_state as u32, Ordering::Release);
    }

    fn state(&self, order: Ordering) -> State {
        State::from_u32(self.state.load(order))
    }
}

pub struct BufferVersion(u32);

/// RAII guard object which behaves as a mutable reference to T.
/// See `DoubleBuffer::try_write()`.
#[derive(Debug)]
pub struct WriteLockGuard<'a, T: Clone> {
    write_index: WriteIndex,

    // Lock: note that lock state must be incremented to WritingIdle upon drop
    lock: &'a DoubleBuffer<T>,
}

impl<'a, T: Clone> Deref for WriteLockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // This is safe because there can only be one WriteLockGuard instance
        let i = self.write_index as usize;
        unsafe { &*self.lock.buffer[i].get() }
    }
}

impl<'a, T: Clone> DerefMut for WriteLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let i = self.write_index as usize;
        // This is safe because there can only be one WriteLockGuard instance
        unsafe { &mut *self.lock.buffer[i].get() }
    }
}

impl<T: Clone> Drop for WriteLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.finish_writing(self.write_index);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BufferWriteError {
    WriteAlreadyInProgress,
}

impl<T: Clone> LatestValueSendChannel<T> for &DoubleBuffer<T> {
    type Error = BufferWriteError;

    fn send(&mut self, val: T) -> Result<(), Self::Error> {
        match self.try_write() {
            Some(mut ptr) => {
                *ptr = val;
                Ok(())
            }
            None => Err(BufferWriteError::WriteAlreadyInProgress),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, Debug)]
    struct TestStruct {
        a: u32,
        b: u32,
    }

    #[test]
    fn reads_valid_while_writing() {
        let lock = DoubleBuffer::new(TestStruct { a: 0, b: 0 });

        assert_eq!(lock.read(), TestStruct { a: 0, b: 0 });

        // Writes occur in this scope. At the end the LockGuard is dropped and the new value is available
        {
            let mut writer = lock.try_write().unwrap();
            writer.a = 23;

            // halfway the write: reads should still return the last value
            assert_eq!(lock.read(), TestStruct { a: 0, b: 0 });
        }
        // write done: reads should reflect updated value
        assert_eq!(lock.read(), TestStruct { a: 23, b: 0 });
    }

    #[test]
    fn reads_consistent_after_multi_write() {
        let lock = DoubleBuffer::new(TestStruct { a: 0, b: 0 });

        assert_eq!(lock.read(), TestStruct { a: 0, b: 0 });

        // First write
        lock.try_write().unwrap().a = 3;
        assert_eq!(lock.read(), TestStruct { a: 3, b: 0 });

        // Second write: only b is changed, a should stay the same
        lock.try_write().unwrap().b = 1337;
        assert_eq!(lock.read(), TestStruct { a: 3, b: 1337 });

        // Third 'write': nothing written, data stays the same
        lock.try_write().unwrap();
        assert_eq!(lock.read(), TestStruct { a: 3, b: 1337 });

        // Fourth 'write': completely overwrite struct
        *lock.try_write().unwrap() = TestStruct { a: 777, b: 333 };
        assert_eq!(lock.read(), TestStruct { a: 777, b: 333 });
    }

    #[test]
    fn writing_locks_writing() {
        let lock = DoubleBuffer::new(TestStruct { a: 0, b: 0 });

        // Write 1: start
        let ptr = lock.try_write().unwrap();

        // Attempt a second writer: must fail (ptr still in scope)
        assert!(lock.try_write().is_none());

        // Write 1: end
        drop(ptr);

        // Attempt a writer: must succeed (old ptr is dropped)
        lock.try_write().unwrap();
    }
}
