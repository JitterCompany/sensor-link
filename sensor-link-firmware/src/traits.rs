use core::ops::DerefMut;

/// This trait enables shared access to a locked/shared resource.
pub trait Arbiter {
    type Shared;

    /// Async version: await the resource becoming available
    async fn access(&self) -> impl DerefMut<Target = Self::Shared>;

    /// Non-blocking version: poll if the resource is available
    fn try_access(&self) -> Option<impl DerefMut<Target = Self::Shared>>;
}

/// Async trigger trait: any external signal that can be awaited
///
/// For example, a rising edge on a GPIO pin
pub trait Trigger {
    /// Wait untill a trigger occurs
    ///
    /// The trigger starts waiting for trigger events and stays Pending. As soon as a trigger happens,
    /// the Future becomes Ready.
    ///
    /// Note: this does not return immediately if a trigger had happened *(just) before* calling this method.
    async fn wait_untill_next_edge(&mut self);

    /// Wait untill a trigger occurs
    ///
    /// The trigger starts waiting for trigger events and stays Pending. As soon as a trigger happens,
    /// the Future becomes Ready.
    ///
    /// Note: this returns immediately if a trigger had happened *(just) before* calling this method.
    async fn wait_untill_any_edge(&mut self);

    /// Polling variant of [Trigger::wait_untill_any_edge].
    ///
    /// Returns true if a trigger has happened since last poll (or wait) and clears it.
    fn poll_ready(&mut self) -> bool;
}

/// This trait gives abstract insight in errors that may occur inside an object.
pub trait ErrorReport {
    /// If true this indicates that some error has occurred.
    fn error_occurred(&self) -> bool;

    /// Clear the error.
    fn reset_error(&mut self);
}

/// This trait should be implemented to allow for external
/// control of Suspend/Resume functionality
pub trait Suspend {
    /// Suspend or deinit peripheral for power saving purposes.
    fn suspend(&mut self);

    /// Resume or reinit peripheral for power saving purposes.
    fn resume(&mut self);

    /// Check if peripheral is suspended
    fn is_suspended(&self) -> bool;
}

impl<T: Suspend> Suspend for &mut T {
    #[inline]
    fn suspend(&mut self) {
        T::suspend(self);
    }

    #[inline]
    fn resume(&mut self) {
        T::resume(self);
    }

    #[inline]
    fn is_suspended(&self) -> bool {
        T::is_suspended(self)
    }
}

/// This trait gives insight in the current suspended state of a peripheral driver
/// that may be resumed by another driver or external event.
pub trait Suspendable {
    fn is_suspended(&self) -> bool;

    async fn await_resume(&self);
}

/// This trait enables splitting off two parts from an object
/// optionally transforming the base object as well
pub trait Split<A, B, Base = Self> {
    /// Split in two parts while converting self to base
    fn split(self) -> (Base, A, B);
}

pub trait U32 {
    fn read(&self) -> u32;
}

pub trait MutU32: U32 {
    fn write(&mut self, new_value: u32);
}

pub trait BaudRateControl {
    fn set_baud_rate(&mut self, baud_rate: u32);
    fn get_baud_rate(&self) -> u32;
}

// blanket impl for  &mut T: BaudRateControl
impl<T: BaudRateControl> BaudRateControl for &mut T {
    #[inline]
    fn set_baud_rate(&mut self, baud_rate: u32) {
        T::set_baud_rate(self, baud_rate);
    }

    #[inline]
    fn get_baud_rate(&self) -> u32 {
        T::get_baud_rate(self)
    }
}

/// CRC8 receive engine.
///
/// Feeding data to the checksum is left out of the trait on purpose,
/// as this may also be implemented by a hardware peripheral
pub trait CRC8Receiver {
    type Error: core::fmt::Debug;

    fn configure_polynomial(&mut self, poly: u8) -> Result<(), Self::Error>;
    fn rx_crc8(&mut self) -> u8;
}

/// Watchdog: must be fed at regular interval
///
/// The implementer must reset/reboot the system if
/// watchdog is not fed within the correct time window
pub trait Watchdog {
    const WINDOW_MS: core::ops::Range<u32>;

    /// Call within WINDOW_MS to prevent reset
    fn feed(&mut self);
}
