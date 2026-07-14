use crate::logic::{
    FiniteStream, LatestValueReceiveChannel, LatestValueSendChannel, ReceiveChannel, SendChannel,
};

/// Lifecycle signal carrying the session identifier
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionState {
    None,
    Started(u32),
    Finished(u32),
}

/// Sender side of a sessioned finite stream
pub struct SessionSender<T, S, LR> {
    data_tx: S,
    lifecycle_rx: LR,
    state: SessionState,
    _phantom: core::marker::PhantomData<T>,
}

/// Receiver side that can start a session and yield an active guard
pub struct SessionReceiver<T, R, LS> {
    data_rx: R,
    lifecycle_tx: LS,
    session_id: u32,
    _phantom: core::marker::PhantomData<T>,
}

/// Active receiver guard. Finishes on drop by sending Finished(session_id)
pub struct SessionActive<'ch, T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    data_rx: &'ch mut R,
    lifecycle_tx: &'ch mut LS,
    should_finish: bool,
    session_id: u32,
    _phantom: core::marker::PhantomData<T>,
}

/// Errors for finishing/waiting lifecycle
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FinishError {
    /// Error: could not communicate finish signal from sender to receiver
    Send,

    /// Error: could not communicate feedback from receiver to sender
    Feedback,

    /// Warning: session was already restarted while we were waiting for it to finish
    Restarted(u32),

    /// Warning: (multiple?) other sessions were already finished while we were waiting for it to finish
    OutOfSync,
}

/// Errors for waiting on session start
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StartError {
    /// Error receiving feedback from receiver
    Feedback,
}

impl<T, S, LR> SessionSender<T, S, LR>
where
    S: SendChannel<FiniteStream<T>>,
    LR: LatestValueReceiveChannel<SessionState>,
{
    pub fn new(data_tx: S, lifecycle_rx: LR) -> Self {
        Self {
            data_tx,
            lifecycle_rx,
            state: SessionState::Finished(0),
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<T, S, LR> traits::SessionSender<T> for SessionSender<T, S, LR>
where
    S: SendChannel<FiniteStream<T>>,
    LR: LatestValueReceiveChannel<SessionState>,
{
    type DataError = S::Error;

    async fn wait_started(&mut self) -> Result<u32, StartError> {
        loop {
            self.state = self
                .lifecycle_rx
                .recv()
                .await
                .map_err(|_| StartError::Feedback)?;
            if let SessionState::Started(session_id) = self.state {
                return Ok(session_id);
            }
        }
    }

    async fn send_reset(&mut self) -> Result<(), S::Error> {
        self.data_tx.send(FiniteStream::Reset).await
    }

    async fn finish_and_wait(&mut self) -> Result<(), FinishError> {
        self.data_tx
            .send(FiniteStream::End)
            .await
            .map_err(|_| FinishError::Send)?;

        let old_state = self.state;
        let sess_id = match old_state {
            SessionState::Finished(_) => return Ok(()),
            SessionState::Started(sess_id) => Some(sess_id),
            SessionState::None => None,
        };
        self.state = self
            .lifecycle_rx
            .recv()
            .await
            .map_err(|_| FinishError::Feedback)?;

        match self.state {
            SessionState::Finished(finish_id) => {
                let start_id = sess_id.unwrap_or(finish_id);
                if start_id == finish_id {
                    Ok(())
                } else {
                    Err(FinishError::OutOfSync)
                }
            }
            SessionState::Started(start_id) => Err(FinishError::Restarted(start_id)),
            SessionState::None => {
                debug_assert!(false); // should never receive "None" state
                Err(FinishError::OutOfSync)
            }
        }
    }
}

impl<T, R, LS> SessionReceiver<T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    pub fn new(data_rx: R, lifecycle_tx: LS) -> Self {
        Self {
            data_rx,
            lifecycle_tx,
            session_id: 0,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<T, R, LS> traits::SessionReceiver<T> for SessionReceiver<T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    type Error = LS::Error;
    type Active<'a>
        = SessionActive<'a, T, R, LS>
    where
        Self: 'a;

    fn start(&mut self) -> Result<Self::Active<'_>, Self::Error> {
        let session_id = self.session_id;
        self.lifecycle_tx.send(SessionState::Started(session_id))?;
        self.session_id = session_id.wrapping_add(1);

        Ok(SessionActive {
            data_rx: &mut self.data_rx,
            lifecycle_tx: &mut self.lifecycle_tx,
            should_finish: false,
            session_id,
            _phantom: core::marker::PhantomData,
        })
    }
}

impl<'ch, T, R, LS> SessionActive<'ch, T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    fn _inner_finish(&mut self) -> Result<(), LS::Error> {
        // empty the queue (in case of stale data or duplicate End markers)
        while let Ok(_) = self.data_rx.try_recv() {}

        self.lifecycle_tx
            .send(SessionState::Finished(self.session_id))
    }
}

impl<'ch, T, R, LS> traits::SessionActive<T> for SessionActive<'ch, T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    /// Drop all incoming data until the sender requests us to finish the session
    async fn flush_until_finish(&mut self) -> Result<(), R::Error> {
        while !self.should_finish {
            self.recv().await?;
        }
        Ok(())
    }

    /// Finish the session
    ///
    /// Notifies the sender the session is finished and flushes the queue.
    ///
    /// Consumes the session, same as drop(self).
    fn finish(mut self) -> Result<(), FinishError> {
        self._inner_finish().map_err(|_| FinishError::Feedback)
    }
}

impl<'ch, T, R, LS> Drop for SessionActive<'ch, T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    fn drop(&mut self) {
        let _ = self._inner_finish();
    }
}

impl<'ch, T, R, LS> ReceiveChannel<FiniteStream<T>> for SessionActive<'ch, T, R, LS>
where
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
{
    type Error = R::Error;

    async fn recv(&mut self) -> Result<FiniteStream<T>, R::Error> {
        if self.should_finish {
            return Ok(FiniteStream::End);
        }
        let data = self.data_rx.recv().await?;
        if let FiniteStream::End = data {
            self.should_finish = true;
        }
        Ok(data)
    }

    fn try_recv(&mut self) -> Result<FiniteStream<T>, R::Error> {
        if self.should_finish {
            return Ok(FiniteStream::End);
        }
        let data = self.data_rx.try_recv()?;
        if let FiniteStream::End = data {
            self.should_finish = true;
        }
        Ok(data)
    }
}

impl<T, S, LR> SendChannel<T> for SessionSender<T, S, LR>
where
    S: SendChannel<FiniteStream<T>>,
    LR: LatestValueReceiveChannel<SessionState>,
{
    type Error = S::Error;

    async fn send(&mut self, value: T) -> Result<(), S::Error> {
        self.data_tx.send(FiniteStream::Data(value)).await
    }

    fn try_send(&mut self, value: T) -> Result<(), T> {
        self.data_tx
            .try_send(FiniteStream::Data(value))
            .map_err(|fs| match fs {
                FiniteStream::Data(v) => v,
                _ => unreachable!(),
            })
    }

    fn is_ready(&self) -> bool {
        self.data_tx.is_ready()
    }
}

pub fn create_session_channel<T, S, R, LS, LR>(
    sender: S,
    receiver: R,
    lifecycle_tx: LS,
    lifecycle_rx: LR,
) -> (SessionSender<T, S, LR>, SessionReceiver<T, R, LS>)
where
    S: SendChannel<FiniteStream<T>>,
    R: ReceiveChannel<FiniteStream<T>>,
    LS: LatestValueSendChannel<SessionState>,
    LR: LatestValueReceiveChannel<SessionState>,
{
    (
        SessionSender::new(sender, lifecycle_rx),
        SessionReceiver::new(receiver, lifecycle_tx),
    )
}

pub mod traits {

    use super::*;

    /// Trait for receiving data organized in a session
    ///
    /// Start a session via `start` and receive data via the returned `ReceiveChannel`.
    /// When the `ReceiveChannel` returns `FiniteStream::End`, the session is finished
    /// and should be dropped. This notifies the sender that all data was handled and the session is complete.
    ///
    /// The sender may start another session after the current one is finished.
    pub trait SessionReceiver<T> {
        type Error: core::fmt::Debug;
        type Active<'a>: SessionActive<T>
        where
            Self: 'a;
        fn start(&mut self) -> Result<Self::Active<'_>, Self::Error>;
    }

    pub trait SessionActive<T>: ReceiveChannel<FiniteStream<T>> {
        async fn flush_until_finish(&mut self) -> Result<(), Self::Error>;
        fn finish(self) -> Result<(), FinishError>;
    }

    pub trait SessionSender<T>: SendChannel<T> {
        /// Associated error type for the underlying data channel.
        type DataError;

        /// Wait untill the receiver has started the session
        ///
        /// Optional, but recommended if the receiver may take some time to initialize
        /// while it is generally expected to handle data with low latency
        ///
        /// Returns the session id (increments on every session start)
        async fn wait_started(&mut self) -> Result<u32, StartError>;

        /// Send a mid-stream reset marker.
        ///
        /// The session stays open; the receiver is expected to drop per-session state
        /// (filters, accumulators, etc.) and continue. Use this when the producer detects
        /// a discontinuity that would contaminate downstream state but the session is
        /// otherwise still valid.
        async fn send_reset(&mut self) -> Result<(), Self::DataError>;

        /// Finish the session and wait untill the receiver has finished
        ///
        /// This signals to the receiver that no further data will be sent.
        /// The receiver should process any remaining data and finish the session.
        ///
        /// After finishing the session, the receiver *may* start another session depending on the application.
        /// In that case it is recommended to call `wait_started` again in the sender to keep the sessions synchronized.
        async fn finish_and_wait(&mut self) -> Result<(), FinishError>;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        traits::{SessionActive as _, SessionReceiver as _, SessionSender as _},
        *,
    };
    use crate::utils::channels as tch;
    use core::time::Duration;

    #[tokio::test]
    async fn session_happy_path() {
        let (data_tx, data_rx) = tch::make_channel::<FiniteStream<u8>>(8);
        let (sig_tx, sig_rx) = tch::make_latest_value_channel(SessionState::Finished(0));
        let (mut s, mut r) =
            create_session_channel::<u8, _, _, _, _>(data_tx, data_rx, sig_tx, sig_rx);

        let mut active = r.start().expect("start ok");
        let sid = s.wait_started().await.unwrap();
        assert_eq!(sid, 0);

        s.send(1).await.unwrap();
        s.try_send(2).unwrap();
        assert!(s.is_ready());
        assert!(matches!(
            active.recv().await.unwrap(),
            FiniteStream::Data(1)
        ));
        assert!(matches!(active.try_recv().unwrap(), FiniteStream::Data(2)));

        let fin = tokio::spawn(async move { s.finish_and_wait().await });
        // consume until End then drop to send Finished
        loop {
            if let FiniteStream::End = active.recv().await.unwrap() {
                break;
            }
        }
        drop(active);
        assert!(fin.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn session_flush_then_finish() {
        let (data_tx, data_rx) = tch::make_channel::<FiniteStream<u16>>(8);
        let (sig_tx, sig_rx) = tch::make_latest_value_channel(SessionState::Finished(0));
        let (mut s, mut r) =
            create_session_channel::<u16, _, _, _, _>(data_tx, data_rx, sig_tx, sig_rx);

        let mut active = r.start().expect("start ok");
        s.wait_started().await.unwrap();
        for i in 0..4u16 {
            s.try_send(i).unwrap();
        }

        let fin = tokio::spawn(async move { s.finish_and_wait().await });
        active.flush_until_finish().await.unwrap();
        drop(active);
        assert!(fin.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn session_restart_detect() {
        let (data_tx, _data_rx) = tch::make_channel::<FiniteStream<u8>>(1);
        let (sig_tx, sig_rx) = tch::make_latest_value_channel(SessionState::Finished(0));
        let (mut s, mut r) =
            create_session_channel::<u8, _, _, _, _>(data_tx, _data_rx, sig_tx, sig_rx);

        let read_sess1 = r.start().expect("start ok");
        s.wait_started().await.unwrap();

        // simulate receiver starting a new session before finish is requested
        read_sess1.finish().unwrap();
        let _read_sess2 = r.start().expect("start ok");

        let err = s.finish_and_wait().await.unwrap_err();

        assert!(matches!(err, FinishError::Restarted(1)));
    }

    #[tokio::test]
    async fn session_restart_error() {
        let (data_tx, _data_rx) = tch::make_channel::<FiniteStream<u8>>(1);
        let (sig_tx, sig_rx) = tch::make_latest_value_channel(SessionState::Finished(0));
        let (mut s, mut r) =
            create_session_channel::<u8, _, _, _, _>(data_tx, _data_rx, sig_tx, sig_rx);

        let read_sess1 = r.start().expect("start ok");
        s.wait_started().await.unwrap();

        // simulate receiver starting a new session before finish is requested..
        read_sess1.finish().unwrap();
        let read_sess2 = r.start().expect("start ok");
        // .. which also finishes before the sender is done.
        read_sess2.finish().unwrap();

        // expect out of sync: receiver has restarted & stopped a next session!
        let err = s.finish_and_wait().await.unwrap_err();
        assert!(matches!(err, FinishError::OutOfSync));
    }

    #[tokio::test]
    async fn session_stopped_before_used() {
        let (data_tx, data_rx) = tch::make_channel::<FiniteStream<u16>>(8);
        let (sig_tx, sig_rx) = tch::make_latest_value_channel(SessionState::Finished(0));
        let (mut s, mut r) =
            create_session_channel::<u16, _, _, _, _>(data_tx, data_rx, sig_tx, sig_rx);

        let active = r.start().expect("start ok");
        drop(active);

        // already stopped, so start wont work (timeout)
        assert!(
            tokio::time::timeout(Duration::from_millis(10), s.wait_started())
                .await
                .is_err()
        );
        for i in 0..4u16 {
            s.try_send(i).unwrap();
        }

        let fin = tokio::spawn(async move { s.finish_and_wait().await });

        assert!(fin.await.unwrap().is_ok());
    }
}
