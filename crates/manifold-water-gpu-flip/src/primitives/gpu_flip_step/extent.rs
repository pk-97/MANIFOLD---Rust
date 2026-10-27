//! Buffer extent rule owned by this node.
use crate::primitives::gpu_flip_bodies::held_bytes as body_pass_bytes;
use crate::primitives::gpu_flip_pressure::scratch_bytes as pressure_scratch_bytes;
use crate::primitives::gpu_flip_step::scratch_bytes as step_scratch_bytes;
use crate::primitives::gpu_flip_step::{
    growth_ceiling, history_slots, read_closed_faces, read_open_faces,
};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::coupling::REACTION_FLOATS;
use manifold_water_liquid::extent::{PARTICLE, body_rows, field_reads, whole};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::primitives::sort_particles_into_cells::key_storage_bytes;
use manifold_water_liquid::tile_pool::pool_tiles_wire;
use manifold_water_liquid::tile_storage::{pool_len, table_words};
use std::mem::size_of;

fn gpu_flip_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("identity", IDENTITY_BYTES)?;
    x.covers_if_bound("identity_out", IDENTITY_BYTES)?;
    let refused = |reason: String| Verdict::Refused(format!("GPU FLIP Step: {reason}"));
    let start = pool_tiles_wire(|name, default| x.scalar(name, default)).map_err(refused)?;
    let walls = read_closed_faces(x.scalar("closed_faces", 0.0)).map_err(refused)?;
    let open = read_open_faces(x.scalar("open_faces", 0.0)).map_err(refused)?;
    // The most the pool can grow to.
    let pool = growth_ceiling(start, walls, open);
    x.provide("faces", pool_len(pool) * size_of::<FaceSample>() as u64);
    x.provide("distance", pool_len(pool) * 4);
    x.provide("tiles", table_words(pool) * 4);
    x.publish("pool_tiles", pool as f32);
    let history = history_slots(whole(x, "live_hit_count", 0.0));
    x.provide("substep_schedule", u64::from(history) * 16);
    x.provide("substep_faces", u64::from(history) * pool_len(pool) * 16);
    x.publish("substep_count", history as f32);
    x.hold(manifold_water_liquid::substep_history::history_bytes(
        pool, history,
    ));
    let slots = x.items("particles").unwrap_or(0);
    x.hold(crate::primitives::gpu_flip_clock::GpuFlipClock::held_bytes(
        slots as u32,
        whole(x, "clock_obstacle_count", 0.0),
        whole(x, "clock_source_count", 0.0),
    ));
    // The emission scan is sized at run time from the tick's sites, which the
    // walk does not know.
    // The sort is keyed on the pool: a range and a count per element of a
    // pooled field, then rank and slot scratch.
    let keys = pool_len(pool);
    x.hold(key_storage_bytes(keys) + keys * 4 + 2 * slots.max(1) * 4);
    // The pooled fields (the published faces and distance among them): the
    // step's cell and face arrays, the world tiles and the solver's fine
    // vectors.
    x.hold(pressure_scratch_bytes(pool) + step_scratch_bytes(pool, slots));
    field_reads(x)?;
    x.covers_if_bound("clock_status", 32)?;
    for (buffer, count) in [
        ("clock_obstacles", "clock_obstacle_count"),
        ("clock_sources", "clock_source_count"),
    ] {
        x.covers_if_bound(buffer, u64::from(whole(x, count, 0.0)) * 96)?;
    }
    x.covers_if_bound("live_hits", u64::from(whole(x, "live_hit_count", 0.0)) * 16)?;
    let rows = body_rows(x)?;
    x.covers_if_bound("bodies", rows * size_of::<LiquidBody>() as u64)?;
    // The posed rows and their mobilities, one per body of the tick.
    // `contacts` is read only within its length, so any size covers it.
    let posed = u64::from(whole(x, "body_count", 0.0)).max(1);
    x.hold(
        posed * (size_of::<LiquidBody>() as u64 + manifold_water_liquid::bodies::MOBILITY_BYTES),
    );
    // A wired reaction holds every body of one tick, as the step clamps
    // body_count; the body passes' sums come with it.
    if x.bytes("reaction").is_some() {
        let bodies = x
            .scalar("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as u64;
        x.covers("reaction", bodies * REACTION_FLOATS as u64 * 4)?;
        x.hold(body_pass_bytes(pool, bodies as u32));
    }
    // It moves min(particles, out) records: every one.
    x.covers("out", slots * PARTICLE)
}
inventory::submit! {
    ExtentRule { type_id: "node.gpu_flip_step", check: gpu_flip_step }
}
