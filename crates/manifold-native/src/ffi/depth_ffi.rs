use crate::depth_estimator::DepthEstimator;
use libloading::{Library, Symbol};
use std::ffi::c_void;

type FnCreate = unsafe extern "C" fn() -> *mut c_void;
type FnDestroy = unsafe extern "C" fn(*mut c_void);
type FnProcess =
    unsafe extern "C" fn(*mut c_void, *const u8, i32, i32, *mut f32, i32, i32) -> i32;
type FnProcessSubjectMask =
    unsafe extern "C" fn(*mut c_void, *const u8, i32, i32, *mut f32, i32, i32) -> i32;
type FnComputeFlow = unsafe extern "C" fn(
    *mut c_void,
    *const u8,
    *const u8,
    i32,
    i32,
    *mut f32,
    i32,
    i32,
    *mut f32,
) -> i32;

/// FFI implementation of DepthEstimator using the DepthEstimator.bundle native plugin.
///
/// Matches Unity's DepthEstimatorNative.cs:
/// - DepthEstimator_Create() → opaque handle (loads ONNX models, optional)
/// - DepthEstimator_Process(handle, rgba, w, h, outDepth, outW, outH) → success
/// - DepthEstimator_ProcessSubjectMask(handle, rgba, w, h, outMask, outW, outH) → success
/// - DepthEstimator_ComputeFlow(handle, prev, curr, w, h, outFlow, outW, outH, outCut) → success
/// - DepthEstimator_Destroy(handle)
pub struct FfiDepthEstimator {
    _library: Library,
    handle: *mut c_void,
    fn_process: FnProcess,
    fn_process_subject_mask: Option<FnProcessSubjectMask>,
    fn_compute_flow: Option<FnComputeFlow>,
    fn_destroy: FnDestroy,
}

unsafe impl Send for FfiDepthEstimator {}

impl FfiDepthEstimator {
    /// Create a new FFI depth estimator.
    ///
    /// Returns None if the native plugin cannot be loaded.
    /// Models are optional inside the plugin — missing models cause Process() to return 0.
    pub fn new() -> Option<Self> {
        // Set KMP_DUPLICATE_LIB_OK before loading (matches Unity's EnsureOmpEnvironmentSafety)
        std::env::set_var("KMP_DUPLICATE_LIB_OK", "TRUE");

        let path = super::resolve_bundle_path("DepthEstimator")?;

        let library = unsafe { Library::new(&path) }.ok()?;

        let (fn_create, fn_process, fn_destroy) = unsafe {
            let create: Symbol<FnCreate> = library.get(b"DepthEstimator_Create").ok()?;
            let process: Symbol<FnProcess> = library.get(b"DepthEstimator_Process").ok()?;
            let destroy: Symbol<FnDestroy> = library.get(b"DepthEstimator_Destroy").ok()?;
            (*create, *process, *destroy)
        };

        // Subject mask and flow are optional (backward compat with older plugin builds)
        let fn_process_subject_mask = unsafe {
            library
                .get::<FnProcessSubjectMask>(b"DepthEstimator_ProcessSubjectMask")
                .ok()
                .map(|s| *s)
        };

        let fn_compute_flow = unsafe {
            library
                .get::<FnComputeFlow>(b"DepthEstimator_ComputeFlow")
                .ok()
                .map(|s| *s)
        };

        let handle = unsafe { fn_create() };
        if handle.is_null() {
            log::warn!("[FfiDepthEstimator] DepthEstimator_Create returned null");
            return None;
        }

        log::info!(
            "[FfiDepthEstimator] Loaded native plugin from {} (subject_mask={}, flow={})",
            path.display(),
            fn_process_subject_mask.is_some(),
            fn_compute_flow.is_some()
        );

        Some(Self {
            _library: library,
            handle,
            fn_process,
            fn_process_subject_mask,
            fn_compute_flow,
            fn_destroy,
        })
    }
}

impl DepthEstimator for FfiDepthEstimator {
    fn process(
        &mut self,
        rgba: &[u8],
        width: i32,
        height: i32,
        out_depth: &mut [f32],
        out_width: i32,
        out_height: i32,
    ) -> i32 {
        if self.handle.is_null() {
            return 0;
        }
        unsafe {
            (self.fn_process)(
                self.handle,
                rgba.as_ptr(),
                width,
                height,
                out_depth.as_mut_ptr(),
                out_width,
                out_height,
            )
        }
    }

    fn process_subject_mask(
        &mut self,
        rgba: &[u8],
        width: i32,
        height: i32,
        out_mask: &mut [f32],
        out_width: i32,
        out_height: i32,
    ) -> i32 {
        let Some(func) = self.fn_process_subject_mask else {
            return 0;
        };
        if self.handle.is_null() {
            return 0;
        }
        unsafe {
            func(
                self.handle,
                rgba.as_ptr(),
                width,
                height,
                out_mask.as_mut_ptr(),
                out_width,
                out_height,
            )
        }
    }

    fn compute_flow(
        &mut self,
        prev_rgba: &[u8],
        curr_rgba: &[u8],
        width: i32,
        height: i32,
        out_flow_packed: &mut [f32],
        out_width: i32,
        out_height: i32,
        out_cut_score: &mut [f32],
    ) -> i32 {
        let Some(func) = self.fn_compute_flow else {
            return 0;
        };
        if self.handle.is_null() {
            return 0;
        }
        unsafe {
            func(
                self.handle,
                prev_rgba.as_ptr(),
                curr_rgba.as_ptr(),
                width,
                height,
                out_flow_packed.as_mut_ptr(),
                out_width,
                out_height,
                out_cut_score.as_mut_ptr(),
            )
        }
    }
}

impl Drop for FfiDepthEstimator {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.fn_destroy)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}
