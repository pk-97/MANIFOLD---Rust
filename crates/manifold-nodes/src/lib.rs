//! Node catalog and bundled presets.
//! Links the image and scene families and owns preset loading, catalog tools
//! and tests that span families.
//! Never depends on UI, editing, IO, the app, compositor or UI paint.

use manifold_node_engine::load::preset_loader;
use manifold_nodes_image as _;
use manifold_nodes_scene as _;

inventory::submit!(preset_loader::PresetAssetsRoot {
    dir: concat!(env!("CARGO_MANIFEST_DIR"), "/assets"),
});

pub mod bundled_generator_presets;
pub mod registry;
pub mod bundled_presets;
pub mod catalog_gen;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
