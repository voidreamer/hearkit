pub mod capture;
pub mod mixer;
pub mod writer;

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A chunk of audio data from a capture source.
#[derive(Clone, Debug)]
pub struct AudioChunk {
    /// Mono f32 PCM samples.
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Timestamp relative to recording start.
    pub timestamp: Duration,
}

/// Configuration for audio recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Desired sample rate for capture.
    pub sample_rate: u32,
    /// Whether to capture system audio, mic, or both.
    pub mode: CaptureMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CaptureMode {
    MicOnly,
    SystemOnly,
    Mixed,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44100,
            mode: CaptureMode::Mixed,
        }
    }
}
