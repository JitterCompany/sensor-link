//! The board-identity straps: `hw_v0_1` (PA4), `hw_v0_2` (PA15) and `hw_v1`
//! (PG1), as named in the SL23-modem-tester pin map.

use core::fmt;

/// Level of one strap pin, read under the internal pull-up and then the
/// pull-down. A pin that follows both pulls is not connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strap {
    Low,
    High,
    Floating,
}

impl Strap {
    const fn name(self) -> &'static str {
        match self {
            Strap::Low => "low",
            Strap::High => "high",
            Strap::Floating => "floating",
        }
    }
}

/// Which version of the board this is, from the `hw_v1` strap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardVersion {
    /// `hw_v1` floating: v0, the pin-compatible board this was developed on
    V0,
    /// `hw_v1` strapped low: the SL23-modem-tester v1
    V1,
    /// `hw_v1` driven high: not a strap this firmware knows about
    Unknown,
}

impl BoardVersion {
    pub const fn name(self) -> &'static str {
        match self {
            BoardVersion::V0 => "v0",
            BoardVersion::V1 => "v1",
            BoardVersion::Unknown => "unknown",
        }
    }
}

/// The straps as read at init.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Straps {
    pub hw_v0_1: Strap,
    pub hw_v0_2: Strap,
    pub hw_v1: Strap,
}

impl Straps {
    /// `hw_v0_1` low and `hw_v0_2` high: the same on every board this firmware
    /// supports. Anything else is a board it was not written for.
    pub fn is_supported(&self) -> bool {
        self.hw_v0_1 == Strap::Low && self.hw_v0_2 == Strap::High
    }

    /// Informational: the pinout is the same across versions, so nothing
    /// should gate on this.
    pub fn board_version(&self) -> BoardVersion {
        match self.hw_v1 {
            Strap::Floating => BoardVersion::V0,
            Strap::Low => BoardVersion::V1,
            Strap::High => BoardVersion::Unknown,
        }
    }
}

impl fmt::Display for Straps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "hw_v0_1={} hw_v0_2={} hw_v1={}",
            self.hw_v0_1.name(),
            self.hw_v0_2.name(),
            self.hw_v1.name()
        )
    }
}
