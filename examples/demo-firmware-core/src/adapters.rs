use crate::logic::settings::Settings;
use demo_protocol::settings::Settings as ProtocolSettings;

impl From<ProtocolSettings> for Settings {
    fn from(s: ProtocolSettings) -> Self {
        let ProtocolSettings {
            data_rate,
            sync_interval_s,
            hash,
        } = s;
        Self {
            data_rate,
            sync_interval_s,
            hash,
        }
    }
}
