use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{MultiPart, SinglePart, header::ContentType},
    transport::smtp::{Error as SmtpError, authentication::Credentials},
};

use crate::{Email, EmailError, SmtpConfig};

/// Abstraction over email delivery backends.
#[async_trait]
pub trait EmailSender: Send + Sync {
    /// Send a fully-rendered email.
    ///
    /// # Errors
    ///
    /// Returns [`EmailError`] on delivery failure.
    async fn send(&self, email: Email) -> Result<(), EmailError>;
}

/// SMTP delivery via [`lettre`] with tokio + rustls.
pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
}

impl SmtpSender {
    /// Build a sender from [`SmtpConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`EmailError::Smtp`] if the relay address is invalid.
    pub fn new(config: &SmtpConfig) -> Result<Self, EmailError> {
        let creds = Credentials::new(config.username.clone(), config.password.clone());
        let transport = AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
            .map_err(|e: SmtpError| EmailError::Smtp(e.to_string()))?
            .port(config.port)
            .credentials(creds)
            .build();
        Ok(Self {
            transport,
            from: config.from_address.clone(),
        })
    }
}

#[async_trait]
impl EmailSender for SmtpSender {
    async fn send(&self, email: Email) -> Result<(), EmailError> {
        let from: lettre::message::Mailbox = self
            .from
            .parse()
            .map_err(|e: lettre::address::AddressError| EmailError::Smtp(e.to_string()))?;
        let to: lettre::message::Mailbox = email
            .to
            .parse()
            .map_err(|e: lettre::address::AddressError| EmailError::Smtp(e.to_string()))?;

        let message = Message::builder()
            .from(from)
            .to(to)
            .subject(email.subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(email.text_body),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(email.html_body),
                    ),
            )
            .map_err(|e| EmailError::Smtp(e.to_string()))?;

        AsyncTransport::send(&self.transport, message)
            .await
            .map_err(|e: SmtpError| EmailError::Smtp(e.to_string()))?;
        Ok(())
    }
}

/// Prints a `[EMAIL]` banner to stdout. Safe for local development.
pub struct ConsoleSender;

#[async_trait]
impl EmailSender for ConsoleSender {
    async fn send(&self, email: Email) -> Result<(), EmailError> {
        println!(
            "[EMAIL] to={} subject={}\n{}\n---",
            email.to, email.subject, email.text_body
        );
        Ok(())
    }
}

/// Silently discards every email. Useful in tests.
pub struct NoopSender;

#[async_trait]
impl EmailSender for NoopSender {
    async fn send(&self, _email: Email) -> Result<(), EmailError> {
        Ok(())
    }
}

/// Create an [`EmailSender`] from the `RB_EMAIL_TRANSPORT` value.
///
/// `smtp` requires a valid `smtp` config; `console` and `noop` ignore it.
///
/// # Errors
///
/// Returns [`EmailError::UnknownTransport`] for unrecognised values.
/// Returns [`EmailError::Smtp`] if the SMTP relay address is invalid.
pub fn from_transport(
    transport: &str,
    smtp: &SmtpConfig,
) -> Result<Box<dyn EmailSender>, EmailError> {
    match transport {
        "smtp" => Ok(Box::new(SmtpSender::new(smtp)?)),
        "console" => Ok(Box::new(ConsoleSender)),
        "noop" => Ok(Box::new(NoopSender)),
        other => Err(EmailError::UnknownTransport(other.to_string())),
    }
}
