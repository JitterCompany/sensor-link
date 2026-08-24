//! Orchestrator signals produced by the generic protocol client
//! (commands, network time, firmware update protocol).

use sensor_link_protocol::{cmd::Cmd, fwupdate::FWChunk};

use crate::logic::time_adjust::{NetworkTime, Timestamp};

/// Signals about the network connection state.
#[derive(Debug, Clone)]
pub enum NetworkSignal {
    /// Network successfully connected to the server
    Connected,
    /// Network disconnected
    Disconnected(DisconnectInfo),
    /// Network connection failed
    ConnectFailed,
    /// The network task could not be started: its slot was still occupied
    /// (e.g. a previous task is still tearing down, or is starting/connecting).
    SpawnFailed,
}

/// Details of a [`NetworkSignal::Disconnected`].
#[derive(Debug, Clone)]
pub enum DisconnectInfo {
    /// Final disconnect (no retry planned)
    Final,

    /// Disconnect (retry planned in N seconds)
    Retry(u32),
}

/// Reason the system (re)booted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BootReason {
    /// Boot reason could not be determined
    Unknown,

    /// Power (re-)applied
    PowerOn,

    /// Hardfault handler
    HardFault,

    /// Watchdog triggered
    Watchdog,

    /// Panic handler
    Panic,

    /// Generic software-triggered reboot (not hardfault or panic)
    /// Could be issued by firmware update or via RTT/debugger
    Software,

    /// Bootloop detection was triggered
    Loop,
}

impl BootReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            BootReason::PowerOn => "PowerOn",
            BootReason::Watchdog => "Watchdog",
            BootReason::Panic => "Panic",
            BootReason::HardFault => "Hardfault",
            BootReason::Software => "Software",
            BootReason::Unknown => "Unknown",
            BootReason::Loop => "Loop",
        }
    }
}

/// Source that produced a [`Signal::Command`].
#[derive(Debug)]
pub enum CmdSource {
    Network,
    Button,
    RTT,
    Orch,
}

impl CmdSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            CmdSource::Network => "Network",
            CmdSource::Button => "Button",
            CmdSource::RTT => "RTT",
            CmdSource::Orch => "Orch",
        }
    }
}

/// Board temperature reading (degrees Celsius).
#[derive(Debug)]
pub struct Temperature {
    pub board_celsius: f32,
}

/// Orchestrator signals produced by the generic protocol client.
// `FirmwareChunk` carries an inline `FW_CHUNK_SIZE` buffer, which dwarfs the other variants.
// Boxing it is not an option: this crate is `no_std` with no global allocator.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Signal {
    /// Network connection state changed.
    Network(NetworkSignal),

    /// Commands from the user.
    Command(Cmd, CmdSource),

    /// New wallclock time has been received and applied.
    /// This means existing timestamps may not be useful anymore.
    NetworkTime(Timestamp, NetworkTime),

    /// Receiving chunk of firmware.
    FirmwareChunk(FWChunk),

    /// Firmware update notification.
    FirmwareUpdateAnnounced,

    /// Firmware download is complete.
    FirmwareUpdateComplete,

    /// Firmware download failed: no update will follow.
    FirmwareUpdateFailed,

    /// Signal to delete firmware update from store
    FirmwareUpdateDelete,

    /// An urgent event occurred and should be uploaded with priority.
    UrgentEvent,

    /// The dispatch pipeline has no more data pending for upload.
    DispatchQueueEmpty,

    /// First signal expected when initialization is done.
    /// This will start off the Orchestrator State Machine.
    Booted(BootReason),

    /// Error signal: measurement running unexpectedly
    MeasurementStillRunning,

    /// Error signal: sensor has failed
    SensorFailed,

    /// Sensor has started measuring
    SensorStarted,

    /// Sensor has stopped measuring
    SensorStopped,

    /// New BoardTemperature info available
    BoardTemperature(Temperature),

    /// User pressed the button
    ButtonPressed,

    /// Watchdog must be fed soon (or the system will reboot)
    WatchdogHungry,
}
