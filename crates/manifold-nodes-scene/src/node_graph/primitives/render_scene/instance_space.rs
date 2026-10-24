//! The renderer's instance space: every draw composes world = A · T_instance · B.

use manifold_node_engine::scene::scene_object::InstanceSpace;

use super::mat4_mul;

pub(super) const IDENTITY_M4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// The two matrices (A, B) a draw composes as world = A · T_instance · B.
///
/// Object space: A = parent · local, B = identity, so every existing draw
/// computes what it did before the instance space existed. Group space:
/// A = parent, B = local, so each instance places the whole group and the
/// part sits inside every copy. A draw without instances folds to
/// (parent · local, identity) in either space: T is the identity there, so
/// the product is the same and every uninstanced path stays untouched.
pub(crate) fn instance_space_pair(
    parent: Option<[[f32; 4]; 4]>,
    local: [[f32; 4]; 4],
    space: InstanceSpace,
    instanced: bool,
) -> ([[f32; 4]; 4], [[f32; 4]; 4]) {
    match (space, instanced) {
        (InstanceSpace::Group, true) => (parent.unwrap_or(IDENTITY_M4), local),
        (InstanceSpace::World, true) => (
            IDENTITY_M4,
            parent.map_or(local, |parent| mat4_mul(parent, local)),
        ),
        _ => (
            parent.map_or(local, |parent| mat4_mul(parent, local)),
            IDENTITY_M4,
        ),
    }
}
