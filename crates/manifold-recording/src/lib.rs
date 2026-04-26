//! Live recording for MANIFOLD.
//!
//! Captures the compositor output and optional audio input into a QuickTime
//! (.mov) file during live performance. Zero impact on the content thread —
//! recording runs on a dedicated thread with a pre-allocated texture pool.
//!
//! Codec: ProRes 422 Proxy for SDR (Apple Silicon ProRes HW encoder, reliable
//! over arbitrary durations). HEVC Main10 for HDR (lower per-second bitrate
//! avoids the HEVC encoder's sustained-4K60 malfunction threshold).
//!
//! # Architecture
//!
//! ```text
//! Content Thread              Core Audio Thread          Recording Thread
//! ├─ render frame             ├─ audio callback          ├─ recv video frame
//! ├─ blit → pool texture      │  push → ring buffer      │  wait GPU fence
//! ├─ submit to channel        │                          │  encode video
//! │                           │                          ├─ drain ring buffer
//! │                           │                          │  encode audio
//! ```

pub mod config;
mod ffi;
#[cfg(target_os = "macos")]
mod format_converter;
#[cfg(target_os = "macos")]
mod recording_thread;
#[cfg(target_os = "macos")]
mod session;
#[cfg(target_os = "macos")]
mod texture_pool;

pub use config::{AudioCodec, LiveRecordingConfig, RecordingResult};
#[cfg(target_os = "macos")]
pub use recording_thread::GpuFence;
#[cfg(target_os = "macos")]
pub use session::LiveRecordingSession;
