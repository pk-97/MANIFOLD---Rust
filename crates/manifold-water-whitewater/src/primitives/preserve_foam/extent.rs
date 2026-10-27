//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;
use manifold_water_liquid::fluid_particles::CellRange;
use std::mem::size_of;

fn preserve_foam(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("cell_ranges", elements * size_of::<CellRange>() as u64)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.preserve_foam", check: preserve_foam }
}
