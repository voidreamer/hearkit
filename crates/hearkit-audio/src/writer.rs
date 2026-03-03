use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use rubato::{FftFixedIn, Resampler};
use std::io::BufWriter;
use std::path::Path;

use crate::AudioChunk;

/// Writes audio chunks to a WAV file.
pub struct AudioFileWriter {
    writer: WavWriter<BufWriter<std::fs::File>>,
    sample_rate: u32,
}

impl AudioFileWriter {
    pub fn new(path: &Path, sample_rate: u32) -> Result<Self> {
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let file = std::fs::File::create(path)
            .with_context(|| format!("failed to create WAV file: {}", path.display()))?;
        let writer = WavWriter::new(BufWriter::new(file), spec)?;
        Ok(Self {
            writer,
            sample_rate,
        })
    }

    /// Write a chunk of audio samples.
    pub fn write_chunk(&mut self, chunk: &AudioChunk) -> Result<()> {
        for &sample in &chunk.samples {
            self.writer.write_sample(sample)?;
        }
        Ok(())
    }

    /// Finalize the WAV file.
    pub fn finalize(self) -> Result<()> {
        self.writer.finalize()?;
        Ok(())
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Resample audio from `from_rate` to `to_rate` (mono).
pub fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }

    let chunk_size = 1024;
    let mut resampler = FftFixedIn::<f32>::new(
        from_rate as usize,
        to_rate as usize,
        chunk_size,
        1, // sub_chunks
        1, // channels (mono)
    )?;

    let mut output = Vec::new();

    for input_chunk in samples.chunks(chunk_size) {
        // Pad last chunk if needed
        let mut padded;
        let input = if input_chunk.len() < chunk_size {
            padded = input_chunk.to_vec();
            padded.resize(chunk_size, 0.0);
            &padded
        } else {
            input_chunk
        };

        let result = resampler.process(&[input], None)?;
        if let Some(channel) = result.first() {
            output.extend_from_slice(channel);
        }
    }

    Ok(output)
}

/// Read a WAV file and return mono f32 samples + sample rate.
pub fn read_wav(path: &Path) -> Result<(Vec<f32>, u32)> {
    let reader =
        hound::WavReader::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let spec = reader.spec();
    let sample_rate = spec.sample_rate;
    let channels = spec.channels as usize;

    let samples: Vec<f32> = match spec.sample_format {
        SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<Result<Vec<f32>, _>>()?,
        SampleFormat::Int => {
            let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .collect::<Result<Vec<i32>, _>>()?
                .into_iter()
                .map(|s| s as f32 / max_val)
                .collect()
        }
    };

    // Downmix to mono if stereo+
    let mono = if channels > 1 {
        samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    } else {
        samples
    };

    Ok((mono, sample_rate))
}
