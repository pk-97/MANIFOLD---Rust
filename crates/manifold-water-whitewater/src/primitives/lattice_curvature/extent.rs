//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{KNOWN_VALUE, tile_pool};

fn lattice_curvature(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("distance", elements * 4)?;
    x.covers("out", elements * KNOWN_VALUE)
}
inventory::submit! {
    ExtentRule { type_id: "node.lattice_curvature", check: lattice_curvature }
}
