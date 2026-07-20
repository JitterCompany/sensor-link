use std::{fmt::Display, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    ClientBuilder,
};
use sensor_link_mqtt::{ControlMessageOut, DeviceControlOut};
use sensor_link_protocol::sms::SMSRequest;
use sensor_link_server_core::{
    event::{EventType, SendStatus},
    store_traits::EventStore,
    DataStoreId,
};
use serde::Serialize;
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::{mpsc, Mutex};

use crate::{mail::Email, EventCodes};

#[derive(Clone, Debug)]
pub struct SMS {
    pub phone_numbers: Vec<String>,
    pub message: String,
    pub group_name: Option<String>,
    pub originator: String,
    pub event_id: DataStoreId,
    /// Timestamp of when the SMS object was created in the backend
    /// Used for retries and fallbacks
    pub created_at: DateTime<Utc>,
    pub use_jitter_gateway: bool,
}

impl From<SMS> for SMSRequest {
    fn from(val: SMS) -> Self {
        SMSRequest {
            id: val.event_id,
            phone_numbers: val.phone_numbers,
            message: val.message,
        }
    }
}

impl Display for SMS {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#?}", self)
    }
}

#[derive(Clone)]
pub struct Config {
    pub api_key: HeaderValue,
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigError {
    Missing,
    Invalid,
}

#[derive(Serialize)]
struct SMSPayload {
    encoding: String,
    body: String,
    route: String,
    originator: String,
    recipients: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
}

impl From<SMS> for SMSPayload {
    fn from(sms: SMS) -> Self {
        SMSPayload {
            encoding: "auto".to_string(),
            body: sms.message,
            route: "business".to_string(),
            originator: sms.originator,
            recipients: sms.phone_numbers,
            reference: sms.group_name,
        }
    }
}

pub fn start_task<DS, CO>(
    cfg: Option<Config>,
    rx: mpsc::Receiver<SMS>,
    tx_to_mqtt: mpsc::Sender<ControlMessageOut<CO>>,
    db: DS,
    mail_tx: mpsc::Sender<Email>,
    on_panic: PanicCallback,
) -> Handle
where
    DS: EventStore + Clone,
    CO: From<DeviceControlOut> + Send + 'static,
{
    let task_function = send_task;
    let rx = Arc::new(Mutex::new(rx));
    Handle::new(
        move |_| {
            task_function(
                cfg.clone(),
                rx.clone(),
                tx_to_mqtt.clone(),
                db.clone(),
                mail_tx.clone(),
            )
        },
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn send_task<DS, CO>(
    config: Option<Config>,
    sms_to_send: Arc<Mutex<mpsc::Receiver<SMS>>>,
    tx_to_mqtt: mpsc::Sender<ControlMessageOut<CO>>,
    db: DS,
    mail_tx: mpsc::Sender<Email>,
) where
    DS: EventStore,
    CO: From<DeviceControlOut> + Send + 'static,
{
    while let Some(sms) = sms_to_send
        .try_lock()
        .expect("SMS receiver channel seems to be locked by another task than SMS task")
        .recv()
        .await
    {
        let event_id = sms.event_id.clone();

        if sms.use_jitter_gateway {
            if Utc::now().signed_duration_since(sms.created_at) < Duration::seconds(15) {
                if let Err(err) = tx_to_mqtt.try_send(ControlMessageOut {
                    device_id: "".to_string(), // not used
                    payload: DeviceControlOut::SMSRequest(sms.clone().into()).into(),
                }) {
                    let error_msg =
                        format!("Error sending SMS request to Jitter SMS gateway: {}", err);
                    tracing::error!(error_msg);
                    send_jitter_fallback_mail(
                        &mail_tx,
                        &sms.phone_numbers,
                        &sms.originator,
                        sms.group_name.as_deref(),
                        &error_msg,
                    )
                    .await;
                } else {
                    tracing::info!("Sent SMS request to Jitter SMS gateway");
                    continue;
                }
            } else {
                let error_msg = format!(
                    "Failed to send SMS to {:?} via Jitter SMS gateway within 15 seconds of event",
                    sms.phone_numbers
                );
                tracing::error!(
                    sms = %sms,
                    error_msg
                );
                send_jitter_fallback_mail(
                    &mail_tx,
                    &sms.phone_numbers,
                    &sms.originator,
                    sms.group_name.as_deref(),
                    &error_msg,
                )
                .await;
            }
        }

        let config = match &config {
            Some(c) => c,
            None => {
                tracing::warn!(
                    "Not sending SMS to {:?} (no SMS gateway configured)",
                    sms.phone_numbers
                );
                if let Err(err) = db.event_mark_sent(&event_id, SendStatus::Failed).await {
                    tracing::error!("Failed to mark event as failed: {}", err);
                }
                continue;
            }
        };
        let url = "https://rest.spryngsms.com/v1/messages";
        let client = ClientBuilder::new()
            .use_rustls_tls()
            .build()
            .unwrap_or_default();
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, config.api_key.clone());
        let sms_request: SMSPayload = sms.into();

        tracing::debug!(
            "serialized sms_request: {:?}",
            serde_json::to_string(&sms_request)
        );

        match client
            .post(url)
            .headers(headers)
            .json(&sms_request)
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    tracing::info!(
                        "[Spryng] Successfully sent SMS to {:?}",
                        sms_request.recipients
                    );
                    if let Err(err) = db.event_mark_sent(&event_id, SendStatus::Sent).await {
                        tracing::error!("Failed to mark event as sent: {}", err);
                    }
                } else {
                    if let Err(err) = db.event_mark_sent(&event_id, SendStatus::Failed).await {
                        tracing::error!("Failed to mark event as Failed: {}", err);
                    }
                    tracing::error!(
                        "Error response for request to send SMS to {:?}: {:?}",
                        sms_request.recipients,
                        response.text().await
                    );
                    if let Err(err) = db
                        .event_update_code(
                            &event_id,
                            EventCodes::ErrorSendingSMS as u32,
                            EventType::warning,
                        )
                        .await
                    {
                        tracing::error!("Failed to update event code: {}", err);
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send request to send SMS to {:?}: {e}",
                    sms_request.recipients
                );
                tracing::debug!("Detailed error for request to send SMS: {e:?}");
                if let Err(err) = db
                    .event_update_code(
                        &event_id,
                        EventCodes::ErrorSendingSMS as u32,
                        EventType::warning,
                    )
                    .await
                {
                    tracing::error!("Failed to update event code: {}", err);
                }
            }
        }
    }
    tracing::info!("Exit SMS task");
}

//TODO: use a callback instead or define SMS task output (enum) that is fed into MPSCs by the application's task handler
async fn send_jitter_fallback_mail(
    tx: &mpsc::Sender<Email>,
    phone_numbers: &[String],
    server_name: &str,
    group_name: Option<&str>,
    reason: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let subject = format!("Spryng SMS fallback used on {server_name}");
    let group = group_name.unwrap_or("(unknown)");
    let msg = format!(
        "An SMS was sent via the Spryng API (billable) on {server_name} because the Jitter SMS gateway failed.\n\n\
        ---\n\
        Time: {now}\n\
        Group: {group}\n\
        Recipients: {phone_numbers:?}\n\
        Reason: {reason}\n\
        ---\n"
    );
    if let Err(err) = tx.try_send(Email::new(
        vec!["monitoring@jitter.company"],
        subject,
        msg,
        "",
    )) {
        tracing::error!("Failed sending monitoring notification email to Jitter about Jitter SMS gateway failure. Error: {err}.")
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn test_serialize_sms_request() {
        std::env::remove_var("ENV_NAME");

        let sms = SMS {
            phone_numbers: vec!["+31612345678".to_string()],
            message: "Hello, world!".to_string(),
            group_name: None,
            originator: "Frogwatch".to_string(),
            event_id: "".to_string(),
            created_at: Utc::now(),
            use_jitter_gateway: false,
        };
        let sms_request: SMSPayload = sms.into();
        let json = serde_json::to_string(&sms_request).unwrap();

        println!("{}", json);

        assert_eq!(
            json,
            r#"{"encoding":"auto","body":"Hello, world!","route":"business","originator":"Frogwatch","recipients":["+31612345678"]}"#
        );

        let sms = SMS {
            phone_numbers: vec!["+31612345678".to_string()],
            message: "Hello, world!".to_string(),
            group_name: Some("Group 1".to_string()),
            originator: "Frogwatch".to_string(),
            event_id: "".to_string(),
            created_at: Utc::now(),
            use_jitter_gateway: false,
        };
        let sms_request: SMSPayload = sms.into();
        let json = serde_json::to_string(&sms_request).unwrap();

        println!("{}", json);

        assert_eq!(
            json,
            r#"{"encoding":"auto","body":"Hello, world!","route":"business","originator":"Frogwatch","recipients":["+31612345678"],"reference":"Group 1"}"#
        );
    }
}
