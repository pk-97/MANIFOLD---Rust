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
}
