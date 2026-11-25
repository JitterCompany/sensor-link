use demo_protocol::Error;

use crate::{logic::network::NetworkClient, pool};

pub use sensor_link_firmware::logic::network::upload::{UploadAlloc, UploadTrait};

pub struct UploadAllocator<EA, DA>
where
    EA: pool::PoolAlloc,
    DA: pool::PoolAlloc,
{
    pub event_allocator: EA,
    pub data_allocator: DA,
}

#[derive(Clone)]
pub enum Upload<ER, DR> {
    Event(ER),
    SensorData(DR),
}

/// Allocator to allocate [Upload]s from multiple application-specific
/// formats to a common [Upload] implementation.
impl<EA, DA> UploadAllocator<EA, DA>
where
    EA: pool::PoolAlloc,
    DA: pool::PoolAlloc,
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
    EA: pool::PoolAlloc,
    DA: pool::PoolAlloc,
    Upload<EA::Arc, DA::Arc>: UploadTrait,
{
    type Upload = Upload<EA::Arc, DA::Arc>;

    type Event = EA::Data;
    type SensorData = DA::Data;

    fn event(
        &self,
    ) -> impl pool::MappedAllocator<
        Input = <Self as UploadAlloc>::Event,
        Output = <Self as UploadAlloc>::Upload,
    > {
        pool::Mapper::new(&self.event_allocator, Upload::Event)
    }
    fn data(
        &self,
    ) -> impl pool::MappedAllocator<
        Input = <Self as UploadAlloc>::SensorData,
        Output = <Self as UploadAlloc>::Upload,
    > {
        pool::Mapper::new(&self.data_allocator, Upload::SensorData)
    }
}

pub use sensor_link_firmware::logic::network::upload::NetworkUploadItem;

impl<C, ER, DR> NetworkUploadItem<C> for Upload<ER, DR>
where
    C: NetworkClient,
    Upload<ER, DR>: UploadTrait<Topic = C::Topic>,
{
    async fn send(&self, client: &mut C) -> Result<(), Error<C::ClientError>> {
        client.send_sendable(self.sendable()).await
    }
}
