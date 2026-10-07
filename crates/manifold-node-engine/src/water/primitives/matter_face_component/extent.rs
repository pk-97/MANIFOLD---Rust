//! Buffer extent rule owned by this node.
use crate::water::liquid::grid::face_len;
use crate::water::matter::grid_bytes;
use crate::water::primitives::face_sample_component::axis_param;
use crate::water::primitives::matter_face_component::matter_cells;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict, whole};

fn matter_face_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| whole(x, name, 71.0));
    let (Some(cells), Some(axis)) = (matter_cells(nodes), axis_param(x.params())) else {
        return Err(Verdict::Refused(format!("a {nodes:?} node lattice has no cells, or the axis is not X, Y or Z")));
    };
    x.covers("grid", grid_bytes(nodes))?;
    x.covers("out", face_len(cells, axis) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_face_component", check: matter_face_component }
}
