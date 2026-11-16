use demo_protocol::settings::Settings;

pub enum DemoControlOut {
    Common(sensor_link_mqtt::DeviceControlOut),
    Settings(Settings),
}

impl From<sensor_link_mqtt::DeviceControlOut> for DemoControlOut {
    fn from(value: sensor_link_mqtt::DeviceControlOut) -> Self {
        DemoControlOut::Common(value)
    }
}

/// Wraps a [sensor_link_mqtt::ControlMessageOut] carrying the generic
/// [sensor_link_mqtt::DeviceControlOut] payload into the demo-wide
/// [sensor_link_mqtt::ControlMessageOut]<[DemoControlOut]>.
///
/// This is a free function rather than a `From` impl because both
/// `sensor_link_mqtt::ControlMessageOut<_>` instantiations are foreign generic
/// types, which the orphan rules do not allow implementing a foreign trait
/// for even when the type argument is local.
pub fn wrap_control_message_out(
    value: sensor_link_mqtt::ControlMessageOut<sensor_link_mqtt::DeviceControlOut>,
) -> sensor_link_mqtt::ControlMessageOut<DemoControlOut> {
    sensor_link_mqtt::ControlMessageOut {
        device_id: value.device_id,
        payload: value.payload.into(),
    }
}

/// Converts a [sensor_link_server_core::status_handler::ControlMessageOut] (whose
/// payload is always [sensor_link_mqtt::DeviceControlOut]) into the demo-wide
/// [sensor_link_mqtt::ControlMessageOut]<[DemoControlOut]>.
pub fn from_status_handler_control_message_out(
    value: sensor_link_server_core::status_handler::ControlMessageOut,
) -> sensor_link_mqtt::ControlMessageOut<DemoControlOut> {
    wrap_control_message_out(value.into())
}
