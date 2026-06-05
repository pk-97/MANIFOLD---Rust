pub mod background_worker;
pub mod chain_dispatch;
pub mod compositor;
pub mod effect;
pub mod effect_chain_graph;
pub mod effects;
pub mod fsr1;
pub mod generator;
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
pub mod preset_context;
pub mod preset_loader;
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

/// Clear `target` to `rgba` and commit the encoder before returning,
/// so the GPU has actually performed the clear by the time the caller
/// uses the texture.
///
/// Test-only convenience: a freshly-created encoder with a
/// `clear_texture` call recorded but never committed silently
/// discards the clear (Metal commands don't execute until commit).
/// Subsequent reads of the texture then see uninitialised
/// (often all-zero) memory, which can pass tests for the wrong
/// reason — black inputs pass against `expected = 0`, white inputs
/// fail noisily. This helper owns the encoder + commit so the bug
/// can't recur.
///
/// Stalls the calling thread until the clear completes; meant for
/// test setup, not hot-path work.
#[cfg(test)]
pub(crate) fn clear_texture_committed(
    device: &manifold_gpu::GpuDevice,
    target: &manifold_gpu::GpuTexture,
    rgba: [f64; 4],
    label: &str,
) {
    let mut enc = device.create_encoder(label);
    {
        let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut enc, device);
        gpu.clear_texture(target, rgba[0], rgba[1], rgba[2], rgba[3]);
    }
    enc.commit_and_wait_completed();
}
