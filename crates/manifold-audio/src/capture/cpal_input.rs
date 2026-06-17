//! Audio **input-device** capture via `cpal` — the `CaptureBackend`
//! implementation for hardware, aggregate, and virtual input devices.
//!
//! Opens a system audio input device (e.g. BlackHole) and streams Float32
//! interleaved samples into a lock-free SPSC ring buffer. The ring buffer
//! consumer is handed downstream (the analysis worker, or the recording thread).
//! Tap-based capture (system / per-app output) lives in a sibling module behind
//! the same [`CaptureBackend`](super::CaptureBackend) trait — see [`super`].
//!
//! The `cpal` audio callback runs on a real-time OS thread — it must never
//! allocate, lock, or log. Only lock-free ring buffer writes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use ringbuf::HeapRb;
use ringbuf::traits::{Producer as ProducerTrait, Split};

use super::{AudioConsumer, CaptureBackend};

/// Information about an available audio input device.
#[derive(Clone, Debug)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

/// Configuration for audio capture.
#[derive(Clone, Debug, Default)]
pub struct AudioCaptureConfig {
    /// Device name to open. `None` = system default input.
    pub device_name: Option<String>,
}

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
    /// Create a new audio capture device. Uses the device's default input
    /// configuration (sample rate, channels) to avoid format mismatches.
    /// Does NOT start capturing until [`start()`] is called.
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

        // Query the device's default input config — use its native sample rate
        // and channel count to avoid format conversion issues.
        let default_config = device
            .default_input_config()
            .map_err(|e| format!("Failed to get default input config: {e}"))?;

        let sample_rate = default_config.sample_rate().0;
        let channels = default_config.channels();

        log::info!(
            "[AudioCapture] Device config: {sample_rate}Hz, {channels}ch, format={:?}",
            default_config.sample_format(),
        );

        let stream_config = StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        // Ring buffer: ~2 seconds of audio, but bounded so an exotic
        // high-channel aggregate (e.g. 64ch @ 96kHz ≈ 49MB) can't balloon the
        // allocation. The cap trims ring *duration*, never channels — the worker
        // drains every tick, so less headroom is harmless. Floored at ~0.25s.
        const MAX_RING_SAMPLES: usize = 4 * 1024 * 1024; // 16 MiB of f32
        let want = (sample_rate as usize) * (channels as usize) * 2;
        let floor = ((sample_rate as usize) * (channels as usize)) / 4;
        let capacity = want.min(MAX_RING_SAMPLES).max(floor).max(1);
        if capacity < want {
            log::warn!(
                "[AudioCapture] {channels}ch @ {sample_rate}Hz: ring capped to \
                 {capacity} samples (~{:.2}s) to bound memory",
                capacity as f32 / (sample_rate as f32 * channels as f32).max(1.0),
            );
        }
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
                        overflow_cb.fetch_add(1, Ordering::Relaxed);
                    }
                },
                move |err| {
                    log::error!("[AudioCapture] Stream error: {err}");
                },
                None,
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

    /// Sample rate of the capture stream (from device default config).
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of channels (from device default config).
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
        let default_name = host.default_input_device().and_then(|d| d.name().ok());

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

impl CaptureBackend for AudioCaptureDevice {
    fn sample_rate(&self) -> u32 {
        AudioCaptureDevice::sample_rate(self)
    }
    fn channels(&self) -> u16 {
        AudioCaptureDevice::channels(self)
    }
    fn take_consumer(&mut self) -> Option<AudioConsumer> {
        AudioCaptureDevice::take_consumer(self)
    }
    fn start(&self) -> Result<(), String> {
        AudioCaptureDevice::start(self)
    }
    fn stop(&self) {
        AudioCaptureDevice::stop(self)
    }
    fn overflow_count(&self) -> u64 {
        AudioCaptureDevice::overflow_count(self)
    }
}
