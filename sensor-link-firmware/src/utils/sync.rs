use tokio::sync::watch;

use crate::{logic::ChangeNotifier, traits};

pub struct ChangeNotification {
    tx: watch::Sender<()>,
    rx: tokio::sync::Mutex<watch::Receiver<()>>,
}

impl ChangeNotification {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(());
        Self {
            tx,
            rx: tokio::sync::Mutex::new(rx),
        }
    }
}

impl ChangeNotifier for ChangeNotification {
    fn notify(&self) {
        self.tx.send(()).unwrap()
    }

    async fn await_change(&self) {
        // (this implementation assumes only one thread is awaiting changes
        // at the same time, as the lock is held across the await..)
        self.rx.lock().await.changed().await.unwrap()
    }
}

pub struct Arbiter<T>(tokio::sync::Mutex<T>);

impl<T> Arbiter<T> {
    pub const fn new(inner: T) -> Self {
        Self(tokio::sync::Mutex::const_new(inner))
    }
}

impl<T> traits::Arbiter for Arbiter<T> {
    type Shared = T;

    async fn access(&self) -> impl core::ops::DerefMut<Target = Self::Shared> {
        self.0.lock().await
    }

    fn try_access(&self) -> Option<impl core::ops::DerefMut<Target = Self::Shared>> {
        self.0.try_lock().ok()
    }
}
