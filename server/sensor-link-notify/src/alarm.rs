use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sensor_link_server_core::{
    event::{Event, EventQuery, EventQueryParams, EventType, SendStatus},
    store_traits::EventStore,
    TimeRange,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::{
    sync::{mpsc, watch::Receiver},
    time::{sleep, timeout},
};

use crate::{mail::Email, sms::SMS, EventCodes};

pub const SMS_RETRY_AFTER_SEC: u64 = 120;
const SMS_RETRY_WINDOW_SEC: u64 = 300;

#[derive(Debug, Deserialize, Serialize)]
pub struct ContactDetails {
    pub id: String,
    pub name: String,
    pub phonenumber: String,
    pub email: String,
}

impl ContactDetails {
    /// Returns the email addresses in the format "name \<email\>"
    pub fn formatted_email(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }
}

#[async_trait]
pub trait NotificationMessageBuilder<D: Serialize + DeserializeOwned + Send + Sync> {
    async fn build_email(
        &self,
        event: &Event<ContactDetails, D>,
        contact_details: &ContactDetails,
    ) -> anyhow::Result<Option<Email>>;

    async fn build_sms(
        &self,
        event: &Event<ContactDetails, D>,
        contact_details: &ContactDetails,
    ) -> anyhow::Result<Option<SMS>>;
}

pub fn start_task<
    DS: EventStore<ContactData = ContactDetails> + Clone,
    NMB: NotificationMessageBuilder<DS::EventData> + Clone + Send + Sync + 'static,
>(
    db: DS,
    mail_sender: mpsc::Sender<Email>,
    sms_sender: mpsc::Sender<SMS>,
    notification_builder: NMB,
    on_panic: PanicCallback,
) -> Handle
where
    DS::EventData: Serialize + DeserializeOwned + Send + Sync,
{
    let task_function = alarm_task;
    let mail_sender_clone = mail_sender.clone();
    Handle::new(
        move |shutdown_rx| {
            task_function(
                shutdown_rx,
                db.clone(),
                mail_sender_clone.clone(),
                sms_sender.clone(),
                notification_builder.clone(),
            )
        },
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

pub async fn alarm_task<
    DS: EventStore<ContactData = ContactDetails> + Clone,
    NMB: NotificationMessageBuilder<DS::EventData> + Sync,
>(
    mut shutdown_rx: Receiver<()>,
    db: DS,
    mail_sender: mpsc::Sender<Email>,
    sms_sender: mpsc::Sender<SMS>,
    notification_builder: NMB,
) where
    DS::EventData: Serialize + DeserializeOwned + Send + Sync,
{
    // Start processing unsent alarms that occured some time before the task (=server) starts.
    // No need to send alarms for very old events (even if they have somehow never been sent before)
    // as they are likely old news by now..
    const START_OFFSET_SEC: u64 = SMS_RETRY_WINDOW_SEC;
    const POLL_INTERVAL: Duration = Duration::from_secs(5);
    let alarms_processed_untill = Utc::now() - Duration::from_secs(START_OFFSET_SEC);

    loop {
        match process_alarms(
            &db,
            &mail_sender,
            &sms_sender,
            &notification_builder,
            alarms_processed_untill,
        )
        .await
        {
            Err(error) => {
                tracing::error!("Failed to process alarm events: {:?}", error);
                // If an error occurs it is likely the next attempt also fails, so don't retry too fast
                sleep(Duration::from_secs(60)).await;
            }
            Ok(n_events) => {
                if n_events > 0 {
                    tracing::debug!("Processed {} alarm events", n_events);
                }
            }
        }

        match timeout(POLL_INTERVAL, shutdown_rx.changed()).await {
            Ok(_) => {
                // shutdown received, break out of loop
                break;
            }
            Err(_) => {
                // timeout done, loop again
            }
        }
    }
}

pub async fn process_alarms<
    DS: EventStore<ContactData = ContactDetails>,
    NMB: NotificationMessageBuilder<DS::EventData> + Sync,
>(
    db: &DS,
    mail_sender: &mpsc::Sender<Email>,
    sms_sender: &mpsc::Sender<SMS>,
    notification_builder: &NMB,
    alarms_processed_untill: DateTime<Utc>,
) -> anyhow::Result<usize>
where
    DS::EventData: Serialize + DeserializeOwned + Send + Sync,
{
    let mut events = db
        .query_events(EventQuery {
            params: EventQueryParams {
                time_range: Some(TimeRange {
                    from: alarms_processed_untill,
                    until: Utc::now(),
                }),
                types: Some(vec![EventType::email, EventType::sms]),
                sent: Some(SendStatus::NotSent),
                has_contact_details: Some(true),
                ..Default::default()
            },
            code: Some(vec![EventCodes::Email as u32, EventCodes::AlarmSMS as u32]),
            sort: 1,
            limit: Some(1024),
            ..Default::default()
        })
        .await?
        .events;

    // Query events with status sending that have been timed out
    let events_retry = db
        .query_events(EventQuery {
            params: EventQueryParams {
                time_range: Some(TimeRange {
                    from: alarms_processed_untill - Duration::from_secs(SMS_RETRY_WINDOW_SEC),
                    until: alarms_processed_untill - Duration::from_secs(SMS_RETRY_AFTER_SEC),
                }),
                types: Some(vec![EventType::sms]),
                sent: Some(SendStatus::Sending),
                has_contact_details: Some(true),
                ..Default::default()
            },
            code: Some(vec![EventCodes::AlarmSMS as u32]),
            sort: 1,
            limit: Some(1024),
            ..Default::default()
        })
        .await?
        .events;

    events.extend(events_retry);
    if !events.is_empty() {
        tracing::debug!("Got {} SBR alarm events to send..", events.len());
    }

    for event in &events {
        let Some(contact_details) = event.contact_details.as_ref() else {
            continue;
        };
        match event._type {
            EventType::email => {
                tracing::debug!("email alarm for configured contact: {:?}", contact_details);
                let Some(email) = notification_builder
                    .build_email(event, contact_details)
                    .await?
                else {
                    continue;
                };
                mail_sender.send(email).await?;
                db.event_mark_sent(&event.id, SendStatus::Sent).await?;
            }
            EventType::sms => {
                tracing::debug!("SMS alarm for configured contact: {:?}", contact_details);
                if let Err(err) = db.event_mark_sent(&event.id, SendStatus::Sending).await {
                    tracing::error!("Failed to mark event as sending: {}", err);
                }
                let Some(sms) = notification_builder
                    .build_sms(event, contact_details)
                    .await?
                else {
                    continue;
                };
                sms_sender.send(sms).await?;
            }
            _ => continue,
        }
    }

    Ok(events.len())
}
