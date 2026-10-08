/// Process-wide cached `GpuDevice` for in-crate tests.
///
/// `GpuDevice::new()` builds Metal pipeline state objects and warms the
/// shader cache — ~200–500ms per call. With 17+ unit tests across
/// renderer modules historically constructing their own device, that
/// added up to most of the renderer-lib test runtime. Callers only
/// need *a* working device, never a fresh one; `GpuDevice` is
/// `Send + Sync` (Metal serializes device operations internally), so
/// sharing across parallel test threads is safe. Mirrors the
/// `tests/parity/harness.rs::shared` pattern.
/// Process-wide lock serializing GPU test bodies. The shared device is safe to
/// *share*, but running dozens of GPU tests concurrently floods the Metal
/// device's transient resources (command buffers, texture/heap pools), which
/// surfaces as nondeterministic parity failures under `cargo test`'s default
/// per-binary parallelism (serial runs are 100% green). Held by the
/// [`TestDevice`] guard for each test's lifetime so GPU work runs one test at a
/// time. Reentrant: a single test may call [`test_device`] more than once on its
/// own thread without deadlocking.
#[cfg(feature = "gpu-proofs")]
static GPU_TEST_LOCK: parking_lot::ReentrantMutex<()> = parking_lot::ReentrantMutex::new(());

/// RAII handle returned by [`test_device`]. Derefs to the shared
/// [`crate::GpuDevice`] (so call sites use it exactly like the old
/// `Arc<GpuDevice>`) and holds [`GPU_TEST_LOCK`] until it drops at end of test.
#[cfg(feature = "gpu-proofs")]
pub struct TestDevice {
    device: std::sync::Arc<crate::GpuDevice>,
    _lock: parking_lot::ReentrantMutexGuard<'static, ()>,
}

#[cfg(feature = "gpu-proofs")]
impl std::ops::Deref for TestDevice {
    type Target = crate::GpuDevice;
    fn deref(&self) -> &Self::Target {
        &self.device
    }
}

#[cfg(feature = "gpu-proofs")]
impl TestDevice {
    /// A cheap `Arc` clone of the shared device, for constructors that now
    /// take ownership of an `Arc<GpuDevice>`.
    pub fn arc(&self) -> std::sync::Arc<crate::GpuDevice> {
        std::sync::Arc::clone(&self.device)
    }
}

#[cfg(feature = "gpu-proofs")]
pub fn test_device() -> TestDevice {
    use std::sync::{Arc, OnceLock};
    static SHARED: OnceLock<Arc<crate::GpuDevice>> = OnceLock::new();
    // Acquire the serialization lock first, then hand back the shared device.
    let _lock = GPU_TEST_LOCK.lock();
    // BUG-290: GPU_TEST_LOCK is invisible across processes. The machine-wide
    // GPU queue (crate::queue) is taken by `new_queued` below, for the
    // process lifetime; waiting there IS the fix, not a hang.
    let device = SHARED
        .get_or_init(|| Arc::new(crate::GpuDevice::new_queued("GPU tests")))
        .clone();
    TestDevice { device, _lock }
}

/// Reuse app shader disk caches without populating live pipeline caches.
pub fn load_disk_shader_caches(device: &crate::GpuDevice) {
    let cache = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .join("Library/Caches/com.latentspace.manifold");
    std::fs::create_dir_all(&cache).expect("shader cache directory");
    device.load_pipeline_archive(&cache.join("pipeline_cache.metallib"));
    device.load_msl_cache(&cache.join("msl_cache"));
}
