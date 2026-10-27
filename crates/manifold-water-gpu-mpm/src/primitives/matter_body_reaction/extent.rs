//! Buffer extent rule owned by this node.
use crate::matter::{grid_bytes, pool_extent};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::scene::world_tiles::table_words;
use manifold_water_liquid::extent::field_reads;

fn matter_body_reaction(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let pool = pool_extent(x)?;
    x.covers("grid", grid_bytes(pool))?;
    x.covers("tiles", table_words(pool) * 4)?;
    field_reads(x)
}
inventory::submit! {
    ExtentRule { type_id: "node.matter_body_reaction", check: matter_body_reaction }
}
