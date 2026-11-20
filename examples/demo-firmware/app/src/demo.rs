//! Demo firmware for the SL23-modem-tester board.
//!
//! Samples the board temperature and the charger's bus voltage, bus current and
//! system voltage, processes them to the configured data rate and uploads them
//! over MQTT through the EC21/EC25 modem. Settings, commands, status, events and
//! firmware updates use the sensor-link protocol. See `examples/README.md`.
#![no_std]
#![no_main]

#[macro_use]
mod channels;
mod led;
mod measurement;
mod network;
mod pools;
mod rtt_logger;
mod sections;
mod sleep_gate;
mod time;

use core::{fmt::Write, mem::MaybeUninit, panic::PanicInfo};
use rtic::app;

use crate::{
    channels::{
        create_latest_value_channel, Arbiter, ChangeNotification, LatestValueReceiver,
        LatestValueSender,
    },
    measurement::{measurement_task, Sensors},
    time::init as time_init,
};
use demo_firmware_core::{
    demo_protocol::{cmd::Cmd, event::Event},
    drivers::{
        boot_stats,
        bq25672::{BatteryCharger as _, BQ25672},
        quectel, spi_flash,
    },
    embassy_sync::{self, blocking_mutex::raw::CriticalSectionRawMutex},
    embedded_hal::digital::InputPin as _,
    log,
    logic::{
        client::Client,
        demo_processing,
        dispatch::{confirmable::Confirmable, demo_buffer::DemoResults},
        orchestrator::{demo_behavior, Action, Orchestrator},
        signal::{CmdSource, CommonSignal, NetworkSignal, Signal, Temperature},
        upload, AppState, Cancel, DeviceStatusUpload, LatestValueSendChannel, NetworkActionList,
        NetworkActionNotifier, ProcessingOutputT, Schedule, SendChannel,
    },
    meta::firmware,
    monotonic_time::{self, delay_ms},
    pool::{self, Pool},
    storage::{
        files::{File, FileStore},
        flash_db::{block_layer::BlockDevice, Database},
    },
    sync::reserving_sender::{create_reserving_channel, NotifyingReceiver, ReservingSender},
    traits::Arbiter as _,
};
use hardware::{internal_flash::InternalFlash, mono::Mono, PowerSwitch as _};
use rtt_target::{DownChannel, UpChannel};

type Measurements = demo_processing::input::Samples;
type ProcessingOutput = ProcessingOutputT<DemoResults>;

define_session_channel!(Measurements, 5);
define_channel!(ProcessingOutput, 5);
define_channel!(Signal, 10);
define_channel!(Event, 10);
define_signal!(Cancel);
type ProcessingSettings = demo_firmware_core::logic::settings::Settings;
define_signal!(ProcessingSettings);

type AppStateRx = LatestValueReceiver<'static, AppState>;

type EventPoolItem = <pools::EventPool as pool::Pool>::Arc;
type DataPoolItem = <pools::SensorDataPool as pool::Pool>::Arc;

type Upload = Confirmable<upload::Upload<EventPoolItem, DataPoolItem>>;
define_channel!(Upload, 1);

type ScheduleState = i64;
type ScheduleSyncNetwork = i64;
type ScheduleDebounce = i64;
type ScheduleStartStop = i64;
define_signal!(ScheduleState);
define_signal!(ScheduleSyncNetwork);
define_signal!(ScheduleStartStop);
define_signal!(ScheduleDebounce);

static APP_STATE_CHANNEL: embassy_sync::signal::Signal<CriticalSectionRawMutex, AppState> =
    embassy_sync::signal::Signal::new();

type ToNetwork =
    NetworkActionNotifier<ChangeNotification, Arbiter<NetworkActionList<DeviceStatusUpload>>>;

static NETWORK_ACTION_NOTIFIER: ToNetwork = NetworkActionNotifier::new(
    Arbiter::new(NetworkActionList::new()),
    ChangeNotification::new(),
);

type MonoTime = monotonic_time::Time;
type Charger = BQ25672<hardware::I2C, hardware::Pin, hardware::Trigger, MonoTime>;
/// The charger is shared by the power-up sequence and the measurements
type ChargerArbiter = Arbiter<Charger>;
/// The ADC is shared by the measurements and the board temperature task
type BoardAdc = Arbiter<hardware::Adc<MonoTime>>;
type Rail5V5 = hardware::GpioPowerSwitch<hardware::Pin, hardware::TriggeredInputPin>;

/// The broker CA, from the file `DEMO_MQTT_CA` names at build time
const MQTT_CA_CERT: &str = include_str!(env!("DEMO_MQTT_CA"));

use cortex_m_rt::{exception, ExceptionFrame};

#[exception]
unsafe fn HardFault(ef: &ExceptionFrame) -> ! {
    // Set flag in backup register to track next reboot reason
    // (Panic handler or watchdog will eventually reboot the system)
    let mut stat = hardware::boot_stat_register(boot_stats::Stat::Fault);
    boot_stats::set_flag(&mut stat);

    panic!("HardFault: {:#?}", ef);
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) {
    panic!("DefaultHandler: unexpected irq {irqn}");
}

#[inline(never)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    critical_section::with(|_| {
        // Set flag in backup register to track next reboot reason
        let mut stat = unsafe { hardware::boot_stat_register(boot_stats::Stat::Panic) };
        boot_stats::set_flag(&mut stat);

        if let Some(mut channel) = unsafe { UpChannel::conjure(0) } {
            channel.set_mode(rtt_target::ChannelMode::BlockIfFull);

            writeln!(channel, "{}", info).ok();
        }

        // Counting to 20M at 72MHz takes ~5secs
        for i in 0..20_000_000u64 {
            core::hint::black_box(i);
        }

        // we should never leave critical section
        // reboot never returns.
        hardware::reboot();
    })
}

const DEVICE_TYPE: firmware::DeviceType = firmware::DeviceType::Demo;

#[app(device=hardware::rtic_device, peripherals = true, dispatchers = [SPI2, CAN1_TX, CAN1_RX0, CAN1_RX1, CAN1_SCE, LCD_TFT])]
mod app {

    use super::*;
    use demo_firmware_core::{
        logic::{
            dispatch::demo_buffer::DemoBuffer, settings::Settings, upload::UploadAllocator,
            ProcessingOutputAdapter,
        },
        meta::{print_boot_message, DeviceDescriptor},
        storage::streams::{ConfirmChannels, StaticDemoStore, Stream, BLOCK_SIZE},
    };

    type Variant = quectel::MiniPcie<hardware::Pin, hardware::Pin>;
    type Modem = quectel::Quectel<hardware::Tx, hardware::Rx, Variant, hardware::UartControl>;
    pub type Connection = Client<Modem>;

    type SPIFlash = spi_flash::SPIFlash<hardware::SPIDevice, 4096>;
    type DB = Arbiter<Database<BlockDevice<SPIFlash, BLOCK_SIZE>, BLOCK_SIZE, File, Stream>>;
    type ReservingUploadTx = ReservingSender<'static, Upload, UploadTx, ChangeNotification>;
    pub type NotifyingUploadRx = NotifyingReceiver<'static, Upload, UploadRx, ChangeNotification>;

    #[shared]
    struct Shared {}

    #[local]
    struct Local {
        device_descriptor: DeviceDescriptor,
        shared_persistent_store: &'static DB,
        boot_stats: boot_stats::Stats,
        connection: Connection,
        modem_backend: quectel::QuectelBackend<hardware::Rx>,
        network_upload_channel_s: ReservingUploadTx,
        network_upload_channel_r: NotifyingUploadRx,
        measurement_cancel_s: CancelTx,
        measurement_cancel_r: CancelRx,
        measurements_s: MeasurementsTx,
        measurements_r: MeasurementsRx,
        sensors: Sensors,
        board_adc: &'static BoardAdc,
        charger: &'static ChargerArbiter,
        rail_5v5: Rail5V5,
        button_held_at_boot: bool,
        button: hardware::TriggeredInputPin,
        processing_settings_s: ProcessingSettingsTx,
        processing_settings_r: ProcessingSettingsRx,
        processed_data_s: ProcessingOutputTx,
        processed_data_r: ProcessingOutputRx,
        orchestrator: Orchestrator<
            demo_behavior::DemoBehavior<FileStore<&'static DB>>,
            SignalRx,
            SignalTx,
            &'static ToNetwork,
            EventTx,
            LatestValueSender<'static, AppState>,
        >,
        rtt_input: DownChannel,
        rtt_output: UpChannel,
        idle_signal_channel: SignalTx,
        app_state_r: AppStateRx,
        event_channel_r: EventRx,
        confirm_channels: &'static ConfirmChannels<2>,
        log_reader: rtt_logger::LogReader,
        leds: Option<hardware::Leds>,
        watchdog: hardware::watchdog::IWDG,
        uart_isr: hardware::UartISR,
        uart_rx_dma_isr: hardware::UartRxDmaISR,
        uart_tx_dma_isr: hardware::UartTxDmaISR,
    }

    const SYSCLK_HZ: u32 = 72_000_000;

    #[init(local=[
        persistent_store: MaybeUninit<DB> = MaybeUninit::uninit(),
        internal_flash: MaybeUninit<InternalFlash> = MaybeUninit::uninit(),
        charger_cell: MaybeUninit<ChargerArbiter> = MaybeUninit::uninit(),
        board_adc_cell: MaybeUninit<BoardAdc> = MaybeUninit::uninit(),
        static_confirm_channels: ConfirmChannels<2> = ConfirmChannels::new(),
        upload_notifier: ChangeNotification = ChangeNotification::new(),
    ])]
    fn init(cx: init::Context) -> (Shared, Local) {
        let (rtt_input, rtt_output) = init_rtt_channels();
        let log_reader = rtt_logger::init_queue_logger(log::LevelFilter::Info);
        print_boot_message(DEVICE_TYPE, "Firmware");

        let mut hw = hardware::Board::<MonoTime>::init(cx.device);
        let internal_flash: &'static mut _ = cx.local.internal_flash.write(hw.internal_flash);

        // Enable watchdog. From now on it needs to be fed at regular intervals to prevent system reboot.
        let mut watchdog = hw.watchdog;
        watchdog.enable();

        let bootloader_version: &'static str = {
            if let Ok(bootloader) = internal_flash.bootloader_metadata() {
                log::info!(
                    "Bootloader: v{} ({})",
                    bootloader.version,
                    bootloader.git_revision
                );
                bootloader.version
            } else {
                log::info!("No bootloader");
                "no bootloader"
            }
        };

        let provisioned_cfg = match internal_flash.config_for_device_type(DEVICE_TYPE) {
            Ok(provisioned_cfg) => {
                log::info!("Serial: {}", provisioned_cfg.serial);
                provisioned_cfg
            }
            Err(err) => panic!("Failed to read provisioned config: {err:?}"),
        };
        let device_descriptor = DeviceDescriptor::new(
            bootloader_version,
            provisioned_cfg.device_type.into(),
            hw.straps.board_version().name(),
        );

        let boot_stats = hardware::boot_stats(&mut hw.rtc);
        log::info!(target:"boot", "Reason: {:?}, {:?} boots in {:?} seconds total", boot_stats.boot_reason, boot_stats.boot_total, boot_stats.uptime_total);
        log::info!(target:"boot", "{} POR, {} panic, {} WWDT, {} faults", boot_stats.por_total, boot_stats.panic_total, boot_stats.wdt_total, boot_stats.fault_total);

        log::info!("Board Init Done");

        let clocks = hw.clocks;
        if clocks.sysclk().raw() != SYSCLK_HZ {
            panic!("Unexpected clock rate");
        }

        let rtc = hw.rtc;
        if rtc.is_initialized() {
            log::info!("Time still running, now {:?}", rtc.time());
        } else {
            log::info!("Time unknown (must be synced from network..)")
        }

        time_init(rtc).unwrap();

        let modem_cfg = {
            let host = core::env!("DEMO_MQTT_HOST");
            let port = core::env!("DEMO_MQTT_PORT");
            let port = port
                .parse::<u16>()
                .expect("Invalid DEMO_MQTT_PORT: cannot parse");

            quectel::Config {
                host,
                port,
                use_tls: true,
            }
        };

        let credentials = quectel::Credentials {
            ca_cert: MQTT_CA_CERT,
            client_cert: provisioned_cfg.client_cert,
            client_key: provisioned_cfg.client_key,
        };

        let (uart, uart_isr, uart_rx_dma_isr, uart_tx_dma_isr) = hw.uart;
        let variant = quectel::MiniPcie::new(hw.modem_enable, hw.modem_reset);
        let (modem, modem_backend) = quectel::Quectel::new(uart, variant, modem_cfg, credentials);

        let connection = Client::new(modem, provisioned_cfg.serial);

        let spi_flash = {
            let descriptor = hardware::SPI_FLASH_DESCRIPTOR
                .validate()
                .expect("Misconfigured SPI flash");
            spi_flash::SPIFlash::new(hw.spi_flash, descriptor)
        };

        let blockdev = BlockDevice::writeable_from(spi_flash).unwrap();
        let persistent_store = Database::new(blockdev);
        let persistent_store: &DB = cx
            .local
            .persistent_store
            .write(Arbiter::new(persistent_store));

        let (measurement_cancel_s, measurement_cancel_r) = create_cancel_signal();

        let (upload_s, upload_r) = {
            let (tx, rx) = create_upload_channel();
            create_reserving_channel(tx, rx, cx.local.upload_notifier)
        };

        let (measurements_s, measurements_r) = create_measurements_session_channel();
        let (processed_data_s, processed_data_r) = create_processingoutput_channel();

        let (event_s, event_r) = create_event_channel();

        let (tx, rx) = create_signal_channel();

        // Delete any stored firmware file if we are in force-standalone mode.
        #[cfg(feature = "force-standalone")]
        let tx = {
            let mut tx = tx;
            tx.try_send(Signal::Common(CommonSignal::FirmwareUpdateDelete))
                .ok();
            tx
        };

        let (app_state_s, app_state_r) = create_latest_value_channel(&APP_STATE_CHANNEL);

        let behavior = demo_behavior::DemoBehavior::new(FileStore::new(persistent_store));
        let orchestrator = Orchestrator::new(
            behavior,
            rx,
            tx.clone(),
            &NETWORK_ACTION_NOTIFIER,
            event_s.clone(),
            app_state_s,
        );

        let charger: &'static ChargerArbiter = cx.local.charger_cell.write(Arbiter::new(
            BQ25672::new(hw.i2c, hw.charger_ce, hw.charger_int),
        ));
        let board_adc: &'static BoardAdc = cx
            .local
            .board_adc_cell
            .write(Arbiter::new(hw.board_temperature));

        // Read before anything else can: a press held through reset resets the settings
        let mut button = hw.button;
        let button_held_at_boot = button.is_low().unwrap_or(false);
        if button_held_at_boot {
            log::info!("Button held at boot");
        }

        let (processing_settings_s, processing_settings_r) = create_processingsettings_signal();

        led_task::spawn().unwrap();
        orchestrator::spawn().unwrap();
        watchdog_task::spawn(tx.clone()).unwrap();

        (
            Shared {},
            Local {
                device_descriptor,
                boot_stats,
                connection,
                modem_backend,
                network_upload_channel_s: upload_s,
                network_upload_channel_r: upload_r,
                processed_data_s,
                processed_data_r,
                measurement_cancel_s,
                measurement_cancel_r,
                measurements_s,
                measurements_r,
                sensors: Sensors {
                    adc: board_adc,
                    charger,
                },
                board_adc,
                charger,
                rail_5v5: hw.rail_5v5,
                button_held_at_boot,
                button,
                processing_settings_s,
                processing_settings_r,
                orchestrator,
                rtt_input,
                rtt_output,
                idle_signal_channel: tx,
                event_channel_r: event_r,
                confirm_channels: cx.local.static_confirm_channels,
                shared_persistent_store: persistent_store,
                log_reader,
                app_state_r,
                leds: Some(hw.leds),
                watchdog,
                uart_isr,
                uart_rx_dma_isr,
                uart_tx_dma_isr,
            },
        )
    }

    #[idle(local=[rtt_input, idle_signal_channel, rtt_output, log_reader], shared=[])]
    fn idle(cx: idle::Context) -> ! {
        let rtt_input = cx.local.rtt_input;
        let rtt_output = cx.local.rtt_output;
        let tx = cx.local.idle_signal_channel;
        let log_reader = cx.local.log_reader;

        loop {
            // Send log data over rtt
            while let Ok(item) = log_reader.try_recv() {
                let n = rtt_output.write(&item);
                if n == item.len() {
                    sleep_gate::note_rtt_write();
                }
            }

            // Read incomming data from rtt (cmds etc)
            let mut buf = [0u8; rtt_logger::RTT_INPUT_SIZE];
            let num_bytes = rtt_input.read(&mut buf);

            if num_bytes > 0 {
                match core::str::from_utf8(&buf[..num_bytes])
                    .map(|s| s.trim_matches(|c| c == '\n' || c == '\r'))
                {
                    Ok("start") => {
                        tx.try_send(Signal::Common(CommonSignal::Command(
                            Cmd::Start,
                            CmdSource::RTT,
                        )))
                        .ok();
                    }
                    Ok("stop")
                        if tx
                            .try_send(Signal::Common(CommonSignal::Command(
                                Cmd::Stop,
                                CmdSource::RTT,
                            )))
                            .is_err() =>
                    {
                        log::error!("Failed to send stop signal");
                    }

                    Ok("sync") => {
                        tx.try_send(Signal::Common(CommonSignal::UrgentEvent)).ok();
                    }
                    Ok("reboot") => {
                        tx.try_send(Signal::Common(CommonSignal::Command(
                            Cmd::Reboot,
                            CmdSource::RTT,
                        )))
                        .ok();
                    }
                    Ok("rm update") => {
                        tx.try_send(Signal::Common(CommonSignal::FirmwareUpdateDelete))
                            .ok();
                    }
                    Ok("rtt") => sleep_gate::log_stats(),
                    Ok("panic") => {
                        panic!("Don't Panic!");
                    }
                    _ => {}
                }
            }

            sleep_gate::sleep_unless_debugger_reading();
        }
    }

    #[task(local= [boot_stats, measurement_cancel_s, processing_settings_s, orchestrator, watchdog, charger, rail_5v5, button_held_at_boot], shared = [], priority=4)]
    async fn orchestrator(cx: orchestrator::Context) {
        let measurement_cancel_s = cx.local.measurement_cancel_s;
        let processing_settings_s = cx.local.processing_settings_s;

        let orch = cx.local.orchestrator;
        let watchdog = cx.local.watchdog;

        #[cfg(not(feature = "disable-sleep"))]
        hardware::set_sleep_level(hardware::SleepLevel::Sleep);

        // The charger has to be configured before the 5V5 rail and the modem
        // come up: the modem only powers up once the network task starts,
        // which needs the orchestrator below.
        power_up(cx.local.charger, cx.local.rail_5v5).await;

        let (mut schedule_state_s, schedule_state_r) = create_schedulestate_signal();
        let _ = orch_timer_state::spawn(schedule_state_r, orch.signal_sender());

        let (mut schedule_sync_s, schedule_sync_r) = create_schedulesyncnetwork_signal();
        let _ = orch_timer_network::spawn(schedule_sync_r, orch.signal_sender());

        let (mut schedule_debounce_s, schedule_debounce_r) = create_scheduledebounce_signal();
        let _ = orch_timer_debounce::spawn(schedule_debounce_r, orch.signal_sender());

        let (mut schedule_startstop_s, schedule_startstop_r) = create_schedulestartstop_signal();
        let _ = orch_timer_startstop::spawn(schedule_startstop_r, orch.signal_sender());

        // Handled in the Boot state, so it has to arrive before Booted
        if *cx.local.button_held_at_boot {
            orch.signal_sender()
                .send(Signal::Common(CommonSignal::ButtonPressed))
                .await
                .ok();
        }
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Booted(
                cx.local.boot_stats.boot_reason,
            )))
            .await
            .ok();

        dispatch::spawn(orch.signal_sender()).ok();
        temperature_task::spawn(orch.signal_sender()).ok();
        button_task::spawn(orch.signal_sender()).ok();

        loop {
            for action in orch.run().await {
                match action {
                    Action::SpawnNetwork => {
                        #[cfg(not(feature = "offline"))]
                        if network::spawn(orch.signal_sender()).is_err() {
                            // Task slot still occupied (previous task tearing down,
                            // or already starting). Tell orch so it defers a retry
                            // via the SyncNetwork timer instead of losing the intent.
                            orch.signal_sender()
                                .try_send(Signal::Common(CommonSignal::Network(
                                    NetworkSignal::SpawnFailed,
                                )))
                                .ok();
                        }
                    }
                    Action::StartMeasuring(settings) => {
                        log::info!(target: "main", "try start measuring");

                        // Try spawn processing and check if it was already running while we didn't expect it.
                        let processing_spawn = processing::spawn(settings, orch.signal_sender());
                        if processing_spawn.is_ok() {
                            // Try to spawn the measurement task
                            let measurement_spawn =
                                measurement::spawn(orch.signal_sender(), orch.event_sender());
                            if measurement_spawn.is_err() {
                                // If this fails the orchestrator detects this as
                                // a timeout (in TryStart state).
                                log::warn!(target: "main", "Failed to spawn measurement");
                                orch.signal_sender()
                                    .try_send(Signal::Common(CommonSignal::SensorFailed))
                                    .ok();
                            }

                        // Processing was still running. This should not happen: outdated settings
                        // and/or filter convergence issues could lead to artefacts (data peaks)
                        } else {
                            log::warn!(target: "main", "Failed to spawn processing (still running?");
                            orch.signal_sender()
                                .try_send(Signal::Common(CommonSignal::MeasurementStillRunning))
                                .ok();
                        }
                    }
                    Action::StopMeasuring => {
                        if measurement_cancel_s.send(Cancel).is_err() {
                            // (can't happen)
                            log::warn!(target: "main", "Failed to cancel measurement");
                        }
                    }
                    Action::ApplySettings(settings) => {
                        processing_settings_s.send(settings).ok();
                    }
                    Action::Reboot => {
                        log::warn!("Rebooting in 3 sec...");
                        delay_ms(3_000).await;
                        hardware::reboot();
                    }
                    Action::Blink => led::identify(),

                    // (re-)schedule changed timers
                    Action::ScheduleTimer(changes) => {
                        for (schedule, dt) in changes.iter() {
                            match schedule {
                                Schedule::State => &mut schedule_state_s,
                                Schedule::SyncNetwork => &mut schedule_sync_s,
                                Schedule::StatusUpdate => &mut schedule_debounce_s,
                                Schedule::StartStop => &mut schedule_startstop_s,
                            }
                            .send(dt)
                            .ok();
                        }
                    }
                    Action::FeedWatchdog => {
                        log::debug!(target: "main", "feeding watchdog");
                        watchdog.feed();
                    }
                }
            }
        }
    }

    /// Charger first, then the 5V5 rail, in the order the modem factory test
    /// established. A failure is logged, not fatal: the device still has to
    /// come online to report it, and the charger channels show the error.
    async fn power_up(charger: &ChargerArbiter, rail_5v5: &mut Rail5V5) {
        let mut charger = charger.access().await;
        match charger.configure().await {
            Ok(()) => {
                // configure() leaves charging off; a fitted battery should charge
                if let Err(err) = charger.enable_charging().await {
                    log::error!(target: "power", "Charger enable charging failed: {err:?}");
                }
            }
            Err(err) => log::error!(target: "power", "Charger configure failed: {err:?}"),
        }
        drop(charger);

        if let Err(err) = rail_5v5.enable().await {
            log::error!(target: "power", "5V5 rail did not come up: {err:?}");
        }
    }

    #[task(local=[board_adc], priority=1)]
    async fn temperature_task(cx: temperature_task::Context, mut signal_sender: SignalTx) {
        log::info!(target: "temperature_task", "Starting task");

        // Fixed 15-minute interval for now. TODO how much power does this consume, maybe orchestrator could configure this for optimization?
        const UPDATE_INTERVAL_MS: u32 = 900_000;
        loop {
            let measurement = cx.local.board_adc.access().await.measure().await;
            match measurement {
                Ok(measurement) => {
                    let temp = measurement.board_temp_millicelsius as f32 / 1000.0;
                    log::info!(target: "temperature_task", "Board temp: {temp:.1} *C");
                    signal_sender
                        .send(Signal::Common(CommonSignal::BoardTemperature(
                            Temperature {
                                board_celsius: temp,
                            },
                        )))
                        .await
                        .ok();
                }
                Err(error) => {
                    log::error!(target: "temperature_task", "Board temp sensing failed: {error:?}");
                }
            }

            delay_ms(UPDATE_INTERVAL_MS).await;
        }
    }

    /// A press forces a sync. The line is the charger's !QON as well, which
    /// is why only short presses are expected here.
    #[task(local=[button], priority=2)]
    async fn button_task(cx: button_task::Context, mut signal_sender: SignalTx) {
        use demo_firmware_core::traits::Trigger as _;

        /// Contact bounce settles well within this
        const DEBOUNCE_MS: u32 = 50;
        let button = cx.local.button;
        loop {
            button.wait_untill_next_edge().await;
            delay_ms(DEBOUNCE_MS).await;
            if button.is_low().unwrap_or(false) {
                signal_sender
                    .send(Signal::Common(CommonSignal::ButtonPressed))
                    .await
                    .ok();
            }
        }
    }

    #[task(priority = 3)]
    async fn watchdog_task(_cx: watchdog_task::Context, mut signal_sender: SignalTx) {
        log::debug!(target: "Watchdog", "Starting");

        // Safety: we assume no other tasks ever accesses UptimeCurrent
        let mut uptime_stat =
            unsafe { hardware::boot_stat_register(boot_stats::Stat::UptimeCurrent) };

        let wdg_delay_ms = hardware::watchdog::WINDOW_RANGE_LOWER_MS;
        loop {
            // Wait the minimum window interval.
            // The orchestrator then has until WINDOW_RANGE_UPPER_MS to feed the watchdog
            delay_ms(wdg_delay_ms).await;

            // Keep track of approximate system uptime in a backup register.
            // This is used for the boot statistics after next reboot
            boot_stats::increment_by(&mut uptime_stat, wdg_delay_ms / 1000);

            log::debug!(target: "Watchdog", "Hungry!");
            signal_sender
                .send(Signal::Common(CommonSignal::WatchdogHungry))
                .await
                .ok();
        }
    }

    #[task(priority = 1)]
    async fn orch_timer_state(
        _cx: orch_timer_state::Context,
        mut cmd_receiver: ScheduleStateRx,
        mut signal_sender: SignalTx,
    ) {
        demo_firmware_core::logic::orch_poll_timer_task::<SignalTx, ScheduleStateRx>(
            &mut cmd_receiver,
            &mut signal_sender,
            Schedule::State,
        )
        .await;
    }

    #[task(priority = 1)]
    async fn orch_timer_network(
        _cx: orch_timer_network::Context,
        mut cmd_receiver: ScheduleSyncNetworkRx,
        mut signal_sender: SignalTx,
    ) {
        demo_firmware_core::logic::orch_poll_timer_task::<SignalTx, ScheduleSyncNetworkRx>(
            &mut cmd_receiver,
            &mut signal_sender,
            Schedule::SyncNetwork,
        )
        .await;
    }

    #[task(priority = 1)]
    async fn orch_timer_startstop(
        _cx: orch_timer_startstop::Context,
        mut cmd_receiver: ScheduleStartStopRx,
        mut signal_sender: SignalTx,
    ) {
        demo_firmware_core::logic::orch_poll_timer_task::<SignalTx, ScheduleStartStopRx>(
            &mut cmd_receiver,
            &mut signal_sender,
            Schedule::StartStop,
        )
        .await;
    }

    #[task(priority = 1)]
    async fn orch_timer_debounce(
        _cx: orch_timer_debounce::Context,
        mut cmd_receiver: ScheduleDebounceRx,
        mut signal_sender: SignalTx,
    ) {
        demo_firmware_core::logic::orch_poll_timer_task::<SignalTx, ScheduleDebounceRx>(
            &mut cmd_receiver,
            &mut signal_sender,
            Schedule::StatusUpdate,
        )
        .await;
    }

    #[task(local= [device_descriptor, connection, network_upload_channel_r], priority=2)]
    async fn network(cx: network::Context, mut signal_sender: SignalTx) {
        crate::network::network(
            &mut signal_sender,
            cx.local.connection,
            cx.local.network_upload_channel_r,
            cx.local.device_descriptor,
        )
        .await
    }

    #[task(local = [modem_backend], priority=2)]
    async fn modem_rx(cx: modem_rx::Context) {
        let modem = cx.local.modem_backend;
        modem.read_incoming().await;
    }

    #[task(local = [processed_data_r, network_upload_channel_s, event_channel_r, shared_persistent_store, confirm_channels], shared = [], priority=3)]
    async fn dispatch(cx: dispatch::Context, mut signal_queue: SignalTx) {
        let processed_data = cx.local.processed_data_r;
        let to_network = cx.local.network_upload_channel_s;
        let events = cx.local.event_channel_r;

        let confirm_channels: &'static ConfirmChannels<2> = cx.local.confirm_channels;
        let db: &DB = cx.local.shared_persistent_store;

        let mut demo_store = StaticDemoStore::new(db, confirm_channels);

        if let Err(error) = demo_store.initialize().await {
            log::error!("Stream store init failed: {error:?}");
        }
        log::info!("Stream store initialized!");

        let mut upload_allocator = UploadAllocator::new(
            pools::EventPool.allocator(),
            pools::SensorDataPool.allocator(),
        );

        let processed_to_dispatch = ProcessingOutputAdapter::new(processed_data);

        let mut serializer = DemoBuffer::with_default_timing(processed_to_dispatch);
        // Required by dispatch_task; nothing requests a barrier
        static BARRIER: demo_firmware_core::logic::dispatch::DispatchBarrier =
            demo_firmware_core::logic::dispatch::DispatchBarrier::new();
        demo_firmware_core::logic::dispatch::dispatch_task(
            &mut demo_store,
            &mut serializer,
            events,
            &mut signal_queue,
            &mut upload_allocator,
            to_network,
            |e| e.is_urgent(),
            &BARRIER,
        )
        .await;
    }

    #[task(local = [measurements_r, processed_data_s, processing_settings_r], priority=4)]
    async fn processing(cx: processing::Context, settings: Settings, mut signal_queue: SignalTx) {
        demo_processing::task(
            cx.local.measurements_r,
            cx.local.processed_data_s,
            settings,
            cx.local.processing_settings_r,
            &mut signal_queue,
        )
        .await;
    }

    #[task(local = [measurement_cancel_r, measurements_s, sensors], shared = [], priority=5)]
    async fn measurement(
        cx: measurement::Context,
        mut signal_sender: SignalTx,
        mut event_sender: EventTx,
    ) {
        measurement_task(
            cx.local.measurement_cancel_r,
            cx.local.measurements_s,
            cx.local.sensors,
            &mut signal_sender,
            &mut event_sender,
        )
        .await;
    }

    #[task(local = [leds, app_state_r], priority=1)]
    async fn led_task(cx: led_task::Context) {
        let leds = cx.local.leds.take().expect("LED task spawned twice");
        led::led_task(leds, cx.local.app_state_r).await
    }

    /// EXTI5..9: 5V5 power-good (PD6), the button (PC7), charger INT (PA8)
    #[task(binds=EXTI9_5, priority=9)]
    fn exti_9_5_isr(_cx: exti_9_5_isr::Context) {
        unsafe {
            hardware::Exti::isr();
        }
    }

    /// TAMP_STAMP = EXTI19 (RTC tamper | RTC timestamp | LSE CSS)
    #[task(binds=TAMP_STAMP, priority=9)]
    fn exti_19_isr(_cx: exti_19_isr::Context) {
        unsafe {
            hardware::Exti::isr();
            hardware::rcc_isr();
        }
    }

    // RCC ISR: fires if LSE clock is ready
    #[task(binds=RCC, priority=8)]
    fn rcc_isr(_cx: rcc_isr::Context) {
        unsafe {
            hardware::rcc_isr();
        }
    }

    #[task(binds=USART3, local=[uart_isr], priority=16)]
    fn uart_isr(cx: uart_isr::Context) {
        unsafe {
            cx.local.uart_isr.handle_interrupt();
        }
    }

    #[task(binds=DMA1_CH1, local=[uart_rx_dma_isr], priority=16)]
    fn uart_rx_dma_isr(cx: uart_rx_dma_isr::Context) {
        unsafe {
            cx.local.uart_rx_dma_isr.handle_interrupt();
        }
    }

    #[task(binds=DMA1_CH2, local=[uart_tx_dma_isr], priority=16)]
    fn uart_tx_dma_isr(cx: uart_tx_dma_isr::Context) {
        unsafe {
            cx.local.uart_tx_dma_isr.handle_interrupt();
        }
    }

    #[task(binds=TIM5, priority=15)]
    fn tim5(_cx: tim5::Context) {
        unsafe {
            Mono::interrupt_handler();
        }
    }

    #[task(binds=SPI1, priority=8)]
    fn spi1_isr(_cx: spi1_isr::Context) {
        unsafe {
            hardware::SPI::isr_bus1();
        }
    }

    #[task(binds=I2C3_EV, priority=8)]
    fn i2c3_ev(_cx: i2c3_ev::Context) {
        unsafe {
            hardware::I2C::isr();
        }
    }

    #[task(binds=I2C3_ER, priority=8)]
    fn i2c3_er(_cx: i2c3_er::Context) {
        unsafe {
            hardware::I2C::isr();
        }
    }

    #[task(binds=ADC1, priority=2)]
    fn adc_isr(_cx: adc_isr::Context) {
        unsafe {
            hardware::Adc::<MonoTime>::isr();
        }
    }
}

fn init_rtt_channels() -> (DownChannel, UpChannel) {
    // Control block pinned at the end of RAM by the linker (0x2009FF00), so
    // capture and provisioning tools can be told the address
    let channels = rtt_target::rtt_init! {
        up: {
            0: {
                size: 2048,
                mode: rtt_target::ChannelMode::NoBlockTrim,
                name: "Log",
            }
        }
        down: {
            0: {
                size: rtt_logger::RTT_INPUT_SIZE,
                mode: rtt_target::ChannelMode::NoBlockSkip,
                name: "Command input",
            }
        }
        section_cb: ".rtt_cb"
    };

    (channels.down.0, channels.up.0)
}
