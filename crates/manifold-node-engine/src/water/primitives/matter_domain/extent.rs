//! Buffer extent rule owned by this node.
use std::mem::size_of;
use manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID;
use crate::scene::fluid_domain::MAX_FLUID_ROLES;
use crate::water::liquid::bodies::LiquidBody;
use crate::water::liquid::bodies::LiquidShape;
use crate::water::liquid::clock::FIELD_RESERVE_INTERVALS;
use crate::water::liquid::fields::FieldFrame;
use crate::water::liquid::fields::FieldLattice;
use crate::water::liquid::fields::STAGING_SLOTS as FIELD_STAGING_SLOTS;
use crate::water::matter::REACTION_WORDS;
use crate::water::primitives::matter_domain::matter_geometry;
use crate::exec::extent::{AtomExtent, ExtentRule, Verdict};

fn matter_domain(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let geometry = matter_geometry(
        |name, default| x.scalar(name, default),
        x.params(),
        x.transform("domain"),
        x.transform("initial_volume"),
    )
    .map_err(Verdict::Refused)?;
    for (name, value) in geometry.outputs() {
        x.publish(name, value);
    }
    // The walk takes a live frame's most force lattices and an impulse tick,
    // so the field reads are checked.
    let field = FieldFrame {
        lattice: FieldLattice::of(&geometry.setup.lattice),
        force_lattices: FIELD_RESERVE_INTERVALS,
        impulse_tick: Some(0),
    };
    let forces = u64::from(FIELD_RESERVE_INTERVALS) * field.lattice.bytes();
    for (name, value) in field.outputs() {
        x.publish(name, value);
    }
    // Body kernels clamp rows and body counts to the bodies array, so the
    // walk takes a scene without colliders; the reaction slot is sized for
    // every body a liquid holds.
    let reaction = MAX_FLUID_ROLES as u64 * u64::from(REACTION_WORDS) * 4;
    for (port, bytes) in [
        ("bodies", size_of::<LiquidBody>() as u64),
        ("shapes", size_of::<LiquidShape>() as u64),
        ("atlas", 4),
        ("reaction", reaction),
        ("forces", forces),
        ("impulses", field.lattice.bytes()),
    ] {
        x.provide(port, bytes);
        x.hold(bytes);
    }
    // The staging ring: the impulse lattice and the force lattices per slot.
    x.hold(FIELD_STAGING_SLOTS as u64 * (field.lattice.bytes() + forces));
    Ok(())
}
inventory::submit! {
    ExtentRule { type_id: MATTER_DOMAIN_TYPE_ID, check: matter_domain }
}
