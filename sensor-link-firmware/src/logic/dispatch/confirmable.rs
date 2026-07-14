use crate::logic::dispatch::ConfirmHandle;

/// RAII wrapper that automatically triggers retry on drop unless explicitly confirmed.
///
/// This wrapper holds data along with a confirmation handle. If the wrapper is dropped
/// without calling `confirm()`, the associated operation will be retried later.
/// Call `confirm()` to indicate successful processing and prevent retry.
pub struct Confirmable<T> {
    inner: T,
    confirm_handle: Option<ConfirmHandle>,
}

impl<T> Confirmable<T> {
    /// Create a new confirmable wrapper with data and its confirmation handle.
    ///
    /// If no confirmation handle is provided, the wrapper will not trigger a retry on drop.
    pub fn new(inner: T, confirm_handle: Option<ConfirmHandle>) -> Self {
        Self {
            inner,
            confirm_handle,
        }
    }

    /// Create a new confirmable wrapper without ConfirmHandle.
    ///
    /// This means data is permanently lost on drop and won't be retried.
    /// Intended for legacy code that doesn't use ConfirmHandle or in case
    /// the ConfirmHandle is not available (e.g. because the store failed).
    #[inline]
    pub fn with_no_retry(inner: T) -> Self {
        Self::new(inner, None)
    }

    /// Get a reference to the inner data.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Confirm successful processing
    ///
    /// This consumes the Confirmable to prevent accidental re-use.
    pub fn confirm(mut self) {
        if let Some(handle) = self.confirm_handle.take() {
            handle.confirm();
        }
    }

    /// Check if this confirmable can be retried on drop.
    ///
    /// If not, the data is permanently lost on drop.
    pub fn can_be_retried(&self) -> bool {
        self.confirm_handle.is_some()
    }
}

impl<T> Drop for Confirmable<T> {
    fn drop(&mut self) {
        if self.confirm_handle.is_some() {
            log::debug!("Confirmable dropped without confirmation - retry will be triggered");
        }
    }
}

impl<T> AsRef<T> for Confirmable<T> {
    fn as_ref(&self) -> &T {
        &self.inner
    }
}

impl<T> core::ops::Deref for Confirmable<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.inner
    }
}
