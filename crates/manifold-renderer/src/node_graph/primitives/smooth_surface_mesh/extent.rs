//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, surface_mesh_pass};

fn smooth_surface_mesh(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    surface_mesh_pass(x, "relaxed")?;
    // The stage retains one ping-pong mesh when iterations exceeds one.
    // A scalar wire can change that count without a graph/extent rebuild.
    x.hold(x.bytes("vertices").unwrap_or(0));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.smooth_surface_mesh", check: smooth_surface_mesh }
}
