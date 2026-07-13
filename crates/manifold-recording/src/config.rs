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
    /// Total video frames actually appended to the file — the native ground
    /// truth read after the async append queue is drained (BUG-085), not a
    /// count of synchronous `LiveRecorder_EncodeVideoFrame` successes.
    pub frames_recorded: u32,
    /// Every video frame that never made it into the file: session-level
    /// pool exhaustion / channel-full drops, synchronous encode failures,
    /// and the native encoder's async `appendPixelBuffer:` drops
    /// (BUG-085/BUG-086 class rule — no path counts a drop as a success, so
    /// `frames_recorded + frames_dropped` always equals frames submitted).
    pub frames_dropped: u32,
    /// Audio sample-frames dropped by the native encoder's backpressure
    /// gate (BUG-084's counter; also a BUG-086 instrument — see
    /// `LiveRecordingSession::audio_frames_dropped`).
    pub audio_frames_dropped: u32,
    /// Total recording duration in seconds.
    pub duration_seconds: f64,
}
