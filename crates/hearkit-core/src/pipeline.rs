use anyhow::{Context, Result};
use chrono::Utc;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use hearkit_audio::writer::AudioFileWriter;
use hearkit_audio::AudioChunk;
use hearkit_llm::MeetingAnalyzer;
use hearkit_notify::Notifier;
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
    transcriber_thread: Option<thread::JoinHandle<()>>,
    live_segments: Arc<Mutex<Vec<hearkit_transcribe::Segment>>>,
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
    notifiers: Vec<Arc<dyn Notifier>>,
}

impl MeetingPipeline {
    pub fn new(config: AppConfig, storage: Storage) -> Self {
        Self {
            config,
            storage,
            transcriber: None,
            analyzer: None,
            notifiers: Vec::new(),
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

    pub fn set_notifiers(&mut self, notifiers: Vec<Arc<dyn Notifier>>) {
        self.notifiers = notifiers;
    }

    pub fn clear_notifiers(&mut self) {
        self.notifiers.clear();
    }

    /// Get cloned Arcs to all notifiers, suitable for use outside a lock.
    pub fn notifiers(&self) -> Vec<Arc<dyn Notifier>> {
        self.notifiers.clone()
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

        let stop_flag = Arc::new(AtomicBool::new(false));
        let sample_rate = self.config.audio.sample_rate;
        let path_clone = audio_path.clone();
        let live_segments: Arc<Mutex<Vec<hearkit_transcribe::Segment>>> =
            Arc::new(Mutex::new(Vec::new()));

        // Decide channel topology based on whether a transcriber is available.
        let transcriber = self.transcriber.clone();
        let has_transcriber = transcriber.is_some();

        let (tx_mic, rx_writer, transcriber_thread) = if let Some(transcriber) = transcriber {
            // Fan-out: mic → fan-out thread → (writer channel, transcriber channel)
            let (tx_mic, rx_mic): (Sender<AudioChunk>, Receiver<AudioChunk>) = bounded(1024);
            let (tx_writer, rx_writer): (Sender<AudioChunk>, Receiver<AudioChunk>) = bounded(1024);
            let (tx_transcribe, rx_transcribe): (Sender<AudioChunk>, Receiver<AudioChunk>) =
                bounded(1024);

            // Fan-out thread: clones each chunk to both consumers
            let stop_fo = stop_flag.clone();
            thread::spawn(move || {
                loop {
                    match rx_mic.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(chunk) => {
                            let _ = tx_writer.send(chunk.clone());
                            let _ = tx_transcribe.send(chunk);
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            if stop_fo.load(Ordering::SeqCst) {
                                while let Ok(chunk) = rx_mic.try_recv() {
                                    let _ = tx_writer.send(chunk.clone());
                                    let _ = tx_transcribe.send(chunk);
                                }
                                break;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Drop senders so downstream receivers see Disconnected
                drop(tx_writer);
                drop(tx_transcribe);
            });

            // Transcriber thread: accumulates samples, runs whisper every ~30s
            let segments = live_segments.clone();
            let stop_tr = stop_flag.clone();
            let tr_handle = thread::spawn(move || {
                let chunk_duration_secs: f64 = 30.0;
                let mut buffer: Vec<f32> = Vec::new();
                let mut source_sample_rate: u32 = 0;
                let mut chunk_index: u64 = 0;

                loop {
                    match rx_transcribe.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(chunk) => {
                            source_sample_rate = chunk.sample_rate;
                            buffer.extend_from_slice(&chunk.samples);

                            let threshold =
                                (source_sample_rate as f64 * chunk_duration_secs) as usize;
                            if buffer.len() >= threshold {
                                process_live_chunk(
                                    &transcriber,
                                    &buffer,
                                    source_sample_rate,
                                    chunk_index,
                                    chunk_duration_secs,
                                    &segments,
                                );
                                buffer.clear();
                                chunk_index += 1;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            if stop_tr.load(Ordering::SeqCst) {
                                // Drain remaining chunks
                                while let Ok(chunk) = rx_transcribe.try_recv() {
                                    source_sample_rate = chunk.sample_rate;
                                    buffer.extend_from_slice(&chunk.samples);
                                }
                                // Process tail
                                if !buffer.is_empty() && source_sample_rate > 0 {
                                    process_live_chunk(
                                        &transcriber,
                                        &buffer,
                                        source_sample_rate,
                                        chunk_index,
                                        chunk_duration_secs,
                                        &segments,
                                    );
                                }
                                break;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            // Process tail
                            if !buffer.is_empty() && source_sample_rate > 0 {
                                process_live_chunk(
                                    &transcriber,
                                    &buffer,
                                    source_sample_rate,
                                    chunk_index,
                                    chunk_duration_secs,
                                    &segments,
                                );
                            }
                            break;
                        }
                    }
                }
                tracing::info!("live transcription finished ({} chunks)", chunk_index + 1);
            });

            (tx_mic, rx_writer, Some(tr_handle))
        } else {
            // No transcriber available — single channel, no fan-out
            let (tx, rx): (Sender<AudioChunk>, Receiver<AudioChunk>) = bounded(1024);
            (tx, rx, None)
        };

        if has_transcriber {
            tracing::info!("live transcription enabled for this recording");
        }

        // Writer thread: receives audio chunks and writes WAV
        let stop_writer = stop_flag.clone();
        let writer_thread = thread::spawn(move || -> Result<()> {
            let mut writer = AudioFileWriter::new(&path_clone, sample_rate)?;

            loop {
                match rx_writer.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(chunk) => writer.write_chunk(&chunk)?,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        if stop_writer.load(Ordering::SeqCst) {
                            // Drain remaining
                            while let Ok(chunk) = rx_writer.try_recv() {
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

        // Capture thread: creates and runs audio capture on its own thread
        // (cpal::Stream is !Send, so it must stay on the thread that created it)
        let stop_capture = stop_flag.clone();
        let capture_mode = self.config.audio.channels.clone();
        let capture_sample_rate = sample_rate;
        let capture_thread = thread::spawn(move || {
            use hearkit_audio::capture::AudioSource;

            let want_mic = capture_mode != "system";
            let want_system = capture_mode != "mic";

            // Start mic capture
            let mut mic = if want_mic {
                match hearkit_audio::capture::MicCapture::new() {
                    Ok(m) => Some(m),
                    Err(e) => {
                        tracing::error!("failed to create mic capture: {e}");
                        None
                    }
                }
            } else {
                None
            };

            // Start system audio capture (macOS only)
            #[cfg(target_os = "macos")]
            let mut system = if want_system {
                match hearkit_audio::capture::system::SystemAudioCapture::new(capture_sample_rate) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::error!("failed to create system audio capture: {e}");
                        None
                    }
                }
            } else {
                None
            };
            #[cfg(not(target_os = "macos"))]
            let mut system: Option<()> = None;

            // If both sources are active, use mixer; otherwise send directly
            let has_mic = mic.is_some();
            #[cfg(target_os = "macos")]
            let has_system = system.is_some();
            #[cfg(not(target_os = "macos"))]
            let has_system = false;

            if has_mic && has_system {
                // Both sources: create per-source channels and a mixer
                let (tx_mic_raw, rx_mic_raw) = bounded::<AudioChunk>(1024);
                let (tx_sys_raw, rx_sys_raw) = bounded::<AudioChunk>(1024);

                if let Some(ref mut m) = mic {
                    if let Err(e) = m.start(tx_mic_raw) {
                        tracing::error!("failed to start mic capture: {e}");
                    }
                }
                #[cfg(target_os = "macos")]
                if let Some(ref mut s) = system {
                    if let Err(e) = s.start(tx_sys_raw) {
                        tracing::error!("failed to start system audio capture: {e}");
                    }
                }

                let mixer = hearkit_audio::mixer::AudioMixer::new(rx_mic_raw, rx_sys_raw);
                tracing::info!("mixed capture started (mic + system audio)");

                while !stop_capture.load(Ordering::SeqCst) {
                    let chunks = mixer.drain_mixed(std::time::Duration::from_millis(50));
                    for chunk in chunks {
                        let _ = tx_mic.send(chunk);
                    }
                    thread::sleep(std::time::Duration::from_millis(50));
                }
                // Final drain
                let chunks = mixer.drain_mixed(std::time::Duration::from_millis(10));
                for chunk in chunks {
                    let _ = tx_mic.send(chunk);
                }
            } else if has_mic {
                // Mic only
                if let Some(ref mut m) = mic {
                    if let Err(e) = m.start(tx_mic.clone()) {
                        tracing::error!("failed to start mic capture: {e}");
                        return;
                    }
                }
                tracing::info!("mic-only capture started");
                while !stop_capture.load(Ordering::SeqCst) {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
            } else if has_system {
                // System only
                #[cfg(target_os = "macos")]
                if let Some(ref mut s) = system {
                    if let Err(e) = s.start(tx_mic.clone()) {
                        tracing::error!("failed to start system audio capture: {e}");
                        return;
                    }
                }
                tracing::info!("system-only capture started");
                while !stop_capture.load(Ordering::SeqCst) {
                    thread::sleep(std::time::Duration::from_millis(50));
                }
            } else {
                tracing::error!("no audio capture source available");
                return;
            }

            // Stop sources
            if let Some(ref mut m) = mic {
                if let Err(e) = m.stop() {
                    tracing::error!("failed to stop mic capture: {e}");
                }
            }
            #[cfg(target_os = "macos")]
            if let Some(ref mut s) = system {
                if let Err(e) = s.stop() {
                    tracing::error!("failed to stop system audio capture: {e}");
                }
            }
        });

        Ok(RecordingHandle {
            id,
            audio_path,
            stop_flag,
            capture_thread: Some(capture_thread),
            writer_thread: Some(writer_thread),
            transcriber_thread,
            live_segments,
            started_at: now,
        })
    }

    /// Stop recording and return a Meeting with audio path set.
    /// If a transcriber was active, the meeting already contains the live transcript.
    pub fn stop_recording(&self, mut handle: RecordingHandle) -> Result<Meeting> {
        // Signal everything to stop
        handle.stop_flag.store(true, Ordering::SeqCst);

        // Wait for capture thread to finish (this stops the mic)
        if let Some(thread) = handle.capture_thread.take() {
            thread.join().map_err(|e| {
                let msg = panic_message(&e);
                anyhow::anyhow!("capture thread panicked: {msg}")
            })?;
        }

        // Wait for writer thread
        if let Some(thread) = handle.writer_thread.take() {
            thread
                .join()
                .map_err(|e| {
                    let msg = panic_message(&e);
                    anyhow::anyhow!("writer thread panicked: {msg}")
                })??;
        }

        // Wait for transcriber thread to finish processing remaining audio
        if let Some(thread) = handle.transcriber_thread.take() {
            thread.join().map_err(|e| {
                let msg = panic_message(&e);
                anyhow::anyhow!("transcriber thread panicked: {msg}")
            })?;
        }

        let ended_at = Utc::now();
        let duration = (ended_at - handle.started_at).num_milliseconds() as f64 / 1000.0;

        // Collect live-transcribed segments into a Transcript (if any)
        let transcript = {
            let segments = handle.live_segments.lock().unwrap();
            if segments.is_empty() {
                None
            } else {
                let full_text = segments
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let last_end = segments.iter().map(|s| s.end).fold(0.0f64, f64::max);
                Some(hearkit_transcribe::Transcript {
                    segments: segments.clone(),
                    full_text,
                    language: "en".to_string(),
                    duration: last_end,
                })
            }
        };

        let meeting = Meeting {
            id: handle.id,
            title: format!("Meeting {}", handle.started_at.format("%Y-%m-%d %H:%M")),
            started_at: handle.started_at,
            ended_at,
            duration_secs: duration,
            audio_path: handle.audio_path,
            transcript: transcript.clone(),
            analysis: None,
        };

        // Save transcript to storage if live transcription produced one
        if let Some(ref transcript) = meeting.transcript {
            self.storage.save_transcript(&meeting.id, transcript)?;
            tracing::info!("live transcript saved ({} segments)", transcript.segments.len());
        }

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

        // Post to all configured notifiers (non-fatal)
        for notifier in &self.notifiers {
            if let Err(e) = notifier.post_summary(&meeting.title, &analysis).await {
                tracing::warn!("failed to post summary to {}: {e}", notifier.name());
            }
        }

        Ok(())
    }
}

/// Extract a human-readable message from a thread panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Resample a buffer to 16kHz and run whisper, appending offset segments to shared vec.
fn process_live_chunk(
    transcriber: &TranscriptionEngine,
    buffer: &[f32],
    source_sample_rate: u32,
    chunk_index: u64,
    chunk_duration_secs: f64,
    segments: &Arc<Mutex<Vec<hearkit_transcribe::Segment>>>,
) {
    let resampled = match hearkit_audio::writer::resample(buffer, source_sample_rate, 16000) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("resample failed for chunk {chunk_index}: {e}");
            return;
        }
    };

    match transcriber.transcribe(&resampled) {
        Ok(transcript) => {
            let offset = chunk_index as f64 * chunk_duration_secs;
            let count = transcript.segments.len();
            let mut segs = segments.lock().unwrap();
            for mut seg in transcript.segments {
                seg.start += offset;
                seg.end += offset;
                segs.push(seg);
            }
            tracing::debug!("transcribed chunk {chunk_index} ({count} segments)");
        }
        Err(e) => {
            tracing::error!("transcription failed for chunk {chunk_index}: {e}");
        }
    }
}
