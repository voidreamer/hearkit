use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::AudioChunk;

/// Trait for audio capture sources.
/// Note: not requiring Send because cpal::Stream is !Send on macOS.
pub trait AudioSource {
    fn start(&mut self, sender: Sender<AudioChunk>) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn sample_rate(&self) -> u32;
}

/// Captures audio from the default microphone via cpal.
pub struct MicCapture {
    stream: Option<cpal::Stream>,
    sample_rate: u32,
    running: Arc<AtomicBool>,
}

impl MicCapture {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no input device available")?;
        let config = device.default_input_config()?;

        Ok(Self {
            stream: None,
            sample_rate: config.sample_rate().0,
            running: Arc::new(AtomicBool::new(false)),
        })
    }
}

impl AudioSource for MicCapture {
    fn start(&mut self, sender: Sender<AudioChunk>) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no input device available")?;
        let supported = device.default_input_config()?;
        let channels = supported.channels() as usize;
        self.sample_rate = supported.sample_rate().0;
        let sample_rate = self.sample_rate;

        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: supported.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        let running = self.running.clone();
        running.store(true, Ordering::SeqCst);
        let start_time = Instant::now();

        let stream = match supported.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !running.load(Ordering::SeqCst) {
                        return;
                    }
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                        .collect();
                    let chunk = AudioChunk {
                        samples: mono,
                        sample_rate,
                        timestamp: start_time.elapsed(),
                    };
                    let _ = sender.send(chunk);
                },
                |err| tracing::error!("mic stream error: {err}"),
                None,
            )?,
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    if !running.load(Ordering::SeqCst) {
                        return;
                    }
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| {
                            frame.iter().map(|&s| s as f32 / i16::MAX as f32).sum::<f32>()
                                / channels as f32
                        })
                        .collect();
                    let chunk = AudioChunk {
                        samples: mono,
                        sample_rate,
                        timestamp: start_time.elapsed(),
                    };
                    let _ = sender.send(chunk);
                },
                |err| tracing::error!("mic stream error: {err}"),
                None,
            )?,
            format => anyhow::bail!("unsupported sample format: {format:?}"),
        };

        stream.play()?;
        self.stream = Some(stream);
        tracing::info!("mic capture started at {sample_rate}Hz");
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        self.stream.take();
        tracing::info!("mic capture stopped");
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Captures system audio (macOS only via ScreenCaptureKit).
#[cfg(target_os = "macos")]
pub mod system {
    use super::*;
    use screencapturekit::cm_sample_buffer::CMSampleBuffer;
    use screencapturekit::sc_content_filter::{InitParams, SCContentFilter};
    use screencapturekit::sc_error_handler::StreamErrorHandler;
    use screencapturekit::sc_output_handler::{SCStreamOutputType, StreamOutput};
    use screencapturekit::sc_shareable_content::SCShareableContent;
    use screencapturekit::sc_stream::SCStream;
    use screencapturekit::sc_stream_configuration::SCStreamConfiguration;
    use std::sync::Mutex;

    struct ErrorHandler;

    impl StreamErrorHandler for ErrorHandler {
        fn on_error(&self) {
            tracing::error!("system audio stream error");
        }
    }

    struct AudioOutputHandler {
        sender: Mutex<Sender<AudioChunk>>,
        start_time: Instant,
        sample_rate: u32,
    }

    impl StreamOutput for AudioOutputHandler {
        fn did_output_sample_buffer(
            &self,
            sample_buffer: CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            if let SCStreamOutputType::Audio = of_type {
                let audio_buffers = sample_buffer.sys_ref.get_av_audio_buffer_list();
                let sample_rate = self.sample_rate;

                for buffer in audio_buffers {
                    let channels = buffer.number_channels as usize;
                    if channels == 0 || buffer.data.is_empty() {
                        continue;
                    }
                    // Data is raw PCM bytes (f32 samples). Convert from &[u8] to &[f32].
                    let float_samples: Vec<f32> = buffer
                        .data
                        .chunks_exact(4)
                        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                        .collect();
                    // Downmix to mono
                    let mono: Vec<f32> = if channels > 1 {
                        float_samples
                            .chunks(channels)
                            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                            .collect()
                    } else {
                        float_samples
                    };
                    let chunk = AudioChunk {
                        samples: mono,
                        sample_rate,
                        timestamp: self.start_time.elapsed(),
                    };
                    let _ = self.sender.lock().unwrap().send(chunk);
                }
            }
        }
    }

    pub struct SystemAudioCapture {
        stream: Option<SCStream>,
        sample_rate: u32,
    }

    impl SystemAudioCapture {
        pub fn new(sample_rate: u32) -> Result<Self> {
            Ok(Self {
                stream: None,
                sample_rate,
            })
        }
    }

    impl AudioSource for SystemAudioCapture {
        fn start(&mut self, sender: Sender<AudioChunk>) -> Result<()> {
            let content = SCShareableContent::current();
            let display = content
                .displays
                .first()
                .context("no display found")?
                .clone();

            let filter = SCContentFilter::new(InitParams::Display(display));

            let config = SCStreamConfiguration {
                captures_audio: true,
                excludes_current_process_audio: false,
                channel_count: 2,
                sample_rate: self.sample_rate,
                ..Default::default()
            };

            let mut stream = SCStream::new(filter, config, ErrorHandler);

            let output_handler = AudioOutputHandler {
                sender: Mutex::new(sender),
                start_time: Instant::now(),
                sample_rate: self.sample_rate,
            };
            stream.add_output(output_handler, SCStreamOutputType::Audio);
            stream
                .start_capture()
                .map_err(|e| anyhow::anyhow!("failed to start system audio capture: {e}"))?;
            self.stream = Some(stream);
            tracing::info!("system audio capture started at {}Hz", self.sample_rate);
            Ok(())
        }

        fn stop(&mut self) -> Result<()> {
            if let Some(stream) = self.stream.take() {
                stream
                    .stop_capture()
                    .map_err(|e| anyhow::anyhow!("failed to stop system audio capture: {e}"))?;
            }
            tracing::info!("system audio capture stopped");
            Ok(())
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }
}
