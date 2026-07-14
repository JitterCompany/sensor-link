use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, Ordering},
};

#[repr(u8)]
enum Init {
    None = 0,
    Busy = 1,
    Done = 2,
}

/// Similar to std::OnceLock but no_std compatible by using Atomics for syncronization
pub struct OnceLock<T> {
    initialized: AtomicU8,
    data: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T> Sync for OnceLock<T> {}

impl<T> OnceLock<T> {
    pub const fn new() -> Self {
        Self {
            initialized: AtomicU8::new(Init::None as u8),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Get a reference to the inner value or initialize if it is not initialized yet
    ///
    /// This only succeeds the first time: the argument `make_timer` is executed and `Ok` is returned.
    /// Any successive calls return an error without running the provided function.
    pub fn get_or_try_init<F>(&self, initializer: F) -> Result<&T, ()>
    where
        F: FnOnce() -> T,
    {
        // Try to go from Init::None to Init::Busy. If it fails the timer is already (being?) initialized
        self.initialized
            .compare_exchange(
                Init::None as u8,
                Init::Busy as u8,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .map_err(|_| ())?;

        // Safe because this write can only happen once and references are only given out after Init::Done is reached
        // The scope enforces mutable references to be dropped before we switch to Init::Done
        {
            let instance = initializer();

            {
                // Step 1: &UnsafeCell to &mut MaybeUnInit. This is the first time ever that the cell is accessed
                let mut_uninit_ptr: &mut MaybeUninit<T> = unsafe { &mut *self.data.get() };

                // Step 2: write the new value into the MaybeUnInit. After this point, assume_init() is valid
                mut_uninit_ptr.write(instance);
            }
        }

        // Mark initialization as complete
        self.initialized.store(Init::Done as u8, Ordering::Release);

        self.get().ok_or(())
    }

    /// Try to get a reference to the inner value.
    /// Fails if not initialized (yet)
    pub fn get(&self) -> Option<&T> {
        if self.initialized.load(Ordering::Acquire) == Init::Done as u8 {
            // Safe because data is not mutated after Init::Done
            let ptr = self.data.get();
            Some(unsafe { (&*ptr).assume_init_ref() })
        } else {
            None
        }
    }
}
