//! Safe Rust wrapper around the native Metal video decoder FFI.
//!
//! Provides `DecoderPool` (shared Metal state) and `DecoderHandle` (per-file decoder)
//! for hardware-accelerated video decode via AVAssetReader + VideoToolbox.

use std::ffi::{CString, c_void};
use std::fmt;

use crate::decoder_ffi;

/// Error codes from the native video decoder.
/// Matches VD_ERR_* defines in MetalVideoDecoderPlugin.m.
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

impl DecoderError {
    pub(crate) fn from_code(code: i32) -> Self {
        match code {
            -1 => Self::Generic,
            -2 => Self::NullHandle,
            -3 => Self::OpenFailed,
            -4 => Self::NoVideoTrack,
            -5 => Self::ReaderFailed,
            -6 => Self::DecodeFailed,
            -7 => Self::ComputeFailed,
            -8 => Self::NullTexture,
            -9 => Self::NullPool,
            -10 => Self::SeekFailed,
            _ => Self::Generic,
        }
    }
}

impl fmt::Display for DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Generic => write!(f, "decoder error"),
            Self::NullHandle => write!(f, "decoder handle is null"),
            Self::OpenFailed => write!(f, "failed to open video file"),
            Self::NoVideoTrack => write!(f, "no video track found"),
            Self::ReaderFailed => write!(f, "AVAssetReader creation failed"),
            Self::DecodeFailed => write!(f, "frame decode failed"),
            Self::ComputeFailed => write!(f, "GPU compute conversion failed"),
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

/// Shared decoder pool state.
///
/// Owns the MTLDevice, CVMetalTextureCache, and NV12→Rgba16Float compute pipeline.
/// Created once, shared across all decoder handles (thread-safe for the compute path
/// since each CopyFrameToTexture call creates its own command buffer).
pub struct DecoderPool {
    pool_handle: *mut c_void,
}

// The pool handle points to heap-allocated Obj-C state.
// CopyFrameToTexture creates per-call command buffers, safe from multiple threads.
// CVMetalTextureCache calls (in DecodeOneFrame) happen on worker threads — each
// worker operates on its own DecoderHandle, so no concurrent cache access.
unsafe impl Send for DecoderPool {}
unsafe impl Sync for DecoderPool {}

impl DecoderPool {
    /// Create a new shared decoder pool.
    pub fn new() -> Result<Self, DecoderError> {
        let handle = unsafe { decoder_ffi::VideoDecoder_CreatePool() };
        if handle.is_null() {
            return Err(DecoderError::Unavailable);
        }
        Ok(Self { pool_handle: handle })
    }

    /// Open a video file and return a decoder handle.
    pub fn open(&self, path: &str) -> Result<DecoderHandle, DecoderError> {
        let c_path = CString::new(path).map_err(|_| DecoderError::OpenFailed)?;
        let handle = unsafe {
            decoder_ffi::VideoDecoder_Open(self.pool_handle, c_path.as_ptr())
        };
        if handle.is_null() {
            return Err(DecoderError::OpenFailed);
        }
        Ok(DecoderHandle { handle })
    }

    /// Run the NV12→Rgba16Float compute shader, copying the decoded frame
    /// into the destination Metal texture.
    ///
    /// # Safety
    /// `dest_metal_texture` must be a valid `id<MTLTexture>` cast to `*mut c_void`.
    /// The decoder handle must have a current decoded frame.
    pub unsafe fn copy_frame_to_texture(
        &self,
        decoder: &DecoderHandle,
        dest_metal_texture: *mut c_void,
    ) -> Result<(), DecoderError> {
        let result = unsafe {
            decoder_ffi::VideoDecoder_CopyFrameToTexture(
                self.pool_handle,
                decoder.handle,
                dest_metal_texture,
            )
        };
        if result != 0 {
            return Err(DecoderError::from_code(result));
        }
        Ok(())
    }

    /// Raw pool handle for FFI calls from worker threads.
    pub(crate) fn raw_handle(&self) -> *mut c_void {
        self.pool_handle
    }

    /// Quick metadata probe without full decoder setup.
    pub fn probe_metadata(path: &str) -> Option<VideoMetadata> {
        let c_path = CString::new(path).ok()?;
        let mut duration: f32 = 0.0;
        let mut width: i32 = 0;
        let mut height: i32 = 0;
        let result = unsafe {
            decoder_ffi::VideoDecoder_ProbeMetadata(
                c_path.as_ptr(),
                &mut duration,
                &mut width,
                &mut height,
            )
        };
        if result != 0 {
            return None;
        }
        Some(VideoMetadata { duration, width, height })
    }
}

impl Drop for DecoderPool {
    fn drop(&mut self) {
        if !self.pool_handle.is_null() {
            unsafe { decoder_ffi::VideoDecoder_DestroyPool(self.pool_handle) };
        }
    }
}

/// Per-file decoder handle.
///
/// Owns an AVAsset + AVAssetReader + current decoded CVPixelBuffer.
/// Not thread-safe — each handle must be used from a single thread.
pub struct DecoderHandle {
    handle: *mut c_void,
}

// Handle is moved to a worker thread and used exclusively there.
unsafe impl Send for DecoderHandle {}

impl DecoderHandle {
    /// Create AVAssetReader, start reading, and decode the first frame.
    pub fn prepare(&mut self) -> Result<(), DecoderError> {
        let result = unsafe { decoder_ffi::VideoDecoder_Prepare(self.handle) };
        if result != 0 {
            return Err(DecoderError::from_code(result));
        }
        Ok(())
    }

    /// Seek to a specific time (seconds). Recreates the AVAssetReader and
    /// decodes one frame at the target position.
    pub fn seek_to(&mut self, seconds: f32) -> Result<(), DecoderError> {
        let result = unsafe { decoder_ffi::VideoDecoder_SeekTo(self.handle, seconds) };
        if result != 0 {
            return Err(DecoderError::from_code(result));
        }
        Ok(())
    }

    /// Decode the next frame in sequence.
    pub fn decode_next_frame(&mut self) -> Result<DecodeStatus, DecoderError> {
        let result = unsafe { decoder_ffi::VideoDecoder_DecodeNextFrame(self.handle) };
        match result {
            0 => Ok(DecodeStatus::FrameReady),
            1 => Ok(DecodeStatus::EndOfFile),
            code => Err(DecoderError::from_code(code)),
        }
    }

    /// Presentation timestamp of the current decoded frame (seconds).
    pub fn frame_time(&self) -> f32 {
        unsafe { decoder_ffi::VideoDecoder_GetFrameTime(self.handle) }
    }

    /// Media duration in seconds.
    pub fn duration(&self) -> f32 {
        unsafe { decoder_ffi::VideoDecoder_GetDuration(self.handle) }
    }

    /// Native video width in pixels.
    pub fn width(&self) -> i32 {
        unsafe { decoder_ffi::VideoDecoder_GetWidth(self.handle) }
    }

    /// Native video height in pixels.
    pub fn height(&self) -> i32 {
        unsafe { decoder_ffi::VideoDecoder_GetHeight(self.handle) }
    }

    /// Detected frame rate (fps).
    pub fn frame_rate(&self) -> f32 {
        unsafe { decoder_ffi::VideoDecoder_GetFrameRate(self.handle) }
    }

    /// True if the first frame has been decoded and is ready.
    pub fn is_prepared(&self) -> bool {
        unsafe { decoder_ffi::VideoDecoder_IsPrepared(self.handle) != 0 }
    }

    /// Raw handle for FFI calls.
    pub(crate) fn raw_handle(&self) -> *mut c_void {
        self.handle
    }
}

impl Drop for DecoderHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { decoder_ffi::VideoDecoder_Close(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}
