//! Buffer extent rule owned by this node.
use crate::water::whitewater::cell_total;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, whitewater_faces, whitewater_lattice};

fn retype_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("distance", cell_total(cells) * 4)?;
    x.covers("cells", cell_total(cells) * 4)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.retype_whitewater", check: retype_whitewater }
}
