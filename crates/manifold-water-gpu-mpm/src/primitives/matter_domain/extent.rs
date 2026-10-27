//! Buffer extent rule owned by this node.
use crate::matter::REACTION_WORDS;
use crate::primitives::matter_domain::matter_geometry;
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_nodes_physics::force_lattice::STAGING_SLOTS as FIELD_STAGING_SLOTS;
use manifold_physics::force_lattice::FORCE_LATTICE_RESERVE;
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::bodies::LiquidShape;
use manifold_water_liquid::bounds::{NO_KILL_HEIGHT, bounds_outputs, face_masks};
use manifold_water_liquid::extent::walked_roles;
use manifold_water_liquid::fields::FieldFrame;
use std::mem::size_of;

fn matter_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (fill, _) = walked_roles(x, "Matter")?;
    let geometry = matter_geometry(
        |name, default| x.scalar(name, default),
        x.params(),
        x.transform("domain"),
        fill,
    )
    .map_err(Verdict::Refused)?;
    for (name, value) in geometry.outputs() {
        x.publish(name, value);
    }
    // The walk has no bodies: the Bounds box as placed, the faces from the
    // toggles, no kill height.
    for (name, value) in bounds_outputs(geometry.bounds_box) {
        x.publish(name, value);
    }
    let (walls, open) = face_masks(
        geometry.setup.bounds,
        manifold_water_liquid::lattice::closed_faces(x.params()),
    );
    x.publish("closed_faces", walls as f32);
    x.publish("open_faces", open as f32);
    x.publish("kill_height", NO_KILL_HEIGHT);
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame {
        lattice: geometry.lattice.force_lattice(),
        force_lattices: FORCE_LATTICE_RESERVE,
        impulse_tick: Some(0),
    };
    let forces = u64::from(FORCE_LATTICE_RESERVE) * field.lattice.force_bytes();
    for (name, value) in field.outputs() {
        x.publish(name, value);
    }
    // Body kernels clamp rows and body counts to the bodies array, so the
    // walk takes a scene without colliders; the reaction slot is sized for
    // every body a liquid holds.
    let reaction = MAX_LIQUID_BODIES as u64 * u64::from(REACTION_WORDS) * 4;
    for (port, bytes) in [
        ("bodies", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("reaction", reaction),
        ("forces", forces),
        ("impulses", field.lattice.impulse_bytes()),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The staging ring: the impulse lattice and the force lattices per slot.
    x.hold(FIELD_STAGING_SLOTS as u64 * (field.lattice.impulse_bytes() + forces));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: MATTER_DOMAIN_TYPE_ID, check: matter_domain }
}
