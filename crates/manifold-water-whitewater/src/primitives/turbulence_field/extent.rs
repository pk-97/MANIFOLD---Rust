//! Buffer extent rule owned by this node.
use manifold_water_liquid::whitewater::cell_total;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::{whitewater_faces, whitewater_lattice};

fn turbulence_field(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("distance", cell_total(cells) * 4)?;
    x.covers("out", cell_total(cells) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.turbulence_field", check: turbulence_field }
}
