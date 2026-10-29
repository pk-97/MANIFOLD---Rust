//! Buffer extent rule owned by this node.
use crate::primitives::liquid_fill::fill_frame;
use crate::primitives::liquid_fill::fill_of;
use crate::primitives::liquid_fill::filled_sites;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_water_liquid::extent::PARTICLE;

fn liquid_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (_, cells, _) = fill_frame(|name, default| x.scalar(name, default))
        .map_err(|reason| Verdict::Refused(format!("Liquid Fill: {reason}")))?;
    let (pool, sites) = fill_of(|name, default| x.scalar(name, default));
    let filled = filled_sites(cells, pool, sites);
    if filled > u64::from(EXACT_F32_COUNT) {
        return Err(Verdict::Refused(format!(
            "Liquid Fill: the fill holds {filled} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly"
        )));
    }
    x.publish("count", filled as f32);
    let bytes = filled.max(1) * PARTICLE;
    x.provide("particles", bytes);
    x.hold(bytes);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_fill", check: liquid_fill }
}
