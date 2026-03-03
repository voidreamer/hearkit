use anyhow::{Context, Result};
use chrono::Utc;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use hearkit_audio::writer::AudioFileWriter;
use hearkit_audio::AudioChunk;
use hearkit_llm::MeetingAnalyzer;
use hearkit_notify::MattermostNotifier;
use hearkit_transcribe::TranscriptionEngine;

use crate::config::AppConfig;
use crate::storage::Storage;
use crate::Meeting;

/// Handle returned when recording starts — used to stop recording.
/// All fields are Send + Sync safe.
pub struct RecordingHandle {
    pub id: String,
    pub audio_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    capture_thread: Option<thread::JoinHandle<()>>,
    writer_thread: Option<thread::JoinHandle<Result<()>>>,
    started_at: chrono::DateTime<Utc>,
}

// Safety: RecordingHandle only holds Arc<AtomicBool> and JoinHandles, all of which are Send.
unsafe impl Send for RecordingHandle {}

/// Orchestrates the record → transcribe → analyze pipeline.
pub struct MeetingPipeline {
    config: AppConfig,
    storage: Storage,
    transcriber: Option<Arc<TranscriptionEngine>>,
    analyzer: Option<Arc<dyn MeetingAnalyzer>>,
    notifier: Option<Arc<MattermostNotifier>>,
}

impl MeetingPipeline {
    pub fn new(config: AppConfig, storage: Storage) -> Self {
        Self {
            config,
            storage,
            transcriber: None,
            analyzer: None,
            notifier: None,
        }
    }

    pub fn set_transcriber(&mut self, transcriber: TranscriptionEngine) {
        self.transcriber = Some(Arc::new(transcriber));
    }

    pub fn clear_transcriber(&mut self) {
        self.transcriber = None;
    }

    pub fn set_analyzer(&mut self, analyzer: Arc<dyn MeetingAnalyzer>) {
        self.analyzer = Some(analyzer);
    }

    pub fn clear_analyzer(&mut self) {
        self.analyzer = None;
    }

    pub fn set_notifier(&mut self, notifier: MattermostNotifier) {
        self.notifier = Some(Arc::new(notifier));
    }

    pub fn clear_notifier(&mut self) {
        self.notifier = None;
    }

    /// Get a cloned Arc to the notifier, suitable for use outside a lock.
    pub fn notifier(&self) -> Option<Arc<MattermostNotifier>> {
        self.notifier.clone()
    }

    /// Get a cloned Arc to the transcriber, suitable for use outside a lock.
    pub fn transcriber(&self) -> Option<Arc<TranscriptionEngine>> {
        self.transcriber.clone()
    }

    /// Get a cloned Arc to the analyzer, suitable for use outside a lock.
    pub fn analyzer(&self) -> Option<Arc<dyn MeetingAnalyzer>> {
        self.analyzer.clone()
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: AppConfig) {
        self.config = config;
    }

    /// Start a new recording.
    pub fn start_recording(&mut self) -> Result<RecordingHandle> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let filename = format!(
            "{}_{}.wav",
            now.format("%Y-%m-%dT%H-%M-%S"),
            &id[..8]
        );
        let audio_path = self.storage.recordings_dir().join(&filename);

        let (tx, rx): (Sender<AudioChunk>, Receiver<AudioChunk>) = bounded(1024);

        let stop_flag = Arc::new(AtomicBool::new(false));
        let sample_rate = self.config.audio.sample_rate;
        let path_clone = audio_path.clone();

        // Writer thread: receives audio chunks and writes WAV
        let stop_writer = stop_flag.clone();
        let writer_thread = thread::spawn(move || -> Result<()> {
            let mut writer = AudioFileWriter::new(&path_clone, sample_rate)?;

            loop {
                match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(chunk) => writer.write_chunk(&chunk)?,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if stop_writer.load(Ordering::SeqCst) {
                            // Drain remaining
                            while let Ok(chunk) = rx.try_recv() {
                                writer.write_chunk(&chunk)?;
                            }
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }

            writer.finalize()?;
            tracing::info!("WAV file written: {}", path_clone.display());
            Ok(())
        });

        // Capture thread: creates and runs MicCapture on its own thread
        // (cpal::Stream is !Send, so it must stay on the thread that created it)
        let stop_capture = stop_flag.clone();
        let capture_thread = thread::spawn(move || {
            use hearkit_audio::capture::{AudioSource, MicCapture};

            let mut mic = match MicCapture::new() {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!("failed to create mic capture: {e}");
                    return;
                }
            };

            if let Err(e) = mic.start(tx) {
                tracing::error!("failed to start mic capture: {e}");
                return;
            }

            // Block until stop is signaled
            while !stop_capture.load(Ordering::SeqCst) {
                thread::sleep(std::time::Duration::from_millis(50));
            }

            if let Err(e) = mic.stop() {
                tracing::error!("failed to stop mic capture: {e}");
            }
        });

        Ok(RecordingHandle {
            id,
            audio_path,
            stop_flag,
            capture_thread: Some(capture_thread),
            writer_thread: Some(writer_thread),
            started_at: now,
        })
    }

    /// Stop recording and return a Meeting with audio path set.
    pub fn stop_recording(&self, mut handle: RecordingHandle) -> Result<Meeting> {
        // Signal everything to stop
        handle.stop_flag.store(true, Ordering::SeqCst);

        // Wait for capture thread to finish (this stops the mic)
        if let Some(thread) = handle.capture_thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("capture thread panicked"))?;
        }

        // Wait for writer thread
        if let Some(thread) = handle.writer_thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("writer thread panicked"))??;
        }

        let ended_at = Utc::now();
        let duration = (ended_at - handle.started_at).num_milliseconds() as f64 / 1000.0;

        let meeting = Meeting {
            id: handle.id,
            title: format!("Meeting {}", handle.started_at.format("%Y-%m-%d %H:%M")),
            started_at: handle.started_at,
            ended_at,
            duration_secs: duration,
            audio_path: handle.audio_path,
            transcript: None,
            analysis: None,
        };

        self.storage.save_meeting(&meeting)?;
        Ok(meeting)
    }

    /// Transcribe a meeting's audio file.
    pub fn transcribe(&self, meeting: &mut Meeting) -> Result<()> {
        let transcriber = self
            .transcriber
            .as_ref()
            .context("transcription engine not initialized")?;

        let transcript = transcriber.transcribe_file(&meeting.audio_path)?;
        self.storage.save_transcript(&meeting.id, &transcript)?;
        meeting.transcript = Some(transcript);
        self.storage.save_meeting(meeting)?;
        Ok(())
    }

    /// Analyze a meeting's transcript with the configured LLM.
    pub async fn analyze(&self, meeting: &mut Meeting) -> Result<()> {
        let analyzer = self
            .analyzer
            .as_ref()
            .context("LLM analyzer not initialized")?;

        let transcript = meeting
            .transcript
            .as_ref()
            .context("no transcript to analyze")?;

        let analysis = analyzer.analyze(transcript, None).await?;
        self.storage.save_summary(&meeting.id, &analysis)?;
        meeting.analysis = Some(analysis.clone());
        self.storage.save_meeting(meeting)?;

        // Post to Mattermost if configured (non-fatal)
        if let Some(notifier) = &self.notifier {
            if let Err(e) = notifier.post_summary(&meeting.title, &analysis).await {
                tracing::warn!("failed to post summary to Mattermost: {e}");
            }
        }

        Ok(())
    }
}
