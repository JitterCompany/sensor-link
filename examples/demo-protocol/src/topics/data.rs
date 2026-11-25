pub struct Data<const MAX_N_SAMPLES: usize = 100> {
    /// Timestamps (microseconds)
    pub t: heapless::Vec<i64, MAX_N_SAMPLES>,

    /// Sampling frequency (Hz)
    pub fs: f32,

    pub board_temperature: heapless::Vec<f32, MAX_N_SAMPLES>,
    pub vbus: heapless::Vec<f32, MAX_N_SAMPLES>,
    pub ibus: heapless::Vec<f32, MAX_N_SAMPLES>,
    pub vsys: heapless::Vec<f32, MAX_N_SAMPLES>,
}

#[cfg(feature = "use-std")]
pub mod std {
    pub struct Data {
        /// Timestamps (microseconds)
        pub t: Vec<i64>,
        /// Sampling frequency (Hz)
        pub fs: f32,
        pub board_temperature: Vec<f32>,
        pub vbus: Vec<f32>,
        pub ibus: Vec<f32>,
        pub vsys: Vec<f32>,
    }

    impl From<super::Data> for Data {
        fn from(data: super::Data) -> Self {
            let super::Data {
                t,
                fs,
                board_temperature,
                vbus,
                ibus,
                vsys,
            } = data;
            Self {
                t: t.to_vec(),
                fs,
                board_temperature: board_temperature.to_vec(),
                vbus: vbus.to_vec(),
                ibus: ibus.to_vec(),
                vsys: vsys.to_vec(),
            }
        }
    }
}
