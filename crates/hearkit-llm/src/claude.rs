use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;

use crate::{build_user_prompt, Analysis, LlmConfig, MeetingAnalyzer, SYSTEM_PROMPT};

pub struct ClaudeAnalyzer {
    client: Client,
    config: LlmConfig,
}

impl ClaudeAnalyzer {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }
}

#[async_trait]
impl MeetingAnalyzer for ClaudeAnalyzer {
    async fn analyze(
        &self,
        transcript: &hearkit_transcribe::Transcript,
        custom_instructions: Option<&str>,
    ) -> Result<Analysis> {
        let user_prompt = build_user_prompt(transcript, custom_instructions);

        let body = json!({
            "model": self.config.model,
            "max_tokens": 4096,
            "system": SYSTEM_PROMPT,
            "messages": [
                {
                    "role": "user",
                    "content": user_prompt
                }
            ]
        });

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("failed to call Claude API")?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Claude API error ({}): {}",
                status,
                resp_body
            );
        }

        // Extract text from content blocks
        let text = resp_body["content"]
            .as_array()
            .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .context("no text in Claude response")?;

        // Parse the JSON from the response (strip markdown fences if present)
        let json_str = text
            .trim()
            .strip_prefix("```json")
            .unwrap_or(text.trim())
            .strip_prefix("```")
            .unwrap_or(text.trim())
            .strip_suffix("```")
            .unwrap_or(text.trim())
            .trim();

        let analysis: Analysis =
            serde_json::from_str(json_str).context("failed to parse analysis JSON from Claude")?;

        Ok(analysis)
    }

    fn name(&self) -> &str {
        "Claude"
    }
}
