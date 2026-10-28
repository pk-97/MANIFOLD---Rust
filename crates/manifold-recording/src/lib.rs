//! Live recording for MANIFOLD.
//!
//! Captures the compositor output and optional audio input into a QuickTime
//! (.mov) file during live performance. Zero impact on the content thread —
//! recording runs on a dedicated thread with a pre-allocated texture pool.
//!
//! Codec on the Mac: ProRes 422 Proxy for SDR (Apple Silicon ProRes HW
//! encoder, reliable over arbitrary durations). HEVC Main10 for HDR (lower
//! per-second bitrate avoids the HEVC encoder's sustained-4K60 malfunction
//! threshold). Elsewhere: H.264 through FFmpeg, SDR only.
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
#[cfg(target_os = "macos")]
mod ffi;
#[cfg(not(target_os = "macos"))]
mod ffmpeg_recorder;
mod format_converter;
/// Feature-gated proof-harness support (pattern writer + ffprobe oracle).
#[cfg(all(target_os = "macos", feature = "recording-proofs"))]
pub mod proofs;
mod recording_thread;
mod session;
mod texture_pool;

// This platform's recorder: AVAssetWriter on the Mac, FFmpeg elsewhere.
#[cfg(target_os = "macos")]
use ffi::{AudioDrops, NativeRecorder as Recorder};
#[cfg(not(target_os = "macos"))]
use ffmpeg_recorder::{AudioDrops, FfmpegRecorder as Recorder};

pub use config::{AudioCodec, LiveRecordingConfig, RecordingResult};
pub use recording_thread::GpuFence;
pub use session::{AudioFeed, LiveRecordingSession};
