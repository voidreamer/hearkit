use anyhow::{Context, Result};
use async_trait::async_trait;
use hearkit_llm::{Analysis, Priority};
use serde::Serialize;

use crate::Notifier;

const DISCORD_MAX_LENGTH: usize = 2000;

/// Sends meeting summaries to a Discord channel via webhook.
pub struct DiscordNotifier {
    webhook_url: String,
    username: Option<String>,
    avatar_url: Option<String>,
    client: reqwest::Client,
}

/// Configuration needed to construct a `DiscordNotifier`.
pub struct DiscordConfig {
    pub webhook_url: String,
    pub username: String,
    pub avatar_url: String,
    pub enabled: bool,
}

#[derive(Serialize)]
struct WebhookPayload {
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
}

impl DiscordNotifier {
    /// Returns `None` if the feature is disabled or no webhook URL is configured.
    pub fn from_config(cfg: &DiscordConfig) -> Option<Self> {
        if !cfg.enabled || cfg.webhook_url.is_empty() {
            return None;
        }

        Some(Self {
            webhook_url: cfg.webhook_url.clone(),
            username: Some(if cfg.username.is_empty() {
                "hearkit".to_string()
            } else {
                cfg.username.clone()
            }),
            avatar_url: non_empty(&cfg.avatar_url),
            client: reqwest::Client::new(),
        })
    }

    async fn send_chunk(&self, content: &str) -> Result<()> {
        let payload = WebhookPayload {
            content: content.to_string(),
            username: self.username.clone(),
            avatar_url: self.avatar_url.clone(),
        };

        let resp = self
            .client
            .post(&self.webhook_url)
            .json(&payload)
            .send()
            .await
            .context("failed to send Discord webhook")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Discord webhook returned {status}: {body}");
        }

        Ok(())
    }
}

#[async_trait]
impl Notifier for DiscordNotifier {
    async fn post_summary(&self, meeting_title: &str, analysis: &Analysis) -> Result<()> {
        let sections = build_sections(meeting_title, analysis);
        let chunks = chunk_sections(&sections, DISCORD_MAX_LENGTH);

        for chunk in &chunks {
            self.send_chunk(chunk).await?;
        }

        tracing::info!("posted meeting summary to Discord ({} message(s))", chunks.len());
        Ok(())
    }

    fn name(&self) -> &str {
        "discord"
    }
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Build separate sections of the formatted message.
fn build_sections(title: &str, analysis: &Analysis) -> Vec<String> {
    let mut sections = Vec::new();

    // Header + summary
    let mut header = format!("**Meeting Summary — {title}**\n\n");
    header.push_str(&analysis.summary);
    sections.push(header);

    // Action items
    if !analysis.action_items.is_empty() {
        let mut s = String::from("**Action Items**\n");
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
            s.push_str(&format!("- {}{}{}\n", item.description, priority, assignee));
        }
        sections.push(s);
    }

    // Key topics
    if !analysis.key_topics.is_empty() {
        let mut s = String::from("**Key Topics**\n");
        for topic in &analysis.key_topics {
            s.push_str(&format!("- {topic}\n"));
        }
        sections.push(s);
    }

    // Decisions
    if !analysis.decisions.is_empty() {
        let mut s = String::from("**Decisions**\n");
        for decision in &analysis.decisions {
            s.push_str(&format!("- {decision}\n"));
        }
        sections.push(s);
    }

    sections
}

/// Group sections into messages that fit within the character limit.
fn chunk_sections(sections: &[String], max_len: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for section in sections {
        // If a single section exceeds the limit, send it on its own (truncated if necessary)
        if section.len() > max_len {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
            }
            chunks.push(section[..max_len].to_string());
            continue;
        }

        let separator = if current.is_empty() { "" } else { "\n" };
        if current.len() + separator.len() + section.len() > max_len {
            chunks.push(current);
            current = section.clone();
        } else {
            current.push_str(separator);
            current.push_str(section);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
