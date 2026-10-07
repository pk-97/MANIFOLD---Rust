//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn whitewater_obstacle_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let bytes = x.lattice()?.solid_bytes() * 4;
    x.provide("solid", bytes); x.hold(bytes); Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_obstacle_source", check: whitewater_obstacle_source }
}
