pub mod background_worker;
pub mod chain_dispatch;
pub mod compositor;
pub mod effect;
pub mod effects;
pub mod frame_status;
pub mod fsr1;
pub mod generator_renderer;
pub mod generators;
pub mod gpu;
pub mod gpu_encoder;
pub mod gpu_readback;
pub mod gpu_types;
pub mod headless_readback;
pub mod presentation;
pub mod display_capture;
pub mod layer_compositor;
pub mod layer_skin;
pub mod denoiser;
pub mod metalfx_temporal_upscaler;
pub mod metalfx_upscaler;
pub mod node_graph;
pub mod plugin_prewarm;
pub mod pq_encoder;
pub mod preset_context;
pub mod preset_loader;
pub mod preset_runtime;
pub mod preset_thumbnail;
pub mod render_target;
pub mod render_target_pool;
#[cfg(target_os = "macos")]
pub mod text_rasterizer;
pub mod tonemap;
pub mod uniform_arena;

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) use manifold_gpu::testkit::{test_device, TestDevice};

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
#[cfg(all(test, feature = "gpu-proofs"))]
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

// Standalone CPU specification; deliberately absent from runtime builds.
#[cfg(test)]
mod live_sim_clock_reference;
