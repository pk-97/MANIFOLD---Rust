//! Buffer extent rule owned by this node.
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{whitewater_grid};

fn liquid_cells(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_grid(x)?;
    x.covers("distance", cells * 4)?;
    x.covers("solid", nodes * 4)?;
    x.covers("out", cells * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_cells", check: liquid_cells }
}
