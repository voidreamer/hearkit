use anyhow::Result;
use hearkit_core::config::AppConfig;
use hearkit_core::pipeline::{MeetingPipeline, RecordingHandle};
use hearkit_core::storage::Storage;
use hearkit_llm::LlmConfig;
use hearkit_notify::discord::DiscordConfig;
use hearkit_notify::email::EmailConfig;
use hearkit_notify::mattermost::MattermostConfig;
use hearkit_notify::slack::SlackConfig;
use hearkit_notify::Notifier;
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

        // Try to init notifiers (non-fatal)
        init_notifiers(&config, &mut pipeline);

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
                "gemini" => hearkit_llm::LlmProvider::Gemini,
                "openai" => hearkit_llm::LlmProvider::OpenAI,
                _ => hearkit_llm::LlmProvider::Claude,
            },
            api_key,
            model: config.llm.model.clone(),
            use_oauth: config.llm.auth_type == "oauth_token",
        };
        let analyzer: Arc<dyn hearkit_llm::MeetingAnalyzer> = match llm_config.provider {
            hearkit_llm::LlmProvider::Claude => {
                Arc::new(hearkit_llm::claude::ClaudeAnalyzer::new(llm_config))
            }
            hearkit_llm::LlmProvider::Gemini => {
                Arc::new(hearkit_llm::gemini::GeminiAnalyzer::new(llm_config))
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

/// Initialize all configured notifiers on a pipeline from the given config.
pub fn init_notifiers(config: &AppConfig, pipeline: &mut MeetingPipeline) {
    let mut notifiers: Vec<Arc<dyn Notifier>> = Vec::new();

    // Mattermost
    let mm = &config.mattermost;
    let mm_cfg = MattermostConfig {
        webhook_url: mm.webhook_url.clone(),
        channel: mm.channel.clone(),
        username: mm.username.clone(),
        icon_url: String::new(),
        enabled: mm.enabled,
    };
    if let Some(notifier) = hearkit_notify::MattermostNotifier::from_config(&mm_cfg) {
        tracing::info!("Mattermost notifier initialized");
        notifiers.push(Arc::new(notifier));
    } else {
        tracing::info!("Mattermost notifications disabled");
    }

    // Slack
    let sl = &config.slack;
    let sl_cfg = SlackConfig {
        webhook_url: sl.webhook_url.clone(),
        channel: sl.channel.clone(),
        username: sl.username.clone(),
        icon_emoji: sl.icon_emoji.clone(),
        enabled: sl.enabled,
    };
    if let Some(notifier) = hearkit_notify::SlackNotifier::from_config(&sl_cfg) {
        tracing::info!("Slack notifier initialized");
        notifiers.push(Arc::new(notifier));
    } else {
        tracing::info!("Slack notifications disabled");
    }

    // Discord
    let dc = &config.discord;
    let dc_cfg = DiscordConfig {
        webhook_url: dc.webhook_url.clone(),
        username: dc.username.clone(),
        avatar_url: dc.avatar_url.clone(),
        enabled: dc.enabled,
    };
    if let Some(notifier) = hearkit_notify::DiscordNotifier::from_config(&dc_cfg) {
        tracing::info!("Discord notifier initialized");
        notifiers.push(Arc::new(notifier));
    } else {
        tracing::info!("Discord notifications disabled");
    }

    // Email
    let em = &config.email;
    let em_cfg = EmailConfig {
        provider: em.provider.clone(),
        smtp_host: em.smtp_host.clone(),
        smtp_port: em.smtp_port,
        smtp_username: em.smtp_username.clone(),
        smtp_password: em.smtp_password.clone(),
        from_address: em.from_address.clone(),
        to_addresses: em.to_addresses.clone(),
        use_tls: em.use_tls,
        resend_api_key: em.resend_api_key.clone(),
        enabled: em.enabled,
    };
    if let Some(notifier) = hearkit_notify::EmailNotifier::from_config(&em_cfg) {
        tracing::info!("Email notifier initialized");
        notifiers.push(Arc::new(notifier));
    } else {
        tracing::info!("Email notifications disabled");
    }

    pipeline.set_notifiers(notifiers);
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
