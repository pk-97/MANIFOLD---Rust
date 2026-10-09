//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::water::primitives::whitewater_lifecycle::DEFAULT_CAPACITY;
use crate::water::whitewater::cell_total;
use crate::water::fluid_particles::WhitewaterSpawn;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{PARTICLE, whitewater_faces, whitewater_lattice, whole};

fn spawn_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (nodes, _) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    whitewater_faces(x, nodes, ["face_cells_x", "face_cells_y", "face_cells_z"])?;
    x.covers("solid", cell_total(nodes) * 4)?;
    if x.wired("emitters") {
        let emitters = u64::from(x.count("emitters", 0.0)?);
        x.covers("particles", emitters * PARTICLE)?;
        x.covers("energy", emitters * 4)?;
        x.covers("offsets", emitters * 4)?;
    }
    let slots = u64::from(whole(x, "capacity", DEFAULT_CAPACITY as f32));
    x.covers("out", slots * size_of::<WhitewaterSpawn>() as u64)
}
inventory::submit! {
    ExtentRule { type_id: "node.spawn_whitewater", check: spawn_whitewater }
}
