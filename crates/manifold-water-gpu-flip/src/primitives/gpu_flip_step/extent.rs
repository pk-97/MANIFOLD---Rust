//! Buffer extent rule owned by this node.
use crate::primitives::gpu_flip_bodies::held_bytes as body_pass_bytes;
use crate::primitives::gpu_flip_pressure::lattice_refusal;
use crate::primitives::gpu_flip_pressure::scratch_bytes as pressure_scratch_bytes;
use crate::primitives::gpu_flip_step::ENGINE_CFL;
use crate::primitives::gpu_flip_step::FACE_VALID_LAYERS;
use crate::primitives::gpu_flip_step::band_layers;
use crate::primitives::gpu_flip_step::history_slots;
use crate::primitives::gpu_flip_step::pool_tiles;
use crate::primitives::gpu_flip_step::ring_max;
use crate::primitives::gpu_flip_step::scratch_bytes as step_scratch_bytes;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::coupling::REACTION_FLOATS;
use manifold_water_liquid::extent::liquid_lattice;
use manifold_water_liquid::extent::{PARTICLE, body_rows, field_reads, whole};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::grid::face_len;
use manifold_water_liquid::lattice::FlipSolverGrid;
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::primitives::prefix_scan::storage_words;
use manifold_water_liquid::primitives::sort_particles_into_cells::key_storage_bytes;
use manifold_water_liquid::tile_storage::{pool_len, tile_total};
use std::mem::size_of;

fn gpu_flip_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("identity", IDENTITY_BYTES)?;
    x.covers_if_bound("identity_out", IDENTITY_BYTES)?;
    let cells = FlipSolverGrid::from_lattice(liquid_lattice(x)?).cells();
    if let Some(reason) = lattice_refusal(cells) {
        return Err(Verdict::Refused(format!(
            "GPU FLIP Step: {reason}. Lower Resolution."
        )));
    }
    let pool = pool_tiles(cells);
    x.provide("faces", pool_len(pool) * size_of::<FaceSample>() as u64);
    x.provide("distance", pool_len(pool) * 4);
    x.provide("tiles", (tile_total(cells) + u64::from(pool)) * 4);
    x.publish("pool_tiles", pool as f32);
    let history = history_slots(whole(x, "live_hit_count", 0.0));
    x.provide("substep_schedule", u64::from(history) * 16);
    for (axis, port) in ["substep_u", "substep_v", "substep_w"]
        .into_iter()
        .enumerate()
    {
        x.provide(port, u64::from(history) * face_len(cells, axis) * 4);
    }
    x.publish("substep_count", history as f32);
    x.hold(manifold_water_liquid::substep_history::history_bytes(
        cells, history,
    ));
    let lattice = FlipSolverGrid::from_lattice(liquid_lattice(x)?);
    x.publish_transform("grid_bounds", lattice.bounds());
    for (port, value) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]
        .into_iter()
        .zip(lattice.nodes())
        .chain(
            ["face_cells_x", "face_cells_y", "face_cells_z"]
                .into_iter()
                .zip(cells),
        )
        .chain([("face_valid_layers", FACE_VALID_LAYERS)])
    {
        x.publish(port, value as f32);
    }
    let slots = x.items("particles").unwrap_or(0);
    x.hold(crate::primitives::gpu_flip_clock::GpuFlipClock::held_bytes(
        slots as u32,
        whole(x, "clock_obstacle_count", 0.0),
        whole(x, "clock_source_count", 0.0),
    ));
    if x.wired("regions") {
        x.hold(
            storage_words(crate::primitives::gpu_flip_step::emit_sites(cells) as usize) as u64 * 4,
        );
    }
    // The sort is keyed on the pool: a range and a count per element of a
    // pooled field, then rank and slot scratch.
    let keys = pool_len(pool);
    x.hold(key_storage_bytes(keys) + keys * 4 + 2 * slots.max(1) * 4);
    // Same configured CFL as the step, never particle travel.
    let ring = ring_max(band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS));
    // The pooled fields (the published faces and distance among them): the
    // step's cell and face arrays and the solver's fine vectors.
    x.hold(pressure_scratch_bytes(cells, pool) + step_scratch_bytes(cells, slots, ring));
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
        x.hold(body_pass_bytes(cells, bodies as u32));
    }
    // It moves min(particles, out) records: every one.
    x.covers("out", slots * PARTICLE)
}
inventory::submit! {
    ExtentRule { type_id: "node.gpu_flip_step", check: gpu_flip_step }
}
