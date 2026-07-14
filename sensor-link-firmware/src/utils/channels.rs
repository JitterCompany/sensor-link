//! Async channels for std code.

use std::sync::mpsc::RecvError;
use tokio::sync::{mpsc, watch};

use crate::logic::{
    LatestValueReceiveChannel, LatestValueSendChannel, ReceiveChannel, SendChannel,
};

pub struct Sender<T>(pub mpsc::Sender<T>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender(self.0.clone())
    }
}

pub struct Receiver<T>(pub mpsc::Receiver<T>);

/// Create a tx/rs channel pair which implement Sender and Receiver for type T.
pub fn make_channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel(capacity);
    let tx = Sender::<T>(tx);
    let rx = Receiver::<T>(rx);
    (tx, rx)
}

impl<T> SendChannel<T> for Sender<T> {
    type Error = ();
    async fn send(&mut self, val: T) -> Result<(), Self::Error> {
        self.0.send(val).await.map_err(|_| ())
    }

    fn try_send(&mut self, val: T) -> Result<(), T> {
        self.0.try_send(val).map_err(|e| match e {
            mpsc::error::TrySendError::Full(el) => el,
            mpsc::error::TrySendError::Closed(el) => el,
        })
    }

    fn is_ready(&self) -> bool {
        self.0.try_reserve().is_ok()
    }
}

impl<T> ReceiveChannel<T> for Receiver<T> {
    type Error = RecvError;

    async fn recv(&mut self) -> Result<T, Self::Error> {
        match self.0.recv().await {
            Some(val) => Ok(val),
            None => {
                println!("Channel is closed");
                Err(RecvError)
            }
        }
    }

    fn try_recv(&mut self) -> Result<T, Self::Error> {
        self.0.try_recv().map_err(|_| RecvError)
    }
}

pub struct LatestValueSender<T>(pub watch::Sender<T>);
pub struct LatestValueReceiver<T>(pub watch::Receiver<T>);

/// Create a tx/rs channel pair which implement Sender and Receiver for type T.
pub fn make_latest_value_channel<T>(initial: T) -> (LatestValueSender<T>, LatestValueReceiver<T>) {
    let (tx, rx) = watch::channel(initial);
    let tx = LatestValueSender::<T>(tx);
    let rx = LatestValueReceiver::<T>(rx);
    (tx, rx)
}

impl<T> LatestValueSendChannel<T> for LatestValueSender<T> {
    type Error = ();

    fn send(&mut self, val: T) -> Result<(), Self::Error> {
        self.0.send(val).map_err(|_| ())
    }
}

impl<T: Clone> LatestValueReceiveChannel<T> for LatestValueReceiver<T> {
    type Error = ();
    async fn recv(&mut self) -> Result<T, Self::Error> {
        self.0.changed().await.map_err(|_| ())?;
        Ok(self.0.borrow_and_update().clone())
    }
}
