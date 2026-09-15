//! A mock device: everything needed to drive the sensor-link dispatch and
//! network tasks against a real MQTT broker, and nothing more.
//!
//! The device has no orchestrator. The pieces the network task is generic over
//! (status payload, device metadata, action queue) are implemented here as the
//! smallest thing that satisfies each trait. The dispatch pipeline behind it is
//! the real one: simulated measurements are buffered, serialized, persisted to
//! an in-memory flash store and uploaded, as are events and the device's own log
//! records.

pub mod buffer;
pub mod pools;
pub mod store;
pub mod upload;

use std::time::Duration;

use chrono::Utc;
use sensor_link_firmware::{
    heapless,
    logic::{
        client::Client,
        dispatch::{confirmable::Confirmable, dispatch_task},
        network::network_task,
        signal::{BootReason, Signal},
        NetworkAction, NetworkActionNotifyReader, NetworkStatus, ReceiveChannel, SendChannel,
    },
    meta::DeviceMetaDataProvider,
    monotonic_time,
    mqtt::log_publish::LogPublisher,
    pool::Pool,
    sensor_link_protocol::{
        device_log::LogMessage,
        event::{Desc, Event},
        status::Status,
        TopicPayloadSerialize, MAX_MESSAGE_LEN,
    },
    storage::{
        backend::InMemoryFlash,
        flash_db::{block_layer::BlockDevice, Database},
    },
    sync::reserving_sender::{create_reserving_channel, ReservableSender},
    utils::{
        channels::{make_channel, Receiver, Sender},
        sync::{Arbiter, ChangeNotification},
    },
};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::{
    device::{
        buffer::{MockBuffer, MockResults, SampleData, MAX_INPUT_SIZE, NUM_CH_MOCK},
        store::{ConfirmChannels, StaticMockStore, BLOCK_SIZE},
        upload::{Upload, UploadAllocator},
    },
    net::Mqtt,
    signal_gen::{Offset, Signal as SimulatedSignal, SineSignal},
    SensorArgs,
};

/// How long the mock waits between syncs when nothing triggers an earlier one.
///
/// Mimics the orchestrator's `Schedule::SyncNetwork`: a real device is offline
/// between syncs and its store holds everything produced in the meantime.
pub const DEFAULT_SYNC_INTERVAL: Duration = Duration::from_secs(300);

/// Sample rate of the simulated sensor [Hz].
const SAMPLE_RATE_HZ: f32 = 1.0;

/// Number of samples per measurement in one result handed to dispatch.
const SAMPLES_PER_RESULT: usize = 1;

const _: () = assert!(SAMPLES_PER_RESULT <= MAX_INPUT_SIZE);

/// Size of the simulated flash the store runs on.
///
/// Must cover the block ranges [`store::Stream`] lays out; if configured
/// smaller, the dispatch task fails with a `CorruptOutOfBounds` error.
const FLASH_MEMORY_BYTES: usize = 32 * 1024 * 1024;

type FlashDBImpl = InMemoryFlash<BLOCK_SIZE>;
type DB =
    Arbiter<Database<BlockDevice<FlashDBImpl, BLOCK_SIZE>, BLOCK_SIZE, store::File, store::Stream>>;

type EventRef = <pools::EventPool as Pool>::Arc;
type SensorDataRef = <pools::SensorDataPool as Pool>::Arc;
type LogRef = <pools::LogPool as Pool>::Arc;

type MockUpload = Upload<EventRef, SensorDataRef, LogRef>;

/// Device-status payload the mock publishes on the `status` topic.
///
/// A real device reports its own operational state here; the mock is always
/// [`Status::Active`] and only the signal strength (injected by the client at
/// send time) actually varies.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MockStatus {
    status: Status,
    signal_strength: i32,
}

impl TopicPayloadSerialize<MAX_MESSAGE_LEN> for MockStatus {}

impl NetworkStatus for MockStatus {
    fn set_signal_strength(&mut self, dbm: i32) {
        self.signal_strength = dbm;
    }
}

/// Wire device type of the mock.
///
/// The metadata provider is generic over this, so the mock only has to be
/// serializable, not a member of any product's device-type enum.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MockDeviceType {
    Mock,
}

/// Device metadata published on the `info` topic.
pub struct MockDescriptor;

impl DeviceMetaDataProvider for MockDescriptor {
    type DeviceType = MockDeviceType;

    fn device_type(&self) -> Self::DeviceType {
        MockDeviceType::Mock
    }

    fn bootloader_version(&self) -> &'static str {
        "0.0.0"
    }

    fn git_rev() -> &'static str {
        "mock"
    }

    fn fw_version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn hw_rev(&self) -> &'static str {
        "tokio"
    }
}

/// The actions the mock asks the network task to perform.
///
/// A real device queues these from its orchestrator; here they come from
/// [`action_task`], which publishes device info once at startup and the status
/// on a fixed interval.
struct ActionQueue(Mutex<Receiver<NetworkAction<MockStatus>>>);

// For `&ActionQueue`, so the same queue can be handed to every reconnect of the
// network task (which takes its action list by value).
impl NetworkActionNotifyReader<MockStatus> for &ActionQueue {
    async fn next_action(&self) -> NetworkAction<MockStatus> {
        loop {
            if let Ok(action) = self.0.lock().await.recv().await {
                return action;
            }
        }
    }

    fn try_next_action(&self) -> Option<NetworkAction<MockStatus>> {
        self.0.try_lock().ok()?.try_recv().ok()
    }
}

/// Run one mock device instance until the process exits.
///
/// `log_source` is the receiving end of the `mqtt-log` queue. Only one instance
/// can own it (the logger is global), so instances spawned beyond the first get
/// `None` and upload no log records of their own.
pub async fn run_instance(
    args: SensorArgs,
    log_source: Option<LogPublisher>,
    mut signal_tx: Sender<Signal>,
    signal_rx: Receiver<Signal>,
) {
    let sync_interval = args.sync_interval;
    let instance_no = args.instance_no;
    log::info!(target: "Device", "Starting mock device {instance_no}");

    // The store is borrowed by the dispatch task for as long as it runs, which
    // is until the process exits.
    let persistent_store: &'static DB = {
        let in_memory_flash = InMemoryFlash::new(FLASH_MEMORY_BYTES);
        let blockdev = BlockDevice::writeable_from(in_memory_flash).unwrap();
        Box::leak(Box::new(Arbiter::new(Database::new(blockdev))))
    };

    signal_tx
        .send(Signal::Booted(BootReason::PowerOn))
        .await
        .ok();

    // Act on what the orchestrator of a real device would act on: this is where
    // commands from the server surface, and where a sync is triggered early.
    // Capacity 1: a trigger that arrives while a sync is already pending is
    // redundant, and the one that fits is enough to start the next sync.
    let (sync_tx, mut sync_rx) = make_channel::<SyncReason>(1);
    tokio::spawn(signal_task(signal_rx, sync_tx));

    let (action_tx, action_rx) = make_channel::<NetworkAction<MockStatus>>(4);
    tokio::spawn(action_task(action_tx, sync_interval));
    let action_list = ActionQueue(Mutex::new(action_rx));

    let (event_tx, event_rx) = make_channel::<Event>(10);
    let (data_tx, data_rx) = make_channel::<MockResults>(5);
    tokio::spawn(measuring_task(instance_no, data_tx, event_tx));

    // The notifier is borrowed by both ends of the reserving channel for as long
    // as they live, which is until the process exits.
    let space_notifier = &*Box::leak(Box::new(ChangeNotification::new()));
    let (upload_tx, upload_rx) = {
        // Capacity 1: the store is the buffer, so an upload is only taken out of
        // it once the network task is ready for the next one.
        let (tx, rx) = make_channel::<Confirmable<MockUpload>>(1);
        create_reserving_channel(tx, rx, space_notifier)
    };

    {
        let signal_tx = signal_tx.clone();
        tokio::spawn(async move {
            dispatch_task_for(
                persistent_store,
                data_rx,
                event_rx,
                log_source,
                signal_tx,
                upload_tx,
            )
            .await;
        });
    }

    let driver = Mqtt::new(args);
    log::info!(target: "Device", "Connecting as client {:?}", driver.client_id);
    let uid =
        heapless::String::try_from(driver.client_id.as_str()).expect("Client id fits MAX_UID_LEN");
    let mut client = Client::<_, MockStatus>::new(driver, uid);

    let mut descriptor = MockDescriptor;
    let mut upload_rx = upload_rx;

    // Stand in for the orchestrator's `Action::SpawnNetwork`: run the network
    // task once per sync, then stay offline until the next one. Everything
    // produced in between waits in the store and is uploaded on the next sync.
    //
    // The first sync happens immediately, so the device reports itself at boot.
    loop {
        network_task::<_, _, _, _, monotonic_time::Time, _, _>(
            &mut signal_tx,
            &mut client,
            &action_list,
            &mut upload_rx,
            &mut descriptor,
        )
        .await;

        log::info!(
            target: "Device",
            "Sync finished, next one in {}s (or when triggered)",
            sync_interval.as_secs(),
        );

        // `Receiver::recv` is awaited through its inner tokio channel because
        // `select!` may cancel it: tokio's is cancel-safe, so a trigger racing
        // the interval is not lost.
        let reason = tokio::select! {
            _ = tokio::time::sleep(sync_interval) => SyncReason::Scheduled,
            trigger = sync_rx.0.recv() => match trigger {
                Some(reason) => reason,
                // The signal task is gone, so nothing can trigger a sync again.
                None => SyncReason::Scheduled,
            },
        };
        log::info!(target: "Device", "Starting sync: {reason:?}");
    }
}

/// Why a sync is starting.
#[derive(Debug, Clone, Copy)]
pub enum SyncReason {
    /// The sync interval elapsed.
    Scheduled,
    /// Something urgent is waiting to be uploaded, or the user asked for a sync.
    Triggered,
}

/// Persists and serializes everything the device has to say, then hands it to
/// the network task in priority order.
///
/// `log_source` is optional because only one instance can own the global
/// logger's queue; an instance without it simply has no log records to dispatch.
async fn dispatch_task_for<U>(
    db: &'static DB,
    data_rx: Receiver<MockResults>,
    mut event_rx: Receiver<Event>,
    log_source: Option<LogPublisher>,
    mut signal_tx: Sender<Signal>,
    mut upload_tx: U,
) where
    U: ReservableSender<Confirmable<MockUpload>>,
{
    let confirm_channels = Box::leak(Box::new(ConfirmChannels::new()));
    let mut store = StaticMockStore::new(db, confirm_channels);

    if let Err(error) = store.initialize().await {
        log::error!(target: "Dispatch", "Store init failed: {error:?}");
        return;
    }
    log::info!(target: "Dispatch", "Store initialized");

    let mut buffer = MockBuffer::with_default_timing(data_rx);

    let mut upload_allocator = UploadAllocator::new(
        pools::EventPool.allocator(),
        pools::SensorDataPool.allocator(),
        pools::LogPool.allocator(),
    );

    let mut log_source = NoLogs::or(log_source);

    dispatch_task(
        &mut store,
        &mut buffer,
        &mut event_rx,
        &mut log_source,
        &mut signal_tx,
        &mut upload_allocator,
        &mut upload_tx,
        Event::is_urgent,
    )
    .await
}

/// Log source for an instance that does not own the global logger's queue.
///
/// It never yields a record, so the dispatch task simply never has a log to
/// persist.
enum NoLogs {
    Publisher(LogPublisher),
    Silent,
}

impl NoLogs {
    fn or(publisher: Option<LogPublisher>) -> Self {
        match publisher {
            Some(publisher) => Self::Publisher(publisher),
            None => Self::Silent,
        }
    }
}

impl ReceiveChannel<LogMessage> for NoLogs {
    type Error = ();

    async fn recv(&mut self) -> Result<LogMessage, Self::Error> {
        match self {
            Self::Publisher(publisher) => publisher.recv().await.map_err(|_| ()),
            Self::Silent => core::future::pending().await,
        }
    }

    fn try_recv(&mut self) -> Result<LogMessage, Self::Error> {
        match self {
            Self::Publisher(publisher) => publisher.try_recv().map_err(|_| ()),
            Self::Silent => Err(()),
        }
    }
}

/// Samples a simulated sensor and feeds the readings into the dispatch pipeline.
async fn measuring_task(
    instance_no: usize,
    mut data_tx: Sender<MockResults>,
    mut event_tx: Sender<Event>,
) {
    log::info!(target: "Sensor", "Starting measuring task");
    event_tx
        .send(Event::Started(desc("mock measuring")))
        .await
        .ok();

    // One slow sine per channel, offset per instance so instances are
    // distinguishable on the wire.
    let base = 20.0 + instance_no as f64;
    let signals: [Offset<SineSignal>; NUM_CH_MOCK] = core::array::from_fn(|ch| {
        Offset::new(
            base + ch as f64 * 10.0,
            SineSignal {
                amplitude: 5.0,
                frequency: 0.01,
                phase: ch as f64 * core::f64::consts::FRAC_PI_2,
            },
        )
    });

    let period_ms = (1000.0 / SAMPLE_RATE_HZ) as i64 * SAMPLES_PER_RESULT as i64;
    let mut interval =
        tokio::time::interval(Duration::from_millis(period_ms.try_into().unwrap_or(1000)));
    let mut t = Utc::now();

    loop {
        interval.tick().await;

        let t_start = t.timestamp_micros();
        let samples: [[f32; SAMPLES_PER_RESULT]; NUM_CH_MOCK] = core::array::from_fn(|ch| {
            core::array::from_fn(|i| {
                let seconds = timestamp_secs(t) + i as f64 / SAMPLE_RATE_HZ as f64;
                signals[ch].compute(seconds) as f32
            })
        });

        let channels: [&[f32]; NUM_CH_MOCK] = core::array::from_fn(|ch| samples[ch].as_slice());
        let data = SampleData::from_slices(t_start, SAMPLE_RATE_HZ, channels);

        if let Err(result) = data_tx.try_send(MockResults::Data(data)) {
            log::warn!(target: "Sensor", "Measuring task: failed to send data: channel full!");
            if data_tx.send(result).await.is_err() {
                break;
            }
        }

        t += chrono::TimeDelta::milliseconds(period_ms);
    }

    event_tx
        .send(Event::Stopped(desc("mock measuring")))
        .await
        .ok();
}

/// Build an event description, truncated to what the payload can hold.
fn desc(message: &str) -> Desc {
    let mut desc = Desc::empty();
    for c in message.chars() {
        if core::fmt::Write::write_char(&mut desc, c).is_err() {
            break;
        }
    }
    desc
}

fn timestamp_secs(t: chrono::DateTime<Utc>) -> f64 {
    t.timestamp() as f64 + t.timestamp_subsec_micros() as f64 * 1e-6
}

/// Stands in for the orchestrator: logs the signals the network and dispatch
/// tasks emit, and triggers an early sync for the ones that warrant it.
async fn signal_task(mut signal_rx: Receiver<Signal>, mut sync_tx: Sender<SyncReason>) {
    while let Ok(signal) = signal_rx.recv().await {
        match signal {
            // Raised once per dispatch-task iteration, so logging it at a level
            // that gets published feeds itself: publishing the record empties
            // the queue, which raises the signal again. Kept at trace, which is
            // below any level worth publishing over the air.
            Signal::DispatchQueueEmpty => {
                log::trace!(target: "Device", "Signal: {signal:?}")
            }
            _ => log::info!(target: "Device", "Signal: {signal:?}"),
        }

        // An urgent event is what the dispatch task raises for data that should
        // not wait for the next scheduled sync; it is also what the `sync`
        // command sends.
        if matches!(signal, Signal::UrgentEvent) {
            // Full queue: a sync is already pending, so this one is redundant.
            sync_tx.try_send(SyncReason::Triggered).ok();
        }
    }
}

/// Queues the network actions the mock performs: its device info once at
/// startup, then its status once per sync.
async fn action_task(mut action_tx: Sender<NetworkAction<MockStatus>>, sync_interval: Duration) {
    action_tx.send(NetworkAction::SendDeviceInfo).await.ok();

    // Once per sync: the device is offline in between, so a faster cadence would
    // only queue up statuses to publish in a burst on the next connection.
    let mut interval = tokio::time::interval(sync_interval);
    loop {
        interval.tick().await;
        let status = MockStatus {
            status: Status::Active,
            signal_strength: 0,
        };
        action_tx.send(NetworkAction::SendStatus(status)).await.ok();
    }
}
