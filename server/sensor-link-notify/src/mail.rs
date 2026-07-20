use std::{ops::ControlFlow, sync::Arc};

use lettre::{
    message::{header::ContentType, Attachment, Body, Mailbox, MultiPart, SinglePart},
    transport::smtp::authentication::{Credentials, Mechanism},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::{mpsc, watch::Receiver, Mutex};

#[derive(Debug, Clone)]
pub struct Email {
    recipients: Vec<String>,
    cc: Option<String>,
    subject: String,
    message: String,
    html: Option<String>,
    /// Path to a logo image attached inline in HTML emails. Empty string means no logo.
    logo_path: String,
}

impl Email {
    /// Creates a new email. All recipients will receive a separate email.
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
    rx: mpsc::Receiver<Email>,
    on_panic: PanicCallback,
    feedback_tx: Option<mpsc::Sender<EmailSendFeedback>>,
) -> Handle {
    let task_function = send_task;
    let rx = Arc::new(Mutex::new(rx));
    Handle::new(
        move |shutdown_rx| task_function(cfg.clone(), rx.clone(), shutdown_rx, feedback_tx.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn send_task(
    config: Option<Config>,
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

    loop {
        let mails = &mut mails
            .try_lock()
            .expect("Email receiver chanel seems to be locked by another task then email task");

        tokio::select! {
            // Prioritize processing emails over shutdown to ensure pending emails are sent
            biased;

            mail = mails.recv() => {
                let Some(mail) = mail else {
                    break;
                };

                let (config, transport) = match (&config, &mailer) {
                    (Some(c), Some(t)) => (c, t),
                    _ => {
                        tracing::warn!(
                            "Not sending e-mail '{}' to {:?} (no mail server configured or transport initialization failed)",
                            mail.subject,
                            mail.recipients
                        );
                        continue;
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
                    if let ControlFlow::Break(_) = build_and_send_email(recipient, config, transport, &mail, feedback_tx.as_ref()).await {
                        continue;
                    }
                }
            }

            _ = shutdown_rx.changed() => break,
        }
    }
    tracing::info!("Exit mail task");
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

                    // Send feedback: Failed status
                    if let Some(tx) = feedback_tx {
                        let _ = tx
                            .try_send(EmailSendFeedback {
                                recipient: recipient.clone(),
                                subject: mail.subject.clone(),
                                status: EmailSendStatus::Failed,
                                error: Some(
                                    "Failed to parse content type for email footer image"
                                        .to_string(),
                                ),
                            })
                            .ok();
                    }

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

                    // Send feedback: Failed status
                    if let Some(tx) = feedback_tx {
                        let _ = tx
                            .try_send(EmailSendFeedback {
                                recipient: recipient.clone(),
                                subject: mail.subject.clone(),
                                status: EmailSendStatus::Failed,
                                error: Some(format!("Failed to build email: {:?}", error)),
                            })
                            .ok();
                    }

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

                    // Send feedback: Sent status
                    if let Some(tx) = feedback_tx {
                        let _ = tx
                            .try_send(EmailSendFeedback {
                                recipient: recipient.clone(),
                                subject: mail.subject.clone(),
                                status: EmailSendStatus::Sent,
                                error: None,
                            })
                            .ok();
                    }
                }

                Err(e) => {
                    tracing::error!(
                        "Failed to send e-mail to {:?} after retries: {e:?}",
                        &recipient
                    );

                    // Send feedback: Failed status
                    if let Some(tx) = feedback_tx {
                        let _ = tx
                            .try_send(EmailSendFeedback {
                                recipient: recipient.clone(),
                                subject: mail.subject.clone(),
                                status: EmailSendStatus::Failed,
                                error: Some(format!("{:?}", e)),
                            })
                            .ok();
                    }
                }
            }
        }

        // To failed to parse: this only affects messages to this address
        Err(e) => {
            tracing::warn!(
                "E-mail addressee could not be parsed: {e:?}. E-mail addresses are expected to be in 'account@server.tld' or 'Name <account@server.tld>' format"
            );

            // Send feedback: Failed status (address parsing error)
            if let Some(tx) = feedback_tx {
                let _ = tx
                    .try_send(EmailSendFeedback {
                        recipient: recipient.clone(),
                        subject: mail.subject.clone(),
                        status: EmailSendStatus::Failed,
                        error: Some(format!("Address parsing error: {:?}", e)),
                    })
                    .ok();
            }
        }
    }
    ControlFlow::Continue(())
}
