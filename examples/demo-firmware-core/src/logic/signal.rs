//! This module defines all signals that can be send to the orchestrator.
//! These define all possible input for the business logic.

use crate::logic::{demo_processing::input::DEMO_N_CH, Schedule};
use demo_protocol::{settings::Settings, status::ChannelStatus};

pub use sensor_link_firmware::logic::signal::{
    BootReason, CmdSource, DisconnectInfo, NetworkSignal, Signal as CommonSignal, Temperature,
};

#[derive(Debug)]
pub struct AllChannelStatus {
    pub sensors: [ChannelStatus; DEMO_N_CH],
}

/// The Signals that the orchestrator can process
// No alloc in firmware, so the large variants cannot be boxed
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Signal {
    /// Manufacturer-generic signals produced by the generic protocol client
    /// (network state, commands, network time, firmware update protocol,
    /// boot, button, watchdog, measurement lifecycle, board temperature).
    Common(CommonSignal),
    /// New settings have been received.
    Settings(Settings),

    /// A scheduled timer has expired
    Poll(Schedule),

    ChannelStatus(AllChannelStatus),
}

impl From<CommonSignal> for Signal {
    fn from(common: CommonSignal) -> Self {
        Signal::Common(common)
    }
}
