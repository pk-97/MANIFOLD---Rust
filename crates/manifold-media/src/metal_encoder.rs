//! Safe Rust wrapper around the native Metal encoder FFI.
//!
//! Provides `MetalEncoder` for zero-copy GPU video encoding via
//! AVAssetWriter + VideoToolbox. Port of Unity MetalEncoderNative.cs.

use std::ffi::{CString, c_void};
use std::fmt;

use crate::metal_ffi;

/// Error codes returned by the native Metal encoder.
/// Matches ME_ERR_* defines in MetalEncoderPlugin.m.
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
}

impl EncoderError {
    fn from_code(code: i32) -> Self {
        match code {
            1 => Self::NullHandle,
            2 => Self::WriterNotReady,
            3 => Self::PixelBufferCreate,
            4 => Self::TextureCreate,
            5 => Self::BlitFailed,
            6 => Self::AppendFailed,
            7 => Self::WriterFailed,
            8 => Self::NullTexture,
            9 => Self::ShaderFailed,
            _ => Self::WriterFailed,
        }
    }
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
        }
    }
}

/// Native Metal GPU video encoder.
///
/// Wraps the Objective-C MetalEncoderPlugin for zero-copy GPU encoding.
/// SDR mode: H.264 High Profile, 50 Mbps, BGRA8.
/// HDR mode: HEVC Main10, 100 Mbps, RGBA16Float, BT.2020/PQ.
pub struct MetalEncoder {
    handle: *mut c_void,
    width: u32,
    height: u32,
    frames_encoded: u32,
    is_hdr: bool,
}

// The native encoder state is single-threaded (called only from content thread).
// The handle is an opaque pointer to heap-allocated Obj-C state.
unsafe impl Send for MetalEncoder {}

impl MetalEncoder {
    /// Check if a Metal device is available for encoding.
    pub fn is_available() -> bool {
        unsafe { metal_ffi::MetalEncoder_IsAvailable() != 0 }
    }

    /// Check if HDR (HEVC 10-bit) encoding is supported.
    pub fn is_hdr_available() -> bool {
        unsafe { metal_ffi::MetalEncoder_IsHDRAvailable() != 0 }
    }

    /// Create a new encoder session.
    ///
    /// - `width`, `height`: output resolution
    /// - `fps`: frame rate (rounded to nearest integer for CMTime)
    /// - `output_path`: file path for the output MP4
    /// - `hdr`: true for HEVC HDR, false for H.264 SDR
    pub fn new(
        width: u32,
        height: u32,
        fps: f32,
        output_path: &str,
        hdr: bool,
    ) -> Result<Self, EncoderError> {
        if !Self::is_available() {
            return Err(EncoderError::Unavailable);
        }
        if hdr && !Self::is_hdr_available() {
            return Err(EncoderError::Unavailable);
        }

        let c_path =
            CString::new(output_path).map_err(|_| EncoderError::WriterFailed)?;

        let handle = unsafe {
            if hdr {
                metal_ffi::MetalEncoder_CreateHDR(
                    width as i32,
                    height as i32,
                    fps,
                    c_path.as_ptr(),
                )
            } else {
                metal_ffi::MetalEncoder_Create(
                    width as i32,
                    height as i32,
                    fps,
                    c_path.as_ptr(),
                )
            }
        };

        if handle.is_null() {
            return Err(EncoderError::WriterFailed);
        }

        log::info!(
            "[MetalEncoder] Created {} encoder {}x{} @ {} fps -> {}",
            if hdr { "HDR" } else { "SDR" },
            width,
            height,
            fps,
            output_path,
        );

        Ok(Self {
            handle,
            width,
            height,
            frames_encoded: 0,
            is_hdr: hdr,
        })
    }

    /// Encode a single frame from a raw Metal texture pointer.
    ///
    /// The `metal_texture_ptr` must be a valid `id<MTLTexture>` cast to `*mut c_void`.
    /// Obtained via `wgpu::Texture::as_hal::<Metal>()`.
    ///
    /// # Safety
    /// The caller must ensure `metal_texture_ptr` points to a valid Metal texture
    /// that is not currently being written to by another GPU command.
    pub unsafe fn encode_frame(
        &mut self,
        metal_texture_ptr: *mut c_void,
    ) -> Result<(), EncoderError> {
        let result = unsafe {
            metal_ffi::MetalEncoder_EncodeFrame(
                self.handle,
                metal_texture_ptr,
                self.frames_encoded as i32,
            )
        };

        if result != 0 {
            log::error!(
                "[MetalEncoder] encode_frame failed at frame {}: {:?}",
                self.frames_encoded,
                EncoderError::from_code(result),
            );
            return Err(EncoderError::from_code(result));
        }

        self.frames_encoded += 1;
        Ok(())
    }

    /// Finalize the encoding session and write the MP4 file.
    /// Consumes the encoder — cannot be used after this call.
    pub fn end_session(self) -> Result<u32, EncoderError> {
        let frames = self.frames_encoded;
        let result = unsafe { metal_ffi::MetalEncoder_EndSession(self.handle) };

        // Skip Drop — handle is consumed by EndSession
        std::mem::forget(self);

        if result != 0 {
            log::error!(
                "[MetalEncoder] end_session failed: {:?}",
                EncoderError::from_code(result),
            );
            return Err(EncoderError::from_code(result));
        }

        log::info!("[MetalEncoder] Session complete, {} frames encoded", frames);
        Ok(frames)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frames_encoded(&self) -> u32 {
        self.frames_encoded
    }

    pub fn is_hdr(&self) -> bool {
        self.is_hdr
    }
}

impl Drop for MetalEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            log::warn!(
                "[MetalEncoder] Encoder dropped without end_session(), cleaning up"
            );
            unsafe {
                metal_ffi::MetalEncoder_EndSession(self.handle);
            }
        }
    }
}
