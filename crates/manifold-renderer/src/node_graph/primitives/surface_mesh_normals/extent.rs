//! Buffer extent rule owned by this node.
use manifold_node_engine::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, surface_mesh_pass};

fn surface_mesh_normals(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "out")
}
inventory::submit! {
    ExtentRule { type_id: "node.surface_mesh_normals", check: surface_mesh_normals }
}
