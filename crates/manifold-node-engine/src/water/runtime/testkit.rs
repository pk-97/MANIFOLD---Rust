//! Exact observations of water runtime state for existing contracts.
use super::WaterRuntimeExt;
use crate::exec::effect_node::FrameTime;
use crate::runtime::PresetRuntime;
#[cfg(feature = "gpu-proofs")]
use crate::{
    exec::effect_node::NodeInstanceId, load::expand::SceneModifierImpulseRoute,
    persistence::PrimitiveRegistry,
};
#[cfg(feature = "gpu-proofs")]
use manifold_core::{NodeId, effect_graph_def::EffectGraphDef};

pub fn surface_inputs() -> &'static [&'static str] {
    super::gpu_flip_surface::SHARED_INPUTS
}

pub fn sampling_mask(runtime: &PresetRuntime) -> Option<&[bool]> {
    runtime
        .extension::<super::WaterRuntimeState>()
        .unwrap()
        .sample_steps
        .as_deref()
}
pub fn last_physics_frame_time(runtime: &PresetRuntime) -> Option<FrameTime> {
    runtime.water_ref().water.last_frame_time
}
#[cfg(feature = "gpu-proofs")]
pub fn set_last_physics_frame_time(runtime: &mut PresetRuntime, value: Option<FrameTime>) {
    runtime.water().water.last_frame_time = value;
}
#[cfg(feature = "gpu-proofs")]
pub fn published_identity(
    runtime: &PresetRuntime,
    fluid: NodeInstanceId,
) -> Option<Result<[u8; 32], String>> {
    runtime
        .water_ref()
        .water
        .sources
        .first()
        .expect("effect source state")
        .published_identity(fluid)
        .map(|result| result.map_err(str::to_owned))
}
pub fn reset_impulse_routes(runtime: &mut PresetRuntime) {
    runtime.water().water.reset();
}
#[cfg(feature = "gpu-proofs")]
pub struct SourceObservation {
    pub fluid: NodeId,
    pub digest: [u8; 32],
    pub control_ids: Vec<String>,
    pub string_targets: Vec<(NodeId, String)>,
    pub asset_nodes: Vec<NodeId>,
}
#[cfg(feature = "gpu-proofs")]
pub fn prepare_sources(
    expanded: &EffectGraphDef,
    canonical: &EffectGraphDef,
    routes: &[SceneModifierImpulseRoute],
    registry: &PrimitiveRegistry,
) -> Result<Vec<SourceObservation>, String> {
    crate::water::runtime::physics_sources::prepare(expanded, canonical, routes, registry).map(
        |sources| {
            sources
                .into_iter()
                .map(|source| SourceObservation {
                    fluid: source.fluid,
                    digest: source.digest,
                    control_ids: source.control_ids,
                    string_targets: source.string_targets,
                    asset_nodes: source.asset_nodes,
                })
                .collect()
        },
    )
}
