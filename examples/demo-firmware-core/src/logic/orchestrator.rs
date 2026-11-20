//! The Orchestrator is the top level director.
//!
//! It makes decisions based on internal state and external signals and outputs actions or delegates work to other tasks.
//!
//! [Orchestrator] is responsible for the basic functionality of the device and is generic over [ApplicationBehavior].
//! Each application implements [ApplicationBehavior] to define its own application- (or platform-)specific implementation and behaviour.
//!

pub mod application_behavior;
pub mod demo_behavior;
mod signal;

use sensor_link_firmware::drivers::time;

use crate::{
    logic::{
        network::{DEFAULT_TIMEOUT_S, INFINITE_TIMEOUT_S},
        orchestrator::application_behavior::Store,
        settings::{Settings, SYNC_INTERVAL_MAX_SEC},
        signal::{BootReason, CmdSource, CommonSignal, NetworkSignal, Signal},
        AppState, LatestValueReceiveChannel, LatestValueSendChannel, NetworkAction, ReceiveChannel,
        Schedule, ScheduleChanges, SendChannel,
    },
    monotonic_time::{self, MonotonicInstant, Timeout},
    utils::select::{select2, Select2},
};
use demo_protocol::{
    cmd::Cmd,
    event::{CommonEvent, Event},
    status::DeviceStatus,
};
use serde::{Deserialize, Serialize};

use super::{
    client::DeviceStatusUpload,
    settings,
    time_adjust::{self, ClockErrorEstimate},
    NetworkActionNotifyWriter,
};

const MINIMUM_NETWORK_TIME_US: i64 = 10_000_000;

/// Delay before retrying a SpawnNetwork that failed because the previous network
/// task hadn't freed its RTIC slot yet. Orch is priority 4, network priority 2, so
/// the `Disconnected` signal preempts the network task before it can `return` and
/// free its slot, so an immediate respawn would just fail again. Deferring via the
/// SyncNetwork timer guarantees the slot is free by the time we retry. Comfortably
/// exceeds worst-case `client.disconnect()` teardown (~1s). Tunable.
const SPAWN_RETRY_DELAY_US: i64 = 5_000_000;

const FIRMWARE_UPDATE_TIMEOUT_US: i64 = 15 * 60 * 1_000_000;
const ERROR_POLL_INTERVAL_US: i64 = 60 * 1_000_000;
const ERROR_REBOOT_DELAY_US: i64 = 10 * ERROR_POLL_INTERVAL_US;

const ERROR_RECOVERY_RETRIES: u32 = 3;

const UPDATE_STATUS_MIN_DEBOUNCE_US: i64 = 300_000;
const UPDATE_STATUS_MAX_DEBOUNCE_US: i64 = MINIMUM_NETWORK_TIME_US / 3;

/// Reboot if no succesful network connection in this amount of time
const SYNC_NO_NETWORK_TIMEOUT_SEC: i32 = 6 * 3600; // Would be nice to make this configurable

// Network syncs are clamped to be within this range. The upper bound is the
// one settings validation uses, so an accepted interval is never cut short.
const SYNC_INTERVAL_MIN_SEC: i32 = 60;

const TRY_STOP_TIMEOUT_US: i64 = 30_000_000;
const TRY_START_TIMEOUT_US: i64 = 30_000_000;

pub type ActionVec = heapless::Vec<Action, 5>; // todo check maximum + log error on overflow

/// Runtime start/stop state. Not persisted: after a reboot the default
/// (started) applies, the system always runs by itself
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgramState {
    // Should the program be started or stopped?
    pub should_start: bool,

    // Timestamp: when was 'should_start' last updated?
    // This is used to compare whether the scheduled start/stop
    // or the last-known (manual?) start/stop takes precendence
    pub should_start_at: i64,
}

impl Default for ProgramState {
    fn default() -> Self {
        // default: started since t=0
        Self {
            should_start: true,
            should_start_at: 0,
        }
    }
}

#[derive(Debug)]
struct Stats {
    /// Keep track of sensor start retries
    sensor_retries: u32,
    /// Used for resetting sensor_retries
    last_update: Timeout,

    /// Timestamp for keeping track of most recent network connection establishment
    last_connect: Option<MonotonicInstant>,
    last_disconnect: Option<MonotonicInstant>,
    last_time_receive: Option<MonotonicInstant>,

    network_connect_recovery_count: u32,
}

impl Default for Stats {
    fn default() -> Self {
        let mut t_sensor = Timeout::new();
        t_sensor.set_ms(0); // initialize so that it starts expired;
        Self {
            sensor_retries: 0,
            last_update: t_sensor,
            last_connect: None,
            last_disconnect: None,
            last_time_receive: None,
            network_connect_recovery_count: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    SpawnNetwork,
    /// Identify the device: fast green blink on the modem LED pair
    Blink,
    Reboot,
    StartMeasuring(Settings),
    StopMeasuring,
    ApplySettings(Settings),
    ScheduleTimer(ScheduleChanges),
    FeedWatchdog,
}

pub struct Orchestrator<App, Rx, Tx, ToNetwork, ToDispatch, ToUI>
where
    App: application_behavior::ApplicationBehavior,
{
    app: App,
    state: AppState,
    rx: Rx,
    tx: Tx,
    settings: Settings,
    clock_estimate: ClockErrorEstimate,

    /// Monotonic Timestamp of last network connection made
    connected_since: Option<monotonic_time::MonotonicInstant>,
    to_network: ToNetwork,
    to_dispatch: ToDispatch,
    to_ui: ToUI,
    status: DeviceStatus,
    program_state: ProgramState,
    stats: Stats,
    /// Timestamp of last state change
    /// Keeps track of time in current state
    /// Best set with monotonic timestamp
    in_state_since: monotonic_time::MonotonicInstant,

    startstop_reason: &'static str,
    measuring_active: bool,
}

impl<App, Rx, Tx, ToNetwork, ToDispatch, ToUI>
    Orchestrator<App, Rx, Tx, ToNetwork, ToDispatch, ToUI>
where
    App: application_behavior::ApplicationBehavior,
    Rx: ReceiveChannel<Signal>,
    Tx: SendChannel<Signal> + Clone,
    ToNetwork: NetworkActionNotifyWriter<DeviceStatusUpload>,
    ToDispatch: SendChannel<Event> + Clone,
    ToUI: LatestValueSendChannel<AppState>,
{
    pub fn new(
        app: App,
        rx: Rx,
        tx: Tx,
        to_network: ToNetwork,
        to_dispatch: ToDispatch,
        to_ui: ToUI,
    ) -> Self {
        let state = AppState::Boot;

        Self {
            app,
            rx,
            tx,
            state,
            settings: Settings::default(),
            clock_estimate: ClockErrorEstimate::new(time::initial_accuracy()),
            to_network,
            to_dispatch,
            to_ui,
            status: DeviceStatus::default(),
            program_state: ProgramState::default(),
            connected_since: None,
            stats: Stats::default(),
            in_state_since: monotonic_time::now(),
            startstop_reason: "auto",
            measuring_active: false,
        }
    }

    pub fn signal_sender(&self) -> Tx {
        self.tx.clone()
    }

    pub fn event_sender(&self) -> ToDispatch {
        self.to_dispatch.clone()
    }

    #[inline]
    pub fn network_is_connected(&self) -> bool {
        self.connected_since.is_some()
    }

    fn time_in_state_exceeds(&self, limit: i64) -> bool {
        self.in_state_since.elapsed_us() > limit as u64
    }

    /// Check whether the program should start according to:
    /// 1. last known program_state
    ///
    /// Note: program state != activity! See [check_should_be_active()].
    async fn should_start(&mut self) -> bool {
        self.program_state.should_start
    }

    async fn send_start_stop_event(&mut self) {
        let ev = match self.measuring_active {
            true => Event::started(self.startstop_reason),
            false => Event::stopped(self.startstop_reason),
        };
        self.send_event(ev).await.ok();

        // reset reason
        self.startstop_reason = "auto";
    }

    /// Re-schedule network sync timer based on sync_interval from settings
    fn reschedule_network_sync_timer(&self, schedule: &mut ScheduleChanges) {
        // clamp sync interval to limits
        let interval_s = self
            .settings
            .sync_interval_s
            .clamp(SYNC_INTERVAL_MIN_SEC, SYNC_INTERVAL_MAX_SEC) as i64;

        let dt = interval_s * 1_000_000i64;
        schedule.update(Schedule::SyncNetwork, dt);
    }

    /// Next sync after the sync interval, once the network task is done or gave up.
    ///
    /// A status update that is still due would spawn the network while the task tears
    /// down, and the SpawnFailed retry would undo the backoff: it goes out with the
    /// next sync instead.
    fn back_off_network_sync(&self, schedule: &mut ScheduleChanges) {
        self.reschedule_network_sync_timer(schedule);
        if let Some(next_sync) = schedule.get(Schedule::SyncNetwork) {
            schedule.update(Schedule::StatusUpdate, next_sync);
        }
    }

    fn process_new_settings(
        &mut self,
        new_settings: demo_protocol::settings::Settings,
    ) -> NetworkAction<DeviceStatusUpload> {
        match settings::validate(new_settings) {
            Ok((new_settings, warning)) => {
                if let Some(warning) = warning {
                    // TODO send some event to server with more info?
                    log::warn!(target: "Orch", "Adjusted settings: {warning:?}");
                }

                self.settings = new_settings;
                self.status.settings_hash = self.settings.hash;
            }
            Err(error) => {
                // Don't confirm new config and keep previous config. No updating of:
                // - self.config stays the same
                // - self.status.measuring_config_hash

                // TODO send some event to server with more info?
                log::error!(target: "Orch", "Rejected settings: {error:?}");
            }
        }

        self.network_timeout()
    }

    /// Idle timeout for the network task. Every session starts at the default,
    /// so this is sent again on each connect, not only when the settings change.
    fn network_timeout(&self) -> NetworkAction<DeviceStatusUpload> {
        if self.settings.sync_interval_s == 0 {
            NetworkAction::SetTimeout(INFINITE_TIMEOUT_S)
        } else {
            NetworkAction::SetTimeout(DEFAULT_TIMEOUT_S)
        }
    }

    async fn send_network_action(
        &mut self,
        action: NetworkAction<DeviceStatusUpload>,
    ) -> Result<(), ()> {
        if self.to_network.set(action).await.is_err() {
            log::warn!(target: "Orch", "Network is stuck?!");

            // Assume network is stuck: trigger reboot
            let _ = self
                .signal_sender()
                .try_send(Signal::Common(CommonSignal::Command(
                    Cmd::Reboot,
                    CmdSource::Orch,
                )));

            self.try_send_event(Event::contact_timeout("network stuck"))
                .ok();
            return Err(());
        }
        Ok(())
    }

    async fn send_status(&mut self) {
        if self.network_is_connected() {
            log::info!(target: "Orch", "Send status {:?}", self.status.status);
            let _ = self
                .send_network_action(NetworkAction::SendStatus(DeviceStatusUpload(
                    self.status.clone(),
                )))
                .await;
        }
    }

    async fn send_event(&mut self, ev: Event) -> Result<(), ()> {
        self.to_dispatch.send(ev).await.map_err(|_| ())?;
        Ok(())
    }

    fn try_send_event(&mut self, ev: Event) -> Result<(), ()> {
        self.to_dispatch.try_send(ev).map_err(|_| ())?;
        Ok(())
    }

    /// Start measuring. The settings are handed to the measurement session,
    /// which keeps using them until it stops.
    ///
    /// On restart paths, this relies on the previous session having fully torn
    /// down before orch processes `SensorStopped`: measurement's
    /// `finish_and_wait` drains the processing task's session, and
    /// same-priority FIFO wake order means processing's tail runs before orch
    /// handles the signal. So the new session can be spawned right away.
    async fn start<const N: usize>(
        &mut self,
        actions: &mut heapless::Vec<Action, N>,
    ) -> Result<(), ()> {
        actions
            .push(Action::StartMeasuring(self.settings.clone()))
            .map_err(|_| ())
    }

    /// Stop measuring
    async fn stop<const N: usize>(
        &mut self,
        actions: &mut heapless::Vec<Action, N>,
    ) -> Result<(), ()> {
        if !self.measuring_active {
            return Ok(());
        }
        log::info!(target: "Orch", "Stop");
        actions.push(Action::StopMeasuring).map_err(|_| ())
    }

    /// Try to transition to NoConfig/Active/Standby/Ready
    async fn determine_state(&mut self) -> AppState {
        // Exhaustive match to make sure all states are considered
        let allow_change = match self.state {
            // whitelist: in these states we can try to determine the next state
            AppState::Ready
            | AppState::TryStart
            | AppState::TryStop
            | AppState::Active
            | AppState::Standby
            | AppState::NoConfig => true,

            // blacklist: these states have defined their own state transitions
            AppState::Boot
            | AppState::ErrorRecovery
            | AppState::Error
            | AppState::FirmwareUpdate => false,
        };

        if !allow_change {
            self.state
        } else if (self.state == AppState::TryStop && self.measuring_active)
            || (self.state == AppState::TryStart && !self.measuring_active)
        {
            // Stay in state if start/stop not succeeded yet
            self.state
        } else if self.should_start().await {
            AppState::Active

        // should not start, but still active (e.g. after scheduled stop)
        } else if self.measuring_active {
            AppState::TryStop

        // Stopped by command (runtime-only, cleared by a reboot): stay
        // in Ready. A still-due self-test waits for measuring; it runs
        // via Active on the next start.
        } else {
            AppState::Ready
        }
    }

    /// Sets state if required. Returns `true` if state was changed.
    async fn set_state<const N: usize>(
        &mut self,
        next_state: AppState,
        actions: &mut heapless::Vec<Action, N>,
        schedule: &mut ScheduleChanges,
    ) {
        self.state_transition(next_state, actions, schedule).await;

        self.status.status = self.state.into();

        // Ignore error as this is not critical
        self.to_ui.send(self.state).ok();

        self.in_state_since = monotonic_time::now();

        log::info!(target: "Orch", "State: {:?}", self.state);
    }

    async fn state_transition<const N: usize>(
        &mut self,
        mut next_state: AppState,
        actions: &mut heapless::Vec<Action, N>,
        schedule: &mut ScheduleChanges,
    ) {
        while next_state != self.state {
            self.state = next_state;
            // Do the actual transition.
            // If the transition causes a state change we run again.
            match self.state {
                AppState::Boot => {}
                AppState::Ready => {
                    next_state = self.determine_state().await;
                }
                AppState::TryStart => {
                    log::info!(target: "Orch", "Start measuring");
                    if self.start(actions).await.is_err() {
                        log::error!(target: "Orch", "Failed to start measuring");
                        next_state = AppState::ErrorRecovery;
                    } else {
                        schedule.update(Schedule::State, TRY_START_TIMEOUT_US);
                    }
                }
                AppState::Active => {
                    if !self.measuring_active {
                        next_state = AppState::TryStart;
                    }
                }
                AppState::TryStop => {
                    if self.measuring_active {
                        let _ = self.stop(actions).await;
                        schedule.update(Schedule::State, TRY_STOP_TIMEOUT_US);
                    } else {
                        next_state = AppState::Ready;
                    }
                }
                AppState::Standby => {
                    if self.measuring_active {
                        next_state = AppState::TryStop;
                    }
                }
                AppState::NoConfig => {}
                AppState::ErrorRecovery => {
                    if self.stats.last_update.is_expired() {
                        self.stats.sensor_retries = 0;
                        self.stats.last_update.set_ms(3_600_000)
                    }
                    if self.stats.sensor_retries < ERROR_RECOVERY_RETRIES {
                        log::warn!(target: "Orch", "Sensor error, retrying...");
                        next_state = AppState::TryStart;
                        self.stats.sensor_retries += 1;
                    } else {
                        log::error!(target: "Orch", "Retries failed");
                        next_state = AppState::Error;
                    }
                }
                AppState::Error => {
                    actions.clear();
                    actions.push(Action::StopMeasuring).ok();
                    actions.push(Action::SpawnNetwork).ok();
                    schedule.update(Schedule::State, ERROR_POLL_INTERVAL_US);
                }
                AppState::FirmwareUpdate => {
                    // Schedule Poll for update timeout
                    schedule.update(Schedule::State, FIRMWARE_UPDATE_TIMEOUT_US);
                }
            }
        }

        self.state = next_state;
    }

    /// Handle state-independent status signals
    ///
    /// These signals are handled the same regardless of the current state
    /// and do not cause a state change.
    async fn handle_status_signal(
        &mut self,
        sig: signal::Status,
        actions: &mut ActionVec,
        schedule: &mut ScheduleChanges,
    ) {
        match sig {
            signal::Status::BoardTemperature(temperature) => {
                self.status.temperature = temperature.board_celsius;
                let drift = self.clock_estimate.update_temp_correction(
                    time_adjust::Timestamp::OffsetAdjusted(time::timestamp_or_default_us()),
                    time::temp_correction(temperature.board_celsius),
                );
                match time::calibrate(&drift) {
                    Ok(_) => {
                        log::info!(target: "Orch", "updated clock drift from temp: {drift:?}");
                    }
                    Err(cal_err) => {
                        log::warn!(target: "Orch", "clock calibration fail: {cal_err:?}")
                    }
                }
            }
            signal::Status::NetworkTime(t_local, t_network) => {
                match self
                    .clock_estimate
                    .update_network_time(t_local, t_network.clone())
                {
                    Ok(()) => {
                        let drift = self.clock_estimate.drift_estimate();
                        match time::calibrate(&drift) {
                            Ok(_) => {
                                if let Err(err) = self
                                    .app
                                    .store()
                                    .write_clock_calibration(&self.clock_estimate.calibration())
                                    .await
                                {
                                    log::warn!(target: "Orch", "Failed to write clock cal: {:?}", err);
                                }
                                log::info!(target: "Orch", "updated clock drift from network: {drift:?}");
                            }
                            Err(cal_err) => {
                                log::warn!(target: "Orch", "clock calibration fail: {cal_err:?}")
                            }
                        }
                    }
                    Err(est_err) => {
                        log::warn!(target: "Orch", "clock estimate not ready yet: {est_err:?}");
                    }
                }

                // TODO time::adjust_us is deprecated
                let offset_limit = match self.state {
                    // during measuring: only adjust if deviation > 1 second
                    // adjustments may cause artifacts in e.g. trace data
                    AppState::TryStart | AppState::Active | AppState::TryStop => 1_000_000,

                    // not measuring: immediately adjust even for small deviation.
                    // 200ms is a bit arbitrary, but selected to be > 100ms and << 1sec
                    AppState::Boot
                    | AppState::Ready
                    | AppState::Standby
                    | AppState::NoConfig
                    | AppState::ErrorRecovery
                    | AppState::Error
                    | AppState::FirmwareUpdate => 200_000,
                };
                match time::adjust_us(t_network.timestamp_server_us, offset_limit) {
                    Ok(t) => {
                        log::info!(target: "Orch", "Time adjusted with: {t} us");
                    }
                    Err(err) => {
                        log::error!(target: "Orch", "Failed to adjust time: {err:?}")
                    }
                }
                // Reschedule wall-clock-time sensitive timers:
                // these run on monotonic timers, but aim to match a specific wall-clock time
                self.stats.last_time_receive = Some(monotonic_time::now());
            }
            signal::Status::FirmwareUpdateDelete => {
                if self.app.store().delete_firmware().await.is_ok() {
                    log::info!(target: "Orch", "Deleted old firmware file");
                } else {
                    log::error!(target: "Orch", "Failed to delete old firmware file.");
                }
            }
            signal::Status::Network(NetworkSignal::Connected) => {
                log::info!(target: "Orch", "Network Connected");
                self.stats.last_connect = Some(monotonic_time::now());
                self.stats.network_connect_recovery_count = 0;
                self.connected_since = Some(monotonic_time::now());
                schedule_status_update(schedule, true);
                self.send_network_action(self.network_timeout()).await.ok();
            }
            // Sent per failed attempt and when the task gives up without ever connecting:
            // only the second needs the backoff, but it is harmless while the task retries.
            signal::Status::Network(NetworkSignal::ConnectFailed) => {
                self.back_off_network_sync(schedule);
            }
            signal::Status::Network(NetworkSignal::SpawnFailed) => {
                // The RTIC network task slot isn't free (still tearing down, or
                // starting/connecting). Defer via the SyncNetwork
                // timer; its poll handler already guards on !network_is_connected(),
                // so this becomes a no-op if the task comes up on its own.
                log::debug!(
                    target: "Orch",
                    "Network spawn failed; retrying via SyncNetwork in {}s",
                    SPAWN_RETRY_DELAY_US / 1_000_000
                );
                schedule.update(Schedule::SyncNetwork, SPAWN_RETRY_DELAY_US);
            }
            signal::Status::Network(NetworkSignal::Disconnected(info)) => {
                self.connected_since = None;
                self.stats.last_disconnect = Some(monotonic_time::now());
                log::info!(target: "Orch", "Network Disconnected {info:?}");
                self.back_off_network_sync(schedule);
            }

            // Dispatch queue empty: disconnect network after some time
            signal::Status::DispatchQueueEmpty => {
                if self.settings.sync_interval_s != 0 {
                    if let Some(connected_since) = &self.connected_since {
                        if connected_since.elapsed_us() > MINIMUM_NETWORK_TIME_US as u64 {
                            log::info!(target: "Orch", "Dispatch complete");
                            self.send_network_action(NetworkAction::Disconnect)
                                .await
                                .ok();
                        }
                    }
                }
            }
            signal::Status::DispatchDrained | signal::Status::StatusSent => {}

            // Reported as an event, so the server can estimate the location
            signal::Status::CellInfo(cell) => {
                self.send_event(CommonEvent::CellInfo(cell).into())
                    .await
                    .ok();
            }
            signal::Status::UrgentEvent => {
                log::info!(target: "Orch", "Urgent event");
                if !self.network_is_connected() {
                    actions.push(Action::SpawnNetwork).ok();
                } else {
                    schedule_status_update(schedule, true);
                }
            }

            signal::Status::ChannelStatus(channels) => {
                // update last known status and trigger a sync
                log::info!(target: "Orch", "ChannelStatus change: {channels:?}");
                self.status.channel_status = Some(channels.sensors);
                if !self.network_is_connected() {
                    actions.push(Action::SpawnNetwork).ok();
                } else {
                    schedule_status_update(schedule, true);
                }
            }

            // Watchdog nearly expires: do some quick sanity checks and feed the watchdog
            // TODO this is a good place to do periodic sanity checks
            signal::Status::WatchdogHungry => {
                if let (Some(last_connect), Some(last_disconnect)) =
                    (&self.stats.last_connect, &self.stats.last_disconnect)
                {
                    // Expected sync interval. Capped at 180 seconds to prevent false positives with very
                    // short sycn intervals (e.g. 0)
                    let sync_interval_s = self.settings.sync_interval_s.max(180);

                    let time_since_disconnect_s = last_disconnect.elapsed_sec() as i32;

                    if last_connect < last_disconnect {
                        if self.stats.network_connect_recovery_count == 0
                            && (time_since_disconnect_s > (sync_interval_s * 2))
                        {
                            // Too long no contact. Try spawning Network and setting timer.
                            self.stats.network_connect_recovery_count += 1;
                            actions.push(Action::SpawnNetwork).ok();
                            self.reschedule_network_sync_timer(schedule);
                        } else if time_since_disconnect_s > SYNC_NO_NETWORK_TIMEOUT_SEC {
                            // Recovery failed. Reboot.
                            actions.push(Action::Reboot).ok();
                            self.try_send_event(Event::contact_timeout("timeout")).ok();
                        }
                    }
                }

                // We check if we have received a message from the server.
                // This is to counter the situation where we are connected but still have no contact with the server.
                // For example if we have a quick disconnect.
                if let Some(last_time_receive) = &self.stats.last_time_receive {
                    if last_time_receive.elapsed_sec() > SYNC_NO_NETWORK_TIMEOUT_SEC as u64 {
                        // Recovery failed. Reboot.
                        actions.push(Action::Reboot).ok();
                    }
                }
                actions.push(Action::FeedWatchdog).ok();
            }
        }
    }

    /// Handle state-specific signals
    ///
    /// Each of these signals is only expected to be handled in a particular state.
    /// In all other states, the signal is ignored.
    async fn handle_specific_signal(
        &mut self,
        sig: signal::State,
        actions: &mut ActionVec,
        schedule: &mut ScheduleChanges,
    ) -> AppState {
        // Ignore if not in expected state (e.g. a self-test result after
        // the deadline already moved on)
        if self.state != sig.expected_state() {
            log::warn!(
                target: "Orch",
                "Dropped a {:?} signal in {:?}",
                sig.expected_state(),
                self.state
            );
            return self.state;
        }

        match sig {
            // Boot: initialize state and start selftest + network
            signal::State::Booted(boot_reason) => {
                self.app.store().initialize_writeable(true).await.ok();

                if let Ok(settings) = self.app.store().read_settings().await {
                    log::trace!(target: "Orch", "Settings read");
                    let network_action = self.process_new_settings(settings);
                    self.send_network_action(network_action).await.ok();
                } else {
                    log::warn!(target: "Orch", "Failed to read settings");
                }

                match self.app.store().read_clock_calibration().await {
                    Ok(cal) => {
                        log::trace!(target: "Orch", "ClockCalibration read");
                        self.clock_estimate = ClockErrorEstimate::new(cal);

                        // attempt to immediately apply the calibration
                        let drift = self.clock_estimate.drift_estimate();
                        let _ = time::calibrate(&drift);
                    }
                    Err(err) => {
                        log::warn!(target: "Orch", "Could not read ClockCalibration: {err:?} (keep initial estimate)");
                    }
                }

                // Send Device Info
                self.send_network_action(NetworkAction::SendDeviceInfo)
                    .await
                    .ok();

                self.send_event(Event::booted(boot_reason.as_str()))
                    .await
                    .ok();

                // Start scheduled network syncs
                self.reschedule_network_sync_timer(schedule);

                // Boot loop: skip to error state for a chance to recover
                if boot_reason == BootReason::Loop {
                    AppState::Error
                } else {
                    // Measuring starts right away (via Ready)
                    actions.push(Action::SpawnNetwork).ok();
                    AppState::Ready
                }
            }

            // Store incoming chunks of firmware update
            signal::State::FirmwareChunk(chunk) => {
                let sequence = chunk.sequence_no;
                if let Err(err) = self
                    .app
                    .store()
                    .write_firmware_fragment(sequence.into(), &chunk.bytes)
                    .await
                {
                    log::error!(target: "Orch", "Failed to write firmware fragment {sequence}: {err:?}");
                }

                self.state
            }

            // Reboot on firmwareupdate completion
            signal::State::FirmwareUpdateComplete => {
                log::info!(target: "Orch", "Firmware update complete, rebooting");
                actions.push(Action::Reboot).ok();
                self.state
            }

            signal::State::FirmwareUpdateFailed => {
                log::error!(target: "Orch", "Firmware update failed: resuming");
                AppState::Ready
            }
        }
    }

    /// Handle generic state-dependent signals
    ///
    /// These signals are handled differently depending on the current state.
    async fn handle_generic_signal(
        &mut self,
        sig: signal::Generic,
        actions: &mut ActionVec,
        schedule: &mut ScheduleChanges,
    ) -> AppState {
        match (sig, self.state) {
            // Button held at boot: reset settings. Arrives before Booted, so the
            // store is not initialized yet; Booted then finds no settings.
            (signal::Generic::ButtonPressed, AppState::Boot) => {
                log::info!(target: "Orch", "Button held at boot: reset settings");
                self.app.store().initialize_writeable(true).await.ok();
                if self.app.store().delete_settings().await.is_err() {
                    log::warn!(target: "Orch", "Failed to delete settings");
                }

                self.state
            }

            // Button in all other states: spawn network + status update
            (signal::Generic::ButtonPressed, _) => {
                log::info!(target: "Orch", "Button press");
                self.send_event(Event::ButtonPressed).await.ok();
                if !self.network_is_connected() {
                    actions.push(Action::SpawnNetwork).ok();
                } else {
                    schedule_status_update(schedule, true);
                }
                self.state
            }

            // FirmwareUpdate state: this state ignores most signals
            // and assumes it can never exit (only through a reboot)
            (signal, AppState::FirmwareUpdate) => {
                match signal {
                    signal::Generic::SensorStarted => {
                        // We don't want to be active in FirmwareUpdate state.
                        // Try to stop immediately again and stay in FirmwareUpdate state.
                        self.startstop_reason = "FW update";
                        self.measuring_active = true;
                        self.stop(actions).await.ok();
                        self.measuring_active = false;
                    }

                    signal::Generic::SensorStopped => {
                        self.measuring_active = false;
                        self.send_start_stop_event().await;
                    }

                    // State timeout triggers reboot as fallback
                    signal::Generic::Poll(schedule) => {
                        let is_timeout = match schedule {
                            // NB: these timers are *not* re-scheduled. AppState::FirmwareUpdate assumes
                            // there won't be any state transitions out (except via reboot)
                            Schedule::State => true,
                            Schedule::SyncNetwork
                            | Schedule::StatusUpdate
                            | Schedule::StartStop => false,
                        };
                        if is_timeout || self.time_in_state_exceeds(FIRMWARE_UPDATE_TIMEOUT_US) {
                            log::error!(target: "Orch", "Firmware update failed");

                            actions.push(Action::Reboot).ok();
                        }
                    }

                    // Explicitly ignore most signals
                    _ => {}
                }

                // Never exit FirmwwareUpdate state
                AppState::FirmwareUpdate
            }

            // Error state: only (re-)schedule some of the timers
            // Note: error state assumes no other state will ever occur except firmware update and reboot
            (
                signal::Generic::Poll(
                    expired_schedule @ (Schedule::SyncNetwork
                    | Schedule::State
                    | Schedule::StartStop),
                ),
                AppState::Error,
            ) => {
                match expired_schedule {
                    Schedule::SyncNetwork | Schedule::State => {
                        schedule_status_update(schedule, true);
                        if !self.network_is_connected() {
                            actions.push(Action::SpawnNetwork).ok();
                        }

                        // Reschedule polling for state timeout.
                        // Note: all other timers (SycnNetwork etc) are not rescheduled, as
                        // Appstate::Error assumes the only way out of the state is reboot anyways
                        schedule.update(Schedule::State, ERROR_POLL_INTERVAL_US);
                    }
                    // Don't reschedule (only way out of Error is reboot or fw update)
                    Schedule::StatusUpdate | Schedule::StartStop => {}
                }

                if self.time_in_state_exceeds(ERROR_REBOOT_DELAY_US) {
                    log::error!(target: "Orch", "Reboot due to Error");
                    actions.push(Action::Reboot).ok();
                }

                self.state
            }

            // Timeout during TryStart: try to recover
            (signal::Generic::Poll(Schedule::State), AppState::TryStart) => {
                log::error!(target: "Orch", "Sensor timeout during start");
                self.measuring_active = false;

                AppState::ErrorRecovery
            }

            // Timeout during TryStop: try to recover
            (signal::Generic::Poll(Schedule::State), AppState::TryStop) => {
                log::error!(target: "Orch", "Timeout while stopping sensor");

                AppState::Error
            }

            // SensorStopped: expected, system ready
            (signal::Generic::SensorStopped, AppState::TryStop) => {
                self.measuring_active = false;
                self.send_start_stop_event().await;

                log::info!(target: "Orch", "Sensor stopped");

                AppState::Ready
            }

            // SensorrStopped: unexpected, try to recover
            (
                signal::Generic::SensorStopped,
                AppState::Ready
                | AppState::TryStart
                | AppState::Active
                | AppState::Standby
                | AppState::NoConfig,
            ) => {
                log::warn!(target: "Orch", "Unexpected sensor stop");
                self.measuring_active = false;

                let ev = Event::stopped("Unexpected");
                self.send_event(ev).await.ok();

                // Try to recover
                AppState::ErrorRecovery
            }

            // SensorStopped: truly unexpected
            // Explicitly ignore to prevent unexpected state transitions
            (
                signal::Generic::SensorStopped,
                AppState::Boot | AppState::Error | AppState::ErrorRecovery,
            ) => {
                // Unexpected stop, but alread in error state so we won't try to recover
                log::warn!(target: "Orch", "Unexpected sensor stop (ignored)");
                self.measuring_active = false;
                self.state
            }

            // SensorStarted: unexpected
            // Try to stop and stay in current state
            (
                signal::Generic::SensorStarted,
                AppState::Boot | AppState::Error | AppState::ErrorRecovery,
            ) => {
                // We don't want to be active in Error state.
                // Try to stop immediately again and stay in Error state.
                self.startstop_reason = "Unexpected";
                self.measuring_active = true;
                self.stop(actions).await.ok();
                self.measuring_active = false;

                self.state
            }

            // SensorStarted while stopping: the session was spawned before the
            // stop was issued. The cancel is already latched, so keep waiting
            // for SensorStopped instead of going Active. No start/stop event:
            // this session never ran, the pending stop reports it.
            (signal::Generic::SensorStarted, AppState::TryStop) => {
                self.measuring_active = true;
                self.state
            }

            // SensorStarted: go to Active
            (
                signal::Generic::SensorStarted,
                AppState::Ready
                | AppState::TryStart
                | AppState::Active
                | AppState::Standby
                | AppState::NoConfig,
            ) => {
                self.measuring_active = true;
                self.send_start_stop_event().await;

                AppState::Active
            }

            // Sensor failed: try to recover
            (
                signal::Generic::SensorFailed | signal::Generic::MeasurementStillRunning,
                state @ (AppState::Ready
                | AppState::TryStart
                | AppState::TryStop
                | AppState::Active
                | AppState::Standby
                | AppState::NoConfig),
            ) => {
                log::error!(target: "Orch", "Sensor failed in state {state:?}");
                self.measuring_active = false;
                AppState::ErrorRecovery
            }

            // Sensor failed in unexpected state: ignore
            (
                signal::Generic::SensorFailed | signal::Generic::MeasurementStillRunning,
                state @ (AppState::Boot | AppState::Error | AppState::ErrorRecovery),
            ) => {
                self.measuring_active = false;
                state
            }

            // New settings: apply live (FirmwareUpdate ignores them above)
            (signal::Generic::Settings(new_settings), _) => {
                log::info!(target: "Orch", "Received new settings");

                if let Err(err) = self.app.store().write_settings(&new_settings).await {
                    log::warn!(target: "Orch", "Failed to write settings: {:?}", err);
                }

                let network_timeout = self.process_new_settings(new_settings);
                self.send_network_action(network_timeout).await.ok();

                schedule_status_update(schedule, false);
                self.reschedule_network_sync_timer(schedule);
                actions
                    .push(Action::ApplySettings(self.settings.clone()))
                    .ok();

                self.determine_state().await
            }

            (signal::Generic::FirmwareUpdateAnnounced, state) => {
                self.startstop_reason = "FW update";
                // This stop does not go via the TryStop state because
                // we need to go to FirmwareUpdate state. It is not that important if stopping fails.
                // Maybe there is a better way?
                self.stop(actions).await.ok();

                if self.app.store().delete_firmware().await.is_ok() {
                    if self
                        .send_network_action(NetworkAction::DownloadUpdate)
                        .await
                        .is_ok()
                    {
                        // Network task is downloading update: enter FirmwareUpdate state
                        AppState::FirmwareUpdate
                    } else {
                        // Network task not responsive: stay in current state
                        log::error!(target: "Orch", "Failed to network DownloadUpdate action.");
                        state
                    }
                } else {
                    // Store failed to make free space for update stay in current state
                    log::error!(target: "Orch", "Failed to delete old firmware file.");
                    state
                }
            }

            // Start while Active: ignore
            (signal::Generic::Command(Cmd::Start, _), AppState::Active) => self.state,

            // Start command: any state except Active
            (signal::Generic::Command(Cmd::Start, source), _) => {
                self.set_program_state(true, source.as_str());
                self.determine_state().await
            }

            // Stop command: any state
            (signal::Generic::Command(Cmd::Stop, source), state) => {
                self.set_program_state(false, source.as_str());
                match state {
                    AppState::TryStart | AppState::Active => {
                        // In TryStart the session is spawned but has not
                        // reported started yet: mark it running so TryStop
                        // actually cancels it (no-op in Active).
                        self.measuring_active = true;
                        AppState::TryStop
                    }
                    AppState::Standby => AppState::TryStop,
                    _ => self.state,
                }
            }

            // Blink command: any state. The event confirms it to the server
            (signal::Generic::Command(Cmd::Blink, _source), _) => {
                self.send_event(CommonEvent::Blink.into()).await.ok();
                actions.push(Action::Blink).ok();
                self.state
            }

            // Reboot: any state
            (signal::Generic::Command(Cmd::Reboot, _source), _) => {
                actions.push(Action::StopMeasuring).ok();
                actions.push(Action::Reboot).ok();
                self.state
            }

            // State-specific timeout: ignore if not explicitly handled
            (signal::Generic::Poll(Schedule::State), _state) => self.state,

            // StatusUpdate debounced: send status to network (most likely already connected)
            (signal::Generic::Poll(Schedule::StatusUpdate), state) => {
                if self.network_is_connected() {
                    self.send_status().await;
                } else {
                    actions.push(Action::SpawnNetwork).ok();
                }
                state
            }

            // Re-schedule the next sync and try to go online
            (signal::Generic::Poll(Schedule::SyncNetwork), state) => {
                self.reschedule_network_sync_timer(schedule);
                schedule_status_update(schedule, true);
                if !self.network_is_connected() {
                    actions.push(Action::SpawnNetwork).ok();
                }
                state
            }

            (signal::Generic::Poll(Schedule::StartStop), _state) => self.determine_state().await, // Note: no implicit wildcard (_,_) match variant should be necessary here
                                                                                                  // This prevents forgetting to handle states or events to be added in the future
        }
    }

    pub async fn run(&mut self) -> ActionVec {
        let mut actions: ActionVec = ActionVec::new();
        let mut schedule = ScheduleChanges::empty();
        let sig = self.rx.recv().await.unwrap();

        // match on orchestrator-internal representation of Signals
        let next_state = match self::signal::Signal::from(sig) {
            signal::Signal::StateSpecific(sig) => {
                self.handle_specific_signal(sig, &mut actions, &mut schedule)
                    .await
            }
            signal::Signal::StateIndependent(sig) => {
                self.handle_status_signal(sig, &mut actions, &mut schedule)
                    .await;
                self.state
            }
            signal::Signal::Generic(sig) => {
                self.handle_generic_signal(sig, &mut actions, &mut schedule)
                    .await
            }
        };

        if self.state != next_state {
            self.set_state(next_state, &mut actions, &mut schedule)
                .await;
            schedule_status_update(&mut schedule, false);
        }

        if !schedule.is_empty() {
            actions.push(Action::ScheduleTimer(schedule)).ok();
        }
        if actions.is_full() {
            log::warn!(target: "Orch", "Actions full, may have overflowed!");
            // TODO this error would be an orchestrator bug. Enter Error state and/or send event + reboot?
        }

        // Before caller executes Action::SpawnNetwork, clear pending disconnect
        let is_spawn_network = actions.iter().any(|a| matches!(a, Action::SpawnNetwork));
        if is_spawn_network {
            self.to_network.clear_pending_disconnect().await;
        }

        actions
    }

    fn set_program_state(&mut self, should_start: bool, source: &'static str) {
        self.program_state = ProgramState {
            should_start,
            should_start_at: time::timestamp_or_default_us(),
        };
        self.startstop_reason = source;
    }
}

pub async fn orch_poll_timer_task<
    SignalTx: SendChannel<Signal>,
    CmdRx: LatestValueReceiveChannel<i64>,
>(
    cmd_queue: &mut CmdRx,
    signal_queue: &mut SignalTx,
    schedule: Schedule,
) {
    let mut wait_us: i64 = 60_000_000; // Dummy initial value. Will be overwritten.
    let mut received_cmd = false;
    loop {
        // If we didn't receive a command with a delay, wait for one
        // Else we should skip this step and start waiting.
        if !received_cmd {
            match cmd_queue.recv().await {
                Ok(cmd) => {
                    wait_us = cmd;
                    log::info!(target: "Orch", "Schedule {schedule:?} in {} s", wait_us / 1000000);
                }
                Err(_) => {
                    continue;
                }
            };
        }

        received_cmd = false;

        let timer_expired = monotonic_time::delay_us(wait_us as u64);
        let new_delay = cmd_queue.recv();
        match select2(new_delay, timer_expired).await {
            Select2::A(cmd) => {
                if let Ok(delay_us) = cmd {
                    wait_us = delay_us;
                    log::info!(target: "Orch", "Reschedule {schedule:?} in {} s", wait_us / 1000000);
                    received_cmd = true;
                }
            }
            Select2::B(_) => {
                signal_queue.send(Signal::Poll(schedule)).await.unwrap();
            }
        }
    }
}

/// Schedule a status update (after a short debounce time)
fn schedule_status_update(schedule: &mut ScheduleChanges, immediately: bool) {
    let mut debounce_time = if immediately {
        UPDATE_STATUS_MIN_DEBOUNCE_US
    } else {
        UPDATE_STATUS_MAX_DEBOUNCE_US
    };

    // Already planned to (re-)schedule? pick the shortest time
    if let Some(scheduled) = schedule.get(Schedule::StatusUpdate) {
        debounce_time = debounce_time.min(scheduled);
    }
    schedule.update(Schedule::StatusUpdate, debounce_time);
}

#[cfg(all(test, feature = "test-mono"))]
mod tests {
    use super::*;
    use crate::{
        logic::{
            orchestrator::{
                application_behavior::ApplicationBehavior, demo_behavior::DemoBehavior,
            },
            signal::{BootReason, CmdSource, CommonSignal, DisconnectInfo, NetworkSignal},
            NetworkAction, NetworkActionList, NetworkActionNotifier,
        },
        storage::{
            backend::InMemoryFlash,
            files::{File, FileStore},
            flash_db::{block_layer::BlockDevice, Database},
            streams::{Stream, BLOCK_SIZE},
        },
        utils::{
            channels::*,
            sync::{Arbiter, ChangeNotification},
        },
    };
    use demo_protocol::{
        cmd::Cmd,
        settings::{DataRate, Settings as ProtoSettings},
        status::Status,
    };
    use sensor_link_firmware::drivers::timer_queue;

    type FlashDBImpl = InMemoryFlash<BLOCK_SIZE>;
    type DB = Arbiter<Database<BlockDevice<FlashDBImpl, BLOCK_SIZE>, BLOCK_SIZE, File, Stream>>;
    type ToNetwork =
        NetworkActionNotifier<ChangeNotification, Arbiter<NetworkActionList<DeviceStatusUpload>>>;
    type TestOrch = Orchestrator<
        DemoBehavior<FileStore<&'static DB>>,
        Receiver<Signal>,
        Sender<Signal>,
        &'static ToNetwork,
        Sender<Event>,
        LatestValueSender<AppState>,
    >;

    struct Timer(pub i64);
    impl time::TimestampSource for Timer {
        fn timestamp_us(&self) -> Result<i64, time::Error> {
            Ok(self.0)
        }
    }
    impl time::AdjustableTimestampSource for Timer {
        fn adjust_us(&self, _target_time_us: i64, _offset_limit: u32) -> Result<i64, time::Error> {
            Ok(0)
        }
    }

    fn init_time() {
        if time::init_timer(&Timer(1714403000 * 1_000_000)).is_err() {
            // already initialized by another test; wait until the timer is usable
            while time::timestamp_us().is_err() {
                std::thread::yield_now();
            }
        }
    }

    fn make_db() -> &'static DB {
        let flash = InMemoryFlash::new(32 * 1024 * 1024);
        let blockdev = BlockDevice::writeable_from(flash).unwrap();
        Box::leak(Box::new(Arbiter::new(Database::new(blockdev))))
    }

    fn make_network_action_notifier() -> &'static ToNetwork {
        Box::leak(Box::new(NetworkActionNotifier::new(
            Arbiter::new(NetworkActionList::new()),
            ChangeNotification::new(),
        )))
    }

    /// Settings payload as received from the network.
    fn proto_settings(sync_interval_s: i32, hash: u64) -> ProtoSettings {
        ProtoSettings {
            data_rate: DataRate::default(),
            sync_interval_s,
            hash,
        }
    }

    fn applied_settings(actions: &[Action]) -> Option<&Settings> {
        actions.iter().find_map(|a| match a {
            Action::ApplySettings(settings) => Some(settings),
            _ => None,
        })
    }

    fn init_in_state(state: AppState) -> TestOrch {
        init_time();

        let (signal_tx, signal_rx) = make_channel::<Signal>(10);
        let network_action_notifier = make_network_action_notifier();
        let (to_dispatch_tx, to_dispatch_rx) = make_channel::<Event>(10);
        // Leak so the channels stay open for the lifetime of the test.
        Box::leak(Box::new(to_dispatch_rx));
        let (to_ui_tx, _to_ui_rx) = make_latest_value_channel::<AppState>(AppState::Boot);

        let behavior = DemoBehavior::new(FileStore::new(make_db()));

        let mut orch = Orchestrator::new(
            behavior,
            signal_rx,
            signal_tx,
            network_action_notifier,
            to_dispatch_tx,
            to_ui_tx,
        );
        orch.state = state;
        if state == AppState::Active {
            orch.program_state = ProgramState {
                should_start: true,
                should_start_at: 0,
            };
        }
        orch
    }

    fn schedule_from_actions(actions: &[Action]) -> Option<ScheduleChanges> {
        let mut result = None;
        for action in actions {
            if let Action::ScheduleTimer(changes) = action {
                result = Some(changes.clone());
            }
        }
        result
    }

    /// Mock the StatusUpdate scheduler: emit a Poll if scheduled within the threshold.
    fn mock_status_update_if_scheduled_within(
        actions: &[Action],
        orch: &TestOrch,
        threshold_us: i64,
    ) -> bool {
        if let Some(schedule) = schedule_from_actions(actions) {
            if let Some(dt) = schedule.get(Schedule::StatusUpdate) {
                if dt <= threshold_us {
                    orch.signal_sender()
                        .try_send(Signal::Poll(Schedule::StatusUpdate))
                        .unwrap();
                    return true;
                }
            }
        }
        false
    }

    #[tokio::test]
    async fn test_orchestrator_starts_in_boot_state() {
        let mut orch = init_in_state(AppState::Boot);
        assert_eq!(orch.state, AppState::Boot);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Booted(BootReason::PowerOn)))
            .await
            .unwrap();
        let _ = orch.run().await;

        assert_ne!(orch.state, AppState::Boot);
    }

    #[tokio::test]
    async fn test_orchestrator_reschedule_poll() {
        let mut orch = init_in_state(AppState::Ready);
        orch.program_state = ProgramState {
            should_start: false,
            should_start_at: 0,
        };
        assert_eq!(orch.state, AppState::Ready);

        // New settings -> orchestrator (re-)schedules the network sync timer.
        orch.signal_sender()
            .send(Signal::Settings(proto_settings(0, 0)))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert!(schedule_from_actions(&actions).is_some());

        orch.signal_sender()
            .send(Signal::Settings(proto_settings(1800, 1)))
            .await
            .unwrap();
        let actions = orch.run().await;
        let changes = schedule_from_actions(&actions).expect("expected ScheduleTimer");
        assert_eq!(
            changes.get(Schedule::SyncNetwork).unwrap(),
            1800 * 1_000_000
        );
    }

    fn pending_timeouts(orch: &mut TestOrch) -> Vec<u32> {
        core::iter::from_fn(|| orch.to_network.try_next_action())
            .filter_map(|action| match action {
                NetworkAction::SetTimeout(timeout) => Some(timeout),
                _ => None,
            })
            .collect()
    }

    /// Always online takes effect in the session the settings arrive in.
    #[tokio::test]
    async fn test_orchestrator_always_online_applies_to_current_session() {
        let mut orch = init_in_state(AppState::Ready);

        orch.signal_sender()
            .send(Signal::Settings(proto_settings(0, 1)))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(pending_timeouts(&mut orch), [INFINITE_TIMEOUT_S]);

        orch.signal_sender()
            .send(Signal::Settings(proto_settings(900, 2)))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(pending_timeouts(&mut orch), [DEFAULT_TIMEOUT_S]);
    }

    /// Every network session starts at the default timeout, so a reconnect
    /// must get the always-online timeout again.
    #[tokio::test]
    async fn test_orchestrator_always_online_survives_reconnect() {
        let mut orch = init_in_state(AppState::Ready);
        orch.signal_sender()
            .send(Signal::Settings(proto_settings(0, 1)))
            .await
            .unwrap();
        let _ = orch.run().await;
        let _ = pending_timeouts(&mut orch);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Network(
                NetworkSignal::Connected,
            )))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(pending_timeouts(&mut orch), [INFINITE_TIMEOUT_S]);
    }

    /// Boot goes straight to measuring: there is no self-test to wait for.
    #[tokio::test]
    async fn test_orchestrator_boot_starts_measuring() {
        let mut orch = init_in_state(AppState::Boot);
        assert_eq!(orch.state, AppState::Boot);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Booted(BootReason::PowerOn)))
            .await
            .unwrap();
        let actions = orch.run().await;

        // Default program state: measuring starts right away
        assert_eq!(orch.state, AppState::TryStart);
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::StartMeasuring(_))));
        assert!(actions.iter().any(|a| matches!(a, Action::SpawnNetwork)));

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorStarted))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::Active);
    }

    /// The button held at boot wipes the stored settings: Booted then reads
    /// none and keeps the defaults.
    #[tokio::test]
    async fn test_orchestrator_button_at_boot_resets_settings() {
        let mut orch = init_in_state(AppState::Boot);
        orch.app.store().initialize_writeable(true).await.unwrap();
        orch.app
            .store()
            .write_settings(&proto_settings(1800, 7))
            .await
            .unwrap();

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::ButtonPressed))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::Boot);
        assert!(orch.app.store().read_settings().await.is_err());

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Booted(BootReason::PowerOn)))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.status.settings_hash, 0);
        assert_eq!(
            orch.settings.sync_interval_s,
            Settings::default().sync_interval_s
        );
    }

    /// A press after boot forces a sync.
    #[tokio::test]
    async fn test_orchestrator_button_forces_sync() {
        let mut orch = init_in_state(AppState::Active);
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::ButtonPressed))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert!(actions.iter().any(|a| matches!(a, Action::SpawnNetwork)));
    }

    /// A boot loop skips measuring and goes to Error, to stay reachable.
    #[tokio::test]
    async fn test_orchestrator_boot_loop_goes_to_error() {
        let mut orch = init_in_state(AppState::Boot);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Booted(BootReason::Loop)))
            .await
            .unwrap();
        let actions = orch.run().await;

        assert_eq!(orch.state, AppState::Error);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::StartMeasuring(_))));
    }

    /// Blink answers with the event and the LED action in any state.
    #[tokio::test]
    async fn test_orchestrator_blink_in_any_state() {
        for state in [AppState::Ready, AppState::Active, AppState::Error] {
            let mut orch = init_in_state(state);
            orch.signal_sender()
                .send(Signal::Common(CommonSignal::Command(
                    Cmd::Blink,
                    CmdSource::Network,
                )))
                .await
                .unwrap();
            let actions = orch.run().await;
            assert!(actions.iter().any(|a| matches!(a, Action::Blink)));
        }
    }

    /// The sync timer uses the same upper bound as settings validation.
    #[tokio::test]
    async fn test_orchestrator_sync_interval_clamped_to_settings_max() {
        let mut orch = init_in_state(AppState::Ready);
        orch.signal_sender()
            .send(Signal::Settings(proto_settings(i32::MAX, 1)))
            .await
            .unwrap();
        let actions = orch.run().await;
        let changes = schedule_from_actions(&actions).expect("expected ScheduleTimer");
        assert_eq!(
            changes.get(Schedule::SyncNetwork).unwrap(),
            SYNC_INTERVAL_MAX_SEC as i64 * 1_000_000
        );
    }

    #[tokio::test]
    async fn test_orchestrator_retry_start_measuring() {
        let mut orch = init_in_state(AppState::Active);

        for _ in 0..ERROR_RECOVERY_RETRIES {
            orch.signal_sender()
                .send(Signal::Common(CommonSignal::SensorFailed))
                .await
                .unwrap();
            let actions = orch.run().await;
            assert_eq!(orch.state, AppState::TryStart);
            assert!(matches!(actions[0], Action::StartMeasuring(_)));
        }

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorFailed))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::Error);
        assert!(actions.iter().any(|a| matches!(a, Action::SpawnNetwork)));
        assert!(actions.iter().any(|a| matches!(a, Action::StopMeasuring)));
    }

    /// As `retry_start_measuring` but recovering from unexpected SensorStopped.
    #[tokio::test]
    async fn test_orchestrator_retry_start_measuring_on_unexpected_stop() {
        let mut orch = init_in_state(AppState::Active);

        for _ in 0..ERROR_RECOVERY_RETRIES {
            orch.signal_sender()
                .send(Signal::Common(CommonSignal::SensorStopped))
                .await
                .unwrap();
            let actions = orch.run().await;
            assert_eq!(orch.state, AppState::TryStart);
            assert!(matches!(actions[0], Action::StartMeasuring(_)));
        }

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorFailed))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::Error);
        assert!(actions.iter().any(|a| matches!(a, Action::SpawnNetwork)));
        assert!(actions.iter().any(|a| matches!(a, Action::StopMeasuring)));
    }

    #[tokio::test]
    async fn test_orchestrator_can_recover_from_error() {
        let mut orch = init_in_state(AppState::TryStart);

        // First start attempt fails: orchestrator retries (stays in TryStart).
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorFailed))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::TryStart);
        assert!(matches!(actions[0], Action::StartMeasuring(_)));

        // Sensor starts: orchestrator becomes Active.
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorStarted))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::Active);
    }

    #[tokio::test]
    async fn test_orchestrator_update_status_in_error_state() {
        let mut orch = init_in_state(AppState::Boot);

        let mut dummy_actions = heapless::Vec::<Action, 2>::new();
        let mut dummy_schedule = ScheduleChanges::empty();
        orch.set_state(AppState::Error, &mut dummy_actions, &mut dummy_schedule)
            .await;

        orch.signal_sender()
            .send(Signal::Poll(Schedule::SyncNetwork))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert!(matches!(actions[0], Action::SpawnNetwork));

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Network(
                NetworkSignal::Connected,
            )))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::Error);

        if mock_status_update_if_scheduled_within(&actions, &orch, UPDATE_STATUS_MIN_DEBOUNCE_US) {
            let _ = orch.run().await;
        }

        let network_action = orch.to_network.try_next_action().unwrap();
        assert!(matches!(network_action, NetworkAction::SendStatus(_)));
        if let NetworkAction::SendStatus(status) = network_action {
            assert_eq!(status.status, Status::Error);
        }
    }

    #[tokio::test]
    async fn test_orchestrator_ready_to_active_to_error() {
        let mut orch = init_in_state(AppState::Ready);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Network(
                NetworkSignal::Connected,
            )))
            .await
            .unwrap();
        let _ = orch.run().await;

        orch.set_program_state(true, "test");

        for _ in 0..ERROR_RECOVERY_RETRIES {
            orch.signal_sender()
                .send(Signal::Common(CommonSignal::SensorFailed))
                .await
                .unwrap();
            let actions = orch.run().await;
            assert_eq!(orch.state, AppState::TryStart);
            assert!(matches!(actions[0], Action::StartMeasuring(_)));
        }

        while orch.to_network.try_next_action().is_some() {}

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorFailed))
            .await
            .unwrap();
        let actions = orch.run().await;
        let schedule = schedule_from_actions(&actions).expect("no schedule in actions");
        assert_eq!(
            schedule.get(Schedule::State).unwrap(),
            ERROR_POLL_INTERVAL_US
        );
        assert_eq!(orch.state, AppState::Error);

        orch.signal_sender()
            .send(Signal::Poll(Schedule::SyncNetwork))
            .await
            .unwrap();
        let actions = orch.run().await;
        if mock_status_update_if_scheduled_within(&actions, &orch, UPDATE_STATUS_MIN_DEBOUNCE_US) {
            let _ = orch.run().await;
        }
        let network_action = orch.to_network.try_next_action().unwrap();
        match network_action {
            NetworkAction::SendStatus(status) => assert_eq!(status.status, Status::Error),
            other => panic!("unexpected network action: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_orchestrator_not_shortcutting_try_stop_state() {
        let mut orch = init_in_state(AppState::TryStop);
        orch.set_program_state(true, "test");
        orch.measuring_active = true;
        assert_eq!(orch.state, AppState::TryStop);

        // Settings while stopping must not shortcut out of TryStop.
        orch.signal_sender()
            .send(Signal::Settings(proto_settings(0, 0)))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::TryStop);

        // Only the SensorStopped confirmation leaves TryStop; should_start -> restart.
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorStopped))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::TryStart);
    }

    #[tokio::test]
    async fn test_orchestrator_not_shortcutting_try_start_state() {
        let mut orch = init_in_state(AppState::TryStart);
        orch.set_program_state(true, "test");
        orch.measuring_active = false;
        assert_eq!(orch.state, AppState::TryStart);

        // Re-evaluating start/stop must not shortcut out of TryStart:
        // only the SensorStarted confirmation leaves it.
        orch.signal_sender()
            .send(Signal::Poll(Schedule::StartStop))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::TryStart);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorStarted))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::Active);
    }

    /// Stop command while the session is starting: it is cancelled right away
    /// instead of coming up Active.
    #[tokio::test]
    async fn test_orchestrator_stop_command_during_try_start() {
        let mut orch = init_in_state(AppState::TryStart);
        orch.set_program_state(true, "test");
        orch.measuring_active = false;

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Command(
                Cmd::Stop,
                CmdSource::Network,
            )))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::TryStop);
        assert!(actions.iter().any(|a| matches!(a, Action::StopMeasuring)));

        // The session reports started before it sees the cancel: stay stopping.
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorStarted))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::TryStop);

        // Stop confirmed: stay stopped, the stop command cleared should_start.
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::SensorStopped))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_ne!(orch.state, AppState::TryStart);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::StartMeasuring(_))));
    }

    /// New settings while the session is starting: forwarded, no restart.
    #[tokio::test]
    async fn test_orchestrator_settings_applied_during_try_start() {
        let mut orch = init_in_state(AppState::TryStart);
        orch.set_program_state(true, "test");
        orch.measuring_active = false;

        orch.signal_sender()
            .send(Signal::Settings(proto_settings(1800, 1)))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::TryStart);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::StopMeasuring | Action::StartMeasuring(_))));
        assert_eq!(applied_settings(&actions).unwrap().hash, 1);
    }

    /// New settings while Active: forwarded to the running session, no restart.
    #[tokio::test]
    async fn test_orchestrator_settings_applied_while_active() {
        let mut orch = init_in_state(AppState::Active);
        orch.set_program_state(true, "test");
        orch.measuring_active = true;

        let mut settings = proto_settings(1800, 1);
        settings.data_rate = DataRate::_1SPS;
        orch.signal_sender()
            .send(Signal::Settings(settings))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::Active);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::StopMeasuring | Action::StartMeasuring(_))));
        let applied = applied_settings(&actions).unwrap();
        assert_eq!(applied.hash, 1);
        assert_eq!(applied.sync_interval_s, 1800);
        assert_eq!(applied.data_rate, DataRate::_1SPS);
        assert_eq!(orch.status.settings_hash, 1);
    }

    /// New settings during a firmware update are ignored and stay unconfirmed.
    #[tokio::test]
    async fn test_orchestrator_settings_ignored_during_firmware_update() {
        let mut orch = init_in_state(AppState::FirmwareUpdate);

        orch.signal_sender()
            .send(Signal::Settings(proto_settings(1800, 1)))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert_eq!(orch.state, AppState::FirmwareUpdate);
        assert!(applied_settings(&actions).is_none());
        assert_eq!(orch.status.settings_hash, 0);
    }

    #[tokio::test]
    async fn test_orchestrator_try_start_timeout() {
        let mut orch = init_in_state(AppState::TryStart);
        orch.set_program_state(true, "test");
        orch.measuring_active = false;
        assert_eq!(orch.state, AppState::TryStart);

        for _ in 0..ERROR_RECOVERY_RETRIES {
            orch.signal_sender()
                .send(Signal::Poll(Schedule::State))
                .await
                .unwrap();
            let actions = orch.run().await;
            assert!(matches!(actions[0], Action::StartMeasuring(_)));
            assert_eq!(orch.state, AppState::TryStart);
        }

        orch.signal_sender()
            .send(Signal::Poll(Schedule::State))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::Error);
    }

    #[tokio::test]
    async fn test_orchestrator_try_stop_timeout() {
        let mut orch = init_in_state(AppState::TryStop);
        orch.set_program_state(true, "test");
        orch.measuring_active = true;
        assert_eq!(orch.state, AppState::TryStop);

        orch.signal_sender()
            .send(Signal::Poll(Schedule::State))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::Error);
    }

    #[tokio::test]
    async fn test_orchestrator_disconnect_when_queue_empty() {
        let mut orch = init_in_state(AppState::Ready);
        orch.settings.sync_interval_s = 180;
        orch.connected_since = Some(MonotonicInstant::test(0));
        timer_queue::timer_queue().set_time_offset(4 * 60 * 1_000_000);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::DispatchQueueEmpty))
            .await
            .unwrap();
        let _ = orch.run().await;

        match orch.to_network.try_next_action() {
            Some(NetworkAction::Disconnect) => {}
            other => panic!("expected disconnect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_orchestrator_no_disconnect_when_sync_interval_0() {
        let mut orch = init_in_state(AppState::Ready);
        orch.settings.sync_interval_s = 0;

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::DispatchQueueEmpty))
            .await
            .unwrap();
        orch.connected_since = Some(MonotonicInstant::test(0));
        timer_queue::timer_queue().set_time_offset(4 * 60 * 1_000_000);

        let _ = orch.run().await;
        assert!(orch.to_network.try_next_action().is_none());
    }

    #[tokio::test]
    async fn test_orchestrator_not_enqueueing_multiple_network_disconnects() {
        let mut orch = init_in_state(AppState::Ready);
        orch.settings.sync_interval_s = 180;
        orch.connected_since = Some(MonotonicInstant::test(0));
        timer_queue::timer_queue().set_time_offset(4 * 60 * 1_000_000);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::DispatchQueueEmpty))
            .await
            .unwrap();
        let _ = orch.run().await;
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::DispatchQueueEmpty))
            .await
            .unwrap();
        let _ = orch.run().await;

        let mut disconnects = 0;
        while let Some(action) = orch.to_network.try_next_action() {
            if matches!(action, NetworkAction::Disconnect) {
                disconnects += 1;
            }
        }
        assert_eq!(disconnects, 1);
    }

    #[tokio::test]
    async fn test_orchestrator_no_cmd_in_update_state() {
        let mut orch = init_in_state(AppState::Active);
        orch.app.store().initialize_writeable(true).await.unwrap();
        orch.set_program_state(true, "test");
        orch.measuring_active = true;
        assert_eq!(orch.state, AppState::Active);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::FirmwareUpdateAnnounced))
            .await
            .unwrap();
        let _ = orch.run().await;
        assert_eq!(orch.state, AppState::FirmwareUpdate);

        // Commands are ignored while updating firmware.
        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Command(
                Cmd::Stop,
                CmdSource::Network,
            )))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn network_end_backs_off_sync_and_status_update() {
        for end in [
            NetworkSignal::Disconnected(DisconnectInfo::Final),
            NetworkSignal::ConnectFailed,
        ] {
            let mut orch = init_in_state(AppState::Ready);

            orch.signal_sender()
                .send(Signal::Common(CommonSignal::Network(end.clone())))
                .await
                .unwrap();
            let actions = orch.run().await;

            let changes = schedule_from_actions(&actions).expect("expected a ScheduleTimer action");
            let next_sync = changes.get(Schedule::SyncNetwork).unwrap();
            assert!(next_sync > SPAWN_RETRY_DELAY_US, "{end:?}");
            assert_eq!(
                changes.get(Schedule::StatusUpdate),
                Some(next_sync),
                "{end:?}: a due status update must not respawn the network right away"
            );
        }
    }

    #[tokio::test]
    async fn spawn_failed_reschedules_syncnetwork_short() {
        let mut orch = init_in_state(AppState::Ready);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Network(
                NetworkSignal::SpawnFailed,
            )))
            .await
            .unwrap();
        let actions = orch.run().await;

        let changes = schedule_from_actions(&actions).expect("expected a ScheduleTimer action");
        assert_eq!(
            changes.get(Schedule::SyncNetwork),
            Some(SPAWN_RETRY_DELAY_US),
            "SpawnFailed must reschedule SyncNetwork to the short retry delay"
        );
    }

    #[tokio::test]
    async fn spawn_failed_then_syncnetwork_poll_spawns_when_offline() {
        let mut orch = init_in_state(AppState::Ready);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Network(
                NetworkSignal::SpawnFailed,
            )))
            .await
            .unwrap();
        let _ = orch.run().await;

        assert!(!orch.network_is_connected());
        orch.signal_sender()
            .send(Signal::Poll(Schedule::SyncNetwork))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert!(actions.iter().any(|a| matches!(a, Action::SpawnNetwork)));
    }

    #[tokio::test]
    async fn spawn_failed_then_syncnetwork_poll_noop_when_online() {
        let mut orch = init_in_state(AppState::Ready);

        orch.signal_sender()
            .send(Signal::Common(CommonSignal::Network(
                NetworkSignal::SpawnFailed,
            )))
            .await
            .unwrap();
        let _ = orch.run().await;

        orch.connected_since = Some(MonotonicInstant::test(0));
        assert!(orch.network_is_connected());
        orch.signal_sender()
            .send(Signal::Poll(Schedule::SyncNetwork))
            .await
            .unwrap();
        let actions = orch.run().await;
        assert!(!actions.iter().any(|a| matches!(a, Action::SpawnNetwork)));
    }
}
