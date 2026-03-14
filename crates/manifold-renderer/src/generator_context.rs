/// Maximum generator parameters per type.
/// Covers all existing generators (max ~10 params). Increase if needed.
pub const MAX_GEN_PARAMS: usize = 16;

/// Per-frame rendering context passed to generators.
/// Copy + fixed-size array = zero allocation on the hot path.
#[derive(Clone, Copy)]
pub struct GeneratorContext {
    pub time: f32,
    pub beat: f32,
    pub dt: f32,
    pub width: u32,
    pub height: u32,
    pub aspect: f32,
    pub anim_progress: f32,
    pub trigger_count: u32,
    /// Generator params copied from Layer.gen_params.param_values.
    pub params: [f32; MAX_GEN_PARAMS],
    pub param_count: u32,
}
