//! Video encoding, decoding, and media pipeline for MANIFOLD.
//!
//! Provides native Metal GPU encoding (macOS) for offline video export,
//! native VideoToolbox hardware decode for video playback,
//! FFmpeg-based audio muxing, and export session orchestration.

pub mod audio_muxer;
#[cfg(target_os = "macos")]
pub mod decode_scheduler;
#[cfg(target_os = "macos")]
pub mod decoder;
#[cfg(target_os = "macos")]
mod decoder_ffi;
pub mod export_config;
pub mod export_session;
#[cfg(target_os = "macos")]
pub mod image_renderer;
pub mod metadata;
#[cfg(target_os = "macos")]
pub mod metal_encoder;
pub mod still_exporter;
#[cfg(target_os = "macos")]
mod metal_ffi;
#[cfg(target_os = "macos")]
pub mod video_renderer;
