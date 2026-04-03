//! Export session state machine.
//!
//! Manages frame counting, progress tracking, audio offset calculation,
//! and encoder lifecycle. The frame loop itself lives in `manifold-app`
//! (ContentThread) because it needs access to the engine and pipeline.

use std::fmt;

use manifold_core::tempo::{TempoMap, TempoMapConverter};
use manifold_core::units::{Beats, Bpm};

use crate::export_config::ExportConfig;

#[cfg(target_os = "macos")]
use crate::metal_encoder::{EncoderError, MetalEncoder};

/// Result of a completed export.
#[derive(Debug)]
pub struct ExportResult {
    pub output_path: String,
    pub frames_encoded: u32,
    pub duration_seconds: f32,
}

/// Errors that can occur during export.
#[derive(Debug)]
pub enum ExportError {
    /// No clips in the export range.
    NoContent,
    /// Export range is invalid (start >= end).
    InvalidRange { start: f32, end: f32 },
    /// Metal encoder error.
    #[cfg(target_os = "macos")]
    Encoder(EncoderError),
    /// Audio mux error.
    AudioMux(String),
    /// Export was cancelled by user.
    Cancelled,
    /// Platform not supported for native encoding.
    #[allow(dead_code)]
    UnsupportedPlatform,
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoContent => write!(f, "no content in export range"),
            Self::InvalidRange { start, end } => {
                write!(f, "invalid export range: {start} >= {end}")
            }
            #[cfg(target_os = "macos")]
            Self::Encoder(e) => write!(f, "encoder error: {e}"),
            Self::AudioMux(msg) => write!(f, "audio mux error: {msg}"),
            Self::Cancelled => write!(f, "export cancelled"),
            Self::UnsupportedPlatform => write!(f, "native encoding not supported"),
        }
    }
}

#[cfg(target_os = "macos")]
impl From<EncoderError> for ExportError {
    fn from(e: EncoderError) -> Self {
        Self::Encoder(e)
    }
}

/// Manages the state of an in-progress video export.
///
/// Created with a config and tempo map, calculates frame counts and timing,
/// wraps the native encoder, and tracks progress.
#[cfg(target_os = "macos")]
pub struct ExportSession {
    config: ExportConfig,
    encoder: MetalEncoder,
    total_frames: u32,
    start_seconds: f32,
    end_seconds: f32,
    audio_offset_seconds: f32,
    cancelled: bool,
}

#[cfg(target_os = "macos")]
impl ExportSession {
    /// Create a new export session.
    ///
    /// Computes the export range in seconds, calculates total frame count,
    /// and initializes the native Metal encoder.
    ///
    /// - `config`: export parameters
    /// - `bpm`: project BPM (fallback for tempo map)
    /// - `tempo_map`: project tempo map for beat->seconds conversion
    pub fn new(
        config: ExportConfig,
        bpm: f32,
        tempo_map: &mut TempoMap,
    ) -> Result<Self, ExportError> {
        let fallback = Bpm(bpm);
        let start_seconds = TempoMapConverter::beat_to_seconds(
            tempo_map,
            Beats::from_f32(config.start_beat),
            fallback,
        )
        .as_f32();
        let end_seconds = TempoMapConverter::beat_to_seconds(
            tempo_map,
            Beats::from_f32(config.end_beat),
            fallback,
        )
        .as_f32();

        if end_seconds <= start_seconds {
            return Err(ExportError::InvalidRange {
                start: config.start_beat,
                end: config.end_beat,
            });
        }

        let duration = end_seconds - start_seconds;
        let total_frames = (duration * config.fps).round() as u32;

        if total_frames == 0 {
            return Err(ExportError::NoContent);
        }

        // Calculate audio offset for post-mux.
        // Matches Unity VideoExporter audio offset logic.
        let audio_offset_seconds = if config.has_audio() {
            let audio_start_seconds = TempoMapConverter::beat_to_seconds(
                tempo_map,
                Beats::from_f32(config.audio_start_beat),
                fallback,
            )
            .as_f32();
            audio_start_seconds - start_seconds - config.audio_encoder_delay
        } else {
            0.0
        };

        // Determine output path for encoder:
        // If audio will be muxed, encode to a temp file first.
        let encoder_output = if config.has_audio() {
            format!("{}.video_only.mp4", config.output_path)
        } else {
            config.output_path.clone()
        };

        let encoder = Self::create_encoder(&config, &encoder_output, None)?;

        log::info!(
            "[ExportSession] {} frames, {:.2}s, beats {:.1}-{:.1}, audio_offset={:.3}s",
            total_frames,
            duration,
            config.start_beat,
            config.end_beat,
            audio_offset_seconds,
        );

        Ok(Self {
            config,
            encoder,
            total_frames,
            start_seconds,
            end_seconds,
            audio_offset_seconds,
            cancelled: false,
        })
    }

    /// Encode a single frame. The caller provides the raw Metal texture pointer.
    ///
    /// # Safety
    /// `metal_texture_ptr` must be a valid `id<MTLTexture>`.
    pub unsafe fn encode_frame(
        &mut self,
        metal_texture_ptr: *mut std::ffi::c_void,
    ) -> Result<(), ExportError> {
        if self.cancelled {
            return Err(ExportError::Cancelled);
        }
        unsafe { self.encoder.encode_frame(metal_texture_ptr)? };
        Ok(())
    }

    /// Create an export session using the content pipeline's Metal device.
    ///
    /// Same as `new()` but shares the device to avoid cross-device GPU sync.
    ///
    /// # Safety
    /// `device_ptr` must be a valid `id<MTLDevice>` that outlives the session.
    pub unsafe fn new_with_device(
        config: ExportConfig,
        bpm: f32,
        tempo_map: &mut TempoMap,
        device_ptr: *mut std::ffi::c_void,
    ) -> Result<Self, ExportError> {
        let fallback = Bpm(bpm);
        let start_seconds = TempoMapConverter::beat_to_seconds(
            tempo_map,
            Beats::from_f32(config.start_beat),
            fallback,
        )
        .as_f32();
        let end_seconds = TempoMapConverter::beat_to_seconds(
            tempo_map,
            Beats::from_f32(config.end_beat),
            fallback,
        )
        .as_f32();

        if end_seconds <= start_seconds {
            return Err(ExportError::InvalidRange {
                start: config.start_beat,
                end: config.end_beat,
            });
        }

        let duration = end_seconds - start_seconds;
        let total_frames = (duration * config.fps).round() as u32;

        if total_frames == 0 {
            return Err(ExportError::NoContent);
        }

        let audio_offset_seconds = if config.has_audio() {
            let audio_start_seconds = TempoMapConverter::beat_to_seconds(
                tempo_map,
                Beats::from_f32(config.audio_start_beat),
                fallback,
            )
            .as_f32();
            audio_start_seconds - start_seconds - config.audio_encoder_delay
        } else {
            0.0
        };

        let encoder_output = if config.has_audio() {
            format!("{}.video_only.mp4", config.output_path)
        } else {
            config.output_path.clone()
        };

        let encoder = Self::create_encoder(&config, &encoder_output, Some(device_ptr))?;

        log::info!(
            "[ExportSession] {} frames, {:.2}s, beats {:.1}-{:.1}, \
             audio_offset={:.3}s (shared device)",
            total_frames,
            duration,
            config.start_beat,
            config.end_beat,
            audio_offset_seconds,
        );

        Ok(Self {
            config,
            encoder,
            total_frames,
            start_seconds,
            end_seconds,
            audio_offset_seconds,
            cancelled: false,
        })
    }

    /// Create the Metal encoder, optionally sharing an external device.
    fn create_encoder(
        config: &ExportConfig,
        encoder_output: &str,
        device_ptr: Option<*mut std::ffi::c_void>,
    ) -> Result<MetalEncoder, EncoderError> {
        if let Some(ptr) = device_ptr {
            unsafe {
                MetalEncoder::new_with_device(
                    config.width,
                    config.height,
                    config.fps,
                    encoder_output,
                    config.hdr,
                    ptr,
                )
            }
        } else {
            MetalEncoder::new(
                config.width,
                config.height,
                config.fps,
                encoder_output,
                config.hdr,
            )
        }
    }

    /// Export progress as a fraction (0.0..1.0).
    pub fn progress(&self) -> f32 {
        if self.total_frames == 0 {
            return 1.0;
        }
        self.encoder.frames_encoded() as f32 / self.total_frames as f32
    }

    /// Formatted status string (e.g. "Exporting 120/600 (20%)").
    pub fn status_text(&self) -> String {
        let pct = (self.progress() * 100.0).round() as u32;
        format!(
            "Exporting {}/{} ({}%)",
            self.encoder.frames_encoded(),
            self.total_frames,
            pct,
        )
    }

    /// Whether all frames have been encoded.
    pub fn is_complete(&self) -> bool {
        self.encoder.frames_encoded() >= self.total_frames
    }

    /// Request cancellation. The next encode_frame() call will return Cancelled.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Finalize the export: close encoder, optionally mux audio.
    ///
    /// `ffmpeg_path`: required if audio muxing is needed.
    pub fn finalize(self, ffmpeg_path: Option<&str>) -> Result<ExportResult, ExportError> {
        let frames_encoded = self.encoder.frames_encoded();
        let duration = self.end_seconds - self.start_seconds;
        let config = self.config.clone();
        let audio_offset = self.audio_offset_seconds;

        // Determine the path the encoder wrote to
        let encoder_output = if config.has_audio() {
            format!("{}.video_only.mp4", config.output_path)
        } else {
            config.output_path.clone()
        };

        // Finalize the encoder (writes MP4 trailer)
        self.encoder.end_session()?;

        // Post-mux audio if needed
        if config.has_audio() {
            let audio_path = config.audio_path.as_ref().unwrap();
            let ffmpeg =
                ffmpeg_path.ok_or_else(|| ExportError::AudioMux("ffmpeg not found".to_string()))?;

            crate::audio_muxer::AudioMuxer::mux(
                ffmpeg,
                &encoder_output,
                audio_path,
                &config.output_path,
                audio_offset,
            )
            .map_err(|e| ExportError::AudioMux(e.to_string()))?;

            // Clean up temp video-only file
            let _ = std::fs::remove_file(&encoder_output);
        }

        log::info!(
            "[ExportSession] Export complete: {} frames, {:.2}s -> {}",
            frames_encoded,
            duration,
            config.output_path,
        );

        Ok(ExportResult {
            output_path: config.output_path,
            frames_encoded,
            duration_seconds: duration,
        })
    }

    /// Fixed frame delta for the engine tick (1.0 / fps).
    pub fn frame_delta(&self) -> f64 {
        1.0 / self.config.fps as f64
    }

    pub fn total_frames(&self) -> u32 {
        self.total_frames
    }

    pub fn frames_encoded(&self) -> u32 {
        self.encoder.frames_encoded()
    }

    pub fn start_beat(&self) -> f32 {
        self.config.start_beat
    }
}
