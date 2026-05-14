//! LiveRecordingSession — public API for the recording system.
//!
//! Ties together the texture pool, audio capture, native encoder, and recording
//! thread. The content thread interacts with this through a minimal surface:
//! acquire texture slot, blit, submit, stop.

use std::ffi::{CString, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{Sender, bounded};
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat};

use crate::config::{AudioCodec, LiveRecordingConfig, RecordingResult};
use crate::ffi;
use crate::format_converter::FormatConverter;
use crate::recording_thread::{self, RecordingFrame};
use crate::texture_pool::{DEFAULT_POOL_SIZE, PoolSlot, TextureRingPool};

/// Live recording session. Created on the content thread, owned by ContentPipeline.
///
/// The session manages:
/// - A pre-allocated texture pool for zero-allocation frame capture
/// - An optional audio capture device (via manifold-audio)
/// - A dedicated recording thread that encodes A/V to MP4
pub struct LiveRecordingSession {
    texture_pool: TextureRingPool,
    format_converter: FormatConverter,
    /// Wrapped in Option so Drop can take it to signal the recording thread.
    frame_tx: Option<Sender<RecordingFrame>>,
    recording_thread: Option<JoinHandle<(u32, u32)>>,
    stop: Arc<AtomicBool>,
    start_time: Instant,
    frames_submitted: u32,
    frames_dropped: u32,
    output_path: String,
    _audio_capture: Option<manifold_audio::capture::AudioCaptureDevice>,
}

unsafe impl Send for LiveRecordingSession {}

impl LiveRecordingSession {
    /// Create and start a new live recording session.
    ///
    /// Allocates the texture pool, opens audio capture (if configured),
    /// creates the native AVAssetWriter encoder, and spawns the recording thread.
    pub fn new(
        config: LiveRecordingConfig,
        device: &GpuDevice,
        width: u32,
        height: u32,
        fps: f32,
    ) -> Result<Self, String> {
        let device_ptr = device.raw_device_ptr();
        let output_path = config.output_path.clone();

        // -- Texture pool (Bgra8Unorm — format conversion done in content thread) --
        let pool_format = GpuTextureFormat::Bgra8Unorm;
        let texture_pool =
            TextureRingPool::new(device, width, height, pool_format, DEFAULT_POOL_SIZE);
        let format_converter = FormatConverter::new(device);

        // -- Audio capture (optional) --
        let (audio_capture, audio_consumer, sample_rate, channels) =
            if let Some(ref device_name) = config.audio_device {
                let audio_config = manifold_audio::capture::AudioCaptureConfig {
                    device_name: Some(device_name.clone()),
                };
                match manifold_audio::capture::AudioCaptureDevice::new(audio_config) {
                    Ok(mut capture) => {
                        let sr = capture.sample_rate();
                        let ch = capture.channels();
                        let consumer = capture.take_consumer();
                        capture
                            .start()
                            .map_err(|e| format!("Failed to start audio capture: {e}"))?;
                        (Some(capture), consumer, sr, ch)
                    }
                    Err(e) => {
                        log::warn!(
                            "[LiveRecording] Audio device '{device_name}' unavailable: {e}. \
                             Recording video only."
                        );
                        (None, None, 0, 0)
                    }
                }
            } else {
                (None, None, 0, 0)
            };

        // -- Native encoder --
        let c_path =
            CString::new(output_path.as_str()).map_err(|_| "Invalid output path".to_string())?;

        let audio_codec_int = match config.audio_codec {
            AudioCodec::Aac => 0,
            AudioCodec::Alac => 1,
        };

        let encoder_handle = unsafe {
            ffi::LiveRecorder_Create(
                width as i32,
                height as i32,
                fps,
                c_path.as_ptr(),
                i32::from(config.hdr),
                device_ptr,
                sample_rate as i32,
                channels as i32,
                audio_codec_int,
            )
        };

        if encoder_handle.is_null() {
            return Err("Failed to create native live recorder".into());
        }

        // -- Recording thread --
        let (frame_tx, frame_rx) = bounded::<RecordingFrame>(DEFAULT_POOL_SIZE * 2);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let start_time = Instant::now();

        let encoder_ptr = encoder_handle as usize; // Send-safe integer

        let recording_thread = std::thread::Builder::new()
            .name("manifold-recording".into())
            .spawn(move || {
                let handle = encoder_ptr as *mut c_void;
                recording_thread::run(
                    frame_rx,
                    audio_consumer,
                    handle,
                    sample_rate,
                    channels,
                    stop_clone,
                    start_time,
                )
            })
            .map_err(|e| format!("Failed to spawn recording thread: {e}"))?;

        log::info!(
            "[LiveRecording] Session started: {width}x{height} @ {fps}fps, pool={}, audio={}",
            DEFAULT_POOL_SIZE,
            audio_capture.is_some(),
        );

        Ok(Self {
            texture_pool,
            format_converter,
            frame_tx: Some(frame_tx),
            recording_thread: Some(recording_thread),
            stop,
            start_time,
            frames_submitted: 0,
            frames_dropped: 0,
            output_path,
            _audio_capture: audio_capture,
        })
    }

    /// Try to acquire a pool texture for recording.
    ///
    /// Returns `(texture_index, pool_slot, gpu_fence)`:
    /// - `texture_index`: use with [`pool_texture()`] to get the blit destination
    /// - `pool_slot`: send to the recording thread after blitting
    /// - `gpu_fence`: the content thread's completion handler must set this to `true`
    ///
    /// Returns `None` if the pool is exhausted (drop this frame).
    pub fn acquire_texture(
        &mut self,
    ) -> Option<(usize, PoolSlot, Arc<crate::recording_thread::GpuFence>)> {
        if let Some((idx, slot)) = self.texture_pool.try_acquire() {
            let fence = Arc::new(crate::recording_thread::GpuFence::new());
            Some((idx, slot, fence))
        } else {
            None
        }
    }

    /// Get a reference to the pool texture at the given index.
    pub fn pool_texture(&self, index: usize) -> &GpuTexture {
        self.texture_pool.texture(index)
    }

    /// Encode the format conversion (Rgba16Float → sRGB Bgra8Unorm) into
    /// the content thread's GpuEncoder. Must be called in the same command
    /// buffer as the IOSurface blit.
    pub fn encode_format_conversion(
        &self,
        encoder: &mut manifold_gpu::GpuEncoder,
        source: &GpuTexture,
        dest: &GpuTexture,
    ) {
        self.format_converter.encode(encoder, source, dest);
    }

    /// Submit a frame to the recording thread. Non-blocking.
    pub fn submit_frame(
        &mut self,
        pool_slot: PoolSlot,
        fence: Arc<crate::recording_thread::GpuFence>,
    ) {
        let Some(ref frame_tx) = self.frame_tx else {
            pool_slot.release();
            self.frames_dropped += 1;
            return;
        };

        let frame = RecordingFrame {
            pool_slot,
            wall_timestamp: Instant::now(),
            gpu_complete: fence,
        };

        match frame_tx.try_send(frame) {
            Ok(()) => {
                self.frames_submitted += 1;
            }
            Err(crossbeam_channel::TrySendError::Full(frame)) => {
                frame.pool_slot.release();
                self.frames_dropped += 1;
                log::warn!(
                    "[LiveRecording] Frame channel full, dropped (total: {})",
                    self.frames_dropped,
                );
            }
            Err(crossbeam_channel::TrySendError::Disconnected(frame)) => {
                frame.pool_slot.release();
                self.frames_dropped += 1;
                log::error!("[LiveRecording] Recording thread disconnected");
            }
        }
    }

    /// Record that a frame was dropped due to pool exhaustion.
    pub fn record_dropped_frame(&mut self) {
        self.frames_dropped += 1;
    }

    /// Number of frames dropped since recording started.
    pub fn frames_dropped(&self) -> u32 {
        self.frames_dropped
    }

    /// Whether the session is active.
    pub fn is_active(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    /// Stop recording, drain remaining frames, finalize the MP4.
    pub fn stop(mut self) -> RecordingResult {
        let (frames_encoded, frames_failed) = self.shutdown();

        let duration = self.start_time.elapsed().as_secs_f64();
        let _ = frames_failed;

        log::info!(
            "[LiveRecording] Stopped: {frames_encoded} encoded, {} dropped, \
             {duration:.1}s -> {}",
            self.frames_dropped,
            self.output_path,
        );

        RecordingResult {
            output_path: self.output_path.clone(),
            frames_recorded: frames_encoded,
            frames_dropped: self.frames_dropped,
            duration_seconds: duration,
        }
    }

    /// Signal the recording thread to stop, drop the channel, and join.
    /// Safe to call multiple times (idempotent). Returns (encoded, failed).
    fn shutdown(&mut self) -> (u32, u32) {
        self.stop.store(true, Ordering::Release);

        // Drop the sender so the recording thread sees disconnection after draining.
        self.frame_tx.take();

        if let Some(thread) = self.recording_thread.take() {
            // Give the recording thread time to finalize (up to 10s).
            // The native encoder has a 30s internal timeout, but we don't want
            // to block the content thread indefinitely on shutdown.
            match thread.join() {
                Ok(stats) => return stats,
                Err(_) => {
                    log::error!("[LiveRecording] Recording thread panicked");
                }
            }
        }
        (0, 0)
    }
}

impl Drop for LiveRecordingSession {
    fn drop(&mut self) {
        if self.recording_thread.is_some() {
            log::warn!(
                "[LiveRecording] Session dropped without stop() — \
                 shutting down recording thread"
            );
            self.shutdown();
        }
    }
}
