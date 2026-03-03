use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A single timestamped segment of transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Start time in seconds.
    pub start: f64,
    /// End time in seconds.
    pub end: f64,
    /// Transcribed text.
    pub text: String,
    /// Speaker label (future: diarization).
    pub speaker: Option<String>,
}

/// A full transcript of a recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<Segment>,
    pub full_text: String,
    pub language: String,
    pub duration: f64,
}

/// Configuration for the transcription engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscribeConfig {
    /// Path to the whisper model file.
    pub model_path: PathBuf,
    /// Language code, or "auto" for detection.
    pub language: String,
    /// Number of threads for whisper inference.
    pub n_threads: i32,
}

impl Default for TranscribeConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/ggml-base.en.bin"),
            language: "en".to_string(),
            n_threads: 4,
        }
    }
}

/// Transcription engine using whisper.cpp (via whisper-rs).
pub struct TranscriptionEngine {
    ctx: WhisperContext,
    config: TranscribeConfig,
}

impl TranscriptionEngine {
    /// Create a new engine, loading the whisper model.
    pub fn new(config: TranscribeConfig) -> Result<Self> {
        let ctx = WhisperContext::new_with_params(
            config
                .model_path
                .to_str()
                .context("invalid model path")?,
            WhisperContextParameters::default(),
        )
        .context("failed to load whisper model")?;

        Ok(Self { ctx, config })
    }

    /// Transcribe mono f32 audio at 16kHz.
    pub fn transcribe(&self, samples_16khz: &[f32]) -> Result<Transcript> {
        let mut state = self.ctx.create_state().context("failed to create whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.config.n_threads);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        if self.config.language != "auto" {
            params.set_language(Some(&self.config.language));
        }

        state
            .full(params, samples_16khz)
            .context("whisper transcription failed")?;

        let n_segments = state.full_n_segments()?;
        let mut segments = Vec::with_capacity(n_segments as usize);
        let mut full_text = String::new();

        for i in 0..n_segments {
            let start = state.full_get_segment_t0(i)? as f64 / 100.0;
            let end = state.full_get_segment_t1(i)? as f64 / 100.0;
            let text = state.full_get_segment_text(i)?;

            if !full_text.is_empty() {
                full_text.push(' ');
            }
            full_text.push_str(text.trim());

            segments.push(Segment {
                start,
                end,
                text: text.trim().to_string(),
                speaker: None,
            });
        }

        let duration = segments
            .last()
            .map(|s| s.end)
            .unwrap_or(0.0);

        let language = self.config.language.clone();

        Ok(Transcript {
            segments,
            full_text,
            language,
            duration,
        })
    }

    /// Transcribe a WAV file. Handles reading + resampling to 16kHz.
    pub fn transcribe_file(&self, wav_path: &Path) -> Result<Transcript> {
        let (samples, sample_rate) = hearkit_audio::writer::read_wav(wav_path)?;
        let samples_16k = hearkit_audio::writer::resample(&samples, sample_rate, 16000)?;
        self.transcribe(&samples_16k)
    }
}
