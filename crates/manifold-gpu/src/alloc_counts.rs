//! Device-wrapper identity and the per-thread allocation counters both
//! backends keep for warmup and steady-state probes.

/// BUG-olp9 diagnostic: MANIFOLD_GPU_ALLOC_LOG=1 prints every texture/buffer
/// allocation (label, dims, format, bytes) to stderr. Census tool for the
/// RT-startup memory transient — not hot-path safe, env-gated, read once.
/// MANIFOLD_GPU_ALLOC_BT=1 additionally prints a caller backtrace per event.
pub(crate) fn alloc_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("MANIFOLD_GPU_ALLOC_LOG").is_some())
}

pub(crate) fn alloc_log_backtrace() {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("MANIFOLD_GPU_ALLOC_BT").is_some()) {
        let bt = std::backtrace::Backtrace::force_capture();
        eprintln!("[gpu-alloc-bt] {bt}");
    }
}

pub(crate) const BUFFER_ALLOCATION: usize = 0;
pub(crate) const TEXTURE_ALLOCATION: usize = 1;
#[cfg_attr(vulkan_backend, allow(dead_code))]
pub(crate) const ACCEL_ALLOCATION: usize = 2;

thread_local! {
    /// Allocations made on this thread, per device: (resource scope id,
    /// [buffers, textures, acceleration structures]). Per thread so a probe
    /// that renders on its own thread counts only its own work while other
    /// threads share the device (the parallel gpu-proofs binary).
    static THREAD_ALLOCATIONS: std::cell::RefCell<Vec<(u64, [u64; 3])>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A fresh process-local identity for one device wrapper's resources.
pub(crate) fn next_resource_scope_id() -> u64 {
    static NEXT_RESOURCE_SCOPE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT_RESOURCE_SCOPE
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| id.checked_add(1),
        )
        .expect("GPU resource scope identifiers exhausted")
}

/// The calling thread's [buffers, textures, acceleration structures] counts
/// on the wrapper with `scope`.
pub(crate) fn allocation_counts(scope: u64) -> [u64; 3] {
    THREAD_ALLOCATIONS.with_borrow(|counts| {
        counts
            .iter()
            .find(|(id, _)| *id == scope)
            .map_or([0; 3], |(_, count)| *count)
    })
}

pub(crate) fn record_allocation(scope: u64, kind: usize) {
    THREAD_ALLOCATIONS.with_borrow_mut(|counts| {
        match counts.iter_mut().find(|(id, _)| *id == scope) {
            Some((_, count)) => count[kind] += 1,
            None => {
                let mut count = [0; 3];
                count[kind] = 1;
                counts.push((scope, count));
            }
        }
    });
}
