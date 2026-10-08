//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn liquid_solid_distance(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // One thread per lattice node over storage sized from the same lattice.
    let solid = x.lattice()?.solid_bytes();
    x.provide("solid", solid);
    x.hold(solid);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_solid_distance", check: liquid_solid_distance }
}
