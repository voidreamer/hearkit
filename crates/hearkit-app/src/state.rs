use anyhow::Result;
use hearkit_core::config::AppConfig;
use hearkit_core::pipeline::{MeetingPipeline, RecordingHandle};
use hearkit_core::storage::Storage;
use hearkit_llm::LlmConfig;
use hearkit_notify::mattermost::MattermostConfig;
use hearkit_transcribe::TranscribeConfig;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct AppState {
    pub pipeline: Mutex<MeetingPipeline>,
    pub recording: Mutex<Option<RecordingHandle>>,
    pub config_path: PathBuf,
}

impl AppState {
    pub fn new() -> Result<Self> {
        let config_path = AppConfig::config_path();
        let config = AppConfig::load_or_default();
        let data_dir = config.data_dir();
        let storage = Storage::new(data_dir)?;
        let mut pipeline = MeetingPipeline::new(config.clone(), storage);

        // Try to init transcription engine (non-fatal if missing)
        let model_path = config.model_path();
        if model_path.exists() {
            match hearkit_transcribe::TranscriptionEngine::new(TranscribeConfig {
                model_path: model_path.clone(),
                language: config.transcription.language.clone(),
                n_threads: 4,
            }) {
                Ok(engine) => {
                    tracing::info!("transcription engine loaded: {}", model_path.display());
                    pipeline.set_transcriber(engine);
                }
                Err(e) => {
                    tracing::warn!("failed to load transcription engine: {e}");
                }
            }
        } else {
            tracing::warn!(
                "whisper model not found at {}, transcription disabled",
                model_path.display()
            );
        }

        // Try to init LLM analyzer (non-fatal if no key)
        init_analyzer(&config, &mut pipeline);

        // Try to init Mattermost notifier (non-fatal)
        init_notifier(&config, &mut pipeline);

        Ok(Self {
            pipeline: Mutex::new(pipeline),
            recording: Mutex::new(None),
            config_path,
        })
    }
}

/// Initialize the LLM analyzer on a pipeline from the given config.
pub fn init_analyzer(config: &AppConfig, pipeline: &mut MeetingPipeline) {
    if let Some(api_key) = config.effective_api_key() {
        let llm_config = LlmConfig {
            provider: match config.llm.provider.as_str() {
                "openai" => hearkit_llm::LlmProvider::OpenAI,
                _ => hearkit_llm::LlmProvider::Claude,
            },
            api_key,
            model: config.llm.model.clone(),
        };
        let analyzer: Arc<dyn hearkit_llm::MeetingAnalyzer> = match llm_config.provider {
            hearkit_llm::LlmProvider::Claude => {
                Arc::new(hearkit_llm::claude::ClaudeAnalyzer::new(llm_config))
            }
            hearkit_llm::LlmProvider::OpenAI => {
                Arc::new(hearkit_llm::openai::OpenAIAnalyzer::new(llm_config))
            }
        };
        tracing::info!("LLM analyzer initialized: {}", analyzer.name());
        pipeline.set_analyzer(analyzer);
    } else {
        tracing::warn!(
            "no API key found (checked {} and direct config), LLM analysis disabled",
            config.llm.api_key_env
        );
        pipeline.clear_analyzer();
    }
}

/// Initialize the Mattermost notifier on a pipeline from the given config.
pub fn init_notifier(config: &AppConfig, pipeline: &mut MeetingPipeline) {
    let mm = &config.mattermost;
    let cfg = MattermostConfig {
        webhook_url: mm.webhook_url.clone(),
        channel: mm.channel.clone(),
        username: mm.username.clone(),
        icon_url: String::new(),
        enabled: mm.enabled,
    };
    match hearkit_notify::MattermostNotifier::from_config(&cfg) {
        Some(notifier) => {
            tracing::info!("Mattermost notifier initialized");
            pipeline.set_notifier(notifier);
        }
        None => {
            tracing::info!("Mattermost notifications disabled");
            pipeline.clear_notifier();
        }
    }
}

/// Reinitialize the transcription engine on a pipeline from the given config.
pub fn init_transcriber(config: &AppConfig, pipeline: &mut MeetingPipeline) {
    let model_path = config.model_path();
    if model_path.exists() {
        match hearkit_transcribe::TranscriptionEngine::new(TranscribeConfig {
            model_path: model_path.clone(),
            language: config.transcription.language.clone(),
            n_threads: 4,
        }) {
            Ok(engine) => {
                tracing::info!("transcription engine reloaded: {}", model_path.display());
                pipeline.set_transcriber(engine);
            }
            Err(e) => {
                tracing::warn!("failed to reload transcription engine: {e}");
                pipeline.clear_transcriber();
            }
        }
    } else {
        tracing::warn!(
            "whisper model not found at {}, transcription disabled",
            model_path.display()
        );
        pipeline.clear_transcriber();
    }
}
