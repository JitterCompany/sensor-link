//! Generic persistent store for dispatchable, serialized data (events + sensor data).
//!
//! This is the sensor-agnostic backbone of the dispatch pipeline: it stores opaque serialized
//! byte payloads ([`SerializedSendable`]) in two FIFO flash streams (events and sensor data) and
//! reconstructs them on read. It is generic over the wire [`Topic`] and over the application's
//! stream-id type `S` (which defines the flash layout via [`flash_db::Circular`]). An
//! implementation pins `Topic` to its concrete device topic type and supplies a concrete `S`.
//!
//! Because the storage logic only ever moves byte slices, this module has no device-link
//! dependency and can back any sensor's dispatch store.

use core::marker::PhantomData;

use crate::{
    serialize::{Builder, SerializedSendable},
    storage::{
        common::{
            queue::{self, ConfirmChannel, SeqNo},
            stream_store::{StreamStore, MAX_PEEKS},
        },
        flash_db::{self, Circular, WriteableCircularStore},
    },
    utils::select::select2,
};
use sensor_link_protocol::{Topic, MAX_EVENT_LEN};

/// Handle used to confirm (acknowledge) that a peeked item has been processed and may be dropped.
pub type ConfirmHandle = queue::ConfirmHandle<'static, MAX_PEEKS>;

/// Trait to allow persistent storage of events and application-specific sensor data.
///
/// Implementation should guarantee FIFO ordering of events and sensor data and persistence
/// (durability) of data from the moment of storage untill the [ConfirmHandle::confirm] method
/// is called.
///
/// Generic over the wire [`Topic`] so the dispatch pipeline stays sensor-agnostic.
pub trait DispatchStore {
    type Error: core::fmt::Debug + Clone;
    type Topic: Topic;

    async fn store_event(
        &mut self,
        event: &SerializedSendable<MAX_EVENT_LEN, Self::Topic>,
    ) -> Result<SeqNo, Self::Error>;
    async fn peek_event(
        &mut self,
    ) -> Result<
        Option<(
            SerializedSendable<MAX_EVENT_LEN, Self::Topic>,
            ConfirmHandle,
        )>,
        Self::Error,
    >;

    async fn store_sensor_data<const MAX_PROCESSING_LEN: usize>(
        &mut self,
        processing: &SerializedSendable<{ MAX_PROCESSING_LEN }, Self::Topic>,
    ) -> Result<SeqNo, Self::Error>;
    async fn peek_sensor_data<const MAX_PROCESSING_LEN: usize>(
        &mut self,
    ) -> Result<
        Option<(
            SerializedSendable<{ MAX_PROCESSING_LEN }, Self::Topic>,
            ConfirmHandle,
        )>,
        Self::Error,
    >;

    /// True if every stored event and sensor data item has been confirmed.
    ///
    /// Items that were peeked but not confirmed yet, or aborted and waiting for a
    /// retry, count as not drained.
    async fn is_drained(&mut self) -> Result<bool, Self::Error>;

    /// Wait until a peeked event or sensor data item is confirmed or aborted.
    ///
    /// Must be cancel safe.
    async fn wait_confirmation(&mut self);
}

/// Application stream-id type that designates which stream holds events and which holds sensor data.
///
/// Implemented by the concrete `Circular` flash-layout enum the application supplies.
pub trait DispatchStreams<const BLOCK_SIZE: usize>:
    Circular<BLOCK_SIZE> + core::fmt::Debug
{
    /// Stream that stores serialized events.
    fn event() -> Self;
    /// Stream that stores serialized sensor data.
    fn sensor_data() -> Self;
}

/// Container for the [ConfirmChannel]s backing a [StreamPairStore].
pub struct ConfirmChannels<const NUM_CHANNELS: usize> {
    pub channels: [ConfirmChannel<MAX_PEEKS>; NUM_CHANNELS],
}

impl<const NUM_CHANNELS: usize> ConfirmChannels<NUM_CHANNELS> {
    pub const fn new() -> Self {
        Self {
            channels: [const { ConfirmChannel::new() }; NUM_CHANNELS],
        }
    }
}

impl<const NUM_CHANNELS: usize> Default for ConfirmChannels<NUM_CHANNELS> {
    fn default() -> Self {
        Self::new()
    }
}

/// Persistent storage for events and sensor data.
///
/// Events and sensor data each have their own dedicated FIFO flash stream. Data is accessed via the
/// [DispatchStore] trait. Generic over the stream-id type `S`, the configured maximum sensor-data
/// payload size, and the wire [`Topic`].
pub struct StreamPairStore<'a, DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T>
{
    pub sensor_data: StreamStore<'a, DB, S, BLOCK_SIZE>,
    pub events: StreamStore<'a, DB, S, BLOCK_SIZE>,
    _topic: PhantomData<T>,
}

impl<'a, DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T>
    StreamPairStore<'a, DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, S> + Clone,
    S: DispatchStreams<BLOCK_SIZE>,
{
    pub fn new(db: DB, confirm_channels: &'a ConfirmChannels<2>) -> Self {
        Self {
            sensor_data: StreamStore::new(
                db.clone(),
                S::sensor_data(),
                &confirm_channels.channels[0],
            ),
            events: StreamStore::new(db.clone(), S::event(), &confirm_channels.channels[1]),
            _topic: PhantomData,
        }
    }
    pub async fn initialize(&mut self) -> Result<(), flash_db::Error> {
        self.sensor_data.initialize().await?;
        self.events.initialize().await?;
        Ok(())
    }
}

impl<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T> DispatchStore
    for StreamPairStore<'static, DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, S> + Clone,
    S: DispatchStreams<BLOCK_SIZE>,
    T: Topic,
{
    type Error = flash_db::Error;
    type Topic = T;

    async fn store_event(
        &mut self,
        event: &SerializedSendable<MAX_EVENT_LEN, T>,
    ) -> Result<SeqNo, Self::Error> {
        self.events.enqueue(event.as_slice()).await
    }

    async fn peek_event(
        &mut self,
    ) -> Result<Option<(SerializedSendable<MAX_EVENT_LEN, T>, ConfirmHandle)>, Self::Error> {
        let mut buffer = Builder::new();
        match self.events.peek_next(&mut buffer.bytes).await? {
            Some((len, handle)) => match buffer.create_with_total_length(len) {
                Ok(buffer) => Ok(Some((buffer, handle))),
                Err(deserialize_error) => {
                    log::warn!("Failed to deserialize event: {deserialize_error:?}");
                    handle.confirm(); // skip item: retry is likely to fail again!
                    Err(flash_db::Error::FragmentNotReadable)
                }
            },
            _ => Ok(None),
        }
    }

    async fn store_sensor_data<const MAX_PROCESSING_LEN: usize>(
        &mut self,
        sendable: &SerializedSendable<MAX_PROCESSING_LEN, T>,
    ) -> Result<SeqNo, Self::Error> {
        // Ensure the sendable is guaranteed to be small enough to be stored
        const { assert!(MAX_PROCESSING_LEN <= MAX_SENSOR_DATA_LEN) }

        self.sensor_data.enqueue(sendable.as_slice()).await
    }

    async fn peek_sensor_data<const MAX_PROCESSING_LEN: usize>(
        &mut self,
    ) -> Result<Option<(SerializedSendable<{ MAX_PROCESSING_LEN }, T>, ConfirmHandle)>, Self::Error>
    {
        // Ensure the result is large enough to handle the stored data
        const { assert!(MAX_PROCESSING_LEN >= MAX_SENSOR_DATA_LEN) }
        let mut buffer = Builder::new();
        match self.sensor_data.peek_next(&mut buffer.bytes).await? {
            Some((len, handle)) => match buffer.create_with_total_length(len) {
                Ok(buffer) => Ok(Some((buffer, handle))),
                Err(deserialize_error) => {
                    log::warn!("Failed to deserialize sensor_data: {deserialize_error:?}");
                    handle.confirm(); // skip item: retry is likely to fail again!
                    Err(flash_db::Error::FragmentNotReadable)
                }
            },
            _ => Ok(None),
        }
    }

    async fn is_drained(&mut self) -> Result<bool, Self::Error> {
        Ok(self.events.is_drained().await? && self.sensor_data.is_drained().await?)
    }

    async fn wait_confirmation(&mut self) {
        select2(
            self.events.wait_confirmation(),
            self.sensor_data.wait_confirmation(),
        )
        .await;
    }
}

/// `'static` wrapper around [StreamPairStore].
///
/// Without this wrapper the dispatch task does not compile (see ADR-0003).
pub struct StaticStreamPairStore<
    DB,
    S,
    const BLOCK_SIZE: usize,
    const MAX_SENSOR_DATA_LEN: usize,
    T,
> {
    store: StreamPairStore<'static, DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>,
}

impl<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T>
    StaticStreamPairStore<DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, S> + Clone,
    S: DispatchStreams<BLOCK_SIZE>,
{
    #[inline]
    pub fn new(db: DB, confirm_channels: &'static ConfirmChannels<2>) -> Self {
        Self {
            store: StreamPairStore::new(db, confirm_channels),
        }
    }

    #[inline]
    pub async fn initialize(&mut self) -> Result<(), flash_db::Error> {
        self.store.initialize().await?;
        Ok(())
    }
}

impl<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T> DispatchStore
    for StaticStreamPairStore<DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, S> + Clone,
    S: DispatchStreams<BLOCK_SIZE>,
    T: Topic,
{
    type Error = flash_db::Error;
    type Topic = T;

    #[inline]
    async fn store_event(
        &mut self,
        event: &SerializedSendable<MAX_EVENT_LEN, T>,
    ) -> Result<SeqNo, Self::Error> {
        self.store.store_event(event).await
    }

    #[inline]
    async fn peek_event(
        &mut self,
    ) -> Result<Option<(SerializedSendable<MAX_EVENT_LEN, T>, ConfirmHandle)>, Self::Error> {
        self.store.peek_event().await
    }

    #[inline]
    async fn store_sensor_data<const MAX_PROCESSING_LEN: usize>(
        &mut self,
        sendable: &SerializedSendable<MAX_PROCESSING_LEN, T>,
    ) -> Result<SeqNo, Self::Error> {
        self.store.store_sensor_data(sendable).await
    }

    #[inline]
    async fn peek_sensor_data<const MAX_PROCESSING_LEN: usize>(
        &mut self,
    ) -> Result<Option<(SerializedSendable<{ MAX_PROCESSING_LEN }, T>, ConfirmHandle)>, Self::Error>
    {
        self.store.peek_sensor_data().await
    }

    #[inline]
    async fn is_drained(&mut self) -> Result<bool, Self::Error> {
        self.store.is_drained().await
    }

    #[inline]
    async fn wait_confirmation(&mut self) {
        self.store.wait_confirmation().await
    }
}
