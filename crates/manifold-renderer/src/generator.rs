use manifold_core::GeneratorTypeId;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;

/// GPU-aware generator processor. Each instance owns its manifold-gpu pipeline(s)
/// and any per-generator GPU state (compute buffers, temporal state, etc.).
///
/// Lifecycle:
/// - `new()` creates the instance and compiles all pipelines
/// - `render()` is called once per frame per active clip of this type
/// - `resize()` recreates any resolution-dependent resources
/// - Drop cleans up GPU resources automatically
pub trait Generator: Send {
    /// Which generator type this handles.
    fn generator_type(&self) -> &GeneratorTypeId;

    /// Render one frame into the target texture.
    /// Returns updated anim_progress for this clip.
    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32;

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32);

    /// Internal resolution scale factor for this generator type.
    /// Generators render at (output_width * scale, output_height * scale) and are
    /// upscaled to full output resolution afterward. Organic/particle generators
    /// use 0.5 (matching Unity), geometric generators use 1.0 (no scaling).
    /// Clamped to [0.125, 1.0]. Default: 1.0 (full resolution).
    fn internal_resolution_scale(&self) -> f32 {
        1.0
    }
}
