//! Exact observations of water runtime state for existing contracts.
use super::WaterRuntimeExt;
use manifold_node_engine::runtime::PresetRuntime;

pub fn surface_inputs() -> &'static [&'static str] {
    crate::migration::gpu_flip_surface::SHARED_INPUTS
}

pub fn sampling_mask(runtime: &PresetRuntime) -> Option<&[bool]> {
    runtime
        .extension::<super::WaterRuntimeState>()
        .unwrap()
        .sample_steps
        .as_deref()
}
/// The played time of the last frame the physics history observed.
pub fn last_physics_frame_time(runtime: &PresetRuntime) -> Option<manifold_core::PlayedTime> {
    runtime
        .water_ref()
        .water
        .last_frame_time
        .map(|frame| frame.played)
}
#[cfg(feature = "gpu-proofs")]
/// Hold `value` as the last observed frame, with no tempo edit behind it.
pub fn set_last_physics_frame_time(
    runtime: &mut PresetRuntime,
    value: Option<manifold_node_engine::exec::effect_node::FrameTime>,
) {
    runtime.water().water.last_frame_time = value.map(|time| super::state::HeldFrame {
        played: manifold_core::PlayedTime::from_transport(
            time.seconds,
            manifold_core::Seconds::ZERO,
        ),
        beats: time.beats,
        frame_count: time.frame_count,
    });
}
pub fn reset_impulse_routes(runtime: &mut PresetRuntime) {
    runtime.water().water.reset();
}
