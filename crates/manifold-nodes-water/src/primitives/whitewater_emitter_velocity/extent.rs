//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::{particle_map, whitewater_grid};

fn whitewater_emitter_velocity(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    for p in ["distance", "cells"] { x.covers(p, cells * 4)?; }
    particle_map(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_emitter_velocity", check: whitewater_emitter_velocity }
}
