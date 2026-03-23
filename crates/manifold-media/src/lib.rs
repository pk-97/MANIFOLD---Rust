//! Video encoding, decoding, and media pipeline for MANIFOLD.
//!
//! Provides native Metal GPU encoding (macOS) for offline video export,
//! FFmpeg-based audio muxing, and export session orchestration.

pub mod audio_muxer;
pub mod export_config;
pub mod export_session;
#[cfg(target_os = "macos")]
pub mod metal_encoder;
#[cfg(target_os = "macos")]
mod metal_ffi;
