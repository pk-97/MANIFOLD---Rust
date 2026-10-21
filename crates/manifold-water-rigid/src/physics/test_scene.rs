//! Test fixtures: one-group scenes and impulse targets in the shapes the
//! engine tests describe them.

use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_physics::FieldValue;

use super::{CopySpawn, RigidBody, RigidSceneInputs};

/// A world of `bodies` and one copy group whose copies body is `prototype`
/// with this spawner. `targeted` is empty, or one field per body then one
/// for the group.
#[allow(clippy::too_many_arguments)]
pub(crate) fn copies_scene(
    bodies: &[Option<RigidBody>],
    prototype: Option<RigidBody>,
    count: f32,
    spacing: f32,
    columns: f32,
    layout: f32,
    gravity: [f32; 3],
    acceleration_field: Option<FieldValue>,
    targeted: &[Option<FieldValue>],
) -> RigidSceneInputs {
    let spawn = CopySpawn { count, spacing, columns, layout };
    RigidSceneInputs {
        bodies: bodies.to_vec(),
        groups: vec![prototype.map(|body| RigidBody { spawn, ..body })],
        gravity,
        acceleration_field,
        targeted_fields: targeted.to_vec(),
    }
}

/// The bodies in `mask` (bit k is body k), and copy group 0 when `copies`.
pub(crate) fn targets(mask: u64, copies: bool) -> RigidImpulseTargets {
    let mut targets = RigidImpulseTargets::default();
    for body in (0..64).filter(|body| mask & (1 << body) != 0) {
        targets.insert_body(body);
    }
    if copies {
        targets.insert_group(0);
    }
    targets
}
