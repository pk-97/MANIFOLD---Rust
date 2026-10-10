//! Exact observations of water runtime state for existing contracts.
use super::WaterRuntimeExt;
use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::runtime::PresetRuntime;

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
pub fn reset_impulse_routes(runtime: &mut PresetRuntime) {
    runtime.water().water.reset();
}
