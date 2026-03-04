use futures_util::StreamExt;
use hearkit_core::config::AppConfig;
use hearkit_core::{Meeting, MeetingSummary};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::state::{self, AppState};

#[tauri::command]
pub fn start_recording(state: State<'_, AppState>) -> Result<String, String> {
    let mut pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    let mut recording = state.recording.lock().map_err(|e| e.to_string())?;

    if recording.is_some() {
        return Err("already recording".to_string());
    }

    let handle = pipeline.start_recording().map_err(|e| e.to_string())?;
    let id = handle.id.clone();
    *recording = Some(handle);

    tracing::info!("recording started: {id}");
    Ok(id)
}

#[tauri::command]
pub fn stop_recording(state: State<'_, AppState>) -> Result<Meeting, String> {
    let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    let mut recording = state.recording.lock().map_err(|e| e.to_string())?;

    let handle = recording
        .take()
        .ok_or_else(|| "not recording".to_string())?;

    let meeting = pipeline
        .stop_recording(handle)
        .map_err(|e| e.to_string())?;

    tracing::info!("recording stopped: {}", meeting.id);
    Ok(meeting)
}

#[tauri::command]
pub fn list_meetings(state: State<'_, AppState>) -> Result<Vec<MeetingSummary>, String> {
    let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    let meetings = pipeline
        .storage()
        .list_meetings()
        .map_err(|e| e.to_string())?;
    Ok(meetings.iter().map(MeetingSummary::from).collect())
}

#[tauri::command]
pub fn get_meeting(state: State<'_, AppState>, id: String) -> Result<Meeting, String> {
    let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    pipeline
        .storage()
        .load_meeting(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn transcribe_meeting(
    state: State<'_, AppState>,
    id: String,
) -> Result<Meeting, String> {
    // Get the transcriber Arc and meeting data while holding the lock, then drop it
    let (transcriber, mut meeting) = {
        let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
        let meeting = pipeline
            .storage()
            .load_meeting(&id)
            .map_err(|e| e.to_string())?;
        let transcriber = pipeline
            .transcriber()
            .ok_or_else(|| {
                "transcription engine not initialized — whisper model file not found".to_string()
            })?;
        (transcriber, meeting)
    };

    // Run transcription off the main thread
    let audio_path = meeting.audio_path.clone();
    let transcript = tokio::task::spawn_blocking(move || {
        transcriber.transcribe_file(&audio_path)
    })
    .await
    .map_err(|e| format!("transcription task panicked: {e}"))?
    .map_err(|e| e.to_string())?;

    // Save results — reacquire the lock
    {
        let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
        pipeline
            .storage()
            .save_transcript(&meeting.id, &transcript)
            .map_err(|e| e.to_string())?;
        meeting.transcript = Some(transcript);
        pipeline
            .storage()
            .save_meeting(&meeting)
            .map_err(|e| e.to_string())?;
    }

    Ok(meeting)
}

#[tauri::command]
pub async fn analyze_meeting(
    state: State<'_, AppState>,
    id: String,
) -> Result<Meeting, String> {
    // Load meeting data while holding the lock, then drop the lock before await
    let (mut meeting, analyzer, notifiers) = {
        let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
        let meeting = pipeline
            .storage()
            .load_meeting(&id)
            .map_err(|e| e.to_string())?;
        let analyzer = pipeline
            .analyzer()
            .ok_or_else(|| "LLM analyzer not configured — set an API key in settings".to_string())?;
        let notifiers = pipeline.notifiers();
        (meeting, analyzer, notifiers)
    };

    let transcript = meeting
        .transcript
        .as_ref()
        .ok_or_else(|| "no transcript to analyze".to_string())?;

    let analysis = analyzer
        .analyze(transcript, None)
        .await
        .map_err(|e| e.to_string())?;

    // Save results
    {
        let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
        pipeline
            .storage()
            .save_summary(&meeting.id, &analysis)
            .map_err(|e| e.to_string())?;
        meeting.analysis = Some(analysis.clone());
        pipeline
            .storage()
            .save_meeting(&meeting)
            .map_err(|e| e.to_string())?;
    }

    // Post to all configured notifiers (non-fatal)
    for notifier in &notifiers {
        if let Err(e) = notifier.post_summary(&meeting.title, &analysis).await {
            tracing::warn!("failed to post summary to {}: {e}", notifier.name());
        }
    }

    Ok(meeting)
}

// ── Settings commands ──────────────────────────────────────────────────

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppConfig, String> {
    let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    Ok(pipeline.config().clone())
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: AppConfig) -> Result<(), String> {
    // Save to disk
    settings
        .save(&state.config_path)
        .map_err(|e| e.to_string())?;

    // Reinitialize engines with new config
    let mut pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    pipeline.set_config(settings.clone());

    // Reinitialize transcriber if model changed
    state::init_transcriber(&settings, &mut pipeline);

    // Reinitialize analyzer with new key/provider
    state::init_analyzer(&settings, &mut pipeline);

    // Reinitialize notifiers
    state::init_notifiers(&settings, &mut pipeline);

    tracing::info!("settings saved and engines reinitialized");
    Ok(())
}

#[derive(Serialize)]
pub struct ModelStatus {
    pub exists: bool,
    pub path: String,
    pub model_name: String,
}

#[tauri::command]
pub fn check_model_status(state: State<'_, AppState>) -> Result<ModelStatus, String> {
    let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
    let config = pipeline.config();
    let model_path = config.model_path();
    Ok(ModelStatus {
        exists: model_path.exists(),
        path: model_path.display().to_string(),
        model_name: config.transcription.model.clone(),
    })
}

// ── Model download command ───────────────────────────────────────────

#[derive(Clone, Serialize)]
struct DownloadProgress {
    downloaded: u64,
    total: u64,
    percent: f64,
}

const ALLOWED_MODELS: &[&str] = &["tiny.en", "base.en", "small.en", "medium.en"];

#[tauri::command]
pub async fn download_model(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    model_name: String,
) -> Result<(), String> {
    if !ALLOWED_MODELS.contains(&model_name.as_str()) {
        return Err(format!(
            "invalid model: {model_name}. Allowed: {}",
            ALLOWED_MODELS.join(", ")
        ));
    }

    // Resolve target path from current config's data_dir
    let (models_dir, target_path) = {
        let pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
        let config = pipeline.config();
        let dir = config.data_dir().join("models");
        let path = dir.join(format!("ggml-{model_name}.bin"));
        (dir, path)
    };

    // Create models directory if needed
    std::fs::create_dir_all(&models_dir)
        .map_err(|e| format!("failed to create models directory: {e}"))?;

    let part_path = target_path.with_extension("bin.part");
    let url = format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model_name}.bin"
    );

    tracing::info!("downloading model from {url}");

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("download request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("download failed with status {}", response.status()));
    }

    let total = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();

    let mut file = std::fs::File::create(&part_path)
        .map_err(|e| format!("failed to create temp file: {e}"))?;

    use std::io::Write;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download stream error: {e}"))?;
        file.write_all(&chunk)
            .map_err(|e| format!("failed to write model data: {e}"))?;
        downloaded += chunk.len() as u64;

        let percent = if total > 0 {
            (downloaded as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        let _ = app_handle.emit(
            "model-download-progress",
            DownloadProgress {
                downloaded,
                total,
                percent,
            },
        );
    }

    drop(file);

    // Atomic rename
    std::fs::rename(&part_path, &target_path)
        .map_err(|e| format!("failed to finalize model file: {e}"))?;

    tracing::info!("model downloaded to {}", target_path.display());

    // Update config with selected model, save, and init transcriber
    {
        let mut pipeline = state.pipeline.lock().map_err(|e| e.to_string())?;
        let mut config = pipeline.config().clone();
        config.transcription.model = model_name.clone();
        config
            .save(&state.config_path)
            .map_err(|e| format!("failed to save config: {e}"))?;
        pipeline.set_config(config.clone());
        state::init_transcriber(&config, &mut pipeline);
    }

    Ok(())
}
