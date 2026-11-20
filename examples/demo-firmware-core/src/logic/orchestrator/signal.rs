use demo_protocol::{cmd::Cmd, fwupdate::FWChunk, settings::Settings};
use sensor_link_firmware::sensor_link_protocol::event::CellInfo;

use crate::logic::{
    signal::{
        AllChannelStatus, BootReason, CmdSource, CommonSignal, NetworkSignal, Signal as Sig,
        Temperature,
    },
    time_adjust, AppState, Schedule,
};

/// State-specific signals.
///
/// These signals explicitly expect to be handled only in a specific orchestrator state.
/// If the orchestrator is not in the expected state, the signal should be ignored.
// No alloc in firmware, so the large variants cannot be boxed
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum State {
    Booted(BootReason),
    FirmwareUpdateComplete,
    FirmwareUpdateFailed,
    FirmwareChunk(FWChunk),
}

impl State {
    pub fn expected_state(&self) -> AppState {
        match self {
            State::Booted(_) => AppState::Boot,
            State::FirmwareUpdateComplete => AppState::FirmwareUpdate,
            State::FirmwareUpdateFailed => AppState::FirmwareUpdate,
            State::FirmwareChunk(_) => AppState::FirmwareUpdate,
        }
    }
}

/// Status signal: state-independent signals.
/// These should be handled independent of the current orchestrator state
/// and do not directly cause a state change.
///
/// Typical side-effects are requesting Actions or storing status info.
// The variant names mirror the common signals they come from
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub enum Status {
    BoardTemperature(Temperature),
    NetworkTime(time_adjust::Timestamp, time_adjust::NetworkTime),
    FirmwareUpdateDelete,
    Network(NetworkSignal),
    DispatchQueueEmpty,
    /// Reply to a dispatch barrier; unused, no barrier is ever requested
    DispatchDrained,
    /// A status was published; unused, nothing waits for it
    StatusSent,
    UrgentEvent,
    WatchdogHungry,
    ChannelStatus(AllChannelStatus),
    CellInfo(CellInfo),
}

/// Generic Signal
///
/// These need to be handled differently depending on the current orchestrator state
/// and may directly cause state changes in the orchestrator.
#[derive(Debug)]
pub enum Generic {
    Command(Cmd, CmdSource),
    ButtonPressed,
    FirmwareUpdateAnnounced,
    MeasurementStillRunning,
    SensorFailed,
    SensorStarted,
    SensorStopped,
    Poll(Schedule),
    Settings(Settings),
}

// No alloc in firmware, so the large variants cannot be boxed
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Signal {
    /// Signals to be handled by one specific state only
    StateSpecific(State),

    /// Signals to be handled in any state. These cannot cause
    /// a state change
    StateIndependent(Status),

    /// Generic signals: these may have different effects based
    /// on the current AppState and cause state transitions
    Generic(Generic),
}

impl From<crate::logic::signal::Signal> for Signal {
    fn from(signal: crate::logic::signal::Signal) -> Self {
        match signal {
            // State-specific signals
            Sig::Common(CommonSignal::Booted(reason)) => {
                Signal::StateSpecific(State::Booted(reason))
            }
            Sig::Common(CommonSignal::FirmwareUpdateComplete) => {
                Signal::StateSpecific(State::FirmwareUpdateComplete)
            }
            Sig::Common(CommonSignal::FirmwareUpdateFailed) => {
                Signal::StateSpecific(State::FirmwareUpdateFailed)
            }
            Sig::Common(CommonSignal::FirmwareChunk(chunk)) => {
                Signal::StateSpecific(State::FirmwareChunk(chunk))
            }

            // State-dependent signals
            Sig::Common(CommonSignal::Command(cmd, src)) => {
                Signal::Generic(Generic::Command(cmd, src))
            }
            Sig::Common(CommonSignal::ButtonPressed) => Signal::Generic(Generic::ButtonPressed),
            Sig::Common(CommonSignal::FirmwareUpdateAnnounced) => {
                Signal::Generic(Generic::FirmwareUpdateAnnounced)
            }
            Sig::Common(CommonSignal::MeasurementStillRunning) => {
                Signal::Generic(Generic::MeasurementStillRunning)
            }
            Sig::Common(CommonSignal::SensorFailed) => Signal::Generic(Generic::SensorFailed),
            Sig::Common(CommonSignal::SensorStarted) => Signal::Generic(Generic::SensorStarted),
            Sig::Common(CommonSignal::SensorStopped) => Signal::Generic(Generic::SensorStopped),
            Sig::Poll(schedule) => Signal::Generic(Generic::Poll(schedule)),
            Sig::Settings(settings) => Signal::Generic(Generic::Settings(settings)),

            // State-independent signals
            Sig::Common(CommonSignal::Network(network)) => {
                Signal::StateIndependent(Status::Network(network))
            }
            Sig::Common(CommonSignal::FirmwareUpdateDelete) => {
                Signal::StateIndependent(Status::FirmwareUpdateDelete)
            }
            Sig::Common(CommonSignal::BoardTemperature(temp)) => {
                Signal::StateIndependent(Status::BoardTemperature(temp))
            }
            Sig::Common(CommonSignal::UrgentEvent) => Signal::StateIndependent(Status::UrgentEvent),
            Sig::Common(CommonSignal::NetworkTime(t_local, t_network)) => {
                Signal::StateIndependent(Status::NetworkTime(t_local, t_network))
            }
            Sig::Common(CommonSignal::DispatchQueueEmpty) => {
                Signal::StateIndependent(Status::DispatchQueueEmpty)
            }
            Sig::Common(CommonSignal::DispatchDrained) => {
                Signal::StateIndependent(Status::DispatchDrained)
            }
            Sig::Common(CommonSignal::StatusSent(_)) => {
                Signal::StateIndependent(Status::StatusSent)
            }
            Sig::Common(CommonSignal::WatchdogHungry) => {
                Signal::StateIndependent(Status::WatchdogHungry)
            }
            Sig::ChannelStatus(status) => Signal::StateIndependent(Status::ChannelStatus(status)),
            Sig::Common(CommonSignal::CellInfo(cell)) => {
                Signal::StateIndependent(Status::CellInfo(cell))
            }
        }
    }
}
