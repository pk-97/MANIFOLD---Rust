//! Intermediate authoring facts used by expansion contract tests.
use super::*;
use crate::{graph::Graph, persistence::PrimitiveRegistry};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::SceneTargetSelection;
use manifold_core::scene_impulse::ImpulseTarget;

#[cfg(feature = "gpu-proofs")]
pub fn authoring_objects(owner: &EffectGraphDef, scene: &NodeId, registry: &PrimitiveRegistry) -> Result<Vec<NodeId>, SceneModifierExpandError> {
    super::acceleration::authoring_objects(owner, scene, registry)
}
pub fn recipient_key(index: &FlatSceneIndex, object: &NodeId, registry: &PrimitiveRegistry) -> Result<Option<(NodeId, String)>, SceneModifierExpandError> {
    super::acceleration::recipient_key(index, object, registry)
}
pub fn impulse_recipients_with_index(index: &FlatSceneIndex, scene: &NodeId, selection: &SceneTargetSelection, registry: &PrimitiveRegistry) -> Result<Vec<(NodeId, ImpulseTarget)>, SceneModifierExpandError> {
    super::acceleration::impulse_recipients_with_index(index, scene, selection, registry)
}
pub fn prepare_impulses(owner: &EffectGraphDef, routes: &[SceneModifierNodeRoute], graph: &mut Graph) -> Result<Vec<SceneModifierImpulseRoute>, SceneModifierExpandError> {
    super::impulses::prepare(owner, routes, graph)
}
pub fn resource_node_id(modifier: &NodeId, target: &NodeId, role: &str) -> NodeId {
    super::compiler::math_resource_node_id(modifier, target, role)
}
pub struct GuardFixture(super::PreparedModifierParameterGuards);
impl GuardFixture {
    pub fn prepare(owner: &EffectGraphDef) -> Result<Self, SceneModifierExpandError> { super::PreparedModifierParameterGuards::prepare(owner).map(Self) }
    pub fn scenes(&self) -> &[NodeId] { self.0.test_scenes() }
    pub fn sources_empty(&self) -> bool { self.0.test_sources_empty() }
    pub fn install(self, graph: &mut Graph) -> Result<(), SceneModifierExpandError> { self.0.install(graph) }
}
