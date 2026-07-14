pub mod active_config;
pub mod client;
pub mod dispatch;
pub mod network;
pub mod serializer;
pub mod signal;
pub mod time_adjust;

use crate::logic::dispatch::confirmable::Confirmable;
use sensor_link_protocol::{TopicPayloadSerialize, MAX_MESSAGE_LEN};

/// Platform-agnostic channel sender trait.
///
/// Provides a common interface that the various mpsc sender implementations
/// (Tokio, RTIC, …) adhere to, so logic can send messages without knowing the
/// concrete queue implementation.
pub trait SendChannel<T> {
    type Error: core::fmt::Debug;
    async fn send(&mut self, val: T) -> Result<(), Self::Error>;
    fn try_send(&mut self, val: T) -> Result<(), T>;

    /// Returns true if the channel is (likely) ready to accept a message
    ///
    /// Can be used as a hint that the next send is unlikely to block.
    /// Note: if there is only one sender, the next send is guaranteed to succeed,
    /// but with multiple senders (e.g. multi-producer channel), the next send may still block
    /// if another sender has just sent a message.
    fn is_ready(&self) -> bool;
}

pub trait ReceiveChannel<T> {
    type Error: core::fmt::Debug;
    async fn recv(&mut self) -> Result<T, Self::Error>;
    fn try_recv(&mut self) -> Result<T, Self::Error>;
}

impl<'a, R, T> ReceiveChannel<T> for &'a mut R
where
    R: ReceiveChannel<T>,
{
    type Error = R::Error;

    #[inline]
    async fn recv(&mut self) -> Result<T, Self::Error> {
        R::recv(self).await
    }

    #[inline]
    fn try_recv(&mut self) -> Result<T, Self::Error> {
        R::try_recv(self)
    }
}

pub trait LatestValueSendChannel<T> {
    type Error: core::fmt::Debug;
    fn send(&mut self, val: T) -> Result<(), Self::Error>;
}

pub trait LatestValueReceiveChannel<T: Clone> {
    type Error: core::fmt::Debug;
    async fn recv(&mut self) -> Result<T, Self::Error>;
}

/// A trait for types that can notify about changes and allow waiting for changes.
///
/// This trait provides a mechanism for notifying listeners about state changes
/// and allowing consumers to await such changes asynchronously.
pub trait ChangeNotifier {
    /// Notifies all listeners that a change has occurred.
    fn notify(&self);

    /// Asynchronously waits until a change notification is received.
    ///
    /// This method will suspend the current task until a change is notified
    /// via the `notify` method.
    async fn await_change(&self);
}

#[derive(Debug, Clone)]
pub enum FiniteStream<T> {
    Data(T),
    /// Reset signal: hint to consumer that data stream was interrupted
    /// (significant discontinuity). Consumer may drop/reset per session state
    /// and start fresh, but the the session itself continues.
    Reset,
    End,
}

/// Lets a message queue expose only its "background" channels (trace, stream) for
/// draining after a disconnect hint, while NOT consuming the primary upload channel
/// (so fresh dispatch data back-pressures into the persistent store for proper bulk
/// batching on the next session).
///
/// Implementors with no background channels (the hub upload receivers) return `None`.
pub trait DrainOnDisconnect<U> {
    fn try_recv_drain_only(&mut self) -> Option<Confirmable<U>>;
}

/// A device-status payload that the network task can publish.
///
/// Kept generic so the network plumbing ([`NetworkAction`] and friends) stays
/// free of any concrete status type; an implementation pins it to its own
/// status type.
pub trait NetworkStatus: TopicPayloadSerialize<MAX_MESSAGE_LEN> {
    /// Inject the live modem signal strength, sampled by the network client at
    /// send time.
    fn set_signal_strength(&mut self, dbm: i32);
}

/// Requests to send to the network connection.
/// These will request the network connection to send data or to do other stuff.
///
/// Generic over the status payload `S` so this stays sensor-agnostic.
#[derive(Debug, Clone)]
pub enum NetworkAction<S> {
    Disconnect,
    SendDeviceInfo,
    SendStatus(S),
    DownloadUpdate,

    /// New timeout value for the network task
    SetTimeout(u32),
}

impl<S> NetworkAction<S> {
    pub fn name(&self) -> &'static str {
        match self {
            NetworkAction::Disconnect => "Disconnect",
            NetworkAction::SendDeviceInfo => "SendDeviceInfo",
            NetworkAction::SendStatus(_) => "SendStatus",
            NetworkAction::DownloadUpdate => "DownloadUpdate",
            NetworkAction::SetTimeout(_) => "SetTimeout",
        }
    }
}

/// Writer-part of a network-action notifier.
///
/// Trait may be used to hide the generic bounds of a concrete notifier.
pub trait NetworkActionNotifyWriter<S> {
    /// Returns Err(()) if the network task is blocked
    async fn set(&self, action: NetworkAction<S>) -> Result<(), ()>;
    async fn clear_pending_disconnect(&self);
}

/// Reader-part of a network-action notifier.
///
/// Trait may be used to hide the generic bounds of a concrete notifier.
pub trait NetworkActionNotifyReader<S> {
    async fn next_action(&self) -> NetworkAction<S>;
    fn try_next_action(&self) -> Option<NetworkAction<S>>;
}
