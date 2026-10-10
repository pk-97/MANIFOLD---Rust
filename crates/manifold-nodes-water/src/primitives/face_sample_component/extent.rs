//! Buffer extent rule owned by this node.
use crate::liquid::extent::liquid_lattice;
use crate::liquid::grid::face_len;
use crate::liquid::lattice::FlipSolverGrid;
use crate::primitives::face_sample_component::axis_param;
use crate::liquid::grid::face_bytes;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn face_sample_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = FlipSolverGrid::from_lattice(liquid_lattice(x)?).cells();
    let Some(axis) = axis_param(x.params()) else {
        return Err(Verdict::Refused("the axis is not X, Y or Z".into()));
    };
    x.covers("faces", face_bytes(cells))?;
    x.covers("out", face_len(cells, axis) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.face_sample_component", check: face_sample_component }
}
