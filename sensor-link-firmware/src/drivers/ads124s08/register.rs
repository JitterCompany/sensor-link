#![allow(non_camel_case_types, non_snake_case)]

#[allow(dead_code)]
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Register {
    ID = 0x00,
    STATUS = 0x01,

    // Written via ADS124S08::configure_input() or ADS124S08::read_and_reconfigure_input()
    INPMUX = 0x02,
    PGA = 0x03,
    DATARATE = 0x04,

    // Written via ADS124S08::configure_ref()
    REF = 0x05,
    IDACMAG = 0x06,
    IDACMUX = 0x07,

    // Initialized by ADS124S08::enable()
    VBIAS = 0x08,
    SYS = 0x09,
    OFCAL0 = 0x0A,
    OFCAL1 = 0x0B,
    OFCAL2 = 0x0C,
    FSCAL0 = 0x0D,
    FSCAL1 = 0x0E,
    FSCAL2 = 0x0F,
    GPIODAT = 0x10,
    GPIOCON = 0x11,
}

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum STATUS {
    /// POR flag
    ///
    /// Indicates a power-on reset (POR) event has occurred
    FL_POR = 1 << 7,

    /// Device ready flag
    ///
    /// Zero if the device has started up and is ready for communication
    NOTRDY = 1 << 6,

    /// Positive PGA output at positive rail flag
    ///
    /// Indicates the positive PGA output is within 150 mV of AVDD
    FL_P_RAILP = 1 << 5,

    /// Positive PGA output at negative rail flag
    ///
    /// Indicates the positive PGA output is within 150 mV of AVSS
    FL_P_RAILN = 1 << 4,

    /// Negative PGA output at positive rail flag
    ///
    /// Indicates the negative PGA output is within 150 mV of AVDD
    FL_N_RAILP = 1 << 3,

    /// Negative PGA output at negative rail flag
    ///
    /// Indicates the negative PGA output is within 150 mV of AVSS
    FL_N_RAILN = 1 << 2,

    /// Reference voltage monitor flag, level 1
    ///
    /// Indicates the external reference voltage is lower than 1/3 of the analog
    /// supply voltage.
    /// NB: whether this is an error or not depends on the application.
    FL_REF_L1 = 1 << 1,

    /// Reference voltage monitor flag, level 0
    ///
    /// Indicates the external reference voltage is lower than 0.3V of the analog
    /// supply voltage.
    /// NB: whether this is an error or not depends on the application.
    FL_REF_L0 = 1,
}

/// Input Multiplexer
pub mod INPMUX {

    #[derive(Debug, Clone)]
    /// Positive ADC input selection
    pub struct MUXP(pub super::AnalogPin);
    impl MUXP {
        pub fn as_u8(self) -> u8 {
            (self.0 as u8) << 4
        }
    }

    #[derive(Debug, Clone)]
    /// Negative ADC input selection
    pub struct MUXN(pub super::AnalogPin);
    impl MUXN {
        pub fn as_u8(self) -> u8 {
            self.0 as u8
        }
    }
}

/// Gain Setting Register
pub mod PGA {
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone, Copy)]
    /// Programmable conversion delay selection
    ///
    /// Sets the programmable conversion delay time for the first conversion after a WREG
    /// when a configuration change resets of the digital filter and triggers a new conversion
    pub enum DELAY {
        /// 1 * t_mod = 3.9μs
        TMod1 = 7 << 5,

        /// 14 * t_mod = 54μs
        TMod14 = 0,

        /// 25 * t_mod = 97μs
        TMod25 = 1 << 5,

        /// 64 * t_mod = 250μs
        TMod64 = 2 << 5,

        /// 256 * t_mod = 1ms
        TMod256 = 3 << 5,

        /// 1024 * t_mod = 4ms
        TMod1024 = 4 << 5,

        /// 2048 * t_mod = 8ms
        TMod2048 = 5 << 5,

        /// 4096 * t_mod = 16ms
        TMod4096 = 6 << 5,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// PGA enable
    pub enum PGA_EN {
        /// PGA powered down and bypassed
        ///
        /// Note: if disabled, set GAIN to 1
        Disable = 0,

        /// PGA enabled
        Enable = 1 << 3,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone, Copy)]
    /// PGA gain selection
    pub enum GAIN {
        G1,
        G2,
        G4,
        G8,
        G16,
        G32,
        G64,
        G128,
    }

    impl GAIN {
        /// Returns the gain value as an integer multiplier
        ///
        /// Example: `assert_eq!(GAIN::G8.as_multiplier(), 8);`
        pub const fn as_multiplier(&self) -> i32 {
            match self {
                GAIN::G1 => 1,
                GAIN::G2 => 2,
                GAIN::G4 => 4,
                GAIN::G8 => 8,
                GAIN::G16 => 16,
                GAIN::G32 => 32,
                GAIN::G64 => 64,
                GAIN::G128 => 128,
            }
        }
    }
}

/// Data Rate Register
pub mod DATARATE {
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Global chop enable
    ///
    /// Enables the global chop function. When enabled, the device automatically
    /// swaps the inputs and takes the average of two consecutive readings to
    /// cancel the offset voltage.
    pub enum G_CHOP {
        Disable = 0,
        Enable = 1 << 7,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Clock source selection
    pub enum CLK {
        /// Internal 4.096-MHz oscillator
        Internal = 0,

        /// External clock
        External = 1 << 6,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Conversion mode selection
    pub enum MODE {
        Continuous = 0,
        SingleShot = 1 << 5,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Digital filter selection
    pub enum FILTER {
        Sinc3 = 0,
        LowLatency = 1 << 4,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone, Copy)]
    /// Data rate selection
    pub enum DR {
        /// 2.5 SPS: f_co = 1.1Hz, 50/60Hz rejection > 92dB (LowLatency filter)
        FS_2_5 = 0,

        /// 5 SPS: f_co = 2.1Hz, 50/60Hz rejection > 81dB  (LowLatency filter)
        FS_5,

        /// 10 SPS: f_co = 4.1Hz, 50/60Hz rejection > 81dB  (LowLatency filter)
        FS_10,

        /// 16.666 SPS: f_co = 7.4Hz, 50/60Hz rejection > 20dB  (LowLatency filter)
        FS_16_7,

        /// 20 SPS: f_co = 13.2Hz, 50/60Hz rejection > 80dB  (LowLatency filter)
        FS_20,

        /// 50 SPS: f_co = 22.1Hz, 50/60Hz rejection > 15dB  (LowLatency filter)
        FS_50,

        /// 60 SPS: f_co = 26.6Hz, 50/60Hz rejection > 12dB  (LowLatency filter)
        FS_60,

        /// 100 SPS: f_co = 44.4Hz, (LowLatency filter)
        FS_100,

        /// 200 SPS: f_co = 89.9Hz, (LowLatency filter)
        FS_200,

        /// 400 SPS: f_co = 190Hz, (LowLatency filter)
        FS_400,

        /// 800 SPS: f_co = 574Hz, (LowLatency filter)
        FS_800,

        /// 1k SPS: f_co = 718Hz, (LowLatency filter)
        FS_1_000,

        /// 2k SPS: f_co = 718Hz, (LowLatency filter)
        FS_2_000,

        /// 4k SPS: f_co = 718Hz, (LowLatency filter)
        FS_4_000,
    }
}

/// Reference Control Register
pub mod REF {

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Reference monitor configuration
    pub enum FL_REF_EN {
        /// Disable monitoring of reference voltage
        Disabled = 0,

        /// FL_REF_L0 monitor enabled, threshold 0.3 V
        L0 = 1 << 6,

        ///  FL_REF_L0 and FL_REF_L1 monitors enabled, thresholds 0.3 V and 1/3 · (AVDD – AVSS)
        L0AndL1 = 2 << 6,

        /// FL_REF_L0 monitor and 10-MΩ pull-together enabled, threshold 0.3 V
        L0With10Meg = 3 << 6,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Positive reference buffer bypass
    ///
    /// Disables the positive reference buffer. Recommended when V(REFPx) is close to AVDD
    pub enum REFP_BUF {
        /// Bypass enabled
        Enabled = 0,

        /// Bypass disabled
        Disabled = 1 << 5,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Negative reference buffer bypass
    ///
    /// Disables the negative reference buffer. Recommended when V(REFNx) is close to AVSS
    pub enum REFN_BUF {
        /// Bypass enabled
        Enabled = 0,

        /// Bypass disabled
        Disabled = 1 << 4,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone, Copy)]
    /// Reference input selection
    pub enum REFSEL {
        /// REFP0, REFN0
        Ref0 = 0,

        /// REFP1, REFN1
        Ref1 = 1 << 2,

        /// Internal reference
        /// Note: [REFN_BUF] and [REFP_BUF] must be disabled
        Internal = 2 << 2,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Internal voltage reference configuration
    ///
    /// Note: must be turned on to use the IDACs
    pub enum REFCON {
        /// Internal voltage reference off
        Off = 0,

        /// Internal voltage reference on, but powers down in power-down mode
        OnExceptPowerdown = 1,

        /// Internal voltage reference always on, even in power-down mode
        On = 2,
    }
}

/// Excitation Current Register 1
pub mod IDACMAG {

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone, Copy)]
    /// PGA output rail flag enable
    ///
    /// Enables the PGA output voltage rail monitor circuit.
    /// four flags that trip when either PGA output goes above AVDD - 0.15V
    /// or below AVSS + 0.15V (datasheet 9.3.10.3).
    /// The monitors are active even when the PGA is bypassed.
    pub enum FL_RAIL_EN {
        Disable = 0,
        Enable = 1 << 7,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    /// Low-side power switch
    ///
    /// Controls the low-side power switch. The low-side power switch opens automatically in power-down mode.
    pub enum PSW {
        Open = 0,
        Closed = 1 << 6,
    }

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone, Copy)]
    /// IDAC magnitude selection
    ///
    /// Selects the value of the excitation current sources. Sets IDAC1 and IDAC2 to the same value.
    pub enum IMAG {
        Off = 0,
        I_10μA = 1,
        I_50μA = 2,
        I_100μA = 3,
        I_250μA = 4,
        I_500μA = 5,
        I_750μA = 6,
        I_1000μA = 7,
        I_1500μA = 8,
        I_2000μA = 9,
    }
}

/// Excitation Current Register 2
pub mod IDACMUX {

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    /// IDAC2 output channel selection
    pub enum I2MUX {
        Disconnect,
        Connect(super::AnalogPin),
    }
    impl I2MUX {
        pub fn as_u8(self) -> u8 {
            match self {
                I2MUX::Disconnect => 0b1111 << 4,
                I2MUX::Connect(pin) => (pin as u8) << 4,
            }
        }
    }
    pub enum I1MUX {
        Disconnect,
        Connect(super::AnalogPin),
    }
    impl I1MUX {
        pub fn as_u8(self) -> u8 {
            match self {
                I1MUX::Disconnect => 0b1111,
                I1MUX::Connect(pin) => pin as u8,
            }
        }
    }
}

/// System Control Register
pub mod SYS {

    /// System monitor configuration
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    pub enum SYS_MON {
        // System monitor disabled
        Disabled = 0,

        /// PGA inputs shorted to (AVDD + AVSS) / 2 and disconnected from AINx and the multiplexer; gain set by user
        PGAShortMidscale = 1 << 5,

        /// Internal temperature sensor measurement; PGA must be enabled
        InternalTemperature = 2 << 5,

        /// (AVDD – AVSS) / 4 measurement; gain set to 1
        SupplyAnalogDiv4 = 3 << 5,

        /// DVDD / 4 measurement; gain set to 1
        SupplyDigitalDiv4 = 4 << 5,

        /// Burn-out current sources enabled, 0.2-µA setting
        Burnout0_2 = 5 << 5,

        /// Burn-out current sources enabled, 1-µA setting
        Burnout1 = 6 << 5,

        /// Burn-out current sources enabled, 10-µA setting
        Burnout10 = 7 << 5,
    }

    /// Calibration sample size selection
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    pub enum CAL_SAMP {
        /// No averaging (1 sample)
        NoAvg = 0 << 3,

        /// Average 4 samples
        Avg4 = 1 << 3,

        /// Average 8 samples (default)
        Avg8 = 2 << 3,

        /// Average 16 samples
        Avg16 = 3 << 3,
    }

    /// SPI timeout enable
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    pub enum TIMEOUT {
        Disabled = 0,
        Enabled = 1 << 2,
    }

    /// CRC enable
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    pub enum CRC {
        Disabled = 0,
        Enabled = 1 << 1,
    }

    /// STATUS byte enable
    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    pub enum SENDSTAT {
        Disabled = 0,
        Enabled = 1,
    }
}

/// GPIO Configuration Register (
pub mod GPIOCON {

    #[allow(dead_code)]
    #[repr(u8)]
    #[derive(Debug, Clone)]
    pub enum CON {
        /// All GPIO-capable pins used as analog inputs
        AllAnalog = 0,

        /// Configure GPIO1 as digital pin
        DigitalGPIO1 = 1,

        /// Configure GPIO2 as digital pin
        DigitalGPIO2 = 1 << 1,

        /// Configure GPIO3 as digital pin
        DigitalGPIO3 = 1 << 2,

        /// Configure GPIO4 as digital pin
        DigitalGPIO4 = 1 << 3,
    }
}

/// Analog pin on the ADS124S08
///
/// All of these can be used for analog input measurement
/// and/or as IDAC outputs.
#[allow(dead_code)]
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum AnalogPin {
    AIN0,
    AIN1,
    AIN2,
    AIN3,
    AIN4,
    AIN5,

    // Note: AIN6 is also REF1P
    AIN6,

    // Note: AIN7 is also REF1N
    AIN7,
    AIN8,
    AIN9,
    AIN10,
    AIN11,
    AINCOM,
}
