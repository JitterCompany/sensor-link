use crate::{mqtt::ProcessingMessage, MeasurementDataStore};
use demo_protocol::data::std::Data;
use std::sync::Arc;
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::{mpsc, Mutex};

pub enum DemoProcessingIn {
    Common(sensor_link_mqtt::ProcessingIn),
    Data(Data),
}

impl From<sensor_link_mqtt::ProcessingIn> for DemoProcessingIn {
    fn from(value: sensor_link_mqtt::ProcessingIn) -> Self {
        DemoProcessingIn::Common(value)
    }
}

pub fn start_task<DB: MeasurementDataStore + Clone + 'static>(
    rx_from_mqtt: mpsc::Receiver<ProcessingMessage>,
    db: DB,
    on_panic: PanicCallback,
) -> Handle {
    let task_function = processing_task;
    let rx_from_mqtt = Arc::new(Mutex::new(rx_from_mqtt));
    Handle::new(
        move |_| task_function(rx_from_mqtt.clone(), db.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn processing_task<DB: MeasurementDataStore>(
    rx_from_mqtt: Arc<Mutex<mpsc::Receiver<ProcessingMessage>>>,
    db: DB,
) {
    while let Some(msg) = rx_from_mqtt.lock().await.recv().await {
        let ProcessingMessage {
            datatype,
            device_id,
            data,
        } = msg;
        match data {
            DemoProcessingIn::Data(d) => {
                if let Err(err) = db.insert_measurement_data(&device_id, &d).await {
                    tracing::error!("Failed to store measurement data from {device_id}: {err}");
                }
            }
            _ => {
                tracing::info!("Received unknown data type from {device_id} ({datatype})");
            }
        }
    }
    tracing::info!("Data logging task stopped");
}
