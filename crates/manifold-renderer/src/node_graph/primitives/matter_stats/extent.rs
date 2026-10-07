//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::node_graph::matter::MatterGridNode;
use crate::node_graph::matter::STATS_WORDS;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, node_extent};

fn matter_stats(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    let nodes = node_extent(x, &lattice)?;
    x.covers("stats", u64::from(STATS_WORDS) * 4)?;
    x.covers("grid", nodes * size_of::<MatterGridNode>() as u64)?;
    x.covers("accum", nodes * 16)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_stats", check: matter_stats }
}
