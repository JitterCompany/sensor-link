//! One simulated demo device: the orchestrator, processing, dispatch and network
//! of the demo firmware on tokio, with simulated measurements and in-memory
//! flash.

use std::{ops::DerefMut, sync::Arc};

use chrono::Utc;
use demo_firmware_core::{
    demo_protocol::{event::Event, status::ChannelStatus, DemoChannel},
    logic::{
        client::Client,
        demo_processing::{self, input::Samples, TASK_INIT_TIMEOUT_MS},
        dispatch::demo_buffer::{DemoBuffer, DemoResults},
        orch_poll_timer_task,
        orchestrator::{demo_behavior, Action, Orchestrator},
        settings::Settings,
        signal::{AllChannelStatus, CommonSignal, Signal},
        upload, AppState, Cancel, DeviceStatusUpload, FiniteStream, NetworkActionList,
        NetworkActionNotifier, ProcessingOutputAdapter, ProcessingOutputT, Schedule,
    },
    meta::{firmware::DeviceType, DeviceDescriptor},
    storage::{
        files::{File, FileStore},
        streams::{ConfirmChannels, StaticDemoStore, Stream, BLOCK_SIZE},
    },
    sync::session_channel::{
        create_session_channel, traits::SessionSender as _, SessionReceiver, SessionSender,
        SessionState,
    },
};
use sensor_link_firmware::{
    heapless,
    logic::{
        dispatch::confirmable::Confirmable, signal::BootReason, LatestValueSendChannel,
        ReceiveChannel, SendChannel,
    },
    monotonic_time::{self, FutureTimeout as _},
    pool::{self, Pool as _},
    storage::{
        backend::InMemoryFlash,
        flash_db::{block_layer::BlockDevice, Database},
    },
    sync::reserving_sender::{create_reserving_channel, ReservableSender},
    utils::{
        channels::*,
        sync::{Arbiter, ChangeNotification},
    },
};
use tokio::{
    select,
    sync::{Mutex, Semaphore},
};

use crate::{
    device::{
        pools,
        sensor::{batch_duration, SignalGenerator},
    },
    net::Mqtt,
    SensorArgs,
};

// Note: 32MiB matches the hardware. If configured smaller than the 'filesystem' (File and Stream enum),
// the dispatch task will fail with a CorruptOutOfBounds error.
const FLASH_MEMORY_BYTES: usize = 32 * 1024 * 1024;
type FlashDBImpl = InMemoryFlash<BLOCK_SIZE>;
type DB = Arbiter<Database<BlockDevice<FlashDBImpl, BLOCK_SIZE>, BLOCK_SIZE, File, Stream>>;

type EventRef = <pools::EventPool as pool::Pool>::Arc;
type SensorDataRef = <pools::SensorDataPool as pool::Pool>::Arc;

type Upload = upload::Upload<EventRef, SensorDataRef>;
type ToNetwork =
    NetworkActionNotifier<ChangeNotification, Arbiter<NetworkActionList<DeviceStatusUpload>>>;
type Orch<'a> = Orchestrator<
    demo_behavior::DemoBehavior<FileStore<&'static DB>>,
    Receiver<Signal>,
    Sender<Signal>,
    &'a ToNetwork,
    Sender<Event>,
    LatestValueSender<AppState>,
>;

type ProcessingOutput = ProcessingOutputT<DemoResults>;
type MeasurementsTx =
    SessionSender<Samples, Sender<FiniteStream<Samples>>, LatestValueReceiver<SessionState>>;
type MeasurementsRx =
    SessionReceiver<Samples, Receiver<FiniteStream<Samples>>, LatestValueSender<SessionState>>;

/// What a measurement session needs, shared by the sessions one after another
struct Session {
    measurements_tx: MeasurementsTx,
    cancel_rx: Receiver<Cancel>,
}

/// What a processing session needs, as [`Session`]
struct Processing {
    measurements_rx: MeasurementsRx,
    processed_tx: Sender<ProcessingOutput>,
    settings_rx: LatestValueReceiver<Settings>,
}

pub async fn run_instance(args: SensorArgs, tx: Sender<Signal>, rx: Receiver<Signal>) {
    orchestrator_task(args, tx, rx).await;
}

async fn orchestrator_task(
    args: SensorArgs,
    signal_tx: Sender<Signal>,
    signal_rx: Receiver<Signal>,
) {
    log::info!(target: "Orchestrator TASK", "Start");

    // Init global state
    let persistent_store: &'static DB = {
        let in_memory_flash = InMemoryFlash::new(FLASH_MEMORY_BYTES);
        let blockdev = BlockDevice::writeable_from(in_memory_flash).unwrap();
        let persistent_store = Database::new(blockdev);
        Box::leak(Box::new(Arbiter::new(persistent_store)))
    };

    let (event_tx, event_rx) = make_channel::<Event>(10);

    let (processed_data_tx, processed_data_rx) = make_channel::<ProcessingOutput>(5);

    let processed_to_dispatch = ProcessingOutputAdapter::new(processed_data_rx);

    // Same capacity as the firmware's upload channel
    let (upload_tx, upload_rx) = {
        let notifier = &*Box::leak(Box::new(ChangeNotification::new()));
        let (upload_tx, upload_rx) = make_channel::<Confirmable<Upload>>(1);
        create_reserving_channel(upload_tx, upload_rx, notifier)
    };

    let network_action_notifier: &'static ToNetwork =
        &*Box::leak(Box::new(NetworkActionNotifier::new(
            Arbiter::new(NetworkActionList::new()),
            ChangeNotification::new(),
        )));

    let mut orch: Orch = {
        let file_store = FileStore::new(persistent_store);
        let behavior = demo_behavior::DemoBehavior::new(file_store);
        let (to_ui_tx, _to_ui_rx) = make_latest_value_channel::<AppState>(AppState::Boot);
        let orch = Orchestrator::<_, _, _, _, _, _>::new(
            behavior,
            signal_rx,
            signal_tx.clone(),
            network_action_notifier,
            event_tx,
            to_ui_tx,
        );
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Booted(BootReason::PowerOn)))
            .await
            .unwrap();
        orch
    };

    let (mut timer_state_tx, mut timer_state_rx) = make_latest_value_channel::<i64>(i64::MAX);
    let (mut timer_sync_tx, mut timer_sync_rx) = make_latest_value_channel::<i64>(i64::MAX);
    let (mut timer_startstop_tx, mut timer_startstop_rx) =
        make_latest_value_channel::<i64>(i64::MAX);
    let (mut timer_debounce_tx, mut timer_debounce_rx) = make_latest_value_channel::<i64>(i64::MAX);

    // spawn all orchestrator timers
    {
        let mut state_tx = orch.signal_sender();
        tokio::spawn(async move {
            orch_poll_timer_task::<Sender<Signal>, LatestValueReceiver<i64>>(
                &mut timer_state_rx,
                &mut state_tx,
                Schedule::State,
            )
            .await
        });
        let mut sync_tx = orch.signal_sender();
        tokio::spawn(async move {
            orch_poll_timer_task::<Sender<Signal>, LatestValueReceiver<i64>>(
                &mut timer_sync_rx,
                &mut sync_tx,
                Schedule::SyncNetwork,
            )
            .await
        });
        let mut startstop_tx = orch.signal_sender();
        tokio::spawn(async move {
            orch_poll_timer_task::<Sender<Signal>, LatestValueReceiver<i64>>(
                &mut timer_startstop_rx,
                &mut startstop_tx,
                Schedule::StartStop,
            )
            .await
        });
        let mut debounce_tx = orch.signal_sender();
        tokio::spawn(async move {
            orch_poll_timer_task::<Sender<Signal>, LatestValueReceiver<i64>>(
                &mut timer_debounce_rx,
                &mut debounce_tx,
                Schedule::StatusUpdate,
            )
            .await
        });
    }

    // Wrap parameters for network task in a mutex as that task is spawned multiple times
    let upload_rx_mutex = Arc::new(Mutex::new(upload_rx));
    let to_network_in_mutex = Arc::new(Mutex::new(network_action_notifier));

    // Spawn dispatch task
    {
        let signal_tx = signal_tx.clone();
        tokio::spawn(async move {
            dispatch_task(
                persistent_store,
                processed_to_dispatch,
                event_rx,
                signal_tx,
                upload_tx,
            )
            .await;
        });
    }

    let net_semaphore: Arc<Semaphore> = Arc::new(Semaphore::new(1));

    // The measurement and processing sessions, as the firmware runs them: the
    // session channel and the settings outlive each session.
    let (measuring_cancel_tx, cancel_rx) = make_channel::<Cancel>(1);
    let (mut settings_tx, settings_rx) = make_latest_value_channel(Settings::default());
    let (measurements_tx, measurements_rx) = {
        let (data_tx, data_rx) = make_channel::<FiniteStream<Samples>>(5);
        let (state_tx, state_rx) = make_latest_value_channel(SessionState::None);
        create_session_channel(data_tx, data_rx, state_tx, state_rx)
    };
    let session = Arc::new(Mutex::new(Session {
        measurements_tx,
        cancel_rx,
    }));
    let processing = Arc::new(Mutex::new(Processing {
        measurements_rx,
        processed_tx: processed_data_tx,
        settings_rx,
    }));

    loop {
        for action in orch.run().await {
            match action {
                Action::SpawnNetwork => {
                    if let Ok(permit) = net_semaphore.clone().try_acquire_owned() {
                        let signal_tx = orch.signal_sender();
                        let rxcl = to_network_in_mutex.clone();
                        let ucl = upload_rx_mutex.clone();
                        let sensor_args = args.clone();
                        tokio::spawn(async move {
                            network_task(sensor_args, signal_tx, rxcl, ucl).await;
                            drop(permit);
                        });
                    }
                }
                Action::Blink => {
                    log::info!(target: "Main", "blink");
                }
                Action::Reboot => {
                    log::info!(target: "Main", "reboot");
                }
                Action::StartMeasuring(settings) => {
                    let (Ok(processing), Ok(session)) = (
                        processing.clone().try_lock_owned(),
                        session.clone().try_lock_owned(),
                    ) else {
                        log::warn!(target: "Main", "Failed to spawn processing (still running?)");
                        orch.signal_sender()
                            .try_send(Signal::Common(CommonSignal::MeasurementStillRunning))
                            .ok();
                        continue;
                    };
                    let signal_tx = orch.signal_sender();
                    tokio::spawn(async move {
                        let mut processing = processing;
                        let Processing {
                            measurements_rx,
                            processed_tx,
                            settings_rx,
                        } = processing.deref_mut();
                        let mut signal_tx = signal_tx;
                        demo_processing::task(
                            measurements_rx,
                            processed_tx,
                            settings,
                            settings_rx,
                            &mut signal_tx,
                        )
                        .await
                    });
                    let signal_tx = orch.signal_sender();
                    let event_tx = orch.event_sender();
                    tokio::spawn(async move {
                        let mut session = session;
                        measuring_task(session.deref_mut(), signal_tx, event_tx).await
                    });
                }
                Action::StopMeasuring => {
                    // Stop measurement task
                    measuring_cancel_tx.0.send(Cancel).await.ok();
                }
                Action::ApplySettings(settings) => {
                    settings_tx.send(settings).ok();
                }
                Action::ScheduleTimer(changes) => {
                    for (schedule, dt) in changes.iter() {
                        match schedule {
                            Schedule::State => &mut timer_state_tx,
                            Schedule::SyncNetwork => &mut timer_sync_tx,
                            Schedule::StartStop => &mut timer_startstop_tx,
                            Schedule::StatusUpdate => &mut timer_debounce_tx,
                        }
                        .send(dt)
                        .unwrap(); // should not fail, panic is ok in mock-sensor
                    }
                }
                Action::FeedWatchdog => {
                    // Nothing to do, mock has no watchdog..
                }
            }
        }
    }
}

async fn dispatch_task<U>(
    db: &'static DB,
    processed_to_dispatch: ProcessingOutputAdapter<Receiver<ProcessingOutput>, DemoResults>,
    mut event_rx: Receiver<Event>,
    mut signal_tx: Sender<Signal>,
    mut upload_tx: U,
) where
    U: ReservableSender<Confirmable<Upload>>,
{
    let confirm_channels = Box::leak(Box::new(ConfirmChannels::new()));
    let mut store = StaticDemoStore::new(db, confirm_channels);

    if let Err(error) = store.initialize().await {
        log::error!(target: "Orchestrator TASK", "Stream store init failed: {error:?}");
        return;
    }
    log::info!(target: "Orchestrator TASK", "Stream store initialized!");

    let mut demo_buffer = DemoBuffer::with_default_timing(processed_to_dispatch);

    let mut upload_allocator = upload::UploadAllocator::new(
        pools::EventPool.allocator(),
        pools::SensorDataPool.allocator(),
    );
    // Required by dispatch_task; nothing requests a barrier
    static BARRIER: demo_firmware_core::logic::dispatch::DispatchBarrier =
        demo_firmware_core::logic::dispatch::DispatchBarrier::new();
    demo_firmware_core::logic::dispatch::dispatch_task(
        &mut store,
        &mut demo_buffer,
        &mut event_rx,
        &mut signal_tx,
        &mut upload_allocator,
        &mut upload_tx,
        |e| e.is_urgent(),
        &BARRIER,
    )
    .await;
}

/// The firmware's measurement task with simulated channels: one batch of
/// samples per second into the processing session.
async fn measuring_task(
    session: &mut Session,
    mut signal_queue: Sender<Signal>,
    mut event_queue: Sender<Event>,
) {
    let Session {
        measurements_tx,
        cancel_rx,
    } = session;
    // A stop sent while no session ran must not end this one
    while cancel_rx.try_recv().is_ok() {}

    log::info!("Starting measuring task");
    let started = measurements_tx
        .wait_started()
        .with_timeout_ms(TASK_INIT_TIMEOUT_MS)
        .await;
    if !matches!(started, Some(Ok(_))) {
        log::error!(target: "Measurement", "Data channel error");
        event_queue.send(Event::sensor_error("Channel")).await.ok();
        signal_queue
            .send(Signal::Common(CommonSignal::SensorFailed))
            .await
            .ok();
        return;
    }

    signal_queue
        .send(Signal::Common(CommonSignal::SensorStarted))
        .await
        .unwrap();

    signal_queue
        .send(Signal::ChannelStatus(AllChannelStatus {
            sensors: DemoChannel::ALL.map(|ch| ChannelStatus::Ok(ch.sensor())),
        }))
        .await
        .unwrap();

    // Note: interval is more accurate than sleep (not sensitive to overhead in the loop)
    let mut interval = tokio::time::interval(batch_duration());
    let mut generator = SignalGenerator::new();
    log::info!("Using signal generator: {generator:?}");
    let mut t = Utc::now();

    loop {
        select! {
            _ = interval.tick() => {}
            _ = cancel_rx.recv() => break,
        }

        let samples = generator.generate(t);
        if let Err(samples) = measurements_tx.try_send(samples) {
            log::warn!("Measuring task: data channel full!");
            event_queue
                .send(Event::processing_warning("Data channel full"))
                .await
                .ok();
            measurements_tx.send(samples).await.ok();
        }

        t += chrono::TimeDelta::from_std(batch_duration()).unwrap();
    }

    // notify processing of the end of stream & wait untill it has finished
    measurements_tx.finish_and_wait().await.ok();
    signal_queue
        .send(Signal::Common(CommonSignal::SensorStopped))
        .await
        .unwrap();
}

async fn network_task<RX>(
    args: SensorArgs,
    mut event_queue: Sender<Signal>,
    action_rx: Arc<Mutex<&ToNetwork>>,
    upload_rx: Arc<Mutex<RX>>,
) where
    RX: ReceiveChannel<Confirmable<Upload>> + demo_firmware_core::logic::DrainOnDisconnect<Upload>,
{
    log::info!(target: "Network", "Starting network task");
    // Note: these locks are necessary:
    // even though only one instance of this task *should* be spawned per sensor at a time,
    // this is not guaranteed or known by the compiler.
    // (the mock sensor spawns multiple network tasks which is not indended and not possible on RTIC)
    let mut action_guard = action_rx.lock().await;
    let mut upload_guard = upload_rx.lock().await;

    let a_rx = *action_guard.deref_mut();
    let u_rx = upload_guard.deref_mut();

    // Create connection.
    let mqtt_driver = Mqtt::new(args.clone());
    log::info!("Connecting as client {:?}", mqtt_driver.client_id);
    let client_id = heapless::String::try_from(mqtt_driver.client_id.as_str()).unwrap();

    let mut conn = Client::new(mqtt_driver, client_id);
    let mut desc = DeviceDescriptor::new("tokio", DeviceType::Demo.into(), "mock-device");

    demo_firmware_core::logic::network_task::<_, _, _, _, monotonic_time::Time, _, _>(
        &mut event_queue,
        &mut conn,
        a_rx,
        u_rx,
        &mut desc,
    )
    .await;

    log::info!(target: "Network", "Network task finished");
}
