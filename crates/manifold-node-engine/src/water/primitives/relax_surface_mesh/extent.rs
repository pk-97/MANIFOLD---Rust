//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, surface_mesh_pass};

fn relax_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "relaxed")
}
inventory::submit! {
    ExtentRule { type_id: "node.relax_surface_mesh", check: relax_surface_mesh }
}
