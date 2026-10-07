//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, size_bounded};

fn gltf_mesh_source(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // The asset's decoded vertex count is unknown here. Uploads truncate to
    // the bound output capacity and copies clamp to dst.size; reserve the
    // largest retained staging buffer that upload can create (at least 1 byte).
    // An unbound vertices port only publishes the CPU source descriptor.
    if let Some(bytes) = x.bytes("vertices") {
        x.hold(bytes.max(1));
    }
    size_bounded(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.gltf_mesh_source", check: gltf_mesh_source }
}
