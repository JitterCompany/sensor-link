pub mod barrier;
pub mod buffer;
pub mod confirmable;
pub mod drain;
mod pending;
pub mod serialization;

use crate::{
    drivers::time,
    logic::{
        dispatch::{
            barrier::DispatchBarrier, confirmable::Confirmable,
            serialization::LatencyControlledSerializer,
        },
        network::upload::UploadAlloc,
        signal::Signal,
        ReceiveChannel, SendChannel,
    },
    monotonic_time::delay_ms,
    pool::MappedAllocator,
    serialize::{AsSendable, SerializedSendable},
    sync::reserving_sender::{ReservableSender, ReservationToken},
    utils::select::{select2, select3, Select2, Select3},
};
use sensor_link_protocol::{event::EventPayload, Microseconds, Topic, MAX_EVENT_LEN};

use futures::FutureExt;
use pending::Pending;

/// Generic, topic-agnostic dispatch store interface and its confirm handle.
pub use crate::storage::dispatch_store::{ConfirmHandle, DispatchStore};

/// A serialized event, addressed to wire topic `T`.
pub type SerializedEvent<T> = SerializedSendable<MAX_EVENT_LEN, T>;

/// prevent busy loop in case store keeps failing.
/// 100ms is chosen as ~10 messages/second,
/// which is a reasonable order-of-magnitude
/// for normal upload throughput
const PREVENT_BUSY_LOOP_DELAY_MS: u32 = 100;

/// How often to re-check whether a requested barrier is reached.
/// The final confirmation comes from the network task, which does not wake dispatch.
const BARRIER_RECHECK_MS: u32 = 1_000;

pub async fn dispatch_task<
    DS,
    LCS,
    EQI,
    SQO,
    UA,
    US,
    T,
    E,
    S,
    IsUrgent,
    const MAX_OUTPUT_SIZE: usize,
>(
    store: &mut DS,
    data_in: &mut LCS,
    event_in: &mut EQI,
    signal_out: &mut SQO,
    upload_alloc: &mut UA,
    upload_tx: &mut US,
    is_urgent: IsUrgent,
    barrier: &DispatchBarrier,
) -> !
where
    T: Topic,
    EventPayload<E>: AsSendable<MAX_EVENT_LEN, T>,
    <EventPayload<E> as AsSendable<MAX_EVENT_LEN, T>>::Error: core::fmt::Debug,
    DS: DispatchStore<Topic = T>,
    EQI: ReceiveChannel<E>,
    LCS: LatencyControlledSerializer<MAX_OUTPUT_SIZE, Topic = T>,
    S: From<Signal>,
    SQO: SendChannel<S>,
    UA: UploadAlloc<
        Event = SerializedEvent<T>,
        SensorData = SerializedSendable<MAX_OUTPUT_SIZE, T>,
    >,
    US: ReservableSender<Confirmable<UA::Upload>>,
    IsUrgent: Fn(&E) -> bool,
{
    log::info!(target: "Dispatch", "Starting dispatch task");

    // pending: to be enqueued to network task
    let mut pending_event = Pending::none(upload_alloc.event());
    let mut pending_data = Pending::none(upload_alloc.data());

    // A barrier was requested and has not been replied to yet
    let mut barrier_active = false;

    loop {
        // retry a failed read from the store on next iteration?
        // used for rate-limiting. TODO is there a cleaner way?
        let mut store_retry = false;

        // 1. try to peek an item from store (event takes priority over processing)
        // a. Nothing to send yet? send next event (if any)
        if let Some(mut ev_writer) = pending_event.try_set() {
            match store.peek_event().await {
                Ok(Some((event, handle))) => {
                    log::debug!(target: "Dispatch", "Trying to send event");
                    ev_writer.write(event, handle);
                }
                Ok(None) => {
                    // TODO None does not necessarily mean no more data is available,
                    // it can also mean that there are no more ConfirmHandles available!
                    // (see #638: won't cause issues as long as upload queue is small enough)
                }
                Err(error) => {
                    log::warn!(target: "Dispatch", "Failed to read Event from store: {error:?}");
                    store_retry = true;
                }
            };
        }
        // b. Still nothing to send? send next processing data (if any)
        if let (false, Some(mut data_writer)) = (pending_event.is_pending(), pending_data.try_set())
        {
            match store.peek_sensor_data().await {
                Ok(Some((data, handle))) => {
                    log::debug!(target: "Dispatch", "Trying to send processing result");
                    data_writer.write(data, handle);
                }
                Ok(None) => {
                    // TODO None does not necessarily mean no more data is available,
                    // it can also mean that there are no more ConfirmHandles available!
                    // (see #638: won't cause issues as long as upload queue is small enough)
                }
                Err(error) => {
                    log::warn!(target: "Dispatch", "Failed to read Processing from store: {error:?}");
                    store_retry = true;
                }
            };
        }

        // Barrier reached: everything received so far is stored, sent and confirmed
        if barrier_active
            && !data_in.is_flushing()
            && !pending_event.is_pending()
            && !pending_data.is_pending()
        {
            match store.is_drained().await {
                Ok(true) => {
                    log::info!(target: "Dispatch", "Barrier reached: drained");
                    signal_out.send(Signal::DispatchDrained.into()).await.ok();
                    barrier_active = false;
                }
                Ok(false) => {}
                Err(error) => {
                    log::warn!(target: "Dispatch", "Failed to check if store is drained: {error:?}");
                }
            }
        }

        // Signal orchestrator that queue is empty
        if !pending_event.is_pending() && !pending_data.is_pending() {
            signal_out
                .send(Signal::DispatchQueueEmpty.into())
                .await
                .ok();
        }

        // Future that transmits any pending data to the network, or never resolves if there is nothing to send.
        // This 'blocking' is intentional, so that the select() statement will wait for the other future to resolve
        let transmit_network_or_block = try_transmit(&mut pending_event, &mut pending_data, upload_tx).then(|res| {
            async move {
                match res {
                    // successful transmission: done
                    Ok(_) => {}

                    // failed: this should not happen in production. If it does, we retry after a timeout to prevent a busy loop.
                    Err(TransmitError::UploadFailed) => {
                        log::error!(target: "Dispatch", "Failed to upload: queue broken or multiple senders on this channel??");
                        delay_ms(PREVENT_BUSY_LOOP_DELAY_MS).await;
                    }

                    // no data pending: block 'forever' unless we should retry after a store read
                    //
                    Err(TransmitError::NothingPending) => {
                        if store_retry {
                            delay_ms(PREVENT_BUSY_LOOP_DELAY_MS).await;
                        } else {
                            core::future::pending::<()>().await;
                        }
                    }
                }
            }
        });

        // Future that resolves on a new barrier request, or periodically while one is active
        let barrier_or_recheck = async move {
            if barrier_active {
                delay_ms(BARRIER_RECHECK_MS).await;
                BarrierWake::Recheck
            } else {
                barrier.wait().await;
                BarrierWake::Requested
            }
        };

        // Select between incoming data, barrier requests and transmission of pending data
        // NOTE: select3 has a bias to the first future, so storing incoming data always takes priority
        // over transmitting network data. This is important to prevent the incoming data queue from overflowing
        // in case of a super fast network connection.
        // It also means the incoming queues are empty when a barrier request is taken.
        match select3(
            incoming(data_in, event_in),
            barrier_or_recheck,
            transmit_network_or_block,
        )
        .await
        {
            Select3::A(incoming) => match incoming {
                Ok(Incoming::Event(event)) => {
                    process_event(store, &mut pending_event, event, signal_out, &is_urgent).await;
                }
                Ok(Incoming::Data(data)) => {
                    process_sensor_data(store, &mut pending_data, data).await;
                }
                Err(error) => {
                    log::error!(target: "Dispatch", "Data loss while receiving: {error:?}");
                }
            },
            Select3::B(BarrierWake::Requested) => {
                log::info!(target: "Dispatch", "Barrier requested");
                // Force buffered data out, and store any events that are still queued
                data_in.flush();
                while let Ok(event) = event_in.try_recv() {
                    process_event(store, &mut pending_event, event, signal_out, &is_urgent).await;
                }
                barrier_active = true;
            }
            Select3::B(BarrierWake::Recheck) => {}
            Select3::C(()) => {}
        }
    }
}

enum BarrierWake {
    Requested,
    Recheck,
}

enum TransmitError {
    UploadFailed,
    NothingPending,
}

/// Try to transmit any pending data to the network
async fn try_transmit<EA, DA, U, US>(
    pending_event: &mut Pending<EA>,
    pending_data: &mut Pending<DA>,
    upload_tx: &mut US,
) -> Result<(), TransmitError>
where
    EA: MappedAllocator<Output = U>,
    DA: MappedAllocator<Output = U>,
    US: ReservableSender<Confirmable<U>>,
{
    let mut result = Err(TransmitError::NothingPending);

    if let Some(reader) = pending_event.try_read() {
        let reserved = upload_tx.reserve().await;
        match reserved.try_send(reader.consume()) {
            Ok(_) => {
                result = Ok(());
            }
            Err(_upl) => {
                log::error!(target: "Dispatch", "Failed to upload: queue broken or multiple senders on this channel??");
                return Err(TransmitError::UploadFailed);
            }
        }
    }
    if let Some(reader) = pending_data.try_read() {
        let reserved = upload_tx.reserve().await;
        match reserved.try_send(reader.consume()) {
            Ok(_) => {
                result = Ok(());
            }
            Err(_upl) => {
                log::error!(target: "Dispatch", "Failed to upload: queue broken or multiple senders on this channel??");
                return Err(TransmitError::UploadFailed);
            }
        }
    }
    result
}

async fn process_event<DS, SQO, PA, T, E, S, IsUrgent>(
    store: &mut DS,
    pending: &mut Pending<PA>,
    event: E,
    signal_out: &mut SQO,
    is_urgent: &IsUrgent,
) where
    T: Topic,
    EventPayload<E>: AsSendable<MAX_EVENT_LEN, T>,
    <EventPayload<E> as AsSendable<MAX_EVENT_LEN, T>>::Error: core::fmt::Debug,
    DS: DispatchStore<Topic = T>,
    S: From<Signal>,
    SQO: SendChannel<S>,
    PA: MappedAllocator<Input = SerializedEvent<T>>,
    IsUrgent: Fn(&E) -> bool,
{
    let is_urgent = is_urgent(&event);
    log::debug!(target: "Dispatch", "Processing {} event...", if is_urgent { "urgent" } else { "" });

    if is_urgent {
        if let Err(_) = signal_out.send(Signal::UrgentEvent.into()).await {
            log::error!("Dispatch: failed to send 'urgent event' signal");
        }
    }

    let now = Microseconds::from_raw_microseconds(time::timestamp_or_default_us());
    let sendable = match EventPayload::from_event_at(event, now).as_sendable() {
        Ok(sendable) => sendable,
        Err(err) => {
            log::error!("Dispatch: failed to serialize event: {err:?}");
            return;
        }
    };

    match store.store_event(&sendable).await {
        Ok(seq_no) => log::debug!(target: "Dispatch", "Stored event #{seq_no}"),

        // store failed: write to pending to try sending it to the network anyways.
        // this may be lossy if an event was already pending, but better than nothing!
        Err(fail) => {
            log::warn!(target: "Dispatch", "Failed to store event: {fail:?}");
            pending.overwrite(sendable);
        }
    }
}

#[inline]
async fn process_sensor_data<DS, PA, T, const MAX_OUTPUT_SIZE: usize>(
    store: &mut DS,
    pending: &mut Pending<PA>,
    data: SerializedSendable<MAX_OUTPUT_SIZE, T>,
) where
    T: Topic,
    DS: DispatchStore<Topic = T>,
    PA: MappedAllocator<Input = SerializedSendable<MAX_OUTPUT_SIZE, T>>,
{
    log::debug!(target: "Dispatch", "Processing data...");

    match store.store_sensor_data(&data).await {
        Ok(seq_no) => log::debug!(target: "Dispatch", "Stored sensor data #{seq_no}"),

        // store failed: write to pending to try sending it to the network anyways.
        // this may be lossy if data was already pending, but better than nothing!
        Err(fail) => {
            log::warn!(target: "Dispatch", "Failed to store sensor data: {fail:?}");
            pending.overwrite(data);
        }
    }
}

enum Incoming<E, T: Topic, const MAX_OUTPUT_SIZE: usize> {
    Event(E),
    Data(SerializedSendable<MAX_OUTPUT_SIZE, T>),
}

#[derive(Debug, Clone, Copy)]
enum IncomingError {
    EventQueueError,
    SerializationError,
}

async fn incoming<LCS, EQI, T, E, const MAX_OUTPUT_SIZE: usize>(
    data_in: &mut LCS,
    event_in: &mut EQI,
) -> Result<Incoming<E, T, MAX_OUTPUT_SIZE>, IncomingError>
where
    T: Topic,
    EQI: ReceiveChannel<E>,
    LCS: LatencyControlledSerializer<MAX_OUTPUT_SIZE, Topic = T>,
{
    // Future that awaits incoming data via LatencyControlledSerializer
    let data_in = async {
        loop {
            match data_in.next_packet().await {
                // None means no packet available right now, continue awaiting the next one
                Ok(None) => {
                    continue;
                }
                Ok(Some(sendable)) => break Ok(Incoming::Data(sendable)),
                Err(err) => {
                    log::error!(target: "Dispatch", "Serialization error: {err:?}");
                    break Err(IncomingError::SerializationError);
                }
            }
        }
    };

    // Await either incoming event or data
    match select2(event_in.recv(), data_in).await {
        Select2::A(event) => {
            log::debug!(target: "Dispatch", "Incoming event...");
            match event {
                Ok(event) => return Ok(Incoming::Event(event)),
                Err(_) => return Err(IncomingError::EventQueueError),
            }
        }
        Select2::B(data_result) => data_result,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use sensor_link_protocol::{event::Event, TopicFromDevice};

    use super::*;
    use crate::{
        logic::network::upload::UploadTrait,
        storage::common::{
            queue::{ConfirmChannel, Queue, SeqNo},
            stream_store::MAX_PEEKS,
        },
        sync::reserving_sender::{create_reserving_channel, NotifyingReceiver},
        utils::{
            channels::{make_channel, Receiver},
            sync::ChangeNotification,
        },
    };

    type T = TopicFromDevice;
    const DATA_LEN: usize = 64;
    type Data = SerializedSendable<DATA_LEN, T>;

    /// In-memory stream backed by the real [Queue], so peeks and confirmations
    /// behave like the flash-backed store.
    struct MockStream {
        queue: Queue<'static, MAX_PEEKS>,
        items: Vec<Vec<u8>>,
    }

    impl MockStream {
        fn new() -> Self {
            let channel: &'static ConfirmChannel<MAX_PEEKS> =
                Box::leak(Box::new(ConfirmChannel::new()));
            Self {
                queue: Queue::new(channel),
                items: Vec::new(),
            }
        }

        fn store(&mut self, bytes: &[u8]) -> SeqNo {
            let seq_no = self.queue.enqueue().unwrap();
            assert_eq!(seq_no as usize, self.items.len());
            self.items.push(bytes.to_vec());
            seq_no
        }

        fn peek<const N: usize>(&mut self) -> Option<(SerializedSendable<N, T>, ConfirmHandle)> {
            let handle = self.queue.peek_next().ok()?;
            let bytes = &self.items[handle.seq_no() as usize];
            let mut builder = crate::serialize::Builder::<N>::new();
            builder.bytes[..bytes.len()].copy_from_slice(bytes);
            Some((
                builder.create_with_total_length(bytes.len()).unwrap(),
                handle,
            ))
        }
    }

    struct MockStore {
        events: MockStream,
        data: MockStream,
    }

    impl DispatchStore for MockStore {
        type Error = ();
        type Topic = T;

        async fn store_event<'a>(
            &mut self,
            event: &'a SerializedEvent<T>,
        ) -> Result<SeqNo, Self::Error> {
            Ok(self.events.store(event.as_slice()))
        }

        async fn peek_event(
            &mut self,
        ) -> Result<Option<(SerializedEvent<T>, ConfirmHandle)>, Self::Error> {
            Ok(self.events.peek())
        }

        async fn store_sensor_data<'a, const N: usize>(
            &mut self,
            data: &'a SerializedSendable<N, T>,
        ) -> Result<SeqNo, Self::Error> {
            Ok(self.data.store(data.as_slice()))
        }

        async fn peek_sensor_data<const N: usize>(
            &mut self,
        ) -> Result<Option<(SerializedSendable<N, T>, ConfirmHandle)>, Self::Error> {
            Ok(self.data.peek())
        }

        async fn is_drained(&mut self) -> Result<bool, Self::Error> {
            Ok(self.events.queue.is_drained() && self.data.queue.is_drained())
        }
    }

    /// Serializer holding buffered packets that are only released by a flush
    struct MockSerializer {
        buffered: VecDeque<Data>,
        flushing: bool,
    }

    impl LatencyControlledSerializer<DATA_LEN> for MockSerializer {
        type Error = ();
        type Topic = T;

        async fn next_packet(&mut self) -> Result<Option<Data>, Self::Error> {
            if self.flushing {
                let packet = self.buffered.pop_front();
                self.flushing = !self.buffered.is_empty();
                if packet.is_some() {
                    return Ok(packet);
                }
            }
            core::future::pending().await
        }

        fn set_timeout(&mut self, _timeout_ms: u32) {}
        fn set_buffer_timeout(&mut self, _timeout_ms: u32) {}

        fn flush(&mut self) {
            self.flushing = !self.buffered.is_empty();
        }

        fn is_flushing(&self) -> bool {
            self.flushing
        }
    }

    #[derive(Clone)]
    enum MockUpload {
        Event(Arc<SerializedEvent<T>>),
        Data(Arc<Data>),
    }

    impl UploadTrait for MockUpload {
        type Topic = T;
        fn sendable(&self) -> &dyn crate::serialize::Sendable<T> {
            match self {
                MockUpload::Event(event) => &**event,
                MockUpload::Data(data) => &**data,
            }
        }
    }

    struct EventAlloc;
    impl MappedAllocator for EventAlloc {
        type Input = SerializedEvent<T>;
        type Output = MockUpload;
        fn alloc(&self, value: Self::Input) -> Result<Self::Output, Self::Input> {
            Ok(MockUpload::Event(Arc::new(value)))
        }
    }

    struct DataAlloc;
    impl MappedAllocator for DataAlloc {
        type Input = Data;
        type Output = MockUpload;
        fn alloc(&self, value: Self::Input) -> Result<Self::Output, Self::Input> {
            Ok(MockUpload::Data(Arc::new(value)))
        }
    }

    struct MockUploadAlloc;
    impl UploadAlloc for MockUploadAlloc {
        type Upload = MockUpload;
        type Event = SerializedEvent<T>;
        type SensorData = Data;
        fn event(&self) -> impl MappedAllocator<Input = Self::Event, Output = Self::Upload> {
            EventAlloc
        }
        fn data(&self) -> impl MappedAllocator<Input = Self::SensorData, Output = Self::Upload> {
            DataAlloc
        }
    }

    fn data_packet() -> Data {
        let mut builder = crate::serialize::Builder::<DATA_LEN>::new();
        builder.bytes[..16].copy_from_slice(&[1; 16]);
        builder.create_with_total_length(16).unwrap()
    }

    /// Next upload handed to the network task
    async fn next_upload<R: ReceiveChannel<Confirmable<MockUpload>>>(
        upload_rx: &mut R,
    ) -> Confirmable<MockUpload> {
        match tokio::time::timeout(Duration::from_secs(3), upload_rx.recv()).await {
            Ok(Ok(upload)) => upload,
            _ => panic!("no upload"),
        }
    }

    /// Wait until dispatch reports drained, ignoring other signals
    async fn wait_drained(signal_rx: &mut Receiver<Signal>, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                if let Ok(Signal::DispatchDrained) = signal_rx.recv().await {
                    return;
                }
            }
        })
        .await
        .is_ok()
    }

    /// A barrier is reached only once everything received before it, including
    /// buffered data, has been sent and confirmed by the network task.
    #[tokio::test]
    async fn barrier_drained_after_all_confirmed() {
        let barrier: &'static DispatchBarrier = Box::leak(Box::new(DispatchBarrier::new()));
        let notifier: &'static ChangeNotification = Box::leak(Box::new(ChangeNotification::new()));

        let (mut event_tx, mut event_rx) = make_channel::<Event>(4);
        let (mut signal_tx, mut signal_rx) = make_channel::<Signal>(16);
        let (upload_tx, upload_rx) = make_channel::<Confirmable<MockUpload>>(1);
        let (mut upload_tx, mut upload_rx): (_, NotifyingReceiver<_, Receiver<_>, _>) =
            create_reserving_channel(upload_tx, upload_rx, notifier);

        tokio::spawn(async move {
            let mut store = MockStore {
                events: MockStream::new(),
                data: MockStream::new(),
            };
            // A data packet waiting in the bulk buffer: only a flush releases it
            let mut serializer = MockSerializer {
                buffered: VecDeque::from([data_packet()]),
                flushing: false,
            };
            dispatch_task::<_, _, _, _, _, _, _, _, Signal, _, DATA_LEN>(
                &mut store,
                &mut serializer,
                &mut event_rx,
                &mut signal_tx,
                &mut MockUploadAlloc,
                &mut upload_tx,
                |_: &Event| false,
                barrier,
            )
            .await
        });

        event_tx.send(Event::Blink).await.unwrap();
        barrier.request();

        // The event is sent, but not confirmed yet: not drained
        let event = next_upload(&mut upload_rx).await;
        assert!(matches!(event.inner(), MockUpload::Event(_)));
        assert!(!wait_drained(&mut signal_rx, Duration::from_millis(1500)).await);
        event.confirm();

        // The flush released the buffered data: not drained until it's confirmed
        let data = next_upload(&mut upload_rx).await;
        assert!(matches!(data.inner(), MockUpload::Data(_)));
        assert!(!wait_drained(&mut signal_rx, Duration::from_millis(1500)).await);
        data.confirm();

        assert!(wait_drained(&mut signal_rx, Duration::from_secs(3)).await);

        // One reply per request
        assert!(!wait_drained(&mut signal_rx, Duration::from_millis(1500)).await);
    }

    /// An aborted upload (dropped without confirming) keeps the barrier waiting until
    /// the retry is confirmed.
    #[tokio::test]
    async fn barrier_waits_for_retry_of_aborted_upload() {
        let barrier: &'static DispatchBarrier = Box::leak(Box::new(DispatchBarrier::new()));
        let notifier: &'static ChangeNotification = Box::leak(Box::new(ChangeNotification::new()));

        let (mut event_tx, mut event_rx) = make_channel::<Event>(4);
        let (mut signal_tx, mut signal_rx) = make_channel::<Signal>(16);
        let (upload_tx, upload_rx) = make_channel::<Confirmable<MockUpload>>(1);
        let (mut upload_tx, mut upload_rx): (_, NotifyingReceiver<_, Receiver<_>, _>) =
            create_reserving_channel(upload_tx, upload_rx, notifier);

        tokio::spawn(async move {
            let mut store = MockStore {
                events: MockStream::new(),
                data: MockStream::new(),
            };
            let mut serializer = MockSerializer {
                buffered: VecDeque::new(),
                flushing: false,
            };
            dispatch_task::<_, _, _, _, _, _, _, _, Signal, _, DATA_LEN>(
                &mut store,
                &mut serializer,
                &mut event_rx,
                &mut signal_tx,
                &mut MockUploadAlloc,
                &mut upload_tx,
                |_: &Event| false,
                barrier,
            )
            .await
        });

        event_tx.send(Event::Blink).await.unwrap();
        barrier.request();

        // Upload fails (dropped): the event must be retried before the barrier is reached
        drop(next_upload(&mut upload_rx).await);
        assert!(!wait_drained(&mut signal_rx, Duration::from_millis(1500)).await);

        // Retried
        next_upload(&mut upload_rx).await.confirm();

        assert!(wait_drained(&mut signal_rx, Duration::from_secs(3)).await);
    }
}
