use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::Meeting;

/// Manages file-based storage for meetings.
pub struct Storage {
    base_dir: PathBuf,
}

impl Storage {
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(base_dir.join("recordings"))?;
        std::fs::create_dir_all(base_dir.join("transcripts"))?;
        std::fs::create_dir_all(base_dir.join("summaries"))?;
        std::fs::create_dir_all(base_dir.join("meetings"))?;
        Ok(Self { base_dir })
    }

    pub fn base_dir(&self) -> PathBuf {
        self.base_dir.clone()
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.base_dir.join("recordings")
    }

    pub fn transcripts_dir(&self) -> PathBuf {
        self.base_dir.join("transcripts")
    }

    pub fn summaries_dir(&self) -> PathBuf {
        self.base_dir.join("summaries")
    }

    fn meetings_dir(&self) -> PathBuf {
        self.base_dir.join("meetings")
    }

    /// Save a meeting's metadata as JSON.
    pub fn save_meeting(&self, meeting: &Meeting) -> Result<()> {
        let path = self.meetings_dir().join(format!("{}.json", meeting.id));
        let json = serde_json::to_string_pretty(meeting)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    /// Load a meeting by ID.
    pub fn load_meeting(&self, id: &str) -> Result<Meeting> {
        let path = self.meetings_dir().join(format!("{id}.json"));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("meeting not found: {id}"))?;
        let meeting: Meeting = serde_json::from_str(&content)?;
        Ok(meeting)
    }

    /// List all stored meetings, sorted by date descending.
    pub fn list_meetings(&self) -> Result<Vec<Meeting>> {
        let mut meetings = Vec::new();
        let dir = self.meetings_dir();

        if !dir.exists() {
            return Ok(meetings);
        }

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        if let Ok(meeting) = serde_json::from_str::<Meeting>(&content) {
                            meetings.push(meeting);
                        }
                    }
                    Err(e) => tracing::warn!("failed to read meeting file {}: {e}", path.display()),
                }
            }
        }

        meetings.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(meetings)
    }

    /// Save a transcript as JSON.
    pub fn save_transcript(
        &self,
        id: &str,
        transcript: &hearkit_transcribe::Transcript,
    ) -> Result<PathBuf> {
        let path = self.transcripts_dir().join(format!("{id}.json"));
        let json = serde_json::to_string_pretty(transcript)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }

    /// Save an analysis/summary as markdown.
    pub fn save_summary(&self, id: &str, analysis: &hearkit_llm::Analysis) -> Result<PathBuf> {
        let path = self.summaries_dir().join(format!("{id}.md"));
        let mut md = String::new();

        md.push_str("# Meeting Summary\n\n");
        md.push_str(&analysis.summary);
        md.push_str("\n\n## Action Items\n\n");

        for item in &analysis.action_items {
            let assignee = item
                .assignee
                .as_deref()
                .unwrap_or("Unassigned");
            let priority = item
                .priority
                .as_ref()
                .map(|p| format!(" [{p:?}]"))
                .unwrap_or_default();
            md.push_str(&format!("- [ ] {}{} — {}\n", item.description, priority, assignee));
        }

        if !analysis.key_topics.is_empty() {
            md.push_str("\n## Key Topics\n\n");
            for topic in &analysis.key_topics {
                md.push_str(&format!("- {topic}\n"));
            }
        }

        if !analysis.decisions.is_empty() {
            md.push_str("\n## Decisions\n\n");
            for decision in &analysis.decisions {
                md.push_str(&format!("- {decision}\n"));
            }
        }

        std::fs::write(&path, md)?;
        Ok(path)
    }
}
