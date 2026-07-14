//! ReservingSender - A channel sender that can await free space before sending.
//!
//! This module provides a wrapper around any SendChannel implementation. Combined with
//! a ChangeNotifier, it provides a ReservableSender trait that allows the sender to
//! await free space before sending.
//!
//! This is useful for sending large or non-`Clone` data, where the overhead of
//! repeatedly cloning the data just to attempt a send is not feasible.

use crate::logic::{
    dispatch::confirmable::Confirmable, ChangeNotifier, DrainOnDisconnect, ReceiveChannel,
    SendChannel,
};

/// Trait for reservation tokens that can send messages without blocking.
///
/// This trait abstracts away the complex generic parameters of Reservation,
/// making it easier to work with reservations in generic code.
pub trait ReservationToken<T> {
    /// Send the message using this reservation.
    /// This consumes the reservation and should not block.
    ///
    /// Guaranteed to succeed *unless* the underlying channel has multiple senders.
    /// Using multiple senders is not recommended, as the reservation can be 'overbooked'
    /// by another sender filling the queue. In that case `try_send` will fail.
    fn try_send(self, value: T) -> Result<(), T>;
}

/// Trait for senders that can reserve space before sending.
///
/// This trait abstracts away the complex generic parameters of ReservingSender,
/// making it easier to use in function signatures and trait bounds.
pub trait ReservableSender<T>: SendChannel<T> {
    /// The type of reservation returned by `reserve()`.
    type Reservation<'r>: ReservationToken<T>
    where
        Self: 'r;

    /// Reserve space for sending a message.
    ///
    /// This method will await until space is available in the underlying channel.
    /// It returns a Reservation token that can be used to send without blocking.
    async fn reserve(&mut self) -> Self::Reservation<'_>;
}

/// A reservation token that guarantees space is available for sending
/// *if* the underlying channel has no other senders.
///
/// This token can be used to send a message without blocking, or it can be
/// dropped at any time without any side effects.
pub struct Reservation<'r, 'ch, T, S, N> {
    sender: &'r mut ReservingSender<'ch, T, S, N>,
}

impl<'r, 'ch, T, S, N> Reservation<'r, 'ch, T, S, N>
where
    S: SendChannel<T>,
    N: ChangeNotifier,
{
    /// Send the message using this reservation.
    /// This consumes the reservation and guarantees the send will not block.
    ///
    /// If the underlying channel has no other senders, the send is guaranteed to succeed
    pub fn try_send(self, value: T) -> Result<(), T> {
        self.sender.try_send(value)
    }
}

impl<'r, 'ch, T, S, N> ReservationToken<T> for Reservation<'r, 'ch, T, S, N>
where
    S: SendChannel<T>,
    N: ChangeNotifier,
{
    fn try_send(self, value: T) -> Result<(), T> {
        self.try_send(value)
    }
}

/// A sender that can reserve space before sending.
///
/// A wrapper around a SendChannel with a ChangeNotifier to allow senders
/// to await available space before attempting to send messages.
///
/// The biggest advantage is for sending data that is not `Clone`:
///
/// For example, if the usual [SendChannel::send] is used in a `select` statement
/// and the future is not completed while the channel is full, the data is consumed
/// and thus lost. The only workaround is to clone the data on every attempt.
///
/// With ReservingSender, the process is split into two steps:
/// 1. [ReservableSender::reserve]: await may be canceled at any time (does not consume anything yet
/// 2. [ReservationToken::try_send]: only consumes the data if it can be sent immediately (non-async)
pub struct ReservingSender<'ch, T, S, N> {
    inner: S,
    space_notifier: &'ch N,
    _phantom: core::marker::PhantomData<T>,
}

impl<'ch, T, S, N> ReservingSender<'ch, T, S, N>
where
    S: SendChannel<T>,
    N: ChangeNotifier,
{
    /// Create a new ReservingSender wrapping the given SendChannel and ChangeNotifier.
    fn new(sender: S, space_notifier: &'ch N) -> Self {
        Self {
            inner: sender,
            space_notifier,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<'ch, T, S, N> ReservingSender<'ch, T, S, N>
where
    S: SendChannel<T>,
    N: ChangeNotifier,
{
    /// Reserve space for sending a message.
    ///
    /// This method will await until space is available in the underlying channel.
    /// It returns a Reservation token that guarantees the next send will not block.
    pub async fn reserve(&mut self) -> Reservation<'_, 'ch, T, S, N>
    where
        S: SendChannel<T>,
    {
        loop {
            // Check if the channel is ready to accept a message
            if self.inner.is_ready() {
                return Reservation { sender: self };
            }

            // Wait for space to become available
            self.space_notifier.await_change().await;
        }
    }

    /// Send a message directly without reserving space first.
    /// This is equivalent to calling the underlying SendChannel::send.
    #[inline]
    pub async fn send(&mut self, value: T) -> Result<(), S::Error>
    where
        S: SendChannel<T>,
    {
        self.inner.send(value).await
    }

    /// Try to send a message immediately without blocking.
    /// This is equivalent to calling the underlying SendChannel::try_send.
    #[inline]
    pub fn try_send(&mut self, value: T) -> Result<(), T>
    where
        S: SendChannel<T>,
    {
        self.inner.try_send(value)
    }
}

impl<'ch, T, S, N> ReservableSender<T> for ReservingSender<'ch, T, S, N>
where
    S: SendChannel<T>,
    N: ChangeNotifier,
{
    type Reservation<'r>
        = Reservation<'r, 'ch, T, S, N>
    where
        Self: 'r;

    #[inline]
    async fn reserve(&mut self) -> Self::Reservation<'_> {
        self.reserve().await
    }
}

impl<'ch, T, S, N> SendChannel<T> for ReservingSender<'ch, T, S, N>
where
    S: SendChannel<T>,
    N: ChangeNotifier,
{
    type Error = S::Error;

    #[inline]
    async fn send(&mut self, val: T) -> Result<(), Self::Error> {
        self.send(val).await
    }

    #[inline]
    fn try_send(&mut self, val: T) -> Result<(), T> {
        self.try_send(val)
    }

    #[inline]
    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }
}

/// A wrapper around a ReceiveChannel that notifies when messages are received.
///
/// This receiver automatically notifies the associated ChangeNotifier whenever
/// a message is successfully received, allowing ReservingSenders to wake up
/// when space becomes available.
pub struct NotifyingReceiver<'ch, T, R, N> {
    inner: R,
    space_notifier: &'ch N,
    _phantom: core::marker::PhantomData<T>,
}

impl<'ch, T, R, N> NotifyingReceiver<'ch, T, R, N>
where
    R: ReceiveChannel<T>,
    N: ChangeNotifier,
{
    /// Create a new NotifyingReceiver wrapping the given ReceiveChannel and ChangeNotifier.
    /// Private API, see create_reserving_channel.
    fn new(receiver: R, space_notifier: &'ch N) -> Self {
        Self {
            inner: receiver,
            space_notifier,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<'ch, T, R, N> ReceiveChannel<T> for NotifyingReceiver<'ch, T, R, N>
where
    R: ReceiveChannel<T>,
    N: ChangeNotifier,
{
    type Error = R::Error;

    async fn recv(&mut self) -> Result<T, Self::Error> {
        let result = self.inner.recv().await;
        if result.is_ok() {
            // Notify that space is now available
            self.space_notifier.notify();
        }
        result
    }

    fn try_recv(&mut self) -> Result<T, Self::Error> {
        let result = self.inner.try_recv();
        if result.is_ok() {
            // Notify that space is now available
            self.space_notifier.notify();
        }
        result
    }
}

// NotifyingReceiver wraps a single upload channel; there are no separate
// background channels to drain on a disconnect hint.
impl<'ch, T, R, N, U> DrainOnDisconnect<U> for NotifyingReceiver<'ch, T, R, N> {
    fn try_recv_drain_only(&mut self) -> Option<Confirmable<U>> {
        None
    }
}

/// Create a reserving channel from the given components.
///
/// This function combines a SendChannel, ReceiveChannel, and ChangeNotifier to create
/// a matched pair where the receiver automatically notifies the sender when space
/// becomes available.
///
/// Returns a tuple of (ReservingSender, NotifyingReceiver) that are connected
/// via the shared ChangeNotifier.
pub fn create_reserving_channel<'ch, T, S, R, N>(
    sender: S,
    receiver: R,
    space_notifier: &'ch N,
) -> (
    ReservingSender<'ch, T, S, N>,
    NotifyingReceiver<'ch, T, R, N>,
)
where
    S: SendChannel<T>,
    R: ReceiveChannel<T>,
    N: ChangeNotifier,
{
    let reserving_sender = ReservingSender::new(sender, space_notifier);
    let notifying_receiver = NotifyingReceiver::new(receiver, space_notifier);
    (reserving_sender, notifying_receiver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::{ChangeNotifier, ReceiveChannel, SendChannel};

    // Mock implementations for testing
    struct MockSender<T> {
        capacity: usize,
        sent_items: Vec<T>,
    }

    impl<T> MockSender<T> {
        fn new(capacity: usize) -> Self {
            Self {
                capacity,
                sent_items: Vec::new(),
            }
        }

        fn is_full(&self) -> bool {
            self.sent_items.len() >= self.capacity
        }
    }

    impl<T> SendChannel<T> for MockSender<T> {
        type Error = ();

        async fn send(&mut self, val: T) -> Result<(), Self::Error> {
            if self.is_full() {
                Err(())
            } else {
                self.sent_items.push(val);
                Ok(())
            }
        }

        fn try_send(&mut self, val: T) -> Result<(), T> {
            if self.is_full() {
                Err(val)
            } else {
                self.sent_items.push(val);
                Ok(())
            }
        }

        fn is_ready(&self) -> bool {
            !self.is_full()
        }
    }

    struct MockNotifier {
        notified: core::cell::Cell<bool>,
        change_count: core::cell::Cell<usize>,
    }

    impl MockNotifier {
        fn new() -> Self {
            Self {
                notified: core::cell::Cell::new(false),
                change_count: core::cell::Cell::new(0),
            }
        }

        fn was_notified(&self) -> bool {
            self.notified.get()
        }
    }

    impl ChangeNotifier for MockNotifier {
        fn notify(&self) {
            self.notified.set(true);
            self.change_count.set(self.change_count.get() + 1);
        }

        async fn await_change(&self) {
            // In a real implementation, this would block until notify() is called
            // For testing, we simulate this by only allowing one await_change per notify
            // This prevents infinite loops in tests while still testing the logic

            // If we've been notified, reset and return
            if self.notified.get() {
                self.notified.set(false);
                return;
            }

            // Otherwise, this would block indefinitely in a real implementation
            // For testing purposes, we'll panic to catch incorrect usage
            panic!("await_change() called but no notification available - this would block forever in real usage");
        }
    }

    #[tokio::test]
    async fn test_reserving_sender_basic() {
        let sender = MockSender::new(2);
        let notifier = MockNotifier::new();
        let mut reserving_sender = ReservingSender::new(sender, &notifier);

        // Test direct send
        let result = reserving_sender.send(42).await;
        assert!(result.is_ok());
        assert_eq!(reserving_sender.inner.sent_items.len(), 1);
        assert_eq!(reserving_sender.inner.sent_items[0], 42);
    }

    #[tokio::test]
    async fn test_reserving_sender_try_send() {
        let sender = MockSender::new(1);
        let notifier = MockNotifier::new();
        let mut reserving_sender = ReservingSender::new(sender, &notifier);

        // First send should succeed
        let result = reserving_sender.try_send(1);
        assert!(result.is_ok());

        // Second send should fail (capacity exceeded)
        let result = reserving_sender.try_send(2);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 2);
    }

    // Mock receiver for testing
    struct MockReceiver<T> {
        items: Vec<T>,
        current_index: usize,
    }

    impl<T> MockReceiver<T> {
        fn new(items: Vec<T>) -> Self {
            Self {
                items,
                current_index: 0,
            }
        }
    }

    impl<T: Clone> ReceiveChannel<T> for MockReceiver<T> {
        type Error = ();

        async fn recv(&mut self) -> Result<T, Self::Error> {
            if self.current_index < self.items.len() {
                let item = self.items[self.current_index].clone();
                self.current_index += 1;
                Ok(item)
            } else {
                Err(())
            }
        }

        fn try_recv(&mut self) -> Result<T, Self::Error> {
            if self.current_index < self.items.len() {
                let item = self.items[self.current_index].clone();
                self.current_index += 1;
                Ok(item)
            } else {
                Err(())
            }
        }
    }

    #[tokio::test]
    async fn test_notifying_receiver() {
        let receiver = MockReceiver::new(vec![1, 2, 3]);
        let notifier = MockNotifier::new();
        let mut notifying_receiver = NotifyingReceiver::new(receiver, &notifier);

        // Receive a message
        let result = notifying_receiver.recv().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        // Check that the notifier was called
        assert!(notifying_receiver.space_notifier.was_notified());
    }

    #[tokio::test]
    async fn test_reserving_channel() {
        let sender = MockSender::new(2);
        let receiver = MockReceiver::new(vec![1, 2]);
        let notifier = MockNotifier::new();

        let (mut reserving_sender, mut notifying_receiver) =
            create_reserving_channel(sender, receiver, &notifier);

        // Send a message
        let result = reserving_sender.send(42).await;
        assert!(result.is_ok());

        // Receive a message (this should notify the sender)
        let result = notifying_receiver.recv().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_reserve_with_available_space() {
        let sender = MockSender::new(2);
        let notifier = MockNotifier::new();
        let mut reserving_sender = ReservingSender::new(sender, &notifier);

        // Channel has space, reserve should return immediately
        let reservation = reserving_sender.reserve().await;

        // Use the reservation to send
        let result = reservation.try_send(42);
        assert!(result.is_ok());

        // Verify the message was sent
        assert_eq!(reserving_sender.inner.sent_items.len(), 1);
        assert_eq!(reserving_sender.inner.sent_items[0], 42);
    }

    #[tokio::test]
    async fn test_reserve_notification_mechanism() {
        let sender = MockSender::new(2);
        let notifier = MockNotifier::new();
        let mut reserving_sender = ReservingSender::new(sender, &notifier);

        // Fill the channel partially
        let result = reserving_sender.try_send(1);
        assert!(result.is_ok());

        // Channel still has space, so reserve should work immediately
        let reservation = reserving_sender.reserve().await;

        // Use the reservation
        let result = reservation.try_send(2);
        assert!(result.is_ok());

        // Verify both messages were sent
        assert_eq!(reserving_sender.inner.sent_items.len(), 2);
        assert_eq!(reserving_sender.inner.sent_items[0], 1);
        assert_eq!(reserving_sender.inner.sent_items[1], 2);
    }

    #[tokio::test]
    async fn test_reservation_can_be_dropped() {
        let sender = MockSender::new(2);
        let notifier = MockNotifier::new();
        let mut reserving_sender = ReservingSender::new(sender, &notifier);

        // Get a reservation
        let reservation = reserving_sender.reserve().await;

        // Drop the reservation without using it (should not panic or leak)
        drop(reservation);

        // Channel should still work normally
        let result = reserving_sender.try_send(42);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_multiple_reservations() {
        let sender = MockSender::new(3);
        let notifier = MockNotifier::new();
        let mut reserving_sender = ReservingSender::new(sender, &notifier);

        // Get first reservation and use it
        let reservation1 = reserving_sender.reserve().await;
        let result = reservation1.try_send(1);
        assert!(result.is_ok());

        // Get second reservation and use it
        let reservation2 = reserving_sender.reserve().await;
        let result = reservation2.try_send(2);
        assert!(result.is_ok());

        // Verify both messages were sent
        assert_eq!(reserving_sender.inner.sent_items.len(), 2);
        assert_eq!(reserving_sender.inner.sent_items[0], 1);
        assert_eq!(reserving_sender.inner.sent_items[1], 2);
    }
}
