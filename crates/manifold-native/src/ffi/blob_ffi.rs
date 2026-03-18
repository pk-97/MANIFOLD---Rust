use crate::blob_detector::BlobDetector;
use libloading::{Library, Symbol};
use std::ffi::c_void;

/// FFI implementation of BlobDetector using the BlobDetector.bundle native plugin.
///
/// Matches Unity's BlobDetectorNative.cs:
/// - BlobDetector_Create(maxBlobs) → opaque handle
/// - BlobDetector_Process(handle, rgbaData, w, h, threshold, sensitivity, outBlobData) → count
/// - BlobDetector_Destroy(handle)
pub struct FfiBlobDetector {
    _library: Library,
    handle: *mut c_void,
    fn_process: unsafe extern "C" fn(
        *mut c_void,
        *const u8,
        i32,
        i32,
        f32,
        f32,
        *mut f32,
    ) -> i32,
    fn_destroy: unsafe extern "C" fn(*mut c_void),
}

// Safety: The native handle is single-threaded (matches Unity's main-thread-only access).
// We enforce single-threaded access through &mut self on process().
unsafe impl Send for FfiBlobDetector {}

impl FfiBlobDetector {
    /// Create a new FFI blob detector.
    ///
    /// - `max_blobs`: maximum blobs to detect per frame (Unity uses 16)
    ///
    /// Returns None if the native plugin cannot be loaded.
    pub fn new(max_blobs: i32) -> Option<Self> {
        let path = super::resolve_bundle_path("BlobDetector")?;

        // Safety: Loading dynamic library. The bundle is self-contained with
        // embedded frameworks via @rpath.
        let library = unsafe { Library::new(&path) }.ok()?;

        let (fn_create, fn_process, fn_destroy) = unsafe {
            let create: Symbol<unsafe extern "C" fn(i32) -> *mut c_void> =
                library.get(b"BlobDetector_Create").ok()?;
            let process: Symbol<
                unsafe extern "C" fn(*mut c_void, *const u8, i32, i32, f32, f32, *mut f32) -> i32,
            > = library.get(b"BlobDetector_Process").ok()?;
            let destroy: Symbol<unsafe extern "C" fn(*mut c_void)> =
                library.get(b"BlobDetector_Destroy").ok()?;
            (*create, *process, *destroy)
        };

        let handle = unsafe { fn_create(max_blobs) };
        if handle.is_null() {
            log::warn!("[FfiBlobDetector] BlobDetector_Create returned null");
            return None;
        }

        log::info!(
            "[FfiBlobDetector] Loaded native plugin from {}",
            path.display()
        );

        Some(Self {
            _library: library,
            handle,
            fn_process,
            fn_destroy,
        })
    }
}

impl BlobDetector for FfiBlobDetector {
    fn process(
        &mut self,
        rgba: &[u8],
        width: i32,
        height: i32,
        threshold: f32,
        sensitivity: f32,
        out_blob_data: &mut [f32],
    ) -> i32 {
        if self.handle.is_null() {
            return 0;
        }

        // Safety: FFI call with correctly-sized buffers.
        // rgba must be width * height * 4 bytes (RGBA8).
        // out_blob_data must be >= max_blobs * 4 floats.
        unsafe {
            (self.fn_process)(
                self.handle,
                rgba.as_ptr(),
                width,
                height,
                threshold,
                sensitivity,
                out_blob_data.as_mut_ptr(),
            )
        }
    }
}

impl Drop for FfiBlobDetector {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.fn_destroy)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}
