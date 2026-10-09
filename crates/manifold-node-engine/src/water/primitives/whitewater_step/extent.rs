//! Buffer extent rule owned by this node.
use std::mem::size_of;
use crate::water::liquid::bodies::LiquidBody;
use crate::water::liquid::bodies::LiquidShape;
use crate::water::primitives::gpu_flip_step::face_bytes;
use crate::water::primitives::whitewater_step::DEFAULT_CAPACITY as STEP_CAPACITY;
use crate::water::primitives::whitewater_step::MAX_CAPACITY as STEP_MAX_CAPACITY;
use crate::water::primitives::whitewater_step::StepShape;
use crate::water::whitewater::cell_total;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::water::liquid::extent::{field_reads, whole};

fn whitewater_step(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let packed = crate::water::primitives::whitewater_step::packed_face_source(
        x.input("distance").is_some(), x.input("faces").is_some(),
        ["face_u", "face_v", "face_w"].map(|port| x.input(port).is_some()),
    ).map_err(|reason| Verdict::Refused(reason.into()))?;
    let capacity = x.scalar("capacity", STEP_CAPACITY as f32).round();
    if !(1.0..=STEP_MAX_CAPACITY as f32).contains(&capacity) {
        return Err(Verdict::Refused(format!("capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}")));
    }
    let triple = |x: &AtomExtent<'_>, names: [&str; 3]| names.map(|name| whole(x, name, 0.0));
    let shape = StepShape::new(
        triple(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]),
        if x.input("distance").is_some() {
            triple(x, ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"])
        } else {
            triple(x, ["level_set_nodes_x", "level_set_nodes_y", "level_set_nodes_z"])
        },
        triple(x, ["face_cells_x", "face_cells_y", "face_cells_z"]),
        x.scalar("face_valid_layers", 0.0),
        x.transform("grid_bounds"),
        capacity as u32,
    )
    .map_err(Verdict::Refused)?;
    for port in ["foam_particles", "bubble_particles", "spray_particles", "dust_particles"] {
        x.provide(port, shape.population_bytes());
    }
    x.hold(shape.held_bytes(x.items("particles").unwrap_or(0), x.input("distance").is_some()));
    if packed {
        x.covers("faces", face_bytes(shape.face_cells))?;
        x.hold(shape.unpacked_face_bytes());
    } else {
        for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
            x.covers(port, shape.face_bytes(axis))?;
        }
    }
    x.provide("pool_out", shape.pool_bytes());
    x.provide("state_out", 32);
    x.provide("counts_out", 36);
    if x.input("obstacle_source").is_some() { x.covers("obstacle_source", shape.solid_bytes() * 4)?; }
    if x.input("substep_schedule").is_some() {
        let slots=u64::from(whole(x,"substep_count",0.0));
        x.covers("substep_schedule",slots*16)?;
        for (axis,port) in ["substep_u","substep_v","substep_w"].into_iter().enumerate() { x.covers(port,slots*shape.face_bytes(axis))?; }
        field_reads(x)?;
        let regions=u64::from(whole(x,"region_count",0.0));
        if regions>0 {
            let row=(whole(x,"tick_index",0.0).saturating_sub(whole(x,"first_tick",0.0))) as u64;
            x.covers("regions",(row+1)*regions*size_of::<LiquidBody>() as u64)?;
            x.covers("shapes",size_of::<LiquidShape>() as u64)?;
            x.covers("atlas",4)?;
        }
    }
    if x.input("distance").is_some() {
        x.covers("distance", cell_total(shape.face_cells) * 4)?;
        x.covers("pool", shape.pool_bytes())?;
        x.covers("pool_state", 32)?;
    } else {
        x.covers("level_set", shape.level_bytes())?;
    }
    x.covers("solid", shape.solid_bytes())
}
inventory::submit! {
    ExtentRule { type_id: "node.whitewater_step", check: whitewater_step }
}
