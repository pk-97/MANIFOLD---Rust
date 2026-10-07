//! Buffer extent rule owned by this node.
use crate::node_graph::liquid::grid::face_len;
use crate::node_graph::liquid::lattice::FlipSolverGrid;
use crate::node_graph::primitives::face_sample_component::axis_param;
use crate::node_graph::primitives::gpu_flip_step::face_bytes;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn face_sample_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = FlipSolverGrid::from_lattice(x.lattice()?).cells();
    let Some(axis) = axis_param(x.params()) else {
        return Err(Verdict::Refused("the axis is not X, Y or Z".into()));
    };
    x.covers("faces", face_bytes(cells))?;
    x.covers("out", face_len(cells, axis) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.face_sample_component", check: face_sample_component }
}
