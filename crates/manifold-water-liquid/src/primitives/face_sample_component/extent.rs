//! Buffer extent rule owned by this node.
use crate::extent::liquid_lattice;
use crate::grid::face_len;
use crate::lattice::FlipSolverGrid;
use crate::primitives::face_sample_component::{axis_param, pool_tiles_wire, pooled_bytes};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn face_sample_component(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = FlipSolverGrid::from_lattice(liquid_lattice(x)?).cells();
    let Some(axis) = axis_param(x.params()) else {
        return Err(Verdict::Refused("the axis is not X, Y or Z".into()));
    };
    let pool_tiles =
        pool_tiles_wire(|name, default| x.scalar(name, default)).map_err(Verdict::Refused)?;
    let (faces, tiles) = pooled_bytes(pool_tiles);
    x.covers("faces", faces)?;
    x.covers("tiles", tiles)?;
    x.covers("out", face_len(cells, axis) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.face_sample_component", check: face_sample_component }
}
