//! Buffer extent rule owned by this node.
use crate::water::primitives::whitewater_lifecycle::DEFAULT_CAPACITY;
use crate::water::primitives::whitewater_lifecycle::MAX_CAPACITY;
use crate::water::whitewater::face_offset;
use crate::water::whitewater::grid_box;
use crate::water::whitewater::require_extended_faces;
use crate::water::whitewater_handoff::OUTPUT_SLOTS;
use crate::water::whitewater_handoff::SNAPSHOT_SLOTS;
use crate::water::whitewater_handoff::SnapshotShape;
use manifold_fluids::WhitewaterGrid;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{PARTICLE, whitewater_lattice, whole};

fn whitewater_lifecycle(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = whole(x, "capacity", DEFAULT_CAPACITY as f32).clamp(1, MAX_CAPACITY);
    let population = u64::from(capacity) * PARTICLE;
    for port in ["foam_particles", "bubble_particles", "spray_particles", "dust_particles"] {
        x.provide(port, population);
    }
    x.hold(OUTPUT_SLOTS as u64 * 3 * population);
    let (nodes, cells) = whitewater_lattice(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"])?;
    let face_cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| whole(x, name, 0.0));
    let face_offset = face_offset(nodes, face_cells).map_err(Verdict::Refused)?;
    require_extended_faces(x.scalar("face_valid_layers", 0.0)).map_err(Verdict::Refused)?;
    let bounds = x.transform("grid_bounds").ok_or_else(|| Verdict::Refused("the grid_bounds input is not wired".into()))?;
    let (origin, cell_size) = grid_box(bounds, nodes).map_err(Verdict::Refused)?;
    let shape = SnapshotShape { grid: WhitewaterGrid { cells, cell_size, origin }, face_cells, face_offset, capacity };
    x.hold(SNAPSHOT_SLOTS as u64 * shape.slot_bytes());
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        x.covers(port, shape.face_bytes(axis))?;
    }
    x.covers("level", shape.level_bytes())?;
    x.covers("solid", shape.solid_bytes())
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_lifecycle", check: whitewater_lifecycle }
}
