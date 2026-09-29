use sensor_link_protocol::{Error, Topic};

use crate::{logic::network::NetworkClient, pool::MappedAllocator, serialize::Sendable};

/// An item the [`network_task`](super::network_task) can upload over a
/// [`NetworkClient`] connection.
///
/// The item owns its own send logic, so the task stays agnostic of how any
/// particular upload reaches the wire. Generic Sendable containers send through
/// [`NetworkClient::send_sendable`]; a consumer with its own format implements
/// this against its concrete client to use client-specific sending.
pub trait NetworkUploadItem<C: NetworkClient> {
    async fn send(&self, client: &mut C) -> Result<(), Error<C::ClientError>>;
}

/// Trait for an allocated container holding a [`Sendable`].
///
/// An instance is created via an implementation of [`UploadAlloc`]. This allows a
/// platform-specific (pool-)allocator to be used to limit the overhead of sending
/// messages over queues.
///
/// Generic over the wire [`Topic`] so this stays sensor-agnostic; an implementation
/// pins `Topic` to its concrete device topic type.
pub trait UploadTrait: Send {
    type Topic: Topic;
    fn sendable(&self) -> &dyn Sendable<Self::Topic>;
}

/// Allocator that maps application-specific event/sensor-data formats to a common
/// [`UploadTrait`] container, so the dispatch pipeline stays agnostic of the
/// concrete pool allocator backing each format.
pub trait UploadAlloc {
    type Upload: UploadTrait + Clone;

    type Event;
    type SensorData;

    fn event(&self) -> impl MappedAllocator<Input = Self::Event, Output = Self::Upload>;
    fn data(&self) -> impl MappedAllocator<Input = Self::SensorData, Output = Self::Upload>;
}
