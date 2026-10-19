//! Exact observations and setup for runtime contract tests.
use crate::runtime::PresetRuntime;
use crate::exec::effect_node::NodeInstanceId;
use manifold_core::NodeId;

#[cfg(feature = "gpu-proofs")]
pub fn slot_count(runtime: &PresetRuntime) -> usize { runtime.effect_nodes.len() }
#[cfg(feature = "gpu-proofs")]
pub fn pending_segments(runtime: &PresetRuntime) -> bool { runtime.pending_segments }
pub fn group_preview(runtime: &PresetRuntime, group: &NodeId) -> Option<(NodeId, String)> {
    runtime.effect_nodes.first().expect("generator has one segment").group_preview_map.iter()
        .find(|(id, _, _)| id == group).map(|(_, producer, port)| (producer.clone(), port.clone()))
}
pub fn insert_state<T: crate::state_store::NodeState>(runtime: &mut PresetRuntime, node: NodeInstanceId, key: crate::state_store::OwnerKey, value: T) {
    runtime.state_store.insert(node, key, value);
}
pub fn has_state<T: crate::state_store::NodeState>(runtime: &mut PresetRuntime, node: NodeInstanceId, key: crate::state_store::OwnerKey) -> bool {
    runtime.state_store.get::<T>(node, key).is_some()
}
pub fn fused_retarget(runtime: &PresetRuntime, node: &NodeId, param: &str) -> Option<(NodeId, String)> {
    runtime.effect_nodes[0].bound.fused_retarget.get(&(node.to_string(), param.into())).cloned()
}

use manifold_core::effect_graph_def::EffectGraphDef;
use crate::persistence::PrimitiveRegistry;
#[cfg(feature = "gpu-proofs")]
use crate::load::loaded_preset_view::LoadedPresetView;
use crate::load::expand::PreparedModifierEvents;

pub fn set_modifier_events(runtime: &mut PresetRuntime, events: Option<PreparedModifierEvents>) { runtime.modifier_events = events; }
pub fn heal_bool_convert_bindings(def: &mut EffectGraphDef, registry: &PrimitiveRegistry) -> usize { super::convert_heal::heal_bool_convert_bindings(def, registry) }
pub fn prepare_surface(def: &mut EffectGraphDef) {
    crate::load::migration::prepare_stage(
        def,
        crate::load::migration::MigrationStage::BeforeSceneModifiers,
    );
}
#[cfg(feature = "gpu-proofs")]
pub fn build_segment_cards(indices: &[usize], effects: &[(usize, &manifold_core::effects::PresetInstance)]) -> Vec<(EffectGraphDef, std::sync::Arc<LoadedPresetView>)> { super::segments::build_segment_cards(indices, effects) }

#[cfg(feature = "gpu-proofs")]
pub fn metal_backend(runtime: &mut PresetRuntime) -> &mut crate::exec::metal_backend::MetalBackend {
    runtime.executor.backend_mut().as_any_mut().and_then(|value| value.downcast_mut::<crate::exec::metal_backend::MetalBackend>()).expect("production path constructs a MetalBackend")
}
