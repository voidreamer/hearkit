use anyhow::{Context, Result};
use async_trait::async_trait;
use hearkit_llm::{Analysis, Priority};
use serde::Serialize;

use crate::Notifier;

/// Sends meeting summaries to a Slack channel via incoming webhook.
pub struct SlackNotifier {
    webhook_url: String,
    channel: Option<String>,
    username: Option<String>,
    icon_emoji: Option<String>,
    client: reqwest::Client,
}

/// Configuration needed to construct a `SlackNotifier`.
pub struct SlackConfig {
    pub webhook_url: String,
    pub channel: String,
    pub username: String,
    pub icon_emoji: String,
    pub enabled: bool,
}

#[derive(Serialize)]
struct WebhookPayload {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_emoji: Option<String>,
}

impl SlackNotifier {
    /// Returns `None` if the feature is disabled or no webhook URL is configured.
    pub fn from_config(cfg: &SlackConfig) -> Option<Self> {
        if !cfg.enabled || cfg.webhook_url.is_empty() {
            return None;
        }

        Some(Self {
            webhook_url: cfg.webhook_url.clone(),
            channel: non_empty(&cfg.channel),
            username: Some(if cfg.username.is_empty() {
                "hearkit".to_string()
            } else {
                cfg.username.clone()
            }),
            icon_emoji: non_empty(&cfg.icon_emoji),
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl Notifier for SlackNotifier {
    async fn post_summary(&self, meeting_title: &str, analysis: &Analysis) -> Result<()> {
        let text = format_analysis(meeting_title, analysis);

        let payload = WebhookPayload {
            text,
            channel: self.channel.clone(),
            username: self.username.clone(),
            icon_emoji: self.icon_emoji.clone(),
        };

        let resp = self
            .client
            .post(&self.webhook_url)
            .json(&payload)
            .send()
            .await
            .context("failed to send Slack webhook")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Slack webhook returned {status}: {body}");
        }

        tracing::info!("posted meeting summary to Slack");
        Ok(())
    }

    fn name(&self) -> &str {
        "slack"
    }
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Format an analysis using Slack mrkdwn (no tables, *bold* instead of **bold**).
fn format_analysis(title: &str, analysis: &Analysis) -> String {
    let mut md = String::new();

    // Header
    md.push_str(&format!(":clipboard: *Meeting Summary — {title}*\n\n"));
    md.push_str(&analysis.summary);
    md.push_str("\n\n---\n\n");

    // Action items (bullet list — Slack webhooks don't render tables)
    if !analysis.action_items.is_empty() {
        md.push_str(":white_check_mark: *Action Items*\n");
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
            md.push_str(&format!("• {}{}{}\n", item.description, priority, assignee));
        }
        md.push_str("\n---\n\n");
    }

    // Key topics
    if !analysis.key_topics.is_empty() {
        md.push_str(":speech_balloon: *Key Topics*\n");
        for topic in &analysis.key_topics {
            md.push_str(&format!("• {topic}\n"));
        }
        md.push('\n');
    }

    // Decisions
    if !analysis.decisions.is_empty() {
        md.push_str(":bulb: *Decisions*\n");
        for decision in &analysis.decisions {
            md.push_str(&format!("• {decision}\n"));
        }
    }

    md
}
