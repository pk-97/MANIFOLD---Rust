//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::water::fluid_particles::bin_total;
use crate::water::fluid_role::MAX_FLUID_ROLES;
use crate::water::liquid::bodies::LiquidBody;
use crate::water::liquid::grid::face_len;
use crate::water::liquid::lattice::FlipSolverGrid;
use crate::water::liquid::coupling::REACTION_FLOATS;
use crate::water::primitives::gpu_flip_bodies::held_bytes as body_pass_bytes;
use crate::water::primitives::prefix_scan::storage_words;
use crate::water::primitives::sort_particles_into_cells::range_storage_bytes;
use crate::water::primitives::gpu_flip_pressure::lattice_refusal;
use crate::water::primitives::gpu_flip_pressure::scratch_bytes as pressure_scratch_bytes;
use crate::water::primitives::gpu_flip_step::ENGINE_CFL;
use crate::water::primitives::gpu_flip_step::FACE_VALID_LAYERS;
use crate::water::primitives::gpu_flip_step::band_layers;
use crate::water::primitives::gpu_flip_step::face_bytes;
use crate::water::primitives::gpu_flip_step::ring_max;
use crate::water::primitives::gpu_flip_step::scratch_bytes as step_scratch_bytes;
use crate::water::whitewater::cell_total;
use crate::water::liquid::extent::{AtomExtent, ExtentRule, PARTICLE, Verdict, body_rows, field_reads, lattice_total, search_fits, whole};

fn gpu_flip_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("identity", 16)?;
    x.covers_if_bound("identity_out", 16)?;
    let cells = FlipSolverGrid::from_lattice(x.lattice()?).cells();
    if let Some(reason) = lattice_refusal(cells) {
        return Err(Verdict::Refused(format!("GPU FLIP Step: {reason}. Lower Resolution.")));
    }
    let faces = face_bytes(cells);
    x.provide("faces", faces);
    x.provide("distance", cell_total(cells) * 4);
    let history_slots = manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS + whole(x,"live_hit_count",0.0);
    x.provide("substep_schedule",u64::from(history_slots)*16);
    for (axis,port) in ["substep_u","substep_v","substep_w"].into_iter().enumerate() {
        x.provide(port,u64::from(history_slots)*face_len(cells,axis)*4);
    }
    x.publish("substep_count",history_slots as f32);
    x.hold(crate::water::liquid::substep_history::history_bytes(cells,history_slots));
    let lattice = FlipSolverGrid::from_lattice(x.lattice()?);
    x.publish_transform("grid_bounds", lattice.bounds());
    for (port, value) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"].into_iter().zip(lattice.nodes())
        .chain(["face_cells_x", "face_cells_y", "face_cells_z"].into_iter().zip(cells))
        .chain([("face_valid_layers", FACE_VALID_LAYERS)]) {
        x.publish(port, value as f32);
    }
    let slots = x.items("particles").unwrap_or(0);
    x.hold(crate::water::primitives::gpu_flip_clock::GpuFlipClock::held_bytes(
        slots as u32,
        whole(x, "clock_obstacle_count", 0.0),
        whole(x, "clock_source_count", 0.0),
    ));
    let cell_bytes = lattice_total(cells) * 4;
    if x.feeds("interior") { x.provide("interior", cell_bytes); x.hold(cell_bytes); }
    if x.scalar("narrow_band", 0.0) != 0.0 {
        // Four distance arrays, support mask, two face grids, lifecycle
        // particles/status and PrefixScan storage. Ferstl et al. (2016).
        x.hold(6 * cell_bytes + 3 * faces + slots * PARTICLE + 16);
        x.hold(storage_words((lattice_total(cells) * 8) as usize) as u64 * 4);
    }
    if x.wired("regions") {
        x.hold(storage_words(crate::water::primitives::gpu_flip_step::emit_sites(cells) as usize) as u64 * 4);
    }
    let ranges = range_storage_bytes(cells);
    search_fits(x, cells, ranges)?;
    // The sort's ranges, cell counts, rank and slot scratch.
    x.hold(ranges + bin_total(cells) * 4 + 2 * slots.max(1) * 4);
    // Same configured CFL as the step, never particle travel.
    let ring = ring_max(band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS));
    x.hold(faces + pressure_scratch_bytes(cells) + step_scratch_bytes(cells, slots, ring));
    field_reads(x)?;
    x.covers_if_bound("clock_status", 32)?;
    for (buffer, count) in [("clock_obstacles", "clock_obstacle_count"), ("clock_sources", "clock_source_count")] {
        x.covers_if_bound(buffer, u64::from(whole(x, count, 0.0)) * 96)?;
    }
    x.covers_if_bound("live_hits", u64::from(whole(x, "live_hit_count", 0.0)) * 16)?;
    let rows = body_rows(x)?;
    x.covers_if_bound("bodies", rows * size_of::<LiquidBody>() as u64)?;
    // The posed rows and their mobilities, one per body of the tick.
    // `contacts` is read only within its length, so any size covers it.
    let posed = u64::from(whole(x, "body_count", 0.0)).max(1);
    x.hold(posed * (size_of::<LiquidBody>() as u64 + crate::water::liquid::bodies::MOBILITY_BYTES));
    // A wired reaction holds every body of one tick, as the step clamps
    // body_count; the body passes' sums come with it.
    if x.bytes("reaction").is_some() {
        let bodies = x.scalar("body_count", 0.0).round().clamp(0.0, MAX_FLUID_ROLES as f32) as u64;
        x.covers("reaction", bodies * REACTION_FLOATS as u64 * 4)?;
        x.hold(body_pass_bytes(cells, bodies as u32));
    }
    // It moves min(particles, out) records: every one.
    x.covers("out", slots * PARTICLE)
}
inventory::submit! {
    ExtentRule { type_id: "node.gpu_flip_step", check: gpu_flip_step }
}
