pub mod claude;
pub mod gemini;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of LLM analysis of a meeting transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub summary: String,
    pub action_items: Vec<ActionItem>,
    pub key_topics: Vec<String>,
    pub decisions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionItem {
    pub description: String,
    pub assignee: Option<String>,
    pub priority: Option<Priority>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Priority {
    Low,
    Medium,
    High,
}

/// Configuration for an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub provider: LlmProvider,
    pub api_key: String,
    pub model: String,
    /// Whether to use OAuth bearer token auth instead of x-api-key (Claude only)
    pub use_oauth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LlmProvider {
    Claude,
    Gemini,
    OpenAI,
}

/// Trait for meeting transcript analysis.
#[async_trait]
pub trait MeetingAnalyzer: Send + Sync {
    async fn analyze(
        &self,
        transcript: &hearkit_transcribe::Transcript,
        custom_instructions: Option<&str>,
    ) -> Result<Analysis>;

    fn name(&self) -> &str;
}

const SYSTEM_PROMPT: &str = r#"You are an expert meeting analyst. Analyze the following meeting transcript and produce a structured analysis.

Return your analysis as valid JSON with exactly this structure:
{
  "summary": "A concise 2-3 sentence summary of the meeting.",
  "action_items": [
    {
      "description": "What needs to be done",
      "assignee": "Person responsible (or null if unclear)",
      "priority": "High" | "Medium" | "Low" | null
    }
  ],
  "key_topics": ["topic1", "topic2"],
  "decisions": ["decision1", "decision2"]
}

Focus on extracting actionable information. Be concise."#;

pub fn build_user_prompt(
    transcript: &hearkit_transcribe::Transcript,
    custom_instructions: Option<&str>,
) -> String {
    let mut prompt = String::from("Meeting transcript:\n\n");

    for segment in &transcript.segments {
        prompt.push_str(&format!(
            "[{:.1}s - {:.1}s] {}\n",
            segment.start, segment.end, segment.text
        ));
    }

    if let Some(instructions) = custom_instructions {
        prompt.push_str(&format!("\n\nAdditional instructions: {instructions}"));
    }

    prompt
}
