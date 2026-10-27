//! Buffer extent rule owned by this node.
use crate::primitives::gpu_flip_domain::region_sites::RegionSites;
use crate::primitives::gpu_flip_domain::{geometry_read, gpu_flip_geometry};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_nodes_physics::force_lattice::STAGING_SLOTS as FIELD_STAGING_SLOTS;
use manifold_physics::clock::TICK;
use manifold_physics::force_lattice::FORCE_LATTICE_RESERVE;
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::bodies::LiquidShape;
use manifold_water_liquid::bounds::{NO_KILL_HEIGHT, bounds_outputs, face_masks};
use manifold_water_liquid::coupling::REACTION_FLOATS;
use manifold_water_liquid::extent::walked_roles;
use manifold_water_liquid::fields::FieldFrame;
use std::mem::size_of;

fn gpu_flip_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (fill, kinds) = walked_roles(x, "GPU FLIP")?;
    let geometry = gpu_flip_geometry(
        geometry_read(x.params(), |name, default| x.scalar(name, default)),
        x.transform("domain"),
        fill,
    )
    .map_err(Verdict::Refused)?
    // The pool the allocator gets: room for an inflow when a role is one.
    .sized_for(kinds);
    for (name, value) in geometry.outputs() {
        x.publish(name, value);
    }
    // The walk has no bodies, so with Bounds off it takes the box of the most
    // nodes; the faces follow the toggles and no height is known.
    let scene = geometry.walk_scene();
    for (name, value) in bounds_outputs(scene) {
        x.publish(name, value);
    }
    let (walls, open) = face_masks(
        geometry.setup.bounds,
        manifold_water_liquid::bounds::closed_faces(x.params()),
    );
    x.publish("closed_faces", walls as f32);
    x.publish("open_faces", open as f32);
    x.publish("kill_height", NO_KILL_HEIGHT);
    // Body kernels clamp rows and body counts to the bodies array, so the
    // walk takes a scene without colliders; the reaction is sized for every
    // body a liquid holds.
    x.publish("body_count", 0.0);
    x.publish("body_rows", 0.0);
    x.publish("dynamic_bodies", 0.0);
    x.publish("region_count", 0.0);
    x.publish("region_site_total", 0.0);
    x.publish("clock_obstacle_count", 0.0);
    x.publish("clock_source_count", 0.0);
    x.publish("live_hit_count", 0.0);
    x.publish("interval_duration", TICK as f32);
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame {
        lattice: geometry.field_lattice(scene),
        force_lattices: FORCE_LATTICE_RESERVE,
        impulse_tick: Some(0),
    };
    let forces = u64::from(FORCE_LATTICE_RESERVE) * field.lattice.force_bytes();
    for (name, value) in field.outputs() {
        x.publish(name, value);
    }
    for (port, bytes) in [
        ("bodies", size_of::<LiquidBody>() as u64),
        (
            "contacts",
            size_of::<manifold_water_liquid::bodies::BodySupports>() as u64,
        ),
        ("regions", size_of::<LiquidBody>() as u64),
        ("region_sites", size_of::<RegionSites>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("clock_obstacles", 96),
        ("clock_sources", 96),
        ("live_hits", 16),
        ("reaction", (MAX_LIQUID_BODIES * REACTION_FLOATS * 4) as u64),
        ("forces", forces),
        ("impulses", field.lattice.impulse_bytes()),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The staging ring: the impulse lattice and the force lattices per slot.
    x.hold(FIELD_STAGING_SLOTS as u64 * (field.lattice.impulse_bytes() + forces + 16));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: GPU_FLIP_DOMAIN_TYPE_ID, check: gpu_flip_domain }
}
