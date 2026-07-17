pub mod buffer;
pub mod confirmable;
pub mod drain;
mod pending;
pub mod serialization;

use crate::{
    drivers::time,
    logic::{
        dispatch::{confirmable::Confirmable, serialization::LatencyControlledSerializer},
        network::upload::UploadAlloc,
        signal::Signal,
        ReceiveChannel, SendChannel,
    },
    monotonic_time::delay_ms,
    pool::MappedAllocator,
    serialize::{AsSendable, SerializedSendable},
    sync::reserving_sender::{ReservableSender, ReservationToken},
    utils::select::{select2, Select2},
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

        // Select between incoming data and transmission of pending data
        // NOTE: select2 has a bias to the first future, so storing incoming data always takes priority
        // over transmitting network data. This is important to prevent the incoming data queue from overflowing
        // in case of a super fast network connection.
        match select2(incoming(data_in, event_in), transmit_network_or_block).await {
            Select2::A(incoming) => match incoming {
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
            Select2::B(()) => {}
        }
    }
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
