//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, KNOWN_VALUE, Verdict, whitewater_grid};

fn extend_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    x.covers("values", cells * KNOWN_VALUE)?;
    x.covers("out", cells * KNOWN_VALUE)
}
inventory::submit! {
    ExtentRule { type_id: "node.extend_lattice", check: extend_lattice }
}
