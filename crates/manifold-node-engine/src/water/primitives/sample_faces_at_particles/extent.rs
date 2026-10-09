//! Buffer extent rule owned by this node.
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{particle_map, whitewater_faces, whitewater_lattice};

fn sample_faces_at_particles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, _) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    particle_map(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.sample_faces_at_particles", check: sample_faces_at_particles }
}
