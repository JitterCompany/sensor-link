//! Audible OK / FAIL feedback so the operator does not have to watch the screen.

use std::time::Duration;

use rodio::{MixerDeviceSink, Player, Source, source::SineWave};

pub struct Sounds {
    sink: Option<MixerDeviceSink>,
}

impl Sounds {
    pub fn new() -> Self {
        let sink = match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("no audio output: {e}");
                None
            }
        };
        Self { sink }
    }

    /// Two short rising tones.
    pub fn ok(&self) {
        self.play(&[(880.0, 120), (1320.0, 160)]);
    }

    /// Long low buzz.
    pub fn fail(&self) {
        self.play(&[(220.0, 450)]);
    }

    fn play(&self, notes: &[(f32, u64)]) {
        let Some(sink) = &self.sink else {
            return;
        };
        let player = Player::connect_new(sink.mixer());
        for (freq, ms) in notes {
            player.append(
                SineWave::new(*freq)
                    .take_duration(Duration::from_millis(*ms))
                    .amplify(0.25),
            );
        }
        player.detach();
    }
}
