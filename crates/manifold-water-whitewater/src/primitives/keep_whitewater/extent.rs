//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::bodies::LiquidShape;
use manifold_water_liquid::extent::{tile_pool, whole};
use manifold_water_liquid::fluid_particles::CellRange;
use std::mem::size_of;

fn keep_whitewater(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let regions = u64::from(whole(x, "region_count", 0.0));
    if regions > 0 {
        x.covers(
            "regions",
            (regions + u64::from(whole(x, "region_offset", 0.0))) * size_of::<LiquidBody>() as u64,
        )?;
        x.covers("shapes", size_of::<LiquidShape>() as u64)?;
        x.covers("atlas", 4)?;
    }
    let elements = tile_pool(x)?;
    x.covers("solid", elements * 4)?;
    x.covers("cell_ranges", elements * size_of::<CellRange>() as u64)?;
    x.covers("out", x.items("pool").unwrap_or(0) * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.keep_whitewater", check: keep_whitewater }
}
