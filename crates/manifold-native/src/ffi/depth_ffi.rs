use crate::depth_estimator::DepthEstimator;
use libloading::{Library, Symbol};
use std::ffi::c_void;
use std::sync::{Arc, OnceLock};

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

/// Shared handle to the loaded DepthEstimator.bundle.
/// Loaded once via OnceLock, symbols resolved once, multiple native handles created from it.
struct SharedDepthLib {
    _library: Library,
    fn_create: FnCreate,
    fn_create_depth_only: Option<FnCreate>,
    fn_create_flow_only: Option<FnCreate>,
    fn_create_subject_only: Option<FnCreate>,
    fn_process: FnProcess,
    fn_process_subject_mask: Option<FnProcessSubjectMask>,
    fn_compute_flow: Option<FnComputeFlow>,
    fn_destroy: FnDestroy,
}

// Safety: SharedDepthLib only stores function pointers and an owned Library.
// The Library handle is Send (file descriptor), and C function pointers are Send.
unsafe impl Send for SharedDepthLib {}
unsafe impl Sync for SharedDepthLib {}

static SHARED_LIB: OnceLock<Option<Arc<SharedDepthLib>>> = OnceLock::new();

fn get_or_load_shared_lib() -> Option<Arc<SharedDepthLib>> {
    SHARED_LIB.get_or_init(|| {
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

        // Specialized factory symbols (optional — old plugin builds won't have them)
        let fn_create_depth_only = unsafe {
            library.get::<FnCreate>(b"DepthEstimator_CreateDepthOnly").ok().map(|s| *s)
        };
        let fn_create_flow_only = unsafe {
            library.get::<FnCreate>(b"DepthEstimator_CreateFlowOnly").ok().map(|s| *s)
        };
        let fn_create_subject_only = unsafe {
            library.get::<FnCreate>(b"DepthEstimator_CreateSubjectOnly").ok().map(|s| *s)
        };

        let fn_process_subject_mask = unsafe {
            library.get::<FnProcessSubjectMask>(b"DepthEstimator_ProcessSubjectMask").ok().map(|s| *s)
        };
        let fn_compute_flow = unsafe {
            library.get::<FnComputeFlow>(b"DepthEstimator_ComputeFlow").ok().map(|s| *s)
        };

        log::info!(
            "[FfiDepthEstimator] Loaded native plugin from {} (subject_mask={}, flow={}, parallel={})",
            path.display(),
            fn_process_subject_mask.is_some(),
            fn_compute_flow.is_some(),
            fn_create_depth_only.is_some() && fn_create_flow_only.is_some() && fn_create_subject_only.is_some(),
        );

        Some(Arc::new(SharedDepthLib {
            _library: library,
            fn_create,
            fn_create_depth_only,
            fn_create_flow_only,
            fn_create_subject_only,
            fn_process,
            fn_process_subject_mask,
            fn_compute_flow,
            fn_destroy,
        }))
    }).clone()
}

/// FFI implementation of DepthEstimator using the DepthEstimator.bundle native plugin.
///
/// Matches Unity's DepthEstimatorNative.cs:
/// - DepthEstimator_Create() → opaque handle (loads ONNX models, optional)
/// - DepthEstimator_Process(handle, rgba, w, h, outDepth, outW, outH) → success
/// - DepthEstimator_ProcessSubjectMask(handle, rgba, w, h, outMask, outW, outH) → success
/// - DepthEstimator_ComputeFlow(handle, prev, curr, w, h, outFlow, outW, outH, outCut) → success
/// - DepthEstimator_Destroy(handle)
pub struct FfiDepthEstimator {
    lib: Arc<SharedDepthLib>,
    handle: *mut c_void,
}

unsafe impl Send for FfiDepthEstimator {}

impl FfiDepthEstimator {
    /// Create a new FFI depth estimator with ALL models (monolithic).
    /// Returns None if the native plugin cannot be loaded.
    pub fn new() -> Option<Self> {
        let lib = get_or_load_shared_lib()?;
        let handle = unsafe { (lib.fn_create)() };
        if handle.is_null() {
            log::warn!("[FfiDepthEstimator] DepthEstimator_Create returned null");
            return None;
        }
        Some(Self { lib, handle })
    }

    /// Create a handle with only the MiDaS depth model loaded.
    /// Returns None if the plugin doesn't support specialized creation.
    pub fn new_depth_only() -> Option<Self> {
        let lib = get_or_load_shared_lib()?;
        let fn_create = lib.fn_create_depth_only?;
        let handle = unsafe { fn_create() };
        if handle.is_null() {
            log::warn!("[FfiDepthEstimator] DepthEstimator_CreateDepthOnly returned null");
            return None;
        }
        Some(Self { lib, handle })
    }

    /// Create a handle with only flow state (no DNN models).
    /// Returns None if the plugin doesn't support specialized creation.
    pub fn new_flow_only() -> Option<Self> {
        let lib = get_or_load_shared_lib()?;
        let fn_create = lib.fn_create_flow_only?;
        let handle = unsafe { fn_create() };
        if handle.is_null() {
            log::warn!("[FfiDepthEstimator] DepthEstimator_CreateFlowOnly returned null");
            return None;
        }
        Some(Self { lib, handle })
    }

    /// Create a handle with only the subject segmentation model loaded.
    /// Returns None if the plugin doesn't support specialized creation.
    pub fn new_subject_only() -> Option<Self> {
        let lib = get_or_load_shared_lib()?;
        let fn_create = lib.fn_create_subject_only?;
        let handle = unsafe { fn_create() };
        if handle.is_null() {
            log::warn!("[FfiDepthEstimator] DepthEstimator_CreateSubjectOnly returned null");
            return None;
        }
        Some(Self { lib, handle })
    }

    /// Returns true if the plugin supports parallel (specialized) worker creation.
    pub fn supports_parallel() -> bool {
        get_or_load_shared_lib()
            .map(|lib| {
                lib.fn_create_depth_only.is_some()
                    && lib.fn_create_flow_only.is_some()
                    && lib.fn_create_subject_only.is_some()
            })
            .unwrap_or(false)
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
            (self.lib.fn_process)(
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
        let Some(func) = self.lib.fn_process_subject_mask else {
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
        let Some(func) = self.lib.fn_compute_flow else {
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
                (self.lib.fn_destroy)(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}
