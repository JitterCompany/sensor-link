//! RTIC implementations of the channel traits the platform-agnostic logic in
//! `demo_firmware_core` is written against.

use rtic_sync::channel::{self, ReceiveError};

use demo_firmware_core::{
    embassy_sync::{
        blocking_mutex::raw::CriticalSectionRawMutex,
        signal::{self, Signal},
    },
    logic::{
        dispatch::confirmable::Confirmable, ChangeNotifier, DrainOnDisconnect,
        LatestValueReceiveChannel, LatestValueSendChannel, ReceiveChannel, SendChannel,
    },
    traits,
};

/// A bounded channel of `$type` with `$len` slots: `<T>Tx`/`<T>Rx` type
/// aliases and a `create_<t>_channel()` constructor.
macro_rules! define_channel {
    ($type:tt, $len:expr) => {
        paste::paste! {

            pub type [<$type Rx>] = $crate::channels::Receiver<$type, $len>;
            pub type [<$type Tx>] = $crate::channels::Sender<$type, $len>;

            pub fn [<create_ $type:lower _channel>] () -> ([<$type Tx>], [<$type Rx>]) {
                let (tx, rx) = rtic_sync::make_channel!($type, $len);
                ($crate::channels::Sender::<$type, $len>(tx), $crate::channels::Receiver::<$type, $len>(rx))
            }
        }
    };
}

/// A latest-value signal of `$type`: `<T>Tx`/`<T>Rx` type aliases and a
/// `create_<t>_signal()` constructor that panics when called twice.
macro_rules! define_signal {
    ($type:tt) => {
        paste::paste! {

            pub type [<$type Rx>] = $crate::channels::LatestValueReceiver<'static, $type>;
            pub type [<$type Tx>] = $crate::channels::LatestValueSender<'static, $type>;

            /// Create latest value sender/receiver pair. Panics if called twice
            pub fn [<create_ $type:lower _signal>] () -> ([<$type Tx>], [<$type Rx>]) {
                static BORROWED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
                static CHANNEL: demo_firmware_core::embassy_sync::signal::Signal<
                    demo_firmware_core::embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
                    $type,
                > = demo_firmware_core::embassy_sync::signal::Signal::new();
                BORROWED.compare_exchange(false, true,
                    core::sync::atomic::Ordering::Relaxed,
                    core::sync::atomic::Ordering::Relaxed).expect("Cannot create same signal twice!");
                $crate::channels::create_latest_value_channel(&CHANNEL)
            }
        }
    };
}

/// A session channel of `$type`: a data channel of `FiniteStream<$type>` plus
/// the latest-value signal that carries the session state.
macro_rules! define_session_channel {
    ($type:tt, $len:expr) => {
        paste::paste! {
            pub type [<$type SessionData>] = demo_firmware_core::logic::FiniteStream<$type>;
            pub type [<$type SessionState>] = demo_firmware_core::sync::session_channel::SessionState;

            define_channel!([<$type SessionData>], $len);
            define_signal!([<$type SessionState>]);

            pub type [<$type Tx>] = demo_firmware_core::sync::session_channel::SessionSender<
                $type,
                $crate::channels::Sender<[<$type SessionData>], $len>,
                $crate::channels::LatestValueReceiver<'static, demo_firmware_core::sync::session_channel::SessionState>
            >;
            pub type [<$type Rx>] = demo_firmware_core::sync::session_channel::SessionReceiver<
                $type,
                $crate::channels::Receiver<[<$type SessionData>], $len>,
                $crate::channels::LatestValueSender<'static, demo_firmware_core::sync::session_channel::SessionState>
            >;

            pub fn [<create_ $type:lower _session_channel>]() -> (
                [<$type Tx>],
                [<$type Rx>],
            ) {
                let (tx, rx) = [<create_ $type:lower sessiondata_channel>]();
                let (sig_tx, sig_rx) = [<create_ $type:lower sessionstate_signal>]();
                demo_firmware_core::sync::session_channel::create_session_channel(tx, rx, sig_tx, sig_rx)
            }
        }
    };
}

#[derive(Debug)]
pub struct Receiver<T: 'static, const N: usize>(pub channel::Receiver<'static, T, N>);

impl<T, const N: usize> ReceiveChannel<T> for Receiver<T, N> {
    type Error = ReceiveError;

    async fn recv(&mut self) -> Result<T, Self::Error> {
        self.0.recv().await
    }

    fn try_recv(&mut self) -> Result<T, Self::Error> {
        self.0.try_recv()
    }
}

// A bare upload receiver has no separate background channels, so there is
// nothing extra to drain on a disconnect hint.
impl<T: 'static, const N: usize, U> DrainOnDisconnect<U> for Receiver<T, N> {
    fn try_recv_drain_only(&mut self) -> Option<Confirmable<U>> {
        None
    }
}

#[derive(Debug)]
pub struct Sender<T: 'static, const N: usize>(pub channel::Sender<'static, T, N>);

// Derive Clone for doesn't work for Sender
impl<T: 'static, const N: usize> Clone for Sender<T, N> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T, const N: usize> SendChannel<T> for Sender<T, N> {
    type Error = (); // Means no receivers

    async fn send(&mut self, sig: T) -> Result<(), Self::Error> {
        self.0.send(sig).await.map_err(|_| ())
    }

    fn try_send(&mut self, sig: T) -> Result<(), T> {
        self.0.try_send(sig).map_err(|err| match err {
            channel::TrySendError::NoReceiver(el) => el,
            channel::TrySendError::Full(el) => el,
        })
    }

    fn is_ready(&self) -> bool {
        !self.0.is_full()
    }
}

pub struct Arbiter<T>(rtic_sync::arbiter::Arbiter<T>);

impl<T> Arbiter<T> {
    pub const fn new(inner: T) -> Self {
        Self(rtic_sync::arbiter::Arbiter::new(inner))
    }
}

impl<T> traits::Arbiter for Arbiter<T> {
    type Shared = T;

    async fn access(&self) -> impl core::ops::DerefMut<Target = T> {
        self.0.access().await
    }

    fn try_access(&self) -> Option<impl core::ops::DerefMut<Target = T>> {
        self.0.try_access()
    }
}

pub struct LatestValueSender<'a, T> {
    inner: &'a signal::Signal<CriticalSectionRawMutex, T>,
}

impl<'a, T: Send> LatestValueSender<'a, T> {
    fn new(signal: &'a signal::Signal<CriticalSectionRawMutex, T>) -> Self {
        Self { inner: signal }
    }
}

impl<T: Send> LatestValueSendChannel<T> for LatestValueSender<'_, T> {
    type Error = ();

    fn send(&mut self, val: T) -> Result<(), Self::Error> {
        self.inner.signal(val);
        Ok(())
    }
}

pub struct LatestValueReceiver<'a, T> {
    inner: &'a signal::Signal<CriticalSectionRawMutex, T>,
}

impl<'a, T: Send> LatestValueReceiver<'a, T> {
    fn new(signal: &'a signal::Signal<CriticalSectionRawMutex, T>) -> Self {
        Self { inner: signal }
    }

    /// Take the latest value without waiting, if a new one was signalled.
    ///
    /// Only for signals that are not concurrently awaited: a Pending poll
    /// replaces the signal's registered waker.
    pub fn try_recv(&mut self) -> Option<T> {
        use demo_firmware_core::futures::FutureExt as _;
        self.inner.wait().now_or_never()
    }
}

impl<T: Send + Clone> LatestValueReceiveChannel<T> for LatestValueReceiver<'_, T> {
    type Error = ();

    async fn recv(&mut self) -> Result<T, Self::Error> {
        Ok(self.inner.wait().await)
    }
}

pub fn create_latest_value_channel<T: Send>(
    signal: &signal::Signal<CriticalSectionRawMutex, T>,
) -> (LatestValueSender<'_, T>, LatestValueReceiver<'_, T>) {
    (
        LatestValueSender::new(signal),
        LatestValueReceiver::new(signal),
    )
}

pub struct ChangeNotification {
    ch: Signal<CriticalSectionRawMutex, ()>,
}

impl ChangeNotification {
    pub const fn new() -> Self {
        Self { ch: Signal::new() }
    }
}

impl ChangeNotifier for ChangeNotification {
    fn notify(&self) {
        self.ch.signal(());
    }

    async fn await_change(&self) {
        self.ch.wait().await;
    }
}
