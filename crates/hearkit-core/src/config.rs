use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub audio: AudioSettings,
    #[serde(default)]
    pub transcription: TranscriptionSettings,
    #[serde(default)]
    pub llm: LlmSettings,
    #[serde(default)]
    pub storage: StorageSettings,
    #[serde(default)]
    pub app: AppSettings,
    #[serde(default)]
    pub mattermost: MattermostSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    pub sample_rate: u32,
    pub channels: String,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            sample_rate: 44100,
            channels: "mixed".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionSettings {
    pub model: String,
    pub language: String,
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            model: "medium.en".to_string(),
            language: "auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSettings {
    pub provider: String,
    pub api_key_env: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            provider: "claude".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            api_key: String::new(),
            model: "claude-sonnet-4-6".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSettings {
    pub data_dir: String,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            data_dir: "~/hearkit".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub hotkey: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            hotkey: "CmdOrCtrl+Shift+R".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MattermostSettings {
    pub webhook_url: String,
    pub channel: String,
    pub username: String,
    pub enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            audio: AudioSettings::default(),
            transcription: TranscriptionSettings::default(),
            llm: LlmSettings::default(),
            storage: StorageSettings::default(),
            app: AppSettings::default(),
            mattermost: MattermostSettings::default(),
        }
    }
}

impl AppConfig {
    /// Load config from a TOML file, or return defaults if it doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content =
                std::fs::read_to_string(path).context("failed to read config file")?;
            let config: AppConfig =
                toml::from_str(&content).context("failed to parse config")?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    /// Save config to a TOML file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Resolve the data directory (expand ~).
    pub fn data_dir(&self) -> PathBuf {
        let dir = &self.storage.data_dir;
        if dir.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&dir[2..]);
            }
        }
        PathBuf::from(dir)
    }

    /// Default config file path: ~/hearkit/config.toml
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hearkit")
            .join("config.toml")
    }

    /// Load from the default config path, falling back to defaults on any error.
    pub fn load_or_default() -> Self {
        let path = Self::config_path();
        match Self::load(&path) {
            Ok(config) => config,
            Err(e) => {
                tracing::warn!("failed to load config from {}: {e}, using defaults", path.display());
                Self::default()
            }
        }
    }

    /// Resolve the whisper model file path.
    pub fn model_path(&self) -> PathBuf {
        self.data_dir()
            .join("models")
            .join(format!("ggml-{}.bin", self.transcription.model))
    }

    /// Get the effective LLM API key (direct key takes priority over env var).
    pub fn effective_api_key(&self) -> Option<String> {
        if !self.llm.api_key.is_empty() {
            return Some(self.llm.api_key.clone());
        }
        std::env::var(&self.llm.api_key_env).ok().filter(|k| !k.is_empty())
    }
}
