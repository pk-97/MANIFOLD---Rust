//! Exact observations and setup for runtime contract tests.
use crate::runtime::PresetRuntime;
use crate::exec::effect_node::{FrameTime, NodeInstanceId};
use manifold_core::NodeId;

#[cfg(feature = "gpu-proofs")]
pub(crate) fn slot_count(runtime: &PresetRuntime) -> usize { runtime.effect_nodes.len() }
#[cfg(feature = "gpu-proofs")]
pub(crate) fn pending_segments(runtime: &PresetRuntime) -> bool { runtime.pending_segments }
pub(crate) fn sampling_mask(runtime: &PresetRuntime) -> Option<&[bool]> { runtime.physics_sample_steps.as_deref() }
pub(crate) fn last_physics_frame_time(runtime: &PresetRuntime) -> Option<FrameTime> { runtime.last_physics_frame_time }
#[cfg(feature = "gpu-proofs")]
pub(crate) fn set_last_physics_frame_time(runtime: &mut PresetRuntime, value: Option<FrameTime>) { runtime.last_physics_frame_time = value; }
#[cfg(feature = "gpu-proofs")]
pub(crate) fn published_identity(runtime: &PresetRuntime, fluid: NodeInstanceId) -> Option<Result<[u8; 32], String>> {
    runtime.effect_nodes.first().expect("effect slot").physics_sources.published_identity(fluid).map(|result| result.map_err(str::to_owned))
}
pub(crate) fn group_preview(runtime: &PresetRuntime, group: &NodeId) -> Option<(NodeId, String)> {
    runtime.effect_nodes.first().expect("generator has one segment").group_preview_map.iter()
        .find(|(id, _, _)| id == group).map(|(_, producer, port)| (producer.clone(), port.clone()))
}
pub(crate) fn insert_state<T: crate::state_store::NodeState>(runtime: &mut PresetRuntime, node: NodeInstanceId, key: crate::state_store::OwnerKey, value: T) {
    runtime.state_store.insert(node, key, value);
}
pub(crate) fn has_state<T: crate::state_store::NodeState>(runtime: &mut PresetRuntime, node: NodeInstanceId, key: crate::state_store::OwnerKey) -> bool {
    runtime.state_store.get::<T>(node, key).is_some()
}
pub(crate) fn reset_impulse_routes(runtime: &mut PresetRuntime) {
    runtime.impulse_identity = std::sync::Arc::new(());
    runtime.reset_modifier_impulses();
    runtime.last_physics_frame_time = None;
}
pub(crate) fn math_view_count(runtime: &PresetRuntime) -> usize { runtime.math_views.len() }
pub(crate) fn math_variant_count(runtime: &PresetRuntime, view: usize) -> usize { runtime.math_views[view].variants.len() }
pub(crate) fn math_mode(runtime: &PresetRuntime, view: usize) -> u32 { runtime.math_views[view].mode(&runtime.graph) }
pub(crate) fn math_variant(runtime: &mut PresetRuntime, view: usize, variant: usize) -> &mut PresetRuntime {
    &mut runtime.math_views[view].variants[variant]
}
pub(crate) fn fused_retarget(runtime: &PresetRuntime, node: &NodeId, param: &str) -> Option<(NodeId, String)> {
    runtime.effect_nodes[0].bound.fused_retarget.get(&(node.to_string(), param.into())).cloned()
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn math_depth_sharing(runtime: &PresetRuntime, view: usize, variant: usize) -> Vec<(bool, (u32, u32), manifold_gpu::GpuTextureFormat)> {
    let prepared = &runtime.math_views[view];
    let parent = runtime.executor.backend();
    let child = prepared.variants[variant].executor.backend();
    prepared.shared_depth[variant].iter().map(|&(source, destination)| {
        let original = parent.texture_2d(parent.slot_for(source).unwrap()).unwrap();
        let borrowed = child.texture_2d(child.slot_for(destination).unwrap()).unwrap();
        (original.ptr_eq(borrowed), (borrowed.width, borrowed.height), borrowed.format)
    }).collect()
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn math_array_sharing(runtime: &PresetRuntime, view: usize, variant: usize) -> Vec<bool> {
    runtime.math_views[view].variants[variant].shared_arrays.iter().map(|(_, retained)| {
        runtime.plan.steps().iter().flat_map(|step| &step.outputs).any(|(_, resource)| {
            runtime.executor.backend().slot_for(*resource)
                .and_then(|slot| runtime.executor.backend().array_buffer(slot))
                .is_some_and(|parent| parent.ptr_eq(retained))
        })
    }).collect()
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn math_weight_buffers(runtime: &PresetRuntime) -> impl Iterator<Item = &manifold_gpu::GpuBuffer> {
    runtime.math_views[0].variants[0].shared_arrays.chunks_exact(2).map(|resources| &resources[1].1)
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn all_math_array_links_share_storage(runtime: &PresetRuntime) -> Vec<bool> {
    let backend = runtime.executor.backend();
    runtime.math_views.iter().flat_map(|view| view.variants.iter().zip(&view.shared_resources))
        .flat_map(|(variant, links)| links.iter().map(move |(parent, child)| {
            let parent = backend.array_buffer(backend.slot_for(*parent).unwrap()).unwrap();
            let child_backend = variant.executor.backend();
            let child = child_backend.array_buffer(child_backend.slot_for(*child).unwrap()).unwrap();
            parent.ptr_eq(child)
        })).collect()
}

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;
use crate::persistence::PrimitiveRegistry;
#[cfg(feature = "gpu-proofs")]
use crate::load::loaded_preset_view::LoadedPresetView;
use crate::load::expand::{PreparedModifierEvents, LegacyMathViewScope};
use super::JsonGeneratorLoadError;
#[cfg(feature = "gpu-proofs")]
use crate::load::expand::SceneModifierImpulseRoute;

pub(crate) fn render_view(def: EffectGraphDef, registry: &PrimitiveRegistry, params: Option<&ParamManifest>, fused: bool, view: Option<(&NodeId, Option<LegacyMathViewScope>)>) -> Result<PresetRuntime, JsonGeneratorLoadError> {
    PresetRuntime::from_def_for_render_view(def, registry, params, fused, view)
}
pub(crate) fn set_modifier_events(runtime: &mut PresetRuntime, events: Option<PreparedModifierEvents>) { runtime.modifier_events = events; }
pub(crate) fn heal_bool_convert_bindings(def: &mut EffectGraphDef, registry: &PrimitiveRegistry) -> usize { super::convert_heal::heal_bool_convert_bindings(def, registry) }
pub(crate) fn prepare_surface(def: &mut EffectGraphDef) { crate::water::runtime::gpu_flip_surface::prepare(def); }
#[cfg(feature = "gpu-proofs")]
pub(crate) fn build_segment_cards(indices: &[usize], effects: &[(usize, &manifold_core::effects::PresetInstance)], registry: &PrimitiveRegistry) -> Vec<(EffectGraphDef, &'static LoadedPresetView)> { super::segments::build_segment_cards(indices, effects, registry) }

#[cfg(feature = "gpu-proofs")]
pub(crate) struct SourceObservation {
    pub(crate) fluid: NodeId,
    pub(crate) digest: [u8; 32],
    pub(crate) control_ids: Vec<String>,
    pub(crate) string_targets: Vec<(NodeId, String)>,
    pub(crate) asset_nodes: Vec<NodeId>,
}
#[cfg(feature = "gpu-proofs")]
pub(crate) fn prepare_sources(expanded: &EffectGraphDef, canonical: &EffectGraphDef, routes: &[SceneModifierImpulseRoute], registry: &PrimitiveRegistry) -> Result<Vec<SourceObservation>, String> {
    crate::water::runtime::physics_sources::prepare(expanded, canonical, routes, registry).map(|sources| sources.into_iter().map(|source| SourceObservation {
        fluid: source.fluid, digest: source.digest, control_ids: source.control_ids,
        string_targets: source.string_targets, asset_nodes: source.asset_nodes,
    }).collect())
}
#[cfg(feature = "gpu-proofs")]
pub(crate) fn metal_backend(runtime: &mut PresetRuntime) -> &mut crate::exec::metal_backend::MetalBackend {
    runtime.executor.backend_mut().as_any_mut().and_then(|value| value.downcast_mut::<crate::exec::metal_backend::MetalBackend>()).expect("production path constructs a MetalBackend")
}

#[cfg(feature = "gpu-proofs")]
pub(crate) fn set_reference_fusion(runtime: &mut PresetRuntime, prior: &PresetRuntime, retarget: ahash::AHashMap<(String, String), (NodeId, String)>) {
    runtime.effect_nodes[0].def_content_key = prior.effect_nodes[0].def_content_key;
    runtime.effect_nodes[0].bound.fused_retarget = retarget;
}
