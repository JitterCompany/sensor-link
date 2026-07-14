use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::monotonic_time::traits::*;

#[allow(unused)]
pub struct MockTimeout {
    remote_tx: mpsc::Sender<()>,
    remote_rx: mpsc::Receiver<()>,
    is_expired: Arc<Mutex<bool>>,
    timeout_is_set: bool,
}

pub struct MockTimeoutCtrl {
    is_expired: Arc<Mutex<bool>>,
}

impl MockTimeoutCtrl {
    pub fn set_expired(&self) {
        *self.is_expired.lock().unwrap() = true;
    }
}

impl MockTimeout {
    #[allow(unused)]
    pub fn get_async_ctrl(&self) -> mpsc::Sender<()> {
        self.remote_tx.clone()
    }

    pub fn get_sync_ctrl(&self) -> MockTimeoutCtrl {
        MockTimeoutCtrl {
            is_expired: self.is_expired.clone(),
        }
    }
}

impl MonotonicTime for MockTimeout {
    async fn delay_ms(_milliseconds: u32) {}

    async fn delay_us(_microseconds: u64) {}

    type Timeout = Self;
    type Instant = MockInstant;

    fn timeout() -> Self::Timeout {
        <Self as Timeout>::new()
    }

    fn now() -> Self::Instant {
        Self::Instant::now()
    }
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct MockInstant(u64);

impl Instant for MockInstant {
    fn now() -> Self {
        Self(0)
    }

    fn elapsed_us(&self) -> u64 {
        123 // TODO dummy, may need more advanced impl to advance time from the unit test
    }

    fn micros_since(&self, earlier: &Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    fn add_micros(&self, micros: u64) -> Self {
        MockInstant(self.0.saturating_add(micros))
    }
}

impl Timeout for MockTimeout {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);
        Self {
            is_expired: Arc::new(Mutex::new(false)),
            remote_tx: tx,
            remote_rx: rx,
            timeout_is_set: false,
        }
    }

    fn set_ms(&mut self, _delay_ms: u32) {
        let mut is_exp = self.is_expired.lock().unwrap();
        *is_exp = false;
        self.timeout_is_set = true;
    }

    fn set_us(&mut self, _delay_micros: u64) {
        self.set_ms(0)
    }

    fn is_expired(&self) -> bool {
        self.is_expired.lock().unwrap().clone()
    }

    async fn wait(&mut self) {
        if !self.is_expired() {
            self.remote_rx.recv().await;
        }
    }

    fn is_set(&self) -> bool {
        self.timeout_is_set
    }

    fn clear(&mut self) {
        self.timeout_is_set = false;
    }

    fn expired_by(&self) -> Option<u64> {
        match self.is_expired() {
            true => Some(999_999), // dummy value (this is a mock impl)
            false => None,
        }
    }
}
