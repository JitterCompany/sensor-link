//! Allocation of the items the network task uploads.
//!
//! Ported from `btb-firmware-core`'s `logic::network::upload` and
//! `logic::network::hub_upload`, extended with the log kind the generic
//! [`UploadAlloc`] now requires.

use sensor_link_firmware::{
    logic::network::{
        upload::{NetworkUploadItem, UploadAlloc, UploadTrait},
        NetworkClient,
    },
    pool::{self, MappedAllocator, PoolAlloc},
    sensor_link_protocol::{Error, TopicFromDevice, MAX_EVENT_LEN, MAX_LOG_LEN},
    serialize::{Sendable, SerializedSendable},
};

use crate::device::store::MAX_SENSOR_DATA_LEN;

pub type SerEvent = SerializedSendable<MAX_EVENT_LEN, TopicFromDevice>;
pub type SerSensorData = SerializedSendable<MAX_SENSOR_DATA_LEN, TopicFromDevice>;
pub type SerLog = SerializedSendable<MAX_LOG_LEN, TopicFromDevice>;

/// One item queued for upload, whichever kind it is.
///
/// The variants hold pooled references rather than the payloads themselves, so
/// moving an upload through the queues never copies a message-sized buffer.
pub enum Upload<ER, DR, LR> {
    Event(ER),
    SensorData(DR),
    Log(LR),
}

// Derived `Clone` would demand `ER: Clone` on the struct itself; the pooled
// references are always cloneable, the payloads they point at need not be.
impl<ER: Clone, DR: Clone, LR: Clone> Clone for Upload<ER, DR, LR> {
    fn clone(&self) -> Self {
        match self {
            Upload::Event(event) => Upload::Event(event.clone()),
            Upload::SensorData(data) => Upload::SensorData(data.clone()),
            Upload::Log(log) => Upload::Log(log.clone()),
        }
    }
}

impl<ER, DR, LR> UploadTrait for Upload<ER, DR, LR>
where
    ER: pool::Ref<SerEvent> + Send,
    DR: pool::Ref<SerSensorData> + Send,
    LR: pool::Ref<SerLog> + Send,
{
    type Topic = TopicFromDevice;

    fn sendable(&self) -> &dyn Sendable<Self::Topic> {
        match self {
            Upload::Event(event) => event.as_ref(),
            Upload::SensorData(data) => data.as_ref(),
            Upload::Log(log) => log.as_ref(),
        }
    }
}

impl<C, ER, DR, LR> NetworkUploadItem<C> for Upload<ER, DR, LR>
where
    C: NetworkClient,
    Upload<ER, DR, LR>: UploadTrait<Topic = C::Topic>,
{
    async fn send(&self, client: &mut C) -> Result<(), Error<C::ClientError>> {
        client.send_sendable(self.sendable()).await
    }
}

/// Maps the per-kind pool allocators onto the common [`Upload`] container, so
/// the dispatch pipeline stays agnostic of which pool backs each kind.
pub struct UploadAllocator<EA, DA, LA>
where
    EA: PoolAlloc,
    DA: PoolAlloc,
    LA: PoolAlloc,
{
    pub event_allocator: EA,
    pub data_allocator: DA,
    pub log_allocator: LA,
}

impl<EA, DA, LA> UploadAllocator<EA, DA, LA>
where
    EA: PoolAlloc,
    DA: PoolAlloc,
    LA: PoolAlloc,
{
    pub fn new(event_allocator: EA, data_allocator: DA, log_allocator: LA) -> Self {
        Self {
            event_allocator,
            data_allocator,
            log_allocator,
        }
    }
}

impl<EA, DA, LA> UploadAlloc for UploadAllocator<EA, DA, LA>
where
    EA: PoolAlloc,
    DA: PoolAlloc,
    LA: PoolAlloc,
    Upload<EA::Arc, DA::Arc, LA::Arc>: UploadTrait + Clone,
{
    type Upload = Upload<EA::Arc, DA::Arc, LA::Arc>;

    type Event = EA::Data;
    type SensorData = DA::Data;
    type Log = LA::Data;

    fn event(&self) -> impl MappedAllocator<Input = Self::Event, Output = Self::Upload> {
        pool::Mapper::new(&self.event_allocator, Upload::Event)
    }

    fn data(&self) -> impl MappedAllocator<Input = Self::SensorData, Output = Self::Upload> {
        pool::Mapper::new(&self.data_allocator, Upload::SensorData)
    }

    fn log(&self) -> impl MappedAllocator<Input = Self::Log, Output = Self::Upload> {
        pool::Mapper::new(&self.log_allocator, Upload::Log)
    }
}
