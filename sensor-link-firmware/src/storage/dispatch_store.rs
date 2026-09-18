//! Generic persistent store for dispatchable, serialized data (events + sensor data + logs).
//!
//! This is the sensor-agnostic backbone of the dispatch pipeline: it stores opaque serialized
//! byte payloads ([`SerializedSendable`]) in three FIFO flash streams (events, sensor data and
//! device logs) and
//! reconstructs them on read. It is generic over the wire [`Topic`] and over the application's
//! stream-id type `S` (which defines the flash layout via [`flash_db::Circular`]). An
//! implementation pins `Topic` to its concrete device topic type and supplies a concrete `S`.
//!
//! Because the storage logic only ever moves byte slices, this module has no device-link
//! dependency and can back any sensor's dispatch store.

use core::marker::PhantomData;

use crate::{
    mqtt::LOG_LANE_TARGET,
    serialize::{Builder, SerializedSendable},
    storage::{
        common::{
            queue::{self, ConfirmChannel, SeqNo},
            stream_store::{StreamStore, MAX_PEEKS},
        },
        flash_db::{self, Circular, WriteableCircularStore},
    },
};
use sensor_link_protocol::{Topic, MAX_EVENT_LEN, MAX_LOG_LEN};

/// Handle used to confirm (acknowledge) that a peeked item has been processed and may be dropped.
pub type ConfirmHandle = queue::ConfirmHandle<'static, MAX_PEEKS>;

/// Trait to allow persistent storage of events, application-specific sensor data and device logs.
///
/// Implementation should guarantee FIFO ordering within each kind and persistence
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

    async fn store_log(
        &mut self,
        log: &SerializedSendable<MAX_LOG_LEN, Self::Topic>,
    ) -> Result<SeqNo, Self::Error>;
    async fn peek_log(
        &mut self,
    ) -> Result<Option<(SerializedSendable<MAX_LOG_LEN, Self::Topic>, ConfirmHandle)>, Self::Error>;
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
    /// Stream that stores serialized device log records.
    ///
    /// Logs get a stream of their own rather than sharing the event stream
    /// because a full stream overwrites its oldest entries: a burst of log
    /// records must not be able to evict events that were never delivered.
    fn log() -> Self;
}

/// Container for the [ConfirmChannel]s backing a [StreamSetStore].
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

/// Persistent storage for events, sensor data and device logs.
///
/// Each kind has its own dedicated FIFO flash stream, so one kind can never evict another. Data is
/// accessed via the [DispatchStore] trait. Generic over the stream-id type `S`, the configured
/// maximum sensor-data payload size, and the wire [`Topic`].
pub struct StreamSetStore<'a, DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T> {
    pub sensor_data: StreamStore<'a, DB, S, BLOCK_SIZE>,
    pub events: StreamStore<'a, DB, S, BLOCK_SIZE>,
    pub logs: StreamStore<'a, DB, S, BLOCK_SIZE>,
    _topic: PhantomData<T>,
}

impl<'a, DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T>
    StreamSetStore<'a, DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, S> + Clone,
    S: DispatchStreams<BLOCK_SIZE>,
{
    pub fn new(db: DB, confirm_channels: &'a ConfirmChannels<3>) -> Self {
        Self {
            sensor_data: StreamStore::new(
                db.clone(),
                S::sensor_data(),
                &confirm_channels.channels[0],
            ),
            events: StreamStore::new(db.clone(), S::event(), &confirm_channels.channels[1]),
            logs: StreamStore::new(db.clone(), S::log(), &confirm_channels.channels[2]),
            _topic: PhantomData,
        }
    }
    pub async fn initialize(&mut self) -> Result<(), flash_db::Error> {
        self.sensor_data.initialize().await?;
        self.events.initialize().await?;
        self.logs.initialize().await?;
        Ok(())
    }
}

impl<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T> DispatchStore
    for StreamSetStore<'static, DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
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

    async fn store_log(
        &mut self,
        log: &SerializedSendable<MAX_LOG_LEN, T>,
    ) -> Result<SeqNo, Self::Error> {
        self.logs.enqueue(log.as_slice()).await
    }

    async fn peek_log(
        &mut self,
    ) -> Result<Option<(SerializedSendable<MAX_LOG_LEN, T>, ConfirmHandle)>, Self::Error> {
        let mut buffer = Builder::new();
        match self.logs.peek_next(&mut buffer.bytes).await? {
            Some((len, handle)) => match buffer.create_with_total_length(len) {
                Ok(buffer) => Ok(Some((buffer, handle))),
                Err(deserialize_error) => {
                    // Under `LOG_LANE_TARGET`, unlike the peeks above: a record
                    // about a log record arrives back on the log lane.
                    log::warn!(target: LOG_LANE_TARGET, "Failed to deserialize log: {deserialize_error:?}");
                    handle.confirm(); // skip item: retry is likely to fail again!
                    Err(flash_db::Error::FragmentNotReadable)
                }
            },
            _ => Ok(None),
        }
    }
}

/// `'static` wrapper around [StreamSetStore].
///
/// Without this wrapper the dispatch task does not compile (see ADR-0003).
pub struct StaticStreamSetStore<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T>
{
    store: StreamSetStore<'static, DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>,
}

impl<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T>
    StaticStreamSetStore<DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
where
    DB: WriteableCircularStore<{ BLOCK_SIZE }, S> + Clone,
    S: DispatchStreams<BLOCK_SIZE>,
{
    #[inline]
    pub fn new(db: DB, confirm_channels: &'static ConfirmChannels<3>) -> Self {
        Self {
            store: StreamSetStore::new(db, confirm_channels),
        }
    }

    #[inline]
    pub async fn initialize(&mut self) -> Result<(), flash_db::Error> {
        self.store.initialize().await?;
        Ok(())
    }
}

impl<DB, S, const BLOCK_SIZE: usize, const MAX_SENSOR_DATA_LEN: usize, T> DispatchStore
    for StaticStreamSetStore<DB, S, BLOCK_SIZE, MAX_SENSOR_DATA_LEN, T>
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
    async fn store_log(
        &mut self,
        log: &SerializedSendable<MAX_LOG_LEN, T>,
    ) -> Result<SeqNo, Self::Error> {
        self.store.store_log(log).await
    }

    #[inline]
    async fn peek_log(
        &mut self,
    ) -> Result<Option<(SerializedSendable<MAX_LOG_LEN, T>, ConfirmHandle)>, Self::Error> {
        self.store.peek_log().await
    }
}
