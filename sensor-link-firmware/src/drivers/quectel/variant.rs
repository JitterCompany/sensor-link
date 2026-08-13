//! Modem variant abstraction: model-specific power sequencing and settings.

use embedded_hal::digital::{InputPin, OutputPin, StatefulOutputPin};

use crate::monotonic_time::delay_ms;

/// Model-specific behavior of a Quectel modem: power control pins and
/// their sequencing, plus a few per-model constants.
pub trait ModemVariant {
    /// Lowercase model prefix as reported by AT+CGMM (e.g. "ec2", "eg915").
    const MODEL_PREFIX: &'static str;

    /// `Some(rate)`: negotiate to this baudrate via AT+IPR and persist with AT&W.
    /// `None`: stay at the modem's default baudrate, never send AT+IPR.
    const TARGET_BAUDRATE: Option<u32>;

    /// Best-effort check whether the modem is already powered.
    fn is_powered(&mut self) -> bool;

    /// Power-on pin sequence including settling delays.
    /// The driver resumes the UART and awaits the RDY URC afterwards.
    async fn power_on(&mut self);

    /// Power-off pin sequence; called after AT+QPOWD completed and the
    /// UART is suspended.
    fn power_off(&mut self);
}

/// EC21/EC25 on the miniPCIe carrier: supply switch + PERST# only.
///
/// `reset`: high = PERST# released (module runs).
pub struct MiniPcie<EP: StatefulOutputPin, RP: OutputPin> {
    enable: EP,
    reset: RP,
}

impl<EP: StatefulOutputPin, RP: OutputPin> MiniPcie<EP, RP> {
    pub fn new(enable: EP, reset: RP) -> Self {
        Self { enable, reset }
    }
}

impl<EP: StatefulOutputPin, RP: OutputPin> ModemVariant for MiniPcie<EP, RP> {
    const MODEL_PREFIX: &'static str = "ec2";
    const TARGET_BAUDRATE: Option<u32> = Some(3_000_000);

    fn is_powered(&mut self) -> bool {
        self.enable.is_set_high().unwrap_or(false)
    }

    async fn power_on(&mut self) {
        self.enable.set_high().ok();
        // Note: reset pulse is not necessary for miniPCIe version. Just keep the !reset pin high
        self.reset.set_high().ok();

        // PSU settling delay before the driver resumes the UART:
        // the modem must be powered before TX goes high.
        delay_ms(2).await;
    }

    fn power_off(&mut self) {
        self.enable.set_low().ok();
        self.reset.set_low().ok();
    }
}

/// EG915N with discrete PWRKEY/RESET/STATUS control.
///
/// `pwrkey` and `reset` are expected to assert (pull the modem pin low)
/// when the MCU pin is driven high, e.g. via an NPN to ground.
pub struct Eg915<EP: StatefulOutputPin, PK: OutputPin, RP: OutputPin, SP: InputPin> {
    enable: EP,
    pwrkey: PK,
    reset: RP,
    status: SP,
}

impl<EP: StatefulOutputPin, PK: OutputPin, RP: OutputPin, SP: InputPin> Eg915<EP, PK, RP, SP> {
    pub fn new(enable: EP, pwrkey: PK, reset: RP, status: SP) -> Self {
        Self {
            enable,
            pwrkey,
            reset,
            status,
        }
    }
}

impl<EP: StatefulOutputPin, PK: OutputPin, RP: OutputPin, SP: InputPin> ModemVariant
    for Eg915<EP, PK, RP, SP>
{
    const MODEL_PREFIX: &'static str = "eg915";
    const TARGET_BAUDRATE: Option<u32> = None;

    fn is_powered(&mut self) -> bool {
        self.enable.is_set_high().unwrap_or(false)
    }

    async fn power_on(&mut self) {
        // Both released before the supply comes up
        self.reset.set_low().ok();
        self.pwrkey.set_low().ok();

        self.enable.set_high().ok();
        // VBAT must be stable >= 30 ms before asserting PWRKEY
        delay_ms(30).await;

        // PWRKEY low (asserted) >= 500 ms turns the module on
        self.pwrkey.set_high().ok();
        delay_ms(600).await;
        self.pwrkey.set_low().ok();

        // Diagnostic only: STATUS is not used to gate the power-on, the
        // driver awaits the RDY URC instead. Note: on the 5101 board STATUS
        // reads low while the modem is on (inverting level shifter).
        log::debug!(target: "quectel", "STATUS pin high: {:?}", self.status.is_high());
    }

    fn power_off(&mut self) {
        self.pwrkey.set_low().ok();
        self.enable.set_low().ok();
        self.reset.set_low().ok();
    }
}
