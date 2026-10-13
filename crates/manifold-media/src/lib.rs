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
pub mod frame_rate;
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

/// The native decoder and encoder make their own Metal device, so in this
/// crate's unit tests they take the machine-wide GPU queue first, exactly as
/// `GpuDevice::new` does in manifold-gpu's own tests. The live app never
/// queues. `manifold-gpu/tests/queued_device_audit.rs` checks every
/// device-making FFI call comes after this.
#[cfg(target_os = "macos")]
#[inline]
pub(crate) fn admit_native_metal() {
    #[cfg(test)]
    manifold_gpu::queue::acquire_for_process("manifold-media native Metal tests");
}
