use manifold_node_engine::load::preset_loader;

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



// Standalone CPU specification; deliberately absent from runtime builds.
#[cfg(test)]
mod live_sim_clock_reference;

#[cfg(any(test, feature = "gpu-proofs"))]
pub mod reference_fixtures;


#[cfg(test)]
mod compositor_tests;

// Catalog contracts keep their engine module identities across the P1 split.
#[cfg(test)]
#[path = "engine_contract_tests/exec.rs"]
mod exec;

#[cfg(test)]
#[path = "engine_contract_tests/freeze.rs"]
mod freeze;

#[cfg(test)]
#[path = "../tests/support/source_roots.rs"]
mod source_roots;

#[cfg(test)]
#[path = "engine_contract_tests/palette.rs"]
mod palette;

#[cfg(test)]
#[path = "engine_contract_tests/preview_encoding.rs"]
mod preview_encoding;

#[cfg(test)]
#[path = "engine_contract_tests/water.rs"]
mod water;

#[cfg(test)]
#[path = "engine_contract_tests/load.rs"]
mod load;

#[cfg(test)]
#[path = "engine_contract_tests/runtime.rs"]
mod runtime;
