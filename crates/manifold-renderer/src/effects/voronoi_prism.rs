use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use crate::effect::{EffectContext, PostProcessEffect};
use super::simple_blit_helper::SimpleBlitHelper;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VoronoiPrismUniforms {
    amount: f32,
    cell_count: f32,
    beat: f32,
    aspect_ratio: f32,
    source_width: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// VoronoiPrism effect — per-cell UV remapping with beat-synchronized pop-in.
/// Unity ref: VoronoiPrismFX.cs / VoronoiPrismEffect.shader
pub struct VoronoiPrismFX {
    helper: SimpleBlitHelper,
}

impl VoronoiPrismFX {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            helper: SimpleBlitHelper::new(
                device,
                include_str!("shaders/fx_voronoi_prism.wgsl"),
                "VoronoiPrism",
                std::mem::size_of::<VoronoiPrismUniforms>() as u64,
            ),
        }
    }
}

impl PostProcessEffect for VoronoiPrismFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::VORONOI_PRISM
    }

    fn apply(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        fx: &EffectInstance,
        ctx: &EffectContext,
        profiler: Option<&crate::gpu_profiler::GpuProfiler>,
    ) {
        let p = &fx.param_values;
        let uniforms = VoronoiPrismUniforms {
            amount: p.first().copied().unwrap_or(0.0),
            cell_count: p.get(1).copied().unwrap_or(16.0),
            beat: ctx.beat,
            aspect_ratio: ctx.width as f32 / ctx.height as f32,
            source_width: ctx.edge_stretch_width,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        self.helper.draw(
            device, queue, encoder,
            source, target,
            bytemuck::bytes_of(&uniforms),
            "VoronoiPrism Pass",
            ctx.width, ctx.height,
            profiler,
        );
    }
}
