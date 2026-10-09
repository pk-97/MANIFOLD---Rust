//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::matter::MatterPoint;
use crate::matter::STATS_WORDS;
use crate::matter::grid_accum_bytes;
use crate::matter::grid_bytes;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::whole;

fn matter_state(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| whole(x, name, 71.0));
    let count = u64::from(x.count("count", 0.0)?);
    let (accum, grid) = (grid_accum_bytes(nodes).max(32), grid_bytes(nodes).max(32));
    x.provide("grid_accum", accum);
    x.provide("grid", grid);
    x.hold(accum + grid + 4 * u64::from(STATS_WORDS) * 4);
    // A tick's first and last substep: the atoms gated on them run.
    x.publish("tick_start", 1.0);
    x.publish("tick_end", 1.0);
    // A new epoch copies the fill into the state.
    x.covers("seed", count * size_of::<MatterPoint>() as u64)?;
    x.covers("out", count * size_of::<MatterPoint>() as u64)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_state", check: matter_state }
}
