//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn push_out_of_solid(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = tile_pool(x)?;
    x.covers("solid", elements * 4)?;
    let particles = x
        .bytes("particles")
        .ok_or_else(|| x.uncovered("particles is unbound".into()))?;
    x.covers("out", particles)
}
inventory::submit! {
    ExtentRule { type_id: "node.push_out_of_solid", check: push_out_of_solid }
}
