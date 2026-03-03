pub mod config;
pub mod pipeline;
pub mod storage;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A recorded meeting with its associated data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: f64,
    pub audio_path: PathBuf,
    pub transcript: Option<hearkit_transcribe::Transcript>,
    pub analysis: Option<hearkit_llm::Analysis>,
}

/// Compact summary for listing meetings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingSummary {
    pub id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub duration_secs: f64,
    pub has_transcript: bool,
    pub has_analysis: bool,
}

impl From<&Meeting> for MeetingSummary {
    fn from(m: &Meeting) -> Self {
        Self {
            id: m.id.clone(),
            title: m.title.clone(),
            started_at: m.started_at,
            duration_secs: m.duration_secs,
            has_transcript: m.transcript.is_some(),
            has_analysis: m.analysis.is_some(),
        }
    }
}
