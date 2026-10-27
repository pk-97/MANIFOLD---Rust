//! Buffer extent rule owned by this node.
use crate::primitives::whitewater_step::StepShape;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::bodies::LiquidShape;
use manifold_water_liquid::extent::{field_reads, whole};
use manifold_water_liquid::primitives::face_sample_component::{pool_tiles_wire, pooled_bytes};
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::tile_storage::pool_len;
use manifold_water_liquid::whitewater::DEFAULT_CAPACITY as STEP_CAPACITY;
use manifold_water_liquid::whitewater::MAX_CAPACITY as STEP_MAX_CAPACITY;
use std::mem::size_of;

fn whitewater_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let packed = crate::primitives::whitewater_step::packed_face_source(
        x.input("faces").is_some(),
        ["face_u", "face_v", "face_w"].map(|port| x.input(port).is_some()),
    )
    .map_err(|reason| Verdict::Refused(reason.into()))?;
    let capacity = x.scalar("capacity", STEP_CAPACITY as f32).round();
    if !(1.0..=STEP_MAX_CAPACITY as f32).contains(&capacity) {
        return Err(Verdict::Refused(format!(
            "capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}"
        )));
    }
    let triple = |x: &AtomExtent<'_>, names: [&str; 3]| names.map(|name| whole(x, name, 0.0));
    let shape = StepShape::new(
        triple(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]),
        triple(x, ["face_cells_x", "face_cells_y", "face_cells_z"]),
        x.scalar("face_valid_layers", 0.0),
        x.transform("grid_bounds"),
        capacity as u32,
    )
    .map_err(Verdict::Refused)?;
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
    if x.input("pool_tiles").is_none() {
        return Err(Verdict::Refused("pool_tiles is not wired".into()));
    }
    let pool_tiles =
        pool_tiles_wire(|name, default| x.scalar(name, default)).map_err(Verdict::Refused)?;
    let (face_bytes, table_bytes) = pooled_bytes(shape.face_cells, pool_tiles);
    x.covers("tiles", table_bytes)?;
    x.covers("distance", pool_len(pool_tiles) * 4)?;
    if packed {
        x.covers("faces", face_bytes)?;
        x.hold(shape.unpacked_face_bytes());
    } else {
        for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
            x.covers(port, shape.face_bytes(axis))?;
        }
    }
    x.provide("pool_out", shape.pool_bytes());
    x.provide("state_out", 32);
    x.provide("counts_out", 36);
    if x.input("obstacle_source").is_some() {
        x.covers("obstacle_source", shape.solid_bytes() * 4)?;
    }
    if x.input("substep_schedule").is_some() {
        let slots = u64::from(whole(x, "substep_count", 0.0));
        x.covers("substep_schedule", slots * 16)?;
        for (axis, port) in ["substep_u", "substep_v", "substep_w"]
            .into_iter()
            .enumerate()
        {
            x.covers(port, slots * shape.face_bytes(axis))?;
        }
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
    x.covers("solid", shape.solid_bytes())
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_step", check: whitewater_step }
}
