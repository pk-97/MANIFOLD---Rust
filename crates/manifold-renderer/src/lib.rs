pub mod background_worker;
pub mod chain_dispatch;
pub mod compositor;
pub mod effect;
pub mod effect_chain_graph;
pub mod effects;
pub mod fsr1;
pub mod generator;
pub mod generator_context;
pub mod generator_renderer;
pub mod generators;
pub mod gpu;
pub mod gpu_encoder;
pub mod gpu_readback;
pub mod gpu_types;
pub mod layer_bitmap_gpu;
pub mod layer_compositor;
pub mod metalfx_upscaler;
#[cfg(target_os = "macos")]
pub mod native_text;
pub mod node_graph;
pub mod plugin_prewarm;
pub mod pq_encoder;
pub mod render_target;
pub mod render_target_pool;
#[cfg(target_os = "macos")]
pub mod text_rasterizer;
pub mod tonemap;
pub mod ui_cache_manager;
pub mod ui_renderer;
pub mod uniform_arena;

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
#[cfg(test)]
pub(crate) fn test_device() -> std::sync::Arc<manifold_gpu::GpuDevice> {
    use std::sync::{Arc, OnceLock};
    static SHARED: OnceLock<Arc<manifold_gpu::GpuDevice>> = OnceLock::new();
    SHARED
        .get_or_init(|| Arc::new(manifold_gpu::GpuDevice::new()))
        .clone()
}
