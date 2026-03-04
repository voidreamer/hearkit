use anyhow::{Context, Result};
use async_trait::async_trait;
use hearkit_llm::{Analysis, Priority};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::Serialize;

use crate::Notifier;

/// Configuration needed to construct an `EmailNotifier`.
pub struct EmailConfig {
    /// "smtp" or "resend"
    pub provider: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub from_address: String,
    pub to_addresses: String,
    pub use_tls: bool,
    pub resend_api_key: String,
    pub enabled: bool,
}

enum EmailTransport {
    Smtp {
        host: String,
        port: u16,
        credentials: Credentials,
        use_tls: bool,
    },
    Resend {
        api_key: String,
        client: reqwest::Client,
    },
}

/// Sends meeting summaries via email (SMTP or Resend API).
pub struct EmailNotifier {
    transport: EmailTransport,
    from_address: String,
    to_addresses: Vec<String>,
}

#[derive(Serialize)]
struct ResendPayload {
    from: String,
    to: Vec<String>,
    subject: String,
    text: String,
}

impl EmailNotifier {
    /// Returns `None` if disabled or missing required config.
    pub fn from_config(cfg: &EmailConfig) -> Option<Self> {
        if !cfg.enabled || cfg.to_addresses.is_empty() {
            return None;
        }

        let to_addresses: Vec<String> = cfg
            .to_addresses
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if to_addresses.is_empty() {
            return None;
        }

        let transport = if cfg.provider == "resend" {
            if cfg.resend_api_key.is_empty() {
                return None;
            }
            EmailTransport::Resend {
                api_key: cfg.resend_api_key.clone(),
                client: reqwest::Client::new(),
            }
        } else {
            // Default: SMTP
            if cfg.smtp_host.is_empty() {
                return None;
            }
            EmailTransport::Smtp {
                host: cfg.smtp_host.clone(),
                port: cfg.smtp_port,
                credentials: Credentials::new(
                    cfg.smtp_username.clone(),
                    cfg.smtp_password.clone(),
                ),
                use_tls: cfg.use_tls,
            }
        };

        Some(Self {
            transport,
            from_address: cfg.from_address.clone(),
            to_addresses,
        })
    }

    async fn send_smtp(
        &self,
        host: &str,
        port: u16,
        credentials: &Credentials,
        use_tls: bool,
        subject: &str,
        body: &str,
    ) -> Result<()> {
        let transport = if use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                .context("failed to create SMTP relay transport")?
                .port(port)
                .credentials(credentials.clone())
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                .context("failed to create SMTP STARTTLS transport")?
                .port(port)
                .credentials(credentials.clone())
                .build()
        };

        for to_addr in &self.to_addresses {
            let email = Message::builder()
                .from(
                    self.from_address
                        .parse()
                        .context("invalid from_address")?,
                )
                .to(to_addr
                    .parse()
                    .with_context(|| format!("invalid to_address: {to_addr}"))?)
                .subject(subject)
                .header(ContentType::TEXT_PLAIN)
                .body(body.to_string())
                .context("failed to build email message")?;

            transport
                .send(email)
                .await
                .with_context(|| format!("failed to send email to {to_addr}"))?;
        }

        Ok(())
    }

    async fn send_resend(
        &self,
        api_key: &str,
        client: &reqwest::Client,
        subject: &str,
        body: &str,
    ) -> Result<()> {
        let payload = ResendPayload {
            from: self.from_address.clone(),
            to: self.to_addresses.clone(),
            subject: subject.to_string(),
            text: body.to_string(),
        };

        let resp = client
            .post("https://api.resend.com/emails")
            .header("Authorization", format!("Bearer {api_key}"))
            .json(&payload)
            .send()
            .await
            .context("failed to call Resend API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Resend API returned {status}: {body}");
        }

        Ok(())
    }
}

#[async_trait]
impl Notifier for EmailNotifier {
    async fn post_summary(&self, meeting_title: &str, analysis: &Analysis) -> Result<()> {
        let subject = format!("Meeting Summary — {meeting_title}");
        let body = format_analysis(meeting_title, analysis);

        match &self.transport {
            EmailTransport::Smtp {
                host,
                port,
                credentials,
                use_tls,
            } => {
                self.send_smtp(host, *port, credentials, *use_tls, &subject, &body)
                    .await?;
            }
            EmailTransport::Resend { api_key, client } => {
                self.send_resend(api_key, client, &subject, &body).await?;
            }
        }

        tracing::info!(
            "sent meeting summary email to {} recipient(s)",
            self.to_addresses.len()
        );
        Ok(())
    }

    fn name(&self) -> &str {
        "email"
    }
}

fn format_analysis(title: &str, analysis: &Analysis) -> String {
    let mut text = String::new();

    text.push_str(&format!("Meeting Summary — {title}\n"));
    text.push_str(&"=".repeat(40));
    text.push_str("\n\n");
    text.push_str(&analysis.summary);
    text.push_str("\n\n");

    if !analysis.action_items.is_empty() {
        text.push_str("Action Items\n");
        text.push_str(&"-".repeat(20));
        text.push('\n');
        for item in &analysis.action_items {
            let priority = match &item.priority {
                Some(Priority::High) => " [High]",
                Some(Priority::Medium) => " [Medium]",
                Some(Priority::Low) => " [Low]",
                None => "",
            };
            let assignee = match &item.assignee {
                Some(a) => format!(" — {a}"),
                None => String::new(),
            };
            text.push_str(&format!("- {}{}{}\n", item.description, priority, assignee));
        }
        text.push('\n');
    }

    if !analysis.key_topics.is_empty() {
        text.push_str("Key Topics\n");
        text.push_str(&"-".repeat(20));
        text.push('\n');
        for topic in &analysis.key_topics {
            text.push_str(&format!("- {topic}\n"));
        }
        text.push('\n');
    }

    if !analysis.decisions.is_empty() {
        text.push_str("Decisions\n");
        text.push_str(&"-".repeat(20));
        text.push('\n');
        for decision in &analysis.decisions {
            text.push_str(&format!("- {decision}\n"));
        }
    }

    text
}
