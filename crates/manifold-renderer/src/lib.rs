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

// This registration moves with the assets to the catalog crate at P3.
inventory::submit!(preset_loader::PresetAssetsRoot {
    dir: concat!(env!("CARGO_MANIFEST_DIR"), "/assets"),
});

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) use manifold_gpu::testkit::{test_device, TestDevice};

#[cfg(any(test, feature = "gpu-proofs"))]
pub mod testkit;

// Standalone CPU specification; deliberately absent from runtime builds.
#[cfg(test)]
mod live_sim_clock_reference;

#[path = "generators/compute_common.rs"]
pub mod particles;
#[path = "generators/mesh_common.rs"]
pub mod mesh;

#[cfg(test)]
mod compositor_tests;
