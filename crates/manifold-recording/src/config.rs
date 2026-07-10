//! Configuration types for live recording.

/// Audio codec for the recorded file.
#[derive(Clone, Copy, Debug, Default)]
pub enum AudioCodec {
    /// AAC-LC 320 kbps — good quality, small files.
    #[default]
    Aac,
    /// Apple Lossless — archival quality, larger files.
    Alac,
}

/// Configuration for a live recording session.
#[derive(Clone, Debug)]
pub struct LiveRecordingConfig {
    /// Output file path. Auto-generated if empty.
    pub output_path: String,
    /// HDR (HEVC Main 10) or SDR (H.264 High Profile).
    pub hdr: bool,
    /// Audio input device name. `None` = no audio capture.
    pub audio_device: Option<String>,
    /// Audio codec for the recorded file.
    pub audio_codec: AudioCodec,
}

impl LiveRecordingConfig {
    /// Generate a default config that writes to ~/Desktop with a timestamp.
    pub fn default_to_desktop() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // Format as YYYY-MM-DD_HH-MM-SS using chrono-free approach.
        let secs = now.as_secs();
        // Simple timestamp — sufficient for unique filenames.
        // .mov container — required for ProRes (SDR) and works fine for HEVC (HDR).
        let output_path = format!("{home}/Desktop/MANIFOLD_{secs}.mov");
        Self {
            output_path,
            hdr: false,
            audio_device: None,
            audio_codec: AudioCodec::default(),
        }
    }
}

/// Result returned after a recording session ends.
#[derive(Clone, Debug)]
pub struct RecordingResult {
    /// Path to the recorded file (.mov — ProRes 422 Proxy for SDR, HEVC
    /// Main10 for HDR; see `LiveRecordingConfig::default_to_desktop`).
    pub output_path: String,
    /// Total video frames written.
    pub frames_recorded: u32,
    /// Video frames dropped due to pool exhaustion.
    pub frames_dropped: u32,
    /// Total recording duration in seconds.
    pub duration_seconds: f64,
}
