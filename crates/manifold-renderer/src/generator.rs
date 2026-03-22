use manifold_core::GeneratorType;
use crate::generator_context::GeneratorContext;

/// GPU-aware generator processor. Each instance owns its wgpu pipeline(s)
/// and any per-generator GPU state (compute buffers, temporal state, etc.).
///
/// Lifecycle:
/// - `new()` creates the instance and compiles all pipelines
/// - `render()` is called once per frame per active clip of this type
/// - `resize()` recreates any resolution-dependent resources
/// - Drop cleans up GPU resources automatically
pub trait Generator: Send {
    /// Which generator type this handles.
    fn generator_type(&self) -> GeneratorType;

    /// Render one frame into the target texture view.
    /// Returns updated anim_progress for this clip.
    fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ctx: &GeneratorContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) -> f32;

    /// Recreate resolution-dependent resources.
    fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32);
}
