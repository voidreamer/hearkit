use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;

use crate::{build_user_prompt, Analysis, LlmConfig, MeetingAnalyzer, SYSTEM_PROMPT};

pub struct OpenAIAnalyzer {
    client: Client,
    config: LlmConfig,
}

impl OpenAIAnalyzer {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }
}

#[async_trait]
impl MeetingAnalyzer for OpenAIAnalyzer {
    async fn analyze(
        &self,
        transcript: &hearkit_transcribe::Transcript,
        custom_instructions: Option<&str>,
    ) -> Result<Analysis> {
        let user_prompt = build_user_prompt(transcript, custom_instructions);

        let body = json!({
            "model": self.config.model,
            "response_format": { "type": "json_object" },
            "messages": [
                {
                    "role": "system",
                    "content": SYSTEM_PROMPT
                },
                {
                    "role": "user",
                    "content": user_prompt
                }
            ]
        });

        let resp = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("failed to call OpenAI API")?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            anyhow::bail!(
                "OpenAI API error ({}): {}",
                status,
                resp_body
            );
        }

        let text = resp_body["choices"][0]["message"]["content"]
            .as_str()
            .context("no content in OpenAI response")?;

        let analysis: Analysis =
            serde_json::from_str(text).context("failed to parse analysis JSON from OpenAI")?;

        Ok(analysis)
    }

    fn name(&self) -> &str {
        "OpenAI"
    }
}
