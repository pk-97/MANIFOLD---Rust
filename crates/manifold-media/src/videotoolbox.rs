//! The Mac's video decoder: AVAssetReader and VideoToolbox in the native
//! plugin (`native/MetalVideoDecoderPlugin.m`), with a Metal compute pass
//! that converts NV12 into the clip's `Rgba16Float` texture.

use std::ffi::{CString, c_void};
use std::sync::Arc;

use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureFormat, GpuTextureUsage};

use crate::decoder::{DecodeStatus, DecoderBackend, DecoderError, VideoDecoder, VideoMetadata};
use crate::decoder_ffi;

/// Error codes from the native video decoder; `VD_ERR_*` in the plugin.
fn error_from_code(code: i32) -> DecoderError {
    match code {
        -2 => DecoderError::NullHandle,
        -3 => DecoderError::OpenFailed,
        -4 => DecoderError::NoVideoTrack,
        -5 => DecoderError::ReaderFailed,
        -6 => DecoderError::DecodeFailed,
        -7 => DecoderError::ComputeFailed,
        -8 => DecoderError::NullTexture,
        -9 => DecoderError::NullPool,
        -10 => DecoderError::SeekFailed,
        _ => DecoderError::Generic,
    }
}

fn check(code: i32) -> Result<(), DecoderError> {
    if code == 0 {
        Ok(())
    } else {
        Err(error_from_code(code))
    }
}

/// The plugin's shared state: the MTLDevice, the CVMetalTextureCache and
/// the NV12 → Rgba16Float pipeline.
struct Pool(*mut c_void);

// The pool points to heap-allocated Obj-C state. Each copy makes its own
// command buffer, so copies from several threads are safe; texture-cache
// calls happen on the worker that owns the decoder.
unsafe impl Send for Pool {}
unsafe impl Sync for Pool {}

impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { decoder_ffi::VideoDecoder_DestroyPool(self.0) };
    }
}

/// Opens files through the native plugin.
pub struct VtBackend {
    pool: Arc<Pool>,
}

impl VtBackend {
    pub fn new() -> Result<Self, DecoderError> {
        let handle = unsafe { decoder_ffi::VideoDecoder_CreatePool() };
        if handle.is_null() {
            return Err(DecoderError::Unavailable);
        }
        Ok(Self {
            pool: Arc::new(Pool(handle)),
        })
    }
}

impl DecoderBackend for VtBackend {
    fn open(
        &self,
        path: &str,
        _width: u32,
        _height: u32,
    ) -> Result<Box<dyn VideoDecoder>, DecoderError> {
        let c_path = CString::new(path).map_err(|_| DecoderError::OpenFailed)?;
        let handle = unsafe { decoder_ffi::VideoDecoder_Open(self.pool.0, c_path.as_ptr()) };
        if handle.is_null() {
            return Err(DecoderError::OpenFailed);
        }
        Ok(Box::new(VtDecoder {
            handle,
            pool: Arc::clone(&self.pool),
        }))
    }

    fn target_format(&self) -> GpuTextureFormat {
        GpuTextureFormat::Rgba16Float
    }

    fn target_usage(&self) -> GpuTextureUsage {
        GpuTextureUsage::RENDER_TARGET_FULL
    }
}

/// One file: an AVAsset, its AVAssetReader and the current CVPixelBuffer.
struct VtDecoder {
    handle: *mut c_void,
    pool: Arc<Pool>,
}

// The decoder moves to its worker and is used there, apart from the
// content thread's copy between jobs.
unsafe impl Send for VtDecoder {}

impl VideoDecoder for VtDecoder {
    fn prepare(&mut self) -> Result<(), DecoderError> {
        check(unsafe { decoder_ffi::VideoDecoder_Prepare(self.handle) })
    }

    fn seek_to(&mut self, seconds: f32) -> Result<(), DecoderError> {
        check(unsafe { decoder_ffi::VideoDecoder_SeekTo(self.handle, seconds) })
    }

    fn decode_next_frame(&mut self) -> Result<DecodeStatus, DecoderError> {
        match unsafe { decoder_ffi::VideoDecoder_DecodeNextFrame(self.handle) } {
            0 => Ok(DecodeStatus::FrameReady),
            1 => Ok(DecodeStatus::EndOfFile),
            code => Err(error_from_code(code)),
        }
    }

    fn frame_time(&self) -> f32 {
        unsafe { decoder_ffi::VideoDecoder_GetFrameTime(self.handle) }
    }

    fn duration(&self) -> f32 {
        unsafe { decoder_ffi::VideoDecoder_GetDuration(self.handle) }
    }

    fn width(&self) -> i32 {
        unsafe { decoder_ffi::VideoDecoder_GetWidth(self.handle) }
    }

    fn height(&self) -> i32 {
        unsafe { decoder_ffi::VideoDecoder_GetHeight(self.handle) }
    }

    fn frame_rate(&self) -> f32 {
        unsafe { decoder_ffi::VideoDecoder_GetFrameRate(self.handle) }
    }

    fn copy_frame_to(&self, _device: &GpuDevice, target: &GpuTexture) -> Result<(), DecoderError> {
        check(unsafe {
            decoder_ffi::VideoDecoder_CopyFrameToTexture(self.pool.0, self.handle, target.raw_ptr())
        })
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        unsafe { decoder_ffi::VideoDecoder_Close(self.handle) };
    }
}

/// Quick metadata probe without full decoder setup.
pub(crate) fn probe_metadata(path: &str) -> Option<VideoMetadata> {
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
    (result == 0).then_some(VideoMetadata {
        duration,
        width,
        height,
    })
}
