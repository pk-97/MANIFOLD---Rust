use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;

const LINE: usize = 0;
const SHAPE: usize = 1;
const SCALE: usize = 2;
const FILL: usize = 3;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BasicShapesSnapUniforms {
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    trigger_count: f32,
    shape_selection: f32,
    fill_mode: f32,
    _pad: [f32; 2],
}

pub struct BasicShapesSnapGenerator {
    pipeline: manifold_gpu::GpuComputePipeline,
}

impl BasicShapesSnapGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/basic_shapes_snap_compute.wgsl"),
            "cs_main",
            "BasicShapesSnap",
        );
        Self { pipeline }
    }
}

impl Generator for BasicShapesSnapGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::BASIC_SHAPES_SNAP
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let line = if ctx.param_count > LINE as u32 { ctx.params[LINE] } else { 0.015 };
        let shape = if ctx.param_count > SHAPE as u32 { ctx.params[SHAPE].round() } else { 0.0 };
        let scale = if ctx.param_count > SCALE as u32 { ctx.params[SCALE] } else { 1.0 };
        let fill = if ctx.param_count > FILL as u32 { ctx.params[FILL].round() } else { 1.0 };

        let uniforms = BasicShapesSnapUniforms {
            aspect_ratio: ctx.aspect,
            line_thickness: line,
            uv_scale: if scale > 0.0 { 1.0 / scale } else { 1.0 },
            trigger_count: ctx.trigger_count as f32,
            shape_selection: shape,
            fill_mode: fill,
            _pad: [0.0; 2],
        };

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BasicShapesSnap",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {}
}
