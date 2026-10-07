//! Buffer extent rule owned by this node.
use crate::water::whitewater::SURFACE_CROSSING_BYTES;
use crate::water::whitewater::cell_total;
use crate::water::whitewater::refinement;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, whitewater_lattice, whole};

fn surface_crossings(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    let levels = ["level_nodes_x", "level_nodes_y", "level_nodes_z"].map(|name| whole(x, name, 211.0));
    refinement(nodes, levels).map_err(Verdict::Refused)?;
    x.covers("out", cell_total(cells) * SURFACE_CROSSING_BYTES)?;
    x.covers("solid", cell_total(nodes) * 4)?;
    x.covers("level_set", cell_total(levels) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_crossings", check: surface_crossings }
}
