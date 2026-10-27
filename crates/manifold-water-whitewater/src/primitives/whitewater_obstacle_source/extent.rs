//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::whitewater_pool;

fn whitewater_obstacle_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = whitewater_pool(x)?;
    let (source, distance) = (elements * 16, elements * 4);
    x.provide("source", source);
    x.provide("distance", distance);
    x.hold(source + distance);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_obstacle_source", check: whitewater_obstacle_source }
}
