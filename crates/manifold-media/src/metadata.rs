//! Video file metadata extraction.
//!
//! Probes video files for duration and resolution through the platform
//! decoder.

/// Video file metadata from a quick probe.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub duration: f32,
    pub width: i32,
    pub height: i32,
}

/// Supported video file extensions.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[".mp4", ".mov", ".webm", ".avi"];

/// Probe video file metadata through the platform decoder: AVAsset on the
/// Mac (about 1 ms per file), FFmpeg elsewhere. Thread-safe.
pub fn probe_video_metadata(path: &str) -> Option<ProbeResult> {
    let meta = crate::decoder::probe_metadata(path)?;
    Some(ProbeResult {
        duration: meta.duration,
        width: meta.width,
        height: meta.height,
    })
}
