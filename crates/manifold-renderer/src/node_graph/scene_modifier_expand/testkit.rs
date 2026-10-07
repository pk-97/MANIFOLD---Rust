//! Intermediate authoring facts used by expansion contract tests.
use super::*;
use crate::node_graph::{Graph, PrimitiveRegistry};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use crate::node_graph::physics_events::ImpulseTarget;

#[cfg(feature = "gpu-proofs")]
pub(crate) fn authoring_objects(owner: &EffectGraphDef, scene: &SceneNodeRef, registry: &PrimitiveRegistry) -> Result<Vec<SceneNodeRef>, SceneModifierExpandError> {
    super::acceleration::authoring_objects(owner, scene, registry)
}
pub(crate) fn recipient_key(index: &FlatSceneIndex, object: &SceneNodeRef, registry: &PrimitiveRegistry) -> Result<Option<(SceneNodeRef, String)>, SceneModifierExpandError> {
    super::acceleration::recipient_key(index, object, registry)
}
pub(crate) fn impulse_recipients_with_index(index: &FlatSceneIndex, scene: &SceneNodeRef, selection: &SceneTargetSelection, registry: &PrimitiveRegistry) -> Result<Vec<(NodeId, ImpulseTarget)>, SceneModifierExpandError> {
    super::acceleration::impulse_recipients_with_index(index, scene, selection, registry)
}
pub(crate) fn prepare_impulses(owner: &EffectGraphDef, routes: &[SceneModifierNodeRoute], graph: &mut Graph) -> Result<Vec<SceneModifierImpulseRoute>, SceneModifierExpandError> {
    super::impulses::prepare(owner, routes, graph)
}
pub(crate) fn resource_node_id(modifier: &NodeId, target: &SceneNodeRef, role: &str) -> NodeId {
    super::math_resource_node_id(modifier, target, role)
}
pub(crate) struct GuardFixture(super::PreparedModifierParameterGuards);
impl GuardFixture {
    pub(crate) fn prepare(owner: &EffectGraphDef) -> Result<Self, SceneModifierExpandError> { super::PreparedModifierParameterGuards::prepare(owner).map(Self) }
    pub(crate) fn scenes(&self) -> &[NodeId] { self.0.test_scenes() }
    pub(crate) fn sources_empty(&self) -> bool { self.0.test_sources_empty() }
    pub(crate) fn install(self, graph: &mut Graph) -> Result<(), SceneModifierExpandError> { self.0.install(graph) }
}
