use manifold_node_engine::load::preset_loader;

pub mod generators;
pub mod node_graph;

// This registration moves with the assets to the catalog crate at P3.
inventory::submit!(preset_loader::PresetAssetsRoot {
    dir: concat!(env!("CARGO_MANIFEST_DIR"), "/assets"),
});



// Standalone CPU specification; deliberately absent from runtime builds.



#[cfg(test)]
mod compositor_tests;

// Catalog contracts keep their engine module identities across the P1 split.


#[cfg(test)]
use manifold_nodes::testkit::source_roots;






use manifold_nodes_image as _;

use manifold_nodes_scene as _;

use manifold_compositor as _;

