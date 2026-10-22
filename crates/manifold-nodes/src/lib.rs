//! Node catalog and bundled presets.
//! Links the image, scene and water families and owns preset loading, catalog tools
//! and tests that span families.
//! Never depends on UI, editing, IO, the app, compositor or UI paint.

use manifold_node_engine::load::preset_loader;
use manifold_nodes_image as _;
use manifold_nodes_scene as _;
use manifold_nodes_water as _;
use manifold_nodes_physics as _;

inventory::submit!(preset_loader::PresetAssetsRoot {
    dir: concat!(env!("CARGO_MANIFEST_DIR"), "/assets"),
});

pub mod bundled_generator_presets;
pub mod registry;
pub mod bundled_presets;
#[cfg(any(test, feature = "engine-scenes"))]
pub mod engine_scenes;
pub mod catalog_gen;
pub mod scene_modifier_qualify;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
