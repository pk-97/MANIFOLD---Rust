//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::node_graph::matter::MatterPoint;
use crate::node_graph::primitives::matter_fill::fill_count;
use crate::node_graph::liquid::extent::{AtomExtent, ExtentRule, Verdict, whole};

fn matter_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let lattice = x.lattice()?;
    let cells = lattice.cells();
    let pool = whole(x, "pool_cells", 3.0).min(cells[1]);
    let column = [["column_x0", "column_x1"], ["column_y0", "column_y1"], ["column_z0", "column_z1"]]
        .map(|[lo, hi]| [whole(x, lo, 0.0), whole(x, hi, 0.0)]);
    let column = std::array::from_fn(|d| [column[d][0].min(cells[d]), column[d][1].min(cells[d])]);
    let ppc = if whole(x, "points_per_cell", 8.0) >= 27 { 27 } else { 8 };
    let count = fill_count(cells, pool, column, ppc).map_err(Verdict::Refused)?;
    x.publish("count", count as f32);
    let points = u64::from(count.max(1)) * size_of::<MatterPoint>() as u64;
    x.provide("points", points);
    x.hold(points);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_fill", check: matter_fill }
}
