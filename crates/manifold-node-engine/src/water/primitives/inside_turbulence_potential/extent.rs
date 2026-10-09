//! Buffer extent rule owned by this node.
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{particle_values, whitewater_grid};

fn inside_turbulence_potential(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells) = whitewater_grid(x)?;
    for p in ["distance", "turbulence", "cells"] { x.covers(p, cells * 4)?; }
    particle_values(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.inside_turbulence_potential", check: inside_turbulence_potential }
}
