//! The hardware-independent business logic of the demo device.

pub use sensor_link_firmware::logic::active_config;
pub mod client;
mod debounced_channel_status;
pub mod demo_processing;
pub mod dispatch;
pub mod led;
mod network;
pub mod orchestrator;
pub mod processing;
pub mod settings;
pub mod signal;
pub mod time_adjust;

use demo_protocol::status::Status;

pub use client::DeviceStatusUpload;
pub use debounced_channel_status::DebouncedChannelStatus;
pub use network::{network_task, upload, upload::NetworkUploadItem, upload_items, NetworkClient};
pub use orchestrator::orch_poll_timer_task;
pub use sensor_link_firmware::logic::DrainOnDisconnect;
use strum::EnumCount;

use sensor_link_firmware::traits::Arbiter;

use crate::monotonic_time::Timeout;

#[derive(Debug, Clone, Copy, strum::EnumCount, strum::EnumIter)]
pub enum Schedule {
    /// Orchestrator-state-specific timeout
    State,

    /// Synchronize to network (connect + status)
    SyncNetwork,

    /// Orchestrator-specific debouncing/throttling
    StatusUpdate,

    /// Planned start/stop. NB: wall-clock-time sensitive
    StartStop,
}

impl Schedule {
    pub fn iter() -> impl Iterator<Item = Schedule> {
        <Schedule as strum::IntoEnumIterator>::iter()
    }
}

/// Represents ScheduleTimer changes as requested by orchestrator.
///
/// See [orchestrator::Action]
///

#[derive(Debug, Clone)]
pub struct ScheduleChanges {
    schedules: [Option<i64>; Schedule::COUNT],
}

impl ScheduleChanges {
    pub fn empty() -> Self {
        Self {
            schedules: [None; Schedule::COUNT],
        }
    }

    /// Update a timeout for the given schedule
    ///
    /// Overwrites previous timeout (if any)
    pub fn update(&mut self, schedule: Schedule, timeout: i64) {
        self.schedules[schedule as usize] = Some(timeout);
    }

    /// Iterator over all changes
    pub fn iter(&self) -> impl Iterator<Item = (Schedule, i64)> + '_ {
        // iterates over all `Schedule` variants, map to tuple and filter out Nones
        Schedule::iter().filter_map(|sch| self.schedules[sch as usize].map(|dt| (sch, dt)))
    }

    pub fn get(&self, schedule: Schedule) -> Option<i64> {
        self.schedules[schedule as usize]
    }

    /// Empty means no changes: nothing needs (re-)scheduling
    pub fn is_empty(&self) -> bool {
        for sch in self.schedules {
            if sch.is_some() {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppState {
    /// Initial state after power on
    Boot,
    /// Device is ready to start measuring
    Ready,

    /// Trying to start measuring activity
    TryStart,

    /// In process of stopping measuring
    TryStop,

    /// Measuring is active
    Active,
    /// Device is in standby, waiting for the right time to start
    Standby,
    /// No valid MeasuringConfig
    NoConfig,

    /// Something went wrong, but we'll try to fix it
    ErrorRecovery,

    /// Fatal Error, unable to recover
    Error,

    /// Only firmware update is available in this state
    FirmwareUpdate,
}

impl From<AppState> for Status {
    fn from(value: AppState) -> Self {
        match value {
            AppState::Boot => Status::Startup,
            AppState::Ready => Status::Inactive,
            AppState::Standby => Status::Idle,
            AppState::TryStart => Status::TryActive,
            AppState::TryStop => Status::Inactive, // This is not entirely correct.
            AppState::Active => Status::Active,
            AppState::NoConfig => Status::NoConfig,
            AppState::Error => Status::Error,
            AppState::ErrorRecovery => Status::Error,
            AppState::FirmwareUpdate => Status::Startup, // todo better state?
        }
    }
}

// The generic network-action interface lives in sensor-link-firmware. The
// concrete `NetworkActionList`/`NetworkActionNotifier` stay here, because they
// pin this crate's `Timeout`/`Arbiter`.
pub use sensor_link_firmware::logic::{
    NetworkAction, NetworkActionNotifyReader, NetworkActionNotifyWriter, NetworkStatus,
};

/// A list of network actions to be done by the network task.
///
/// This struct only retains the latest value for each type.
#[derive(Debug)]
pub struct NetworkActionList<S> {
    disconnect: bool,
    send_status: Option<S>,
    download_update: bool,

    // Only reset by network task.
    send_device_info: bool,
    set_timeout: Option<u32>,

    /// Keep track of last time action was requested
    network_failure_timeout: Timeout,
}

/// A notifier for network actions. This makes use of [NetworkActionList] to only retain the latest value for each [NetworkAction] type.
///
/// This struct provides a way to notify about changes to the network action list
/// and wait for changes asynchronously.
pub struct NetworkActionNotifier<Change: ChangeNotifier, A: Arbiter> {
    inner: A,
    notifier: Change,
}

impl<Change: ChangeNotifier, A: Arbiter> NetworkActionNotifier<Change, A> {
    pub const fn new(inner: A, notifier: Change) -> Self {
        Self { inner, notifier }
    }
}

impl<Change, A, S> NetworkActionNotifier<Change, A>
where
    Change: ChangeNotifier,
    A: Arbiter<Shared = NetworkActionList<S>>,
{
    pub async fn next_action(&self) -> NetworkAction<S> {
        loop {
            {
                let mut action_guard = self.inner.access().await;
                if let Some(action) = action_guard.next_action() {
                    return action;
                }
                drop(action_guard); // without drop we block the other side
            }
            self.notifier.await_change().await;
        }
    }

    pub fn try_next_action(&self) -> Option<NetworkAction<S>> {
        let mut action_guard = self.inner.try_access()?;
        action_guard.next_action()
    }

    /// Returns Err(()) if the network task is blocked
    #[allow(clippy::result_unit_err)] // matches NetworkActionNotifyWriter::set
    pub async fn set(&self, action: NetworkAction<S>) -> Result<(), ()> {
        let mut action_guard = self.inner.access().await;
        if action_guard.set(action) {
            self.notifier.notify();
        }
        if action_guard.is_network_failure() {
            return Err(());
        }
        Ok(())
    }

    /// Clear any pending NetworkAction::Disconnect
    ///
    /// Call this before (re-)connecting
    pub async fn clear_pending_disconnect(&self) {
        self.inner.access().await.disconnect = false;
    }
}

impl<Change, A, S> NetworkActionNotifyWriter<S> for &NetworkActionNotifier<Change, A>
where
    Change: ChangeNotifier,
    A: Arbiter<Shared = NetworkActionList<S>>,
{
    #[inline]
    async fn set(&self, action: NetworkAction<S>) -> Result<(), ()> {
        NetworkActionNotifier::<Change, A>::set(self, action).await
    }

    #[inline]
    async fn clear_pending_disconnect(&self) {
        NetworkActionNotifier::<Change, A>::clear_pending_disconnect(self).await
    }
}

impl<Change, A, S> NetworkActionNotifyReader<S> for &NetworkActionNotifier<Change, A>
where
    Change: ChangeNotifier,
    A: Arbiter<Shared = NetworkActionList<S>>,
{
    #[inline]
    async fn next_action(&self) -> NetworkAction<S> {
        NetworkActionNotifier::<Change, A>::next_action(self).await
    }

    #[inline]
    fn try_next_action(&self) -> Option<NetworkAction<S>> {
        NetworkActionNotifier::<Change, A>::try_next_action(self)
    }
}

impl<S> Default for NetworkActionList<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> NetworkActionList<S> {
    pub const fn new() -> Self {
        Self {
            disconnect: false,
            send_status: None,
            download_update: false,
            send_device_info: false,
            set_timeout: None,
            network_failure_timeout: Timeout::new(),
        }
    }
    pub fn next_action(&mut self) -> Option<NetworkAction<S>> {
        self.network_failure_timeout.clear();
        if self.disconnect {
            self.disconnect = false;
            return Some(NetworkAction::Disconnect);
        }
        if self.send_device_info {
            self.send_device_info = false;
            return Some(NetworkAction::SendDeviceInfo);
        }
        if let Some(status) = self.send_status.take() {
            return Some(NetworkAction::SendStatus(status));
        }
        if self.download_update {
            self.download_update = false;
            return Some(NetworkAction::DownloadUpdate);
        }
        if let Some(timeout) = self.set_timeout.take() {
            return Some(NetworkAction::SetTimeout(timeout));
        }
        None
    }

    /// Returns true if the action was changed
    pub fn set(&mut self, action: NetworkAction<S>) -> bool {
        if !self.network_failure_timeout.is_set() {
            self.network_failure_timeout
                .set_ms(network::NETWORK_DISCONNECTED_TIMEOUT_MS);
        }
        match action {
            NetworkAction::Disconnect => {
                if !self.disconnect {
                    self.clear();
                    self.disconnect = true;
                    return true;
                }
            }
            NetworkAction::SendDeviceInfo => {
                if !self.send_device_info {
                    self.send_device_info = true;
                    return true;
                }
            }
            NetworkAction::SendStatus(status) => {
                // Always notify
                self.send_status = Some(status);
                return true;
            }
            NetworkAction::DownloadUpdate => {
                if !self.download_update {
                    self.download_update = true;
                    return true;
                }
            }
            NetworkAction::SetTimeout(timeout) => {
                if self.set_timeout != Some(timeout) {
                    self.set_timeout = Some(timeout);
                    return true;
                }
            }
        }
        false
    }

    pub fn is_network_failure(&self) -> bool {
        self.network_failure_timeout.is_expired()
    }

    /// Only called by Orchestrator.
    fn clear(&mut self) {
        self.disconnect = false;
        self.send_status = None;
        self.download_update = false;

        // do not clear
        // - send_device_info
        // - set_timeout
    }
}

/// Platform-agnostic channel traits, providing a common interface for the various
/// mpsc sender/receiver implementations (Tokio, RTIC, …) to adhere to. Orchestrator
/// needs them because it is platform agnostic: it must send/receive messages without
/// knowing implementation details about the underlying queue.
pub use sensor_link_firmware::logic::{
    ChangeNotifier, LatestValueReceiveChannel, LatestValueSendChannel, ReceiveChannel, SendChannel,
};

pub use sensor_link_firmware::logic::FiniteStream;

/// Output of the demo processing pipeline.
///
/// Processing produces plain data only (no alarm envelope), so this is a thin
/// single-variant wrapper. Kept as an enum so the shape can grow
/// (e.g. trace triggers) without churn at call sites.
pub enum ProcessingOutputT<T> {
    /// Processing results: available at a steady pace
    Data(T),
}

/// Adapter that unwraps a channel of `ProcessingOutputT<T>` into a channel of `T`.
///
/// The pipeline emits data only, so this simply forwards the `Data`
/// payload. (Kept as an adapter so an event side-channel can be reintroduced
/// later without changing consumers.)
pub struct ProcessingOutputAdapter<RX, T> {
    rx: RX,
    _phantom: core::marker::PhantomData<T>,
}

impl<T, RX> ProcessingOutputAdapter<RX, T>
where
    RX: ReceiveChannel<ProcessingOutputT<T>>,
{
    pub fn new(rx: RX) -> Self {
        Self {
            rx,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<T, RX> ReceiveChannel<T> for ProcessingOutputAdapter<RX, T>
where
    RX: ReceiveChannel<ProcessingOutputT<T>>,
{
    type Error = ();
    async fn recv(&mut self) -> Result<T, Self::Error> {
        match self.rx.recv().await.map_err(|_| ())? {
            ProcessingOutputT::Data(data) => Ok(data),
        }
    }

    fn try_recv(&mut self) -> Result<T, Self::Error> {
        match self.rx.try_recv().map_err(|_| ())? {
            ProcessingOutputT::Data(data) => Ok(data),
        }
    }
}

/// Cancel token to exit task
#[derive(Debug, Copy, Clone)]
pub struct Cancel;
