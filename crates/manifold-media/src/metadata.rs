//! Video file metadata extraction.
//!
//! Probes video files for duration, resolution, and codec information
//! using native AVAsset (macOS) for fast, thread-safe metadata access.

/// Video file metadata from a quick probe.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub duration: f32,
    pub width: i32,
    pub height: i32,
}

/// Supported video file extensions.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[".mp4", ".mov", ".webm", ".avi"];

/// Probe video file metadata using native AVAsset.
/// Fast (~1ms per file), thread-safe: creates a local AVAsset, reads properties, releases.
#[cfg(target_os = "macos")]
pub fn probe_video_metadata(path: &str) -> Option<ProbeResult> {
    let meta = crate::decoder::DecoderPool::probe_metadata(path)?;
    Some(ProbeResult {
        duration: meta.duration,
        width: meta.width,
        height: meta.height,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn probe_video_metadata(_path: &str) -> Option<ProbeResult> {
    None
}

/// Check if a file path has a supported video extension.
pub fn is_supported_video_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    SUPPORTED_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}
