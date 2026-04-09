use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;
use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::PARAMETRIC_SURFACE,
        display_name: "Parametric Surface",
        is_line_based: false,
        available: true,
        osc_prefix: "parametricSurface",
        legacy_discriminant: Some(13),
        params: &[
            ParamSpec::whole_labels("Shape", 0.0, 4.0, 0.0, &["Gyroid","Schwarz P","Schwarz D","Torus Knot","Klein"], "shape"),
            ParamSpec::continuous("Morph", 0.0, 1.0, 0.0, "F2", "morph"),
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 1.0, "snap"),
        ],
        string_params: &[],
    }
}

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::PARAMETRIC_SURFACE,
        create: |device| Box::new(ParametricSurfaceGenerator::new(device)),
    }
}

// Parameter indices matching Unity ComputeParametricSurfaceGenerator
const SHAPE: usize = 0;
const MORPH: usize = 1;
const SPEED: usize = 2;
const SCALE: usize = 3;
const SNAP: usize = 4;
const SURFACE_COUNT: u32 = 5;

const VOL_SIZE: u32 = 128;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BakeUniforms {
    shape: f32,
    morph: f32,
    vol_res: f32,
    _pad0: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RaymarchUniforms {
    time_val: f32,
    speed: f32,
    aspect_ratio: f32,
    uv_scale: f32,
}

pub struct ParametricSurfaceGenerator {
    bake_pipeline: manifold_gpu::GpuComputePipeline,
    raymarch_pipeline: manifold_gpu::GpuComputePipeline,
    volume_texture: manifold_gpu::GpuTexture,
    sampler: manifold_gpu::GpuSampler,
    // Dirty tracking: only re-bake when shape or morph changes (matches Unity ShouldRebake)
    last_shape: f32,
    last_morph: f32,
}

impl ParametricSurfaceGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let bake_pipeline = device.create_compute_pipeline(
            include_str!("shaders/parametric_surface_bake.wgsl"),
            "cs_main",
            "ParametricSurface Bake",
        );
        let raymarch_pipeline = device.create_compute_pipeline(
            include_str!("shaders/parametric_surface_raymarch_compute.wgsl"),
            "cs_main",
            "ParametricSurface Raymarch",
        );

        // 3D Volume Texture
        // Unity: RenderTextureFormat.RHalf. Use Rgba16Float for filterable storage on all backends.
        let volume_texture = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: VOL_SIZE,
            height: VOL_SIZE,
            depth: VOL_SIZE,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D3,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "ParametricSurface Volume",
            mip_levels: 1,
        });

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            ..Default::default()
        });

        Self {
            bake_pipeline,
            raymarch_pipeline,
            volume_texture,
            sampler,
            last_shape: f32::MIN,
            last_morph: f32::MIN,
        }
    }

    // Matches Unity: Mathf.Approximately uses ~0.00001 epsilon
    fn needs_rebake(&self, shape: f32, morph: f32) -> bool {
        (self.last_shape - shape).abs() > 0.00001 || (self.last_morph - morph).abs() > 0.00001
    }

    // Matches Unity ResolveShape: when snap > 0.5, shape = trigger_count % SURFACE_COUNT
    fn resolve_shape(ctx: &GeneratorContext) -> f32 {
        let snap = if ctx.param_count > SNAP as u32 {
            ctx.params[SNAP]
        } else {
            0.0
        };
        if snap > 0.5 {
            (ctx.trigger_count % SURFACE_COUNT) as f32
        } else if ctx.param_count > SHAPE as u32 {
            ctx.params[SHAPE]
        } else {
            0.0
        }
    }
}

impl Generator for ParametricSurfaceGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::PARAMETRIC_SURFACE
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let shape = Self::resolve_shape(ctx);
        let morph = if ctx.param_count > MORPH as u32 {
            ctx.params[MORPH]
        } else {
            0.0
        };
        let speed = if ctx.param_count > SPEED as u32 {
            ctx.params[SPEED]
        } else {
            1.0
        };
        let scale = if ctx.param_count > SCALE as u32 {
            ctx.params[SCALE]
        } else {
            1.0
        };

        // UV scale: matches Unity base class — rawScale > 0 ? 1/rawScale : 1
        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };

        // Bake if needed
        if self.needs_rebake(shape, morph) {
            let bake_uniforms = BakeUniforms {
                shape: shape.clamp(0.0, 4.0),
                morph: morph.clamp(0.0, 1.0),
                vol_res: VOL_SIZE as f32,
                _pad0: 0.0,
            };
            gpu.native_enc.dispatch_compute(
                &self.bake_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&bake_uniforms),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: &self.volume_texture,
                    },
                ],
                [VOL_SIZE / 4, VOL_SIZE / 4, VOL_SIZE / 4],
                "ParametricSurface Bake",
            );
            self.last_shape = shape;
            self.last_morph = morph;
        }

        // Raymarch — every frame (camera orbits with time)
        let rm_uniforms = RaymarchUniforms {
            time_val: ctx.time as f32 * speed,
            speed,
            aspect_ratio: ctx.aspect,
            uv_scale,
        };
        gpu.native_enc.dispatch_compute(
            &self.raymarch_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&rm_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: &self.volume_texture,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "ParametricSurface Raymarch",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Volume texture is fixed at 128^3; no resize needed
    }

    fn internal_resolution_scale(&self) -> f32 {
        1.0
    }
}
