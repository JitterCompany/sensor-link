//! Request to confirm that everything handed to dispatch so far has been sent.
//!
//! A [`DispatchBarrier`] lives as a `static` per app, shared by the task that
//! requests it and the dispatch task. On a request, the dispatch task stores
//! everything it has received so far, forces buffered data out, and replies
//! with [`Signal::DispatchDrained`](crate::logic::signal::Signal::DispatchDrained)
//! once every stored item has been confirmed by the network task.
//!
//! The requester must make sure no new data is produced after the request if it
//! needs the reply to cover exactly the data from before it: data received after
//! the request is included as well, and can delay the reply.

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

/// Shared request flag for a dispatch drain barrier.
///
/// Requests don't queue up: a request made while the previous one is still
/// outstanding is answered by the same reply.
pub struct DispatchBarrier {
    requested: Signal<CriticalSectionRawMutex, ()>,
}

impl DispatchBarrier {
    pub const fn new() -> Self {
        Self {
            requested: Signal::new(),
        }
    }

    /// Ask the dispatch task to reply once everything received so far is sent.
    pub fn request(&self) {
        self.requested.signal(());
    }

    /// Wait for the next request. Used by the dispatch task.
    pub async fn wait(&self) {
        self.requested.wait().await
    }

    /// Take a pending request without waiting. Returns `true` if there was one.
    ///
    /// A request made between the check and the reset merges into the one
    /// taken, which is fine because requests don't queue up anyway.
    pub fn try_take(&self) -> bool {
        let requested = self.requested.signaled();
        if requested {
            self.requested.reset();
        }
        requested
    }
}

impl core::fmt::Debug for DispatchBarrier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DispatchBarrier")
            .field("requested", &self.requested.signaled())
            .finish()
    }
}

impl Default for DispatchBarrier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_do_not_queue_up() {
        let barrier = DispatchBarrier::new();
        assert!(!barrier.try_take());

        barrier.request();
        barrier.request();
        assert!(barrier.try_take());
        assert!(!barrier.try_take());
    }
}
