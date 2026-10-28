//! The video decoder seam. A `DecoderBackend` opens files into
//! `VideoDecoder`s, which live on the decode workers; the content thread
//! copies each decoded frame into a clip's texture. The Mac decodes through
//! VideoToolbox (`videotoolbox.rs`); everything else through FFmpeg on the
//! CPU (`ffmpeg_decoder.rs`).

use std::fmt;
use std::sync::Arc;

use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, GpuTextureUsage};

/// Why a decoder call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderError {
    Generic,
    NullHandle,
    OpenFailed,
    NoVideoTrack,
    ReaderFailed,
    DecodeFailed,
    ComputeFailed,
    NullTexture,
    NullPool,
    SeekFailed,
    Unavailable,
}

impl fmt::Display for DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generic => write!(f, "decoder error"),
            Self::NullHandle => write!(f, "decoder handle is null"),
            Self::OpenFailed => write!(f, "failed to open video file"),
            Self::NoVideoTrack => write!(f, "no video track found"),
            Self::ReaderFailed => write!(f, "the file reader failed"),
            Self::DecodeFailed => write!(f, "frame decode failed"),
            Self::ComputeFailed => write!(f, "GPU frame conversion failed"),
            Self::NullTexture => write!(f, "texture pointer is null"),
            Self::NullPool => write!(f, "decoder pool is null"),
            Self::SeekFailed => write!(f, "seek failed"),
            Self::Unavailable => write!(f, "video decoder not available"),
        }
    }
}

/// Result of a decode operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    /// Frame decoded successfully.
    FrameReady,
    /// Reached end of file.
    EndOfFile,
}

/// Video file metadata from a quick probe.
#[derive(Debug, Clone)]
pub struct VideoMetadata {
    pub duration: f32,
    pub width: i32,
    pub height: i32,
}

/// One open video file. Each lives on one decode worker; the content thread
/// only calls `copy_frame_to`, and only while no job for it is in flight.
pub trait VideoDecoder: Send {
    /// Start reading and decode the first frame.
    fn prepare(&mut self) -> Result<(), DecoderError>;
    /// Seek to `seconds` and decode the frame there.
    fn seek_to(&mut self, seconds: f32) -> Result<(), DecoderError>;
    /// Decode the next frame in sequence.
    fn decode_next_frame(&mut self) -> Result<DecodeStatus, DecoderError>;
    /// Presentation time of the current frame, in seconds.
    fn frame_time(&self) -> f32;
    /// Media duration in seconds.
    fn duration(&self) -> f32;
    /// Native video width in pixels.
    fn width(&self) -> i32;
    /// Native video height in pixels.
    fn height(&self) -> i32;
    /// Frame rate in frames per second.
    fn frame_rate(&self) -> f32;
    /// Copy the current frame into `target`, fitted inside it with
    /// transparent padding. `target` was made with the backend's
    /// `target_format` and `target_usage`.
    fn copy_frame_to(&self, device: &GpuDevice, target: &GpuTexture) -> Result<(), DecoderError>;
}

/// Opens video files for the decode workers.
pub trait DecoderBackend: Send + Sync {
    /// Open `path` for frames copied into `width × height` targets.
    fn open(
        &self,
        path: &str,
        width: u32,
        height: u32,
    ) -> Result<Box<dyn VideoDecoder>, DecoderError>;
    /// The format clip textures are made in.
    fn target_format(&self) -> GpuTextureFormat;
    /// The usage clip textures are made with.
    fn target_usage(&self) -> GpuTextureUsage;
}

/// A worker's decoder whose current frame the content thread copies.
#[derive(Clone, Copy)]
pub struct FrameSource(*const dyn VideoDecoder);

// The worker hands a decoder over only between jobs: the content thread's
// `decode_pending` flag keeps the next job for the clip from being
// submitted until the copy is done.
unsafe impl Send for FrameSource {}

impl FrameSource {
    pub(crate) fn new(decoder: &(dyn VideoDecoder + 'static)) -> Self {
        Self(decoder as *const dyn VideoDecoder)
    }

    /// Copy the decoder's current frame into `target`.
    ///
    /// # Safety
    /// The decoder is still open and no job for it is in flight.
    pub(crate) unsafe fn copy_to(
        self,
        device: &GpuDevice,
        target: &GpuTexture,
    ) -> Result<(), DecoderError> {
        unsafe { (*self.0).copy_frame_to(device, target) }
    }
}

/// This platform's decoder.
pub fn platform_backend() -> Result<Arc<dyn DecoderBackend>, DecoderError> {
    #[cfg(target_os = "macos")]
    return Ok(Arc::new(crate::videotoolbox::VtBackend::new()?));
    #[cfg(not(target_os = "macos"))]
    return Ok(Arc::new(crate::ffmpeg_decoder::FfmpegBackend::new()?));
}

/// Duration and size of the video at `path` without opening a decoder.
pub fn probe_metadata(path: &str) -> Option<VideoMetadata> {
    #[cfg(target_os = "macos")]
    return crate::videotoolbox::probe_metadata(path);
    #[cfg(not(target_os = "macos"))]
    return crate::ffmpeg_decoder::probe_metadata(path);
}
