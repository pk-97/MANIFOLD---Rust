//! Buffer extent rule owned by this node.
use crate::water::liquid::grid::face_len;
use crate::water::whitewater::cell_total;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, field_reads, whitewater_faces, whitewater_lattice, whole};

fn advect_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, _) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    let cells=whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    let steps=u64::from(whole(x,"substep_count",0.0));
    if steps>0 {
        x.covers("substep_schedule",steps*16)?;
        for (axis,port) in ["substep_u","substep_v","substep_w"].into_iter().enumerate() { x.covers(port,steps*face_len(cells,axis)*4)?; }
    }
    field_reads(x)?;
    x.covers("solid", cell_total(nodes) * 4)?;
    x.covers("out", x.bytes("pool").unwrap_or(0))
}
inventory::submit! {
    ExtentRule { type_id: "node.advect_whitewater", check: advect_whitewater }
}
