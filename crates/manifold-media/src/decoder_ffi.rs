//! Raw FFI bindings to the native Metal video decoder plugin.
//!
//! These map 1:1 to the C functions exported by `native/MetalVideoDecoderPlugin.m`.
//! Use `DecoderPool` / `DecoderHandle` in `decoder.rs` for the safe Rust wrapper.

use std::ffi::c_void;
use std::os::raw::c_char;

unsafe extern "C" {
    /// Create shared decoder pool (MTLDevice, compute pipeline, CVMetalTextureCache).
    /// Returns opaque pool handle, or NULL on failure.
    pub fn VideoDecoder_CreatePool() -> *mut c_void;

    /// Release shared decoder pool.
    pub fn VideoDecoder_DestroyPool(pool: *mut c_void);

    /// Open a video file. Returns per-file decoder handle, or NULL on failure.
    pub fn VideoDecoder_Open(pool: *mut c_void, path: *const c_char) -> *mut c_void;

    /// Create AVAssetReader and decode first frame. Returns 0 on success.
    pub fn VideoDecoder_Prepare(handle: *mut c_void) -> i32;

    /// Seek to a specific time (seconds). Recreates reader, decodes one frame.
    /// Returns 0 on success.
    pub fn VideoDecoder_SeekTo(handle: *mut c_void, seconds: f32) -> i32;

    /// Decode the next frame. Returns 0=ok, 1=EOF, negative=error.
    pub fn VideoDecoder_DecodeNextFrame(handle: *mut c_void) -> i32;

    /// Run NV12→Rgba16Float compute shader, writing to destination Metal texture.
    /// `dest_metal_texture_ptr` is `id<MTLTexture>` cast to `*mut c_void`.
    /// Returns 0 on success.
    pub fn VideoDecoder_CopyFrameToTexture(
        pool: *mut c_void,
        handle: *mut c_void,
        dest_metal_texture_ptr: *mut c_void,
    ) -> i32;

    /// Presentation timestamp of the current decoded frame (seconds).
    pub fn VideoDecoder_GetFrameTime(handle: *mut c_void) -> f32;

    /// Media duration in seconds.
    pub fn VideoDecoder_GetDuration(handle: *mut c_void) -> f32;

    /// Native video width in pixels.
    pub fn VideoDecoder_GetWidth(handle: *mut c_void) -> i32;

    /// Native video height in pixels.
    pub fn VideoDecoder_GetHeight(handle: *mut c_void) -> i32;

    /// Detected frame rate (fps).
    pub fn VideoDecoder_GetFrameRate(handle: *mut c_void) -> f32;

    /// Returns 1 if first frame has been decoded, 0 otherwise.
    pub fn VideoDecoder_IsPrepared(handle: *mut c_void) -> i32;

    /// Release all resources for this decoder handle.
    pub fn VideoDecoder_Close(handle: *mut c_void);

    /// Quick metadata probe without full decoder setup.
    /// Returns 0 on success.
    pub fn VideoDecoder_ProbeMetadata(
        path: *const c_char,
        out_duration: *mut f32,
        out_width: *mut i32,
        out_height: *mut i32,
    ) -> i32;
}
