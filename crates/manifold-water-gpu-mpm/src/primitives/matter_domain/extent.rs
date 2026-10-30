//! Buffer extent rule owned by this node.
use crate::matter::{MAX_MATTER_SLOTS, REACTION_WORDS};
use crate::primitives::matter_domain::{FILL_ROW_WORDS, matter_geometry, roles_per_slot};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use manifold_nodes_physics::force_lattice::STAGING_SLOTS as FIELD_STAGING_SLOTS;
use manifold_physics::force_lattice::FORCE_LATTICE_RESERVE;
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::bodies::LiquidShape;
use manifold_water_liquid::bounds::{NO_KILL_HEIGHT, bounds_outputs, closed_faces, face_masks};
use manifold_node_engine::scene::transform::Transform;
use manifold_water_liquid::fluid_role::{FluidRoleKind, initial_fill};
use manifold_water_liquid::fields::FieldFrame;
use std::mem::size_of;

/// Each slot's Initial Fill as the walk resolves its role wires, by the
/// node's rule (`take_slot_fills`).
fn walked_fills(x: &AtomExtent<'_>) -> Result<[Option<Transform>; MAX_MATTER_SLOTS], Verdict> {
    let roles = manifold_water_liquid::role_count(x.params());
    let per_slot = roles_per_slot(x.params(), roles);
    let mut fills = [None; MAX_MATTER_SLOTS];
    for (slot, start) in (0..roles).step_by(per_slot).enumerate() {
        let run = (start..(start + per_slot).min(roles))
            .filter_map(|index| x.object(&format!("role_{index}")))
            .filter(|(kind, _)| *kind == FluidRoleKind::InitialFill.index())
            .map(|(_, placed)| placed);
        let Some(fill) =
            initial_fill(run).map_err(|reason| Verdict::Refused(format!("Matter: {reason}")))?
        else {
            continue;
        };
        *fills.get_mut(slot).ok_or_else(|| {
            Verdict::Refused(format!(
                "Matter: one Matter solver holds {MAX_MATTER_SLOTS} Sand items"
            ))
        })? = Some(fill);
    }
    Ok(fills)
}

fn matter_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let fills = walked_fills(x)?;
    let geometry = matter_geometry(
        |name, default| x.scalar(name, default),
        x.params(),
        x.transform("domain"),
        &fills,
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
    let (walls, open) = face_masks(geometry.setup.bounds, closed_faces(x.params()));
    x.publish("closed_faces", walls as f32);
    x.publish("open_faces", open as f32);
    x.publish("kill_height", NO_KILL_HEIGHT);
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame {
        lattice: geometry.force_lattice(),
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
        ("fills", (FILL_ROW_WORDS * MAX_MATTER_SLOTS * 4) as u64),
        ("bodies", size_of::<LiquidBody>() as u64),
        ("regions", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("collider_nodes", 16),
        ("collider_triangles", 16),
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
