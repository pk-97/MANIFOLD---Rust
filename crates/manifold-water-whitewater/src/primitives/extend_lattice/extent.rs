//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{KNOWN_VALUE, whitewater_pool};

fn extend_lattice(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let elements = whitewater_pool(x)?;
    x.covers("values", elements * KNOWN_VALUE)?;
    x.covers("out", elements * KNOWN_VALUE)
}
inventory::submit! {
    ExtentRule { type_id: "node.extend_lattice", check: extend_lattice }
}
