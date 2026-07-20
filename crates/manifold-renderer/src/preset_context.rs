/// Maximum generator parameters per type.
/// FluidSim3D has 26 params (the most of any generator).
/// Set to 32 for alignment and future headroom.
pub const MAX_GEN_PARAMS: usize = 32;

/// Per-frame rendering context shared by effects and generators.
///
/// This is the single unified context for the preset runtime — the union
/// of what effects and generators each need. `time` / `beat` are stored at
/// `f64` to match the `Beats` / `Seconds` precision used throughout the
/// engine; at WGSL-uniform boundaries, narrow
/// explicitly with `as f32` at the write site (the stored value stays f64,
/// only the uniform write narrows).
///
/// Copy + fixed-size array = zero allocation on the hot path.
///
/// Effect construction sites set the generator-only fields
/// (`aspect`, `anim_progress`, `trigger_count`) to sensible defaults;
/// generator construction sites set the effect-only fields (`owner_key`,
/// `is_clip_level`, `frame_count`) to their generator semantics (owner 0,
/// not clip-level, frame_count 0 unless available). Card param values are no
/// longer staged here — the generator's [`ParamManifest`] is passed directly
/// to [`crate::preset_runtime::PresetRuntime::render`].
#[derive(Clone, Copy)]
pub struct PresetContext {
    pub time: f64,
    pub beat: f64,
    pub dt: f32,
    /// Render-resolution dimensions (may be < output dims when scaling is active).
    pub width: u32,
    pub height: u32,
    /// Final output dimensions after upscaling. Use these for pixel-count-dependent
    /// logic (texel sizes, block counts, pattern spacing) so presets are
    /// resolution-invariant across render scales.
    pub output_width: u32,
    pub output_height: u32,
    pub aspect: f32,
    /// Owner key for per-owner state management in stateful effects.
    /// 0 = master, layer_index+1 = layer, hash(clip_id) = clip.
    pub owner_key: i64,
    pub is_clip_level: bool,
    /// Global frame counter — equivalent to Unity's Time.frameCount.
    /// Used by BlobTrackingFX to throttle GPU readbacks.
    pub frame_count: i64,
    pub anim_progress: f32,
    pub trigger_count: u32,
}
