//! The export video encoder, one API on every platform: the Mac encodes
//! through the AVFoundation plugin (`metal_encoder.rs`), everything else
//! through FFmpeg (`ffmpeg_encoder.rs`). Both take the export texture: the
//! linear SDR-mapped frame, or the PQ-encoded one for HDR.

use std::fmt;

#[cfg(not(target_os = "macos"))]
pub use crate::ffmpeg_encoder::FfmpegEncoder as VideoEncoder;
#[cfg(target_os = "macos")]
pub use crate::metal_encoder::MetalEncoder as VideoEncoder;

/// Why an encoder call failed. The first nine are the Mac plugin's
/// `ME_ERR_*` codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderError {
    /// Encoder handle is NULL.
    NullHandle,
    /// AVAssetWriter not in writing state or input not ready.
    WriterNotReady,
    /// Failed to create CVPixelBuffer from pool.
    PixelBufferCreate,
    /// Failed to create Metal texture from CVPixelBuffer.
    TextureCreate,
    /// GPU compute blit/copy failed.
    BlitFailed,
    /// Failed to append pixel buffer to AVAssetWriter.
    AppendFailed,
    /// AVAssetWriter finalization failed.
    WriterFailed,
    /// Source Metal texture pointer is NULL.
    NullTexture,
    /// Metal compute shader compilation failed.
    ShaderFailed,
    /// Metal encoder is not available on this system.
    Unavailable,
    /// This FFmpeg build has no H.264 encoder.
    CodecUnavailable,
    /// HDR export is Mac-only so far.
    HdrUnavailable,
    /// Reading the frame back from the GPU failed.
    ReadbackFailed,
    /// The FFmpeg encoder rejected a frame or failed to flush.
    EncodeFailed,
    /// Creating or writing the video file failed.
    FileFailed,
}

impl fmt::Display for EncoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NullHandle => write!(f, "encoder handle is null"),
            Self::WriterNotReady => write!(f, "AVAssetWriter not ready"),
            Self::PixelBufferCreate => write!(f, "CVPixelBuffer creation failed"),
            Self::TextureCreate => write!(f, "Metal texture creation failed"),
            Self::BlitFailed => write!(f, "GPU compute blit failed"),
            Self::AppendFailed => write!(f, "pixel buffer append failed"),
            Self::WriterFailed => write!(f, "AVAssetWriter finalization failed"),
            Self::NullTexture => write!(f, "source Metal texture is null"),
            Self::ShaderFailed => write!(f, "Metal shader compilation failed"),
            Self::Unavailable => write!(f, "Metal encoder not available"),
            Self::CodecUnavailable => write!(f, "no H.264 encoder in this FFmpeg"),
            Self::HdrUnavailable => write!(f, "HDR export is not available on this platform yet"),
            Self::ReadbackFailed => write!(f, "reading the frame back from the GPU failed"),
            Self::EncodeFailed => write!(f, "the video encoder failed"),
            Self::FileFailed => write!(f, "writing the video file failed"),
        }
    }
}
