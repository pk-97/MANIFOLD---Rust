//! Buffer extent rule owned by this node.
use std::mem::size_of;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict, size_bounded, whole_param};

fn make_triangles(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cols = u64::from(whole_param(x, "src_cols", 256.0).max(2));
    let rows = u64::from(whole_param(x, "src_rows", 256.0).max(2));
    let bytes = (cols * rows).checked_mul(size_of::<MeshVertex>() as u64)
        .ok_or_else(|| x.uncovered("source grid byte size overflows u64".into()))?;
    // Quad corners and finite-difference neighbours gather across the grid.
    x.covers("in", bytes)?;
    // Writes use dst capacity as their guard; excess slots are degenerate padding.
    size_bounded(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.make_triangles", check: make_triangles }
}
