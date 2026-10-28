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
    // The disk shader caches let every test process reuse the shaders an
    // earlier one compiled instead of running naga and spirv-opt again.
    let device = SHARED
        .get_or_init(|| {
            let device = crate::GpuDevice::new_queued("GPU tests");
            load_disk_shader_caches(&device);
            Arc::new(device)
        })
        .clone();
    TestDevice { device, _lock }
}

/// The first `len` bytes of `buffer`, a buffer of the shared test device
/// whose GPU work is done: straight from its mapping, or for a `Gpu` buffer
/// through a copy into a `Readback` one.
#[cfg(feature = "gpu-proofs")]
pub fn read_bytes(buffer: &crate::GpuBuffer, len: usize) -> Vec<u8> {
    if let Some(ptr) = buffer.mapped_ptr() {
        // SAFETY: a mapped buffer at least `len` bytes long, its GPU work done.
        return unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    }
    let device = test_device();
    let staged = device.create_buffer(len as u64, crate::BufferUse::Readback);
    let mut enc = device.create_encoder("testkit readback");
    enc.copy_buffer_to_buffer(buffer, &staged, len as u64);
    enc.commit_and_wait().expect("GPU work completes");
    let ptr = staged.mapped_ptr().expect("readback buffers are mapped");
    // SAFETY: the readback buffer is `len` bytes and its copy has completed.
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Reuse the app's shader disk caches without populating live pipeline
/// caches. Each backend keeps its own files there: Metal a metallib archive
/// and MSL, Vulkan a `VkPipelineCache` blob and SPIR-V.
pub fn load_disk_shader_caches(device: &crate::GpuDevice) {
    let cache = manifold_foundation::AppDirs::current()
        .expect("a home directory")
        .cache;
    std::fs::create_dir_all(&cache).expect("shader cache directory");
    device.load_pipeline_archive(&cache.join("pipeline_cache.metallib"));
    device.load_msl_cache(&cache.join("msl_cache"));
}
