use crossbeam_channel::Receiver;
use std::time::Duration;

use crate::AudioChunk;

/// Mixes two audio streams (e.g., mic + system) into one.
pub struct AudioMixer {
    mic_rx: Receiver<AudioChunk>,
    system_rx: Receiver<AudioChunk>,
}

impl AudioMixer {
    pub fn new(mic_rx: Receiver<AudioChunk>, system_rx: Receiver<AudioChunk>) -> Self {
        Self { mic_rx, system_rx }
    }

    /// Drains available chunks from both channels, mixes them, and returns combined chunks.
    /// This is a simple additive mix with clamping.
    pub fn drain_mixed(&self, _timeout: Duration) -> Vec<AudioChunk> {
        let mut mic_chunks = Vec::new();
        let mut sys_chunks = Vec::new();

        // Drain mic
        while let Ok(chunk) = self.mic_rx.try_recv() {
            mic_chunks.push(chunk);
        }
        // Drain system
        while let Ok(chunk) = self.system_rx.try_recv() {
            sys_chunks.push(chunk);
        }

        // If only one source has data, return it directly
        if sys_chunks.is_empty() {
            return mic_chunks;
        }
        if mic_chunks.is_empty() {
            return sys_chunks;
        }

        // Simple mix: interleave by timestamp, combine overlapping samples
        let mut mixed = Vec::new();
        let mic_samples: Vec<f32> = mic_chunks.iter().flat_map(|c| &c.samples).copied().collect();
        let sys_samples: Vec<f32> = sys_chunks.iter().flat_map(|c| &c.samples).copied().collect();

        let len = mic_samples.len().max(sys_samples.len());
        let mut samples = Vec::with_capacity(len);
        for i in 0..len {
            let m = mic_samples.get(i).copied().unwrap_or(0.0);
            let s = sys_samples.get(i).copied().unwrap_or(0.0);
            samples.push((m + s).clamp(-1.0, 1.0));
        }

        let sample_rate = mic_chunks
            .first()
            .map(|c| c.sample_rate)
            .unwrap_or(44100);
        let timestamp = mic_chunks
            .first()
            .map(|c| c.timestamp)
            .unwrap_or(Duration::ZERO);

        mixed.push(AudioChunk {
            samples,
            sample_rate,
            timestamp,
        });

        mixed
    }
}
