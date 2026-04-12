//! Audio input capture via `cpal`.
//!
//! Opens a system audio input device (e.g. BlackHole) and streams Float32
//! interleaved samples into a lock-free SPSC ring buffer. The ring buffer
//! consumer is handed to the recording thread for muxing into the MP4.
//!
//! The `cpal` audio callback runs on a real-time OS thread — it must never
//! allocate, lock, or log. Only lock-free ring buffer writes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use ringbuf::traits::{Producer as ProducerTrait, Split};
use ringbuf::HeapRb;

/// Information about an available audio input device.
#[derive(Clone, Debug)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Configuration for audio capture.
#[derive(Clone, Debug)]
pub struct AudioCaptureConfig {
    /// Device name to open. `None` = system default input.
    pub device_name: Option<String>,
    /// Desired sample rate. Falls back to device default if unsupported.
    pub sample_rate: u32,
    /// Number of channels (typically 2 for stereo).
    pub channels: u16,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: 48_000,
            channels: 2,
        }
    }
}

/// Ring buffer consumer type for reading captured audio samples.
pub type AudioConsumer = ringbuf::HeapCons<f32>;

/// Captures audio from a system input device into a lock-free ring buffer.
pub struct AudioCaptureDevice {
    stream: Stream,
    consumer: Option<AudioConsumer>,
    sample_rate: u32,
    channels: u16,
    running: Arc<AtomicBool>,
    overflow_count: Arc<std::sync::atomic::AtomicU64>,
}

// cpal::Stream is !Send by default on some platforms but we control the lifecycle.
unsafe impl Send for AudioCaptureDevice {}

impl AudioCaptureDevice {
    /// Create a new audio capture device. Does NOT start capturing until
    /// [`start()`] is called.
    pub fn new(config: AudioCaptureConfig) -> Result<Self, String> {
        let host = cpal::default_host();

        let device = if let Some(ref name) = config.device_name {
            host.input_devices()
                .map_err(|e| format!("Failed to enumerate input devices: {e}"))?
                .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
                .ok_or_else(|| format!("Audio input device not found: {name}"))?
        } else {
            host.default_input_device()
                .ok_or_else(|| "No default audio input device".to_string())?
        };

        let device_name = device.name().unwrap_or_else(|_| "Unknown".into());
        log::info!("[AudioCapture] Using device: {device_name}");

        let sample_rate = config.sample_rate;
        let channels = config.channels;

        let stream_config = StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Ring buffer: 2 seconds of audio. Generous headroom for the recording
        // thread's drain rate.
        let capacity = (sample_rate as usize) * (channels as usize) * 2;
        let ring = HeapRb::<f32>::new(capacity);
        let (mut producer, consumer) = ring.split();

        let running = Arc::new(AtomicBool::new(false));
        let running_cb = running.clone();
        let overflow_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let overflow_cb = overflow_count.clone();

        // Build the input stream. The callback runs on a real-time OS thread.
        // RULES: no alloc, no lock, no log, no panic. Only ring buffer writes.
        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                    if !running_cb.load(Ordering::Relaxed) {
                        return;
                    }
                    let written = producer.push_slice(data);
                    if written < data.len() {
                        // Overflow — recording thread can't keep up. Count it
                        // but never block. Lost samples create a tiny glitch;
                        // blocking the audio thread would cause system-wide dropout.
                        overflow_cb.fetch_add(1, Ordering::Relaxed);
                    }
                },
                move |err| {
                    // Error callback — can't do much here on a real-time thread.
                    // The recording session will detect the stream stopping.
                    log::error!("[AudioCapture] Stream error: {err}");
                },
                None, // no timeout
            )
            .map_err(|e| format!("Failed to build input stream: {e}"))?;

        log::info!(
            "[AudioCapture] Stream configured: {}Hz, {}ch, ring={}",
            sample_rate,
            channels,
            capacity,
        );

        Ok(Self {
            stream,
            consumer: Some(consumer),
            sample_rate,
            channels,
            running,
            overflow_count,
        })
    }

    /// Start capturing audio. Samples begin flowing into the ring buffer.
    pub fn start(&self) -> Result<(), String> {
        self.running.store(true, Ordering::Release);
        self.stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {e}"))?;
        log::info!("[AudioCapture] Capture started");
        Ok(())
    }

    /// Stop capturing audio. The stream is paused but can be restarted.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
        let _ = self.stream.pause();
        log::info!("[AudioCapture] Capture stopped");
    }

    /// Take the ring buffer consumer. Can only be called once — the consumer
    /// is handed to the recording thread.
    pub fn take_consumer(&mut self) -> Option<AudioConsumer> {
        self.consumer.take()
    }

    /// Sample rate of the capture stream.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of channels.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Number of ring buffer overflow events since creation.
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count.load(Ordering::Relaxed)
    }

    /// Enumerate available audio input devices.
    pub fn list_devices() -> Vec<AudioDeviceInfo> {
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|d| d.name().ok());

        let mut devices = Vec::new();
        if let Ok(inputs) = host.input_devices() {
            for device in inputs {
                if let Ok(name) = device.name() {
                    let is_default = default_name.as_deref() == Some(&name);
                    devices.push(AudioDeviceInfo { name, is_default });
                }
            }
        }
        devices
    }
}

impl Drop for AudioCaptureDevice {
    fn drop(&mut self) {
        self.stop();
    }
}
