//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::surface_mesh_pass;

fn surface_mesh_normals(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "out")
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_mesh_normals", check: surface_mesh_normals }
}
