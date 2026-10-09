//! Buffer extent rule owned by this node.
use crate::water::liquid::extent::liquid_lattice;
use crate::ports::EXACT_F32_COUNT;
use crate::water::primitives::liquid_fill::fill_of;
use crate::water::primitives::liquid_fill::filled_sites;
use crate::water::primitives::liquid_fill::pool_slots;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{PARTICLE};

fn liquid_fill(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let cells = liquid_lattice(x)?.cells();
    let (pool, sites) = fill_of(|name, default| x.scalar(name, default));
    let placed = pool_slots(filled_sites(cells, pool, sites), x.scalar("particle_capacity", 0.0));
    if placed > u64::from(EXACT_F32_COUNT) {
        return Err(Verdict::Refused(format!(
            "Liquid Fill: the pool holds {placed} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly"
        )));
    }
    x.publish("count", placed as f32);
    let bytes = placed.max(1) * PARTICLE;
    x.provide("particles", bytes);
    x.hold(bytes);
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_fill", check: liquid_fill }
}
