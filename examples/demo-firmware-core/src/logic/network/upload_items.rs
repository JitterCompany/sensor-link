//! The demo device's upload items: events and sensor data
//!

use crate::{
    logic::{network::upload, upload::Upload},
    pool,
    storage::streams::MAX_SENSOR_DATA_LEN,
    Sendable, SerializedSendable,
};
use demo_protocol::{TopicFromDevice, MAX_EVENT_LEN};

pub type SerEvent = SerializedSendable<MAX_EVENT_LEN>;
pub type SerSensorData = SerializedSendable<MAX_SENSOR_DATA_LEN>;

impl<ER, DR> upload::UploadTrait for Upload<ER, DR>
where
    ER: pool::Ref<SerEvent> + Send,
    DR: pool::Ref<SerSensorData> + Send,
{
    type Topic = TopicFromDevice;

    fn sendable(&self) -> &Sendable {
        match self {
            Upload::Event(ev) => ev.as_ref(),
            Upload::SensorData(data) => data.as_ref(),
        }
    }
}
