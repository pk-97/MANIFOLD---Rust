/// Maximum generator parameters per type.
/// FluidSimulation3D has 26 params (the most of any generator).
/// Set to 32 for alignment and future headroom.
pub const MAX_GEN_PARAMS: usize = 32;

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
    /// Native Metal texture for the render target (macOS only).
    /// Set by GeneratorRenderer when native encoding is active.
    /// Raw pointer avoids lifetime issues; valid for the frame duration.
    #[cfg(target_os = "macos")]
    pub native_target: Option<*const manifold_gpu::GpuTexture>,
}
