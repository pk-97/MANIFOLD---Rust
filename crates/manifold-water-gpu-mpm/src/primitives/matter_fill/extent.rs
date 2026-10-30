//! Buffer extent rule owned by this node.
use crate::matter::MAX_MATTER_SLOTS;
use crate::matter::MatterPoint;
use crate::primitives::matter_domain::FILL_ROW_WORDS;
use crate::primitives::matter_fill::FillGrid;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::whole;
use std::mem::size_of;

fn matter_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    // The grid wires are refused by name as the node refuses them.
    FillGrid::read(|name, default| x.scalar(name, default)).map_err(Verdict::Refused)?;
    let count = whole(x, "fill_points", 0.0);
    x.covers("fills", (FILL_ROW_WORDS * MAX_MATTER_SLOTS * 4) as u64)?;
    x.publish("count", count as f32);
    let points = u64::from(count.max(1)) * size_of::<MatterPoint>() as u64;
    x.provide("points", points);
    x.hold(points);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_fill", check: matter_fill }
}
