use std::{collections::VecDeque, ops::ControlFlow, sync::Arc};

use lettre::{
    message::{header::ContentType, Attachment, Body, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::{Credentials, Mechanism},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use sensor_link_mqtt::metrics;
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::{
    sync::{mpsc, watch::Receiver, Mutex},
    time::Instant,
};

use crate::mail_throttle::{RateLimiter, ThrottleConfig};

/// Meter name for all email metrics.
const METER: &str = "mail";

/// Helper for metrics without attributes (an empty slice needs a concrete attribute type).
const NO_ATTRIBUTES: &[(&str, &str)] = &[];

/// Maximum number of non-urgent emails kept in the throttle queue. When the queue is full, the
/// oldest queued email is dropped.
const MAX_QUEUED_EMAILS: usize = 10_000;

#[derive(Debug, Clone)]
pub struct Email {
    recipients: Vec<String>,
    cc: Option<String>,
    subject: String,
    message: String,
    html: Option<String>,
    /// Path to a logo image attached inline in HTML emails. Empty string means no logo.
    logo_path: String,
    /// Urgent emails are sent as soon as possible. Non-urgent emails may be delayed to stay within
    /// the send limits of the mail server. See [`ThrottleConfig`].
    urgent: bool,
}

impl Email {
    /// Creates a new urgent email. All recipients will receive a separate email.
    pub fn new(
        recipients: Vec<impl Into<String>>,
        subject: impl Into<String>,
        message: impl Into<String>,
        logo_path: impl Into<String>,
    ) -> Self {
        Email {
            recipients: recipients.into_iter().map(|a| a.into()).collect(),
            cc: None,
            subject: subject.into(),
            message: message.into(),
            html: None,
            logo_path: logo_path.into(),
            urgent: true,
        }
    }

    pub fn with_cc(mut self, cc: impl Into<String>) -> Self {
        self.cc = Some(cc.into());
        self
    }

    pub fn with_html(mut self, html: impl Into<String>) -> Self {
        self.html = Some(html.into());
        self
    }

    /// Marks the email as non-urgent: it is queued and sent at a rate that keeps the mail server
    /// within its send limits. Use this for bulk email that is not time critical.
    pub fn non_urgent(mut self) -> Self {
        self.urgent = false;
        self
    }

    pub fn is_urgent(&self) -> bool {
        self.urgent
    }

    /// Number of separate emails that sending this email costs.
    pub fn send_count(&self) -> usize {
        self.recipients.len()
    }
}

#[derive(Clone)]
pub struct Config {
    pub from: Mailbox,
    pub smtp_server: String,
    pub smtp_username: String,
    pub smtp_password: String,
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigError {
    Missing,
    Invalid,
}

/// Feedback about email send attempts
#[derive(Debug, Clone)]
pub struct EmailSendFeedback {
    pub recipient: String,
    pub subject: String,
    pub status: EmailSendStatus,
    pub error: Option<String>,
}

/// Status of an email send attempt
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailSendStatus {
    Sent,
    Failed,
}

pub fn start_task(
    cfg: Option<Config>,
    throttle: ThrottleConfig,
    rx: mpsc::Receiver<Email>,
    on_panic: PanicCallback,
    feedback_tx: Option<mpsc::Sender<EmailSendFeedback>>,
) -> Handle {
    let task_function = send_task;
    let rx = Arc::new(Mutex::new(rx));
    Handle::new(
        move |shutdown_rx| {
            task_function(
                cfg.clone(),
                throttle,
                rx.clone(),
                shutdown_rx,
                feedback_tx.clone(),
            )
        },
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

/// A non-urgent email waiting for send capacity.
struct QueuedEmail {
    mail: Email,
    queued_at: Instant,
}

/// Statistics about the batch of non-urgent emails that is currently being sent.
///
/// A batch starts when an email is queued while no batch is in progress, and ends once the queue
/// has stayed empty for [`ThrottleConfig::batch_idle_time`].
struct Batch {
    started_at: Instant,
    last_sent_at: Instant,
    sent: u64,
}

async fn send_task(
    config: Option<Config>,
    throttle: ThrottleConfig,
    mails: Arc<Mutex<mpsc::Receiver<Email>>>,
    mut shutdown_rx: Receiver<()>,
    feedback_tx: Option<mpsc::Sender<EmailSendFeedback>>,
) {
    // Create the mailer once at startup if config is available
    let mailer: Option<AsyncSmtpTransport<Tokio1Executor>> = config.as_ref().and_then(|cfg| {
        match AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_server) {
            Ok(builder) => {
                let transport = builder
                    .credentials(Credentials::new(
                        cfg.smtp_username.to_owned(),
                        cfg.smtp_password.to_owned(),
                    ))
                    .authentication(vec![Mechanism::Plain])
                    .build();
                tracing::info!("SMTP transport initialized successfully");
                Some(transport)
            }
            Err(error) => {
                tracing::error!(
                    "Failed to build SMTPTransport at startup: SMTP server misconfigured? ({error:?})"
                );
                None
            }
        }
    });

    let mut limiter = RateLimiter::new(throttle);
    let mut queue: VecDeque<QueuedEmail> = VecDeque::new();
    let mut batch: Option<Batch> = None;
    let mut shutting_down = false;

    loop {
        let mails = &mut mails
            .try_lock()
            .expect("Email receiver chanel seems to be locked by another task then email task");

        if shutting_down && queue.is_empty() {
            break;
        }

        // A batch is done once the queue has stayed empty for a while, see `batch_idle_time`.
        let batch_done_at = batch
            .as_ref()
            .filter(|_| queue.is_empty())
            .map(|batch| batch.last_sent_at + throttle.batch_idle_time());

        // When may the email at the head of the queue be sent? `None` means the queue is empty,
        // `Some(None)` means it may be sent right away.
        let release_at = queue
            .front()
            .map(|queued| limiter.next_allowed(Instant::now(), queued.mail.send_count()));

        tokio::select! {
            // Prioritize processing emails over shutdown to ensure pending emails are sent
            biased;

            mail = mails.recv(), if !shutting_down => {
                match mail {
                    // All senders are gone: drain what is queued, then exit
                    None => shutting_down = true,

                    Some(mail) if mail.is_urgent() => {
                        limiter.record(Instant::now(), mail.send_count());
                        send_email(&config, &mailer, &mail, feedback_tx.as_ref()).await;
                    }

                    Some(mail) => {
                        if batch.is_none() {
                            batch = Some(Batch {
                                started_at: Instant::now(),
                                last_sent_at: Instant::now(),
                                sent: 0,
                            });
                        }
                        if queue.len() >= MAX_QUEUED_EMAILS {
                            if let Some(dropped) = queue.pop_front() {
                                tracing::error!(
                                    "Throttled e-mail queue is full ({MAX_QUEUED_EMAILS}): dropping oldest queued e-mail {:?}",
                                    dropped.mail.subject
                                );
                                report_dropped(feedback_tx.as_ref(), &dropped.mail, "queue_full");
                            }
                        }
                        queue.push_back(QueuedEmail { mail, queued_at: Instant::now() });
                        metrics::record_gauge(METER, "mail_throttle_queue_depth", queue.len() as u64, NO_ATTRIBUTES);
                    }
                }
            }

            _ = wait_until(release_at.flatten()), if release_at.is_some() => {
                let Some(queued) = queue.pop_front() else {
                    continue;
                };
                let now = Instant::now();

                limiter.record(now, queued.mail.send_count());
                metrics::record_histogram(
                    METER,
                    "mail_throttle_wait_seconds",
                    (now - queued.queued_at).as_secs_f64(),
                    NO_ATTRIBUTES,
                );
                metrics::record_gauge(METER, "mail_throttle_queue_depth", queue.len() as u64, NO_ATTRIBUTES);

                send_email(&config, &mailer, &queued.mail, feedback_tx.as_ref()).await;

                if let Some(batch) = batch.as_mut() {
                    batch.sent += 1;
                    batch.last_sent_at = Instant::now();
                }
            }

            _ = wait_until(batch_done_at), if batch_done_at.is_some() => {
                if let Some(batch) = batch.take() {
                    record_batch_metrics(batch);
                }
            }

            _ = shutdown_rx.changed() => {
                if !queue.is_empty() {
                    tracing::info!(
                        "Shutdown requested: draining {} throttled e-mail(s) first",
                        queue.len()
                    );
                }
                shutting_down = true;
            },
        }
    }

    if let Some(batch) = batch.take() {
        record_batch_metrics(batch);
    }
    tracing::info!("Exit mail task");
}

/// Waits until `deadline`, or returns immediately if there is none.
async fn wait_until(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

fn record_batch_metrics(batch: Batch) {
    let duration = (batch.last_sent_at - batch.started_at).as_secs_f64();
    tracing::info!(
        "Sent a batch of {} throttled e-mail(s) in {duration:.0}s",
        batch.sent
    );
    metrics::record_histogram(
        METER,
        "mail_throttle_batch_duration_seconds",
        duration,
        NO_ATTRIBUTES,
    );
    metrics::record_histogram(
        METER,
        "mail_throttle_batch_size",
        batch.sent as f64,
        NO_ATTRIBUTES,
    );
}

/// Sends an email to each of its recipients, if the mail server is configured.
async fn send_email(
    config: &Option<Config>,
    mailer: &Option<AsyncSmtpTransport<Tokio1Executor>>,
    mail: &Email,
    feedback_tx: Option<&mpsc::Sender<EmailSendFeedback>>,
) {
    let (config, transport) = match (config, mailer) {
        (Some(c), Some(t)) => (c, t),
        _ => {
            tracing::warn!(
                "Not sending e-mail '{}' to {:?} (no mail server configured or transport initialization failed)",
                mail.subject,
                mail.recipients
            );
            for recipient in &mail.recipients {
                report_send_result(
                    feedback_tx,
                    mail,
                    recipient,
                    EmailSendStatus::Failed,
                    Some("No mail server configured".to_string()),
                );
            }
            return;
        }
    };

    tracing::debug!("Trying to send e-mail to {:?} ...", mail.recipients);

    // Health check: test connection before sending
    // This helps detect stale connections (lettre issue #743)
    if let Err(e) = transport.test_connection().await {
        tracing::warn!(
            "SMTP connection health check failed: {e:?}. Will attempt to send anyway (retry logic will handle failures)."
        );
    }

    for recipient in &mail.recipients {
        if let ControlFlow::Break(_) =
            build_and_send_email(recipient, config, transport, mail, feedback_tx).await
        {
            continue;
        }
    }
}

/// Retry sending with exponential backoff
async fn send_with_retry<F, Fut>(
    mut send_fn: F,
    max_attempts: u32,
) -> Result<lettre::transport::smtp::response::Response, lettre::transport::smtp::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<
        Output = Result<
            lettre::transport::smtp::response::Response,
            lettre::transport::smtp::Error,
        >,
    >,
{
    let mut attempt = 0;
    loop {
        attempt += 1;
        match send_fn().await {
            Ok(response) => return Ok(response),
            Err(e) => {
                if attempt >= max_attempts {
                    tracing::error!("Failed to send email after {max_attempts} attempts: {e:?}");
                    return Err(e);
                }

                // Retry with exponential backoff: 1s, 2s, 4s, ...
                let delay_secs = 2u64.pow(attempt - 1);
                tracing::warn!(
                    "SMTP error on attempt {attempt}/{max_attempts}, retrying in {delay_secs}s: {e:?}"
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)).await;
            }
        }
    }
}

pub async fn build_and_send_email(
    recipient: &String,
    config: &Config,
    mailer: &AsyncSmtpTransport<Tokio1Executor>,
    mail: &Email,
    feedback_tx: Option<&mpsc::Sender<EmailSendFeedback>>,
) -> ControlFlow<()> {
    let to: Result<Mailbox, _> = recipient.parse();
    match to {
        Ok(to) => {
            let builder = Message::builder()
                .from(config.from.clone())
                .reply_to(config.from.clone())
                .to(to)
                .subject(&mail.subject);

            let email = match if let Some(html) = mail.html.as_ref() {
                let image = std::fs::read(&mail.logo_path).unwrap_or_else(|err| {
                    tracing::warn!(
                        "Failed to read image for email footer. Maybe path was empty. Using empty image instead. Error: {:?}",
                        err
                    );
                    Vec::new()
                });

                let Ok(content_type) = "image/png".parse() else {
                    tracing::error!("Failed to parse content type for email footer image");

                    report_send_result(
                        feedback_tx,
                        mail,
                        recipient,
                        EmailSendStatus::Failed,
                        Some("Failed to parse content type for email footer image".to_string()),
                    );

                    return ControlFlow::Break(());
                };
                builder.multipart(
                    MultiPart::alternative()
                        .singlepart(SinglePart::plain(mail.message.clone()))
                        .multipart(
                            MultiPart::related()
                                .singlepart(SinglePart::html(html.clone()))
                                .singlepart(
                                    Attachment::new_inline("footer_image@frogwatch".to_string())
                                        .body(Body::new(image), content_type),
                                ),
                        ),
                )
            } else {
                builder
                    .header(ContentType::TEXT_PLAIN)
                    .body(mail.message.clone())
            } {
                Ok(email) => email,
                Err(error) => {
                    tracing::error!("Failed to build email: {error:?}");

                    report_send_result(
                        feedback_tx,
                        mail,
                        recipient,
                        EmailSendStatus::Failed,
                        Some(format!("Failed to build email: {:?}", error)),
                    );

                    return ControlFlow::Break(());
                }
            };

            // Send the email with retry logic
            match send_with_retry(
                || {
                    let email_clone = email.clone();
                    async move { mailer.send(email_clone).await }
                },
                3, // max 3 attempts
            )
            .await
            {
                Ok(response) => {
                    tracing::info!(
                        "E-mail sent successfully to {:?} ({}): {}",
                        &recipient,
                        &mail.subject,
                        response.message().collect::<Vec<_>>().join(" ")
                    );

                    report_send_result(feedback_tx, mail, recipient, EmailSendStatus::Sent, None);
                }

                Err(e) => {
                    tracing::error!(
                        "Failed to send e-mail to {:?} after retries: {e:?}",
                        &recipient
                    );

                    report_send_result(
                        feedback_tx,
                        mail,
                        recipient,
                        EmailSendStatus::Failed,
                        Some(format!("{:?}", e)),
                    );
                }
            }
        }

        // To failed to parse: this only affects messages to this address
        Err(e) => {
            tracing::warn!(
                "E-mail addressee could not be parsed: {e:?}. E-mail addresses are expected to be in 'account@server.tld' or 'Name <account@server.tld>' format"
            );

            report_send_result(
                feedback_tx,
                mail,
                recipient,
                EmailSendStatus::Failed,
                Some(format!("Address parsing error: {:?}", e)),
            );
        }
    }
    ControlFlow::Continue(())
}

/// Reports an e-mail that is dropped without ever being sent.
///
/// Counted as a drop with its own reason, and reported as a failed send per recipient so that
/// consumers of the feedback channel learn about it just like they would about a send error.
fn report_dropped(
    feedback_tx: Option<&mpsc::Sender<EmailSendFeedback>>,
    mail: &Email,
    reason: &'static str,
) {
    metrics::increment_counter_with_attribute(METER, "mail_dropped", 1u64, "reason", reason);
    for recipient in &mail.recipients {
        report_send_result(
            feedback_tx,
            mail,
            recipient,
            EmailSendStatus::Failed,
            Some(format!("E-mail dropped before sending ({reason})")),
        );
    }
}

/// Records the outcome of a send attempt as a metric and reports it on the feedback channel.
fn report_send_result(
    feedback_tx: Option<&mpsc::Sender<EmailSendFeedback>>,
    mail: &Email,
    recipient: &str,
    status: EmailSendStatus,
    error: Option<String>,
) {
    let status_name = match status {
        EmailSendStatus::Sent => "sent",
        EmailSendStatus::Failed => "failed",
    };
    metrics::increment_counter(
        METER,
        "mail_sent",
        1u64,
        &[
            ("urgent", mail.urgent.to_string()),
            ("status", status_name.to_string()),
        ],
    );

    if let Some(tx) = feedback_tx {
        let _ = tx.try_send(EmailSendFeedback {
            recipient: recipient.to_string(),
            subject: mail.subject.clone(),
            status,
            error,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::watch;

    use super::*;

    fn email(subject: &str) -> Email {
        Email::new(vec!["someone@example.com"], subject, "body", "")
    }

    /// Runs `send_task` without a mail server: every send attempt is reported as failed on the
    /// feedback channel, which is enough to observe *when* and *in which order* the task sends.
    #[tokio::test(start_paused = true)]
    async fn urgent_email_overtakes_the_throttled_queue() {
        let (mail_tx, mail_rx) = mpsc::channel(32);
        let (feedback_tx, mut feedback_rx) = mpsc::channel(32);
        let (_shutdown_tx, shutdown_rx) = watch::channel(());

        // One email per minute, so the queued emails are clearly spread out in time.
        let throttle = ThrottleConfig {
            per_minute: 1,
            per_hour: 100,
            per_day: 100,
        };
        let task = tokio::spawn(send_task(
            None,
            throttle,
            Arc::new(Mutex::new(mail_rx)),
            shutdown_rx,
            Some(feedback_tx),
        ));

        for subject in ["report 1", "report 2", "report 3"] {
            mail_tx.send(email(subject).non_urgent()).await.unwrap();
        }
        mail_tx.send(email("alarm")).await.unwrap();

        let start = Instant::now();
        let mut sent = Vec::new();
        for _ in 0..4 {
            let feedback = feedback_rx.recv().await.unwrap();
            sent.push((feedback.subject, start.elapsed()));
        }
        drop(mail_tx);
        task.await.unwrap();

        let subjects: Vec<&str> = sent.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            subjects,
            vec!["alarm", "report 1", "report 2", "report 3"],
            "the urgent email should be sent before the queued non-urgent ones"
        );

        // The urgent email is sent immediately, the queued ones at one per minute. The first
        // queued email also has to wait, because the urgent one used up the budget.
        assert!(sent[0].1 < Duration::from_secs(1));
        assert!(sent[1].1 >= Duration::from_secs(60));
        assert!(sent[2].1 >= Duration::from_secs(120));
        assert!(sent[3].1 >= Duration::from_secs(180));
    }

    /// After a shutdown request the task keeps sending what is already queued.
    #[tokio::test(start_paused = true)]
    async fn queued_emails_are_drained_on_shutdown() {
        let (mail_tx, mail_rx) = mpsc::channel(32);
        let (feedback_tx, mut feedback_rx) = mpsc::channel(32);
        let (shutdown_tx, shutdown_rx) = watch::channel(());

        let task = tokio::spawn(send_task(
            None,
            ThrottleConfig {
                per_minute: 1,
                per_hour: 100,
                per_day: 100,
            },
            Arc::new(Mutex::new(mail_rx)),
            shutdown_rx,
            Some(feedback_tx),
        ));

        mail_tx.send(email("report 1").non_urgent()).await.unwrap();
        mail_tx.send(email("report 2").non_urgent()).await.unwrap();

        // Let the task pick up both emails before requesting shutdown.
        tokio::time::sleep(Duration::from_millis(10)).await;
        shutdown_tx.send(()).unwrap();

        assert_eq!(feedback_rx.recv().await.unwrap().subject, "report 1");
        assert_eq!(feedback_rx.recv().await.unwrap().subject, "report 2");
        task.await.unwrap();
    }
}
