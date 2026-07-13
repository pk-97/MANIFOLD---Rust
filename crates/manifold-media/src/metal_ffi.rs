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
    ///
    /// `fps_num`/`fps_den` is an exact rational frame rate (`fps ==
    /// fps_num / fps_den`, e.g. 30000/1001 for 29.97) — see
    /// `crate::frame_rate::fps_to_rational`. BUG-129: passing the rational
    /// pair instead of a rounded float lets the native encoder stamp every
    /// frame's CMTime presentation time exactly, instead of drifting the
    /// picture track against the muxed audio track.
    /// Returns an opaque handle, or NULL on failure.
    pub fn MetalEncoder_Create(
        width: i32,
        height: i32,
        fps_num: i32,
        fps_den: i32,
        output_path: *const c_char,
    ) -> *mut c_void;

    /// Create an HDR (HEVC 10-bit) encoder session. See `MetalEncoder_Create`
    /// for the `fps_num`/`fps_den` contract.
    /// Returns an opaque handle, or NULL on failure.
    pub fn MetalEncoder_CreateHDR(
        width: i32,
        height: i32,
        fps_num: i32,
        fps_den: i32,
        output_path: *const c_char,
    ) -> *mut c_void;

    /// Create an SDR encoder session using an external Metal device.
    /// The device_ptr must be a valid `id<MTLDevice>`. See
    /// `MetalEncoder_Create` for the `fps_num`/`fps_den` contract.
    pub fn MetalEncoder_CreateWithDevice(
        width: i32,
        height: i32,
        fps_num: i32,
        fps_den: i32,
        output_path: *const c_char,
        device_ptr: *mut c_void,
    ) -> *mut c_void;

    /// Create an HDR encoder session using an external Metal device.
    /// The device_ptr must be a valid `id<MTLDevice>`. See
    /// `MetalEncoder_Create` for the `fps_num`/`fps_den` contract.
    pub fn MetalEncoder_CreateHDRWithDevice(
        width: i32,
        height: i32,
        fps_num: i32,
        fps_den: i32,
        output_path: *const c_char,
        device_ptr: *mut c_void,
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
