use manifold_node_engine::load::preset_loader;
use manifold_nodes_image as _;
use manifold_nodes_scene as _;

inventory::submit!(preset_loader::PresetAssetsRoot {
    dir: concat!(env!("CARGO_MANIFEST_DIR"), "/assets"),
});

pub mod bundled_generator_presets;
pub mod registry;
pub(crate) mod bundled_presets;
pub mod catalog_gen;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
