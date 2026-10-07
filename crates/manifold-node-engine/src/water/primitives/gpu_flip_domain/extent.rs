//! Buffer extent rule owned by this node.
use std::mem::size_of;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use crate::water::fluid_role::MAX_FLUID_ROLES;
use crate::water::liquid::bodies::LiquidBody;
use crate::water::liquid::bodies::LiquidShape;
use crate::water::liquid::clock::FIELD_RESERVE_INTERVALS;
use crate::water::liquid::fields::FieldFrame;
use crate::water::liquid::fields::STAGING_SLOTS as FIELD_STAGING_SLOTS;
use crate::water::liquid::coupling::REACTION_FLOATS;
use crate::water::primitives::gpu_flip_domain::gpu_flip_geometry;
use crate::water::fluid::TICK;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, Verdict};

fn gpu_flip_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let geometry = gpu_flip_geometry(
        |name, default| x.scalar(name, default),
        x.transform("domain"),
        x.transform("initial_volume"),
    )
    .map_err(Verdict::Refused)?;
    for (name, value) in geometry.outputs() {
        x.publish(name, value);
    }
    // Body kernels clamp rows and body counts to the bodies array, so the
    // walk takes a scene without colliders; the reaction is sized for every
    // body a liquid holds.
    x.publish("body_count", 0.0);
    x.publish("body_rows", 0.0);
    x.publish("dynamic_bodies", 0.0);
    x.publish("region_count", 0.0);
    x.publish("clock_obstacle_count", 0.0);
    x.publish("clock_source_count", 0.0);
    x.publish("live_hit_count", 0.0);
    x.publish("interval_duration", TICK as f32);
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame { lattice: geometry.field_lattice(), force_lattices: FIELD_RESERVE_INTERVALS, impulse_tick: Some(0) };
    let forces = u64::from(FIELD_RESERVE_INTERVALS) * field.lattice.bytes();
    for (name, value) in field.outputs() {
        x.publish(name, value);
    }
    for (port, bytes) in [
        ("bodies", size_of::<LiquidBody>() as u64),
        ("contacts", size_of::<crate::water::liquid::bodies::BodySupports>() as u64),
        ("regions", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("clock_obstacles", 96),
        ("clock_sources", 96),
        ("live_hits", 16),
        ("reaction", (MAX_FLUID_ROLES * REACTION_FLOATS * 4) as u64),
        ("forces", forces),
        ("impulses", field.lattice.bytes()),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The staging ring: the impulse lattice and the force lattices per slot.
    x.hold(FIELD_STAGING_SLOTS as u64 * (field.lattice.bytes() + forces + 16));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: GPU_FLIP_DOMAIN_TYPE_ID, check: gpu_flip_domain }
}
