//! Raw FFI bindings to the native Metal encoder plugin.
//!
//! These map 1:1 to the C functions exported by `native/MetalEncoderPlugin.m`.
//! Use `MetalEncoder` in `metal_encoder.rs` for the safe Rust wrapper.

use std::ffi::c_void;
use std::os::raw::c_char;

unsafe extern "C" {
    /// Returns 1 if a Metal device is available, 0 otherwise.
    pub fn MetalEncoder_IsAvailable() -> i32;

    /// Returns 1 if HEVC (HDR) encoding is supported, 0 otherwise.
    pub fn MetalEncoder_IsHDRAvailable() -> i32;

    /// Create an SDR (H.264) encoder session.
    /// Returns an opaque handle, or NULL on failure.
    pub fn MetalEncoder_Create(
        width: i32,
        height: i32,
        fps: f32,
        output_path: *const c_char,
    ) -> *mut c_void;

    /// Create an HDR (HEVC 10-bit) encoder session.
    /// Returns an opaque handle, or NULL on failure.
    pub fn MetalEncoder_CreateHDR(
        width: i32,
        height: i32,
        fps: f32,
        output_path: *const c_char,
    ) -> *mut c_void;

    /// Encode a single frame from a Metal texture.
    /// Returns 0 (ME_OK) on success, or an error code.
    pub fn MetalEncoder_EncodeFrame(
        handle: *mut c_void,
        metal_texture_ptr: *mut c_void,
        frame_index: i32,
    ) -> i32;

    /// Finalize and close the encoder session.
    /// Returns 0 (ME_OK) on success, or an error code.
    pub fn MetalEncoder_EndSession(handle: *mut c_void) -> i32;
}
