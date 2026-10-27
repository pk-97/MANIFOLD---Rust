//! Buffer extent rule owned by this node.
use super::{FACE_RECORD, HISTORY_RECORD, SOURCE, StepShape};
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::bodies::LiquidShape;
use manifold_water_liquid::extent::{field_reads, tile_pool, whole};
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::tile_pool::TilePool;
use manifold_water_liquid::whitewater::DEFAULT_CAPACITY as STEP_CAPACITY;
use manifold_water_liquid::whitewater::MAX_CAPACITY as STEP_MAX_CAPACITY;
use std::mem::size_of;

fn whitewater_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let capacity = x.scalar("capacity", STEP_CAPACITY as f32).round();
    if !(1.0..=STEP_MAX_CAPACITY as f32).contains(&capacity) {
        return Err(Verdict::Refused(format!(
            "capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}"
        )));
    }
    let pool = TilePool {
        pool_tiles: whole(x, "pool_tiles", 1.0).max(1),
        cell_size: x.scalar("cell_size", 0.0625),
    };
    if !(pool.cell_size > 0.0 && pool.cell_size.is_finite()) {
        return Err(Verdict::Refused(format!(
            "cell size {} is not a length",
            pool.cell_size
        )));
    }
    let shape = StepShape::new(pool, capacity as u32).map_err(Verdict::Refused)?;
    // The tiles cover the table; every grid array holds the pool's elements.
    let elements = tile_pool(x)?;
    for port in [
        "foam_particles",
        "bubble_particles",
        "spray_particles",
        "dust_particles",
    ] {
        x.provide(port, shape.population_bytes());
    }
    x.hold(shape.held_bytes(x.items("particles").unwrap_or(0)));
    // The emit passes dispatch and count from the identity's record.
    x.covers("identity", IDENTITY_BYTES)?;
    x.covers("distance", elements * 4)?;
    x.covers("faces", elements * FACE_RECORD)?;
    x.provide("pool_out", shape.pool_bytes());
    x.provide("state_out", 32);
    x.provide("counts_out", 36);
    if x.input("obstacle_source").is_some() {
        x.covers("obstacle_source", elements * SOURCE)?;
    }
    if x.input("substep_schedule").is_some() {
        let slots = u64::from(whole(x, "substep_count", 0.0));
        x.covers("substep_schedule", slots * 16)?;
        x.covers("substep_faces", slots * elements * HISTORY_RECORD)?;
        field_reads(x)?;
        let regions = u64::from(whole(x, "region_count", 0.0));
        if regions > 0 {
            let row =
                (whole(x, "tick_index", 0.0).saturating_sub(whole(x, "first_tick", 0.0))) as u64;
            x.covers(
                "regions",
                (row + 1) * regions * size_of::<LiquidBody>() as u64,
            )?;
            x.covers("shapes", size_of::<LiquidShape>() as u64)?;
            x.covers("atlas", 4)?;
        }
    }
    x.covers("pool", shape.pool_bytes())?;
    x.covers("pool_state", 32)?;
    x.covers("solid", elements * 4)
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_step", check: whitewater_step }
}
