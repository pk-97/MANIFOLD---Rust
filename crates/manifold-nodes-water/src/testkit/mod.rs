//! Water-specific graph fixtures and proof helpers.

pub mod conformance;

pub mod preset_extents;

#[cfg(any(test, feature = "testkit"))]
pub mod liquid_extents;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod whitewater_scene;

/// Couple liquid domain `fluid` with Physics World `rigid` as a scene's
/// install hook does: the liquid pushes the `colliders` bodies and flows
/// round them.
pub fn couple(
    graph: &mut manifold_node_engine::graph::Graph,
    fluid: manifold_node_engine::exec::effect_node::NodeInstanceId,
    rigid: manifold_node_engine::exec::effect_node::NodeInstanceId,
    colliders: manifold_core::scene_impulse::RigidImpulseTargets,
) -> Result<(), manifold_node_engine::validation::GraphError> {
    crate::physics_scene::add_coupled_scene(graph, fluid, rigid, colliders)
}
