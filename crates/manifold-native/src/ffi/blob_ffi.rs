// Mechanical port of BlobDetectorNative.cs — FFI wrapper.
// Unity: DllImport("BlobDetector") → Rust: dlopen "BlobDetector.bundle" at runtime.
//
// BlobDetectorNative.cs:
//   BlobDetector_Create(int maxBlobs)   → IntPtr
//   BlobDetector_Destroy(IntPtr ptr)
//   BlobDetector_Process(IntPtr, byte[], int, int, float, float, float[]) → int

use crate::blob_detector::BlobDetector;

// Raw FFI function signatures, loaded via libloading at runtime.
// Matches BlobDetectorNative.cs extern signatures exactly.
type FnCreate  = unsafe extern "C" fn(max_blobs: i32) -> *mut std::ffi::c_void;
type FnDestroy = unsafe extern "C" fn(ptr: *mut std::ffi::c_void);
type FnProcess = unsafe extern "C" fn(
    ptr: *mut std::ffi::c_void,
    rgba_data: *const u8,
    width: i32,
    height: i32,
    threshold: f32,
    sensitivity: f32,
    out_blob_data: *mut f32,
) -> i32;

/// FFI-backed BlobDetector that loads the native plugin at runtime.
/// If the plugin is not found, construction returns None and the effect
/// runs without any blob detection (matching Unity's DllNotFoundException path).
pub struct FfiBlobDetector {
    // Keep _lib alive so the symbols remain valid.
    _lib: libloading::Library,
    fn_destroy: FnDestroy,
    fn_process: FnProcess,
    handle: *mut std::ffi::c_void,
}

// SAFETY: The native library is single-threaded by convention; we serialize
// access through &self which requires the caller to hold &mut BlobTrackingFX.
unsafe impl Send for FfiBlobDetector {}

impl FfiBlobDetector {
    /// Try to load the native blob detector plugin and create a handle.
    /// Returns None if the plugin is not found (matching Unity DllNotFoundException).
    pub fn new(max_blobs: i32) -> Option<Self> {
        // Search for the .bundle / .dylib / .so in the same directory as the binary.
        let lib_name = Self::lib_name();
        let lib = unsafe { libloading::Library::new(lib_name) }.ok()?;

        let fn_create: libloading::Symbol<FnCreate> =
            unsafe { lib.get(b"BlobDetector_Create\0") }.ok()?;
        let fn_destroy: libloading::Symbol<FnDestroy> =
            unsafe { lib.get(b"BlobDetector_Destroy\0") }.ok()?;
        let fn_process: libloading::Symbol<FnProcess> =
            unsafe { lib.get(b"BlobDetector_Process\0") }.ok()?;

        let handle = unsafe { fn_create(max_blobs) };
        if handle.is_null() {
            return None;
        }

        // Transmute symbol lifetimes away — safe because _lib outlives them.
        let fn_destroy: FnDestroy = unsafe { std::mem::transmute(*fn_destroy) };
        let fn_process: FnProcess = unsafe { std::mem::transmute(*fn_process) };

        Some(Self { _lib: lib, fn_destroy, fn_process, handle })
    }

    #[cfg(target_os = "macos")]
    fn lib_name() -> &'static str { "BlobDetector.bundle" }
    #[cfg(target_os = "linux")]
    fn lib_name() -> &'static str { "libBlobDetector.so" }
    #[cfg(target_os = "windows")]
    fn lib_name() -> &'static str { "BlobDetector.dll" }
}

impl Drop for FfiBlobDetector {
    fn drop(&mut self) {
        // BlobDetectorNative.cs: BlobDetector_Destroy(nativeHandle)
        unsafe { (self.fn_destroy)(self.handle) };
    }
}

impl BlobDetector for FfiBlobDetector {
    fn process(
        &self,
        rgba_data: &[u8],
        width: i32,
        height: i32,
        threshold: f32,
        sensitivity: f32,
        out_blob_data: &mut [f32],
    ) -> i32 {
        // BlobDetectorNative.cs: BlobDetector_Process(nativeHandle, pixelBuffer,
        //   READBACK_WIDTH, READBACK_HEIGHT, threshold, sensitivity, nativeBlobOutput)
        unsafe {
            (self.fn_process)(
                self.handle,
                rgba_data.as_ptr(),
                width,
                height,
                threshold,
                sensitivity,
                out_blob_data.as_mut_ptr(),
            )
        }
    }
}
