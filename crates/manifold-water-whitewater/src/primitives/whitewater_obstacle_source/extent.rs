//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::liquid_lattice;

fn whitewater_obstacle_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let bytes = liquid_lattice(x)?.solid_bytes() * 4;
    x.provide("solid", bytes);
    x.hold(bytes);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_obstacle_source", check: whitewater_obstacle_source }
}
