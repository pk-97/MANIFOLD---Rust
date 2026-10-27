//! Buffer extent rule owned by this node.
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::extent::tile_pool;

fn carry_liquid_into_solids(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let field = tile_pool(x)? * 4;
    x.covers("levelset", field)?;
    x.covers("out", field)?;
    x.covers("solid", field)?;
    x.covers_if_bound("drawn_solid", field)
}
inventory::submit! {
    ExtentRule { type_id: "node.carry_liquid_into_solids", check: carry_liquid_into_solids }
}
