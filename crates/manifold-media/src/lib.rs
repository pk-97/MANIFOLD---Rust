//! Video encoding, decoding, and media pipeline for MANIFOLD.
//!
//! Provides native Metal GPU encoding (macOS) for offline video export,
//! video decode (VideoToolbox on the Mac, FFmpeg elsewhere),
//! FFmpeg-based audio muxing, and export session orchestration.

pub mod audio_muxer;
pub mod decode_scheduler;
pub mod decoder;
#[cfg(target_os = "macos")]
mod decoder_ffi;
pub mod encoder;
pub mod export_config;
pub mod export_session;
#[cfg(not(target_os = "macos"))]
mod ffmpeg_decoder;
#[cfg(not(target_os = "macos"))]
mod ffmpeg_encoder;
pub mod frame_rate;
pub mod image_renderer;
pub mod metadata;
#[cfg(target_os = "macos")]
pub mod metal_encoder;
#[cfg(target_os = "macos")]
mod metal_ffi;
pub mod still_exporter;
pub mod video_renderer;
#[cfg(target_os = "macos")]
mod videotoolbox;
