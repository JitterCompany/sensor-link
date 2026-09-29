//! Allocation of the items the network task uploads, implementing the generic
//! [`UploadAlloc`] for the mock's sensor data and events.

use sensor_link_firmware::{
    logic::network::{
        upload::{NetworkUploadItem, UploadAlloc, UploadTrait},
        NetworkClient,
    },
    pool::{self, MappedAllocator, PoolAlloc},
    sensor_link_protocol::{Error, TopicFromDevice, MAX_EVENT_LEN},
    serialize::{Sendable, SerializedSendable},
};

use crate::device::store::MAX_SENSOR_DATA_LEN;

pub type SerEvent = SerializedSendable<MAX_EVENT_LEN, TopicFromDevice>;
pub type SerSensorData = SerializedSendable<MAX_SENSOR_DATA_LEN, TopicFromDevice>;

/// One item queued for upload, whichever kind it is.
///
/// The variants hold pooled references rather than the payloads themselves, so
/// moving an upload through the queues never copies a message-sized buffer.
pub enum Upload<ER, DR> {
    Event(ER),
    SensorData(DR),
}

// Derived `Clone` would demand `ER: Clone` on the struct itself; the pooled
// references are always cloneable, the payloads they point at need not be.
impl<ER: Clone, DR: Clone> Clone for Upload<ER, DR> {
    fn clone(&self) -> Self {
        match self {
            Upload::Event(event) => Upload::Event(event.clone()),
            Upload::SensorData(data) => Upload::SensorData(data.clone()),
        }
    }
}

impl<ER, DR> UploadTrait for Upload<ER, DR>
where
    ER: pool::Ref<SerEvent> + Send,
    DR: pool::Ref<SerSensorData> + Send,
{
    type Topic = TopicFromDevice;

    fn sendable(&self) -> &dyn Sendable<Self::Topic> {
        match self {
            Upload::Event(event) => event.as_ref(),
            Upload::SensorData(data) => data.as_ref(),
        }
    }
}

impl<C, ER, DR> NetworkUploadItem<C> for Upload<ER, DR>
where
    C: NetworkClient,
    Upload<ER, DR>: UploadTrait<Topic = C::Topic>,
{
    async fn send(&self, client: &mut C) -> Result<(), Error<C::ClientError>> {
        client.send_sendable(self.sendable()).await
    }
}

/// Maps the per-kind pool allocators onto the common [`Upload`] container, so
/// the dispatch pipeline stays agnostic of which pool backs each kind.
pub struct UploadAllocator<EA, DA>
where
    EA: PoolAlloc,
    DA: PoolAlloc,
{
    pub event_allocator: EA,
    pub data_allocator: DA,
}

impl<EA, DA> UploadAllocator<EA, DA>
where
    EA: PoolAlloc,
    DA: PoolAlloc,
{
    pub fn new(event_allocator: EA, data_allocator: DA) -> Self {
        Self {
            event_allocator,
            data_allocator,
        }
    }
}

impl<EA, DA> UploadAlloc for UploadAllocator<EA, DA>
where
    EA: PoolAlloc,
    DA: PoolAlloc,
    Upload<EA::Arc, DA::Arc>: UploadTrait + Clone,
{
    type Upload = Upload<EA::Arc, DA::Arc>;

    type Event = EA::Data;
    type SensorData = DA::Data;

    fn event(&self) -> impl MappedAllocator<Input = Self::Event, Output = Self::Upload> {
        pool::Mapper::new(&self.event_allocator, Upload::Event)
    }

    fn data(&self) -> impl MappedAllocator<Input = Self::SensorData, Output = Self::Upload> {
        pool::Mapper::new(&self.data_allocator, Upload::SensorData)
    }
}
