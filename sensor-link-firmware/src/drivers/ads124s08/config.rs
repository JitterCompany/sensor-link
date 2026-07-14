use super::register;
pub use super::register::{
    AnalogPin,
    DATARATE::DR as DataRate,
    IDACMAG::IMAG as IDACMagnitude,
    PGA::{DELAY as Delay, GAIN as Gain},
    REF::REFSEL as RefSource,
};

/// Config related to the reference and IDAC sources
pub struct RefConfig {
    /// Select which voltage reference to use
    ///
    /// Reference buffers are enabled unless `RefSource::Internal` is chosen.
    pub ref_sel: RefSource,

    /// Magnitude of the current out of IDAC1 and IDAC2
    pub idac_mag: IDACMagnitude,

    /// Pin connected to IDAC1
    pub idac1_out: Option<AnalogPin>,

    /// Pin connected to IDAC2
    pub idac2_out: Option<AnalogPin>,
}

impl RefConfig {
    /// IDACMAG register value
    #[inline]
    pub(super) fn reg_idacmag(&self) -> u8 {
        register::IDACMAG::FL_RAIL_EN::Enable as u8
            | register::IDACMAG::PSW::Open as u8
            | self.idac_mag as u8
    }

    /// REF register
    #[inline]
    pub(super) fn reg_ref(&self) -> u8 {
        let ref_buf_enabled = match self.ref_sel {
            register::REF::REFSEL::Internal => {
                register::REF::REFP_BUF::Disabled as u8 | register::REF::REFN_BUF::Disabled as u8
            }
            _ => register::REF::REFP_BUF::Enabled as u8 | register::REF::REFN_BUF::Enabled as u8,
        };

        register::REF::FL_REF_EN::L0AndL1 as u8
            | ref_buf_enabled
            | self.ref_sel as u8
            | register::REF::REFCON::On as u8
    }

    // IDACMUX register
    #[inline]
    pub(super) fn reg_idacmux(&self) -> u8 {
        let idac1 = match self.idac1_out {
            Some(output) => register::IDACMUX::I1MUX::Connect(output),
            None => register::IDACMUX::I1MUX::Disconnect,
        };
        let idac2 = match self.idac2_out {
            Some(output) => register::IDACMUX::I2MUX::Connect(output),
            None => register::IDACMUX::I2MUX::Disconnect,
        };

        idac1.as_u8() | idac2.as_u8()
    }
}

impl Default for RefConfig {
    fn default() -> Self {
        Self {
            ref_sel: RefSource::Internal,
            idac_mag: IDACMagnitude::Off,
            idac1_out: None,
            idac2_out: None,
        }
    }
}

/// Config related to the input being sampled
pub struct InputConfig {
    /// ADC input pin (positive)
    pub in_p: AnalogPin,

    /// ADC input pin (negative)
    pub in_n: AnalogPin,

    /// Delay before first conversion
    pub delay: Delay,

    /// PGA Gain. None = bypass
    pub pga: Option<Gain>,

    /// DataRate: trade-off between conversion time and noise rejection
    pub datarate: DataRate,
}

impl InputConfig {
    pub(super) fn reg_inpmux(&self) -> u8 {
        register::INPMUX::MUXP(self.in_p).as_u8() | register::INPMUX::MUXN(self.in_n).as_u8()
    }

    pub(super) fn reg_pga(&self) -> u8 {
        self.delay as u8
            | match self.pga {
                None => register::PGA::PGA_EN::Disable as u8 | register::PGA::GAIN::G1 as u8,
                Some(gain) => register::PGA::PGA_EN::Enable as u8 | gain as u8,
            }
    }

    pub(super) fn reg_datarate(&self) -> u8 {
        register::DATARATE::G_CHOP::Disable as u8
            | register::DATARATE::CLK::Internal as u8
            | register::DATARATE::MODE::SingleShot as u8
            | register::DATARATE::FILTER::LowLatency as u8
            | self.datarate as u8
    }
}

pub enum BurnoutSource {
    /// Burn-out current sources enabled, 0.2-µA setting
    Burnout0_2,

    /// Burn-out current sources enabled, 1-µA setting
    Burnout1,

    /// Burn-out current sources enabled, 10-µA setting
    Burnout10,

    /// Burn-out current sources disabled
    BurnoutDisabled,
}

impl BurnoutSource {
    pub(super) fn reg_sys_mon(&self) -> u8 {
        match self {
            BurnoutSource::Burnout0_2 => register::SYS::SYS_MON::Burnout0_2 as u8,
            BurnoutSource::Burnout1 => register::SYS::SYS_MON::Burnout1 as u8,
            BurnoutSource::Burnout10 => register::SYS::SYS_MON::Burnout10 as u8,
            BurnoutSource::BurnoutDisabled => register::SYS::SYS_MON::Disabled as u8,
        }
    }
}
