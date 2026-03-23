//! Export configuration types.

/// Configuration for a video export session.
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Output file path for the final MP4.
    pub output_path: String,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Frame rate (e.g. 60.0).
    pub fps: f32,
    /// HDR export (HEVC 10-bit) vs SDR (H.264).
    pub hdr: bool,
    /// Export range start beat. 0.0 = use content range.
    pub start_beat: f32,
    /// Export range end beat. 0.0 = use content range.
    pub end_beat: f32,
    /// Optional audio file path for post-mux.
    pub audio_path: Option<String>,
    /// Beat position where the audio starts on the timeline.
    pub audio_start_beat: f32,
    /// Encoder delay compensation in seconds (e.g. 0.05).
    pub audio_encoder_delay: f32,
}

impl ExportConfig {
    /// Whether this export has an audio track to mux.
    pub fn has_audio(&self) -> bool {
        self.audio_path
            .as_ref()
            .is_some_and(|p| !p.is_empty())
    }
}
