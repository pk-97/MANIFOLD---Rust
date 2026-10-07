pub mod compositor;
pub mod fsr1;
pub mod generator_renderer;
pub mod generators;
pub mod gpu_readback;
pub mod headless_readback;
pub mod presentation;
pub mod display_capture;
pub mod layer_compositor;
pub mod denoiser;
pub mod metalfx_temporal_upscaler;
pub mod metalfx_upscaler;
pub mod node_graph;
pub mod pq_encoder;
pub mod preset_thumbnail;
#[cfg(target_os = "macos")]
pub mod text_rasterizer;
pub mod tonemap;

// This registration moves with the assets to the catalog crate at P3.
inventory::submit!(preset_loader::PresetAssetsRoot {
    dir: concat!(env!("CARGO_MANIFEST_DIR"), "/assets"),
});

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) use manifold_gpu::testkit::{test_device, TestDevice};


// Standalone CPU specification; deliberately absent from runtime builds.
#[cfg(test)]
mod live_sim_clock_reference;

#[cfg(any(test, feature = "gpu-proofs"))]
pub mod reference_fixtures;


#[cfg(test)]
mod compositor_tests;
