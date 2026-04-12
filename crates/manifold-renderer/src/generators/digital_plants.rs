//! Digital Plants — procedural plant topology generator.
//!
//! Pipeline:
//!   1. Compute: 160K instance simulation (UV → noise → cylinder → torus → morph)
//!   2. Shadow: depth-only render from light perspective
//!   3. Render: instanced cubes with cel shading + PCF shadow sampling
//!   4. Blur: optional luma-masked separable Gaussian

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::mesh_pipeline;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::DIGITAL_PLANTS,
        create: |device| Box::new(DigitalPlantsGenerator::new(device)),
    }
}

const GRID_SIZE: u32 = 400;
const INSTANCE_COUNT: u32 = GRID_SIZE * GRID_SIZE; // 160,000
const INSTANCE_STRIDE: u64 = 32; // 2 × vec4<f32>
const SHADOW_MAP_SIZE: u32 = 2048;

// Parameter indices (must match generator_metadata_submissions order)
const P_NOISE_SCALE: usize = 0;
const P_ANIM_SPEED: usize = 1;
const P_MORPH: usize = 2;
const P_BASE_RADIUS: usize = 3;
const P_HEIGHT: usize = 4;
const P_TAPER: usize = 5;
const P_TORUS_RADIUS: usize = 6;
const P_PETAL_AMP: usize = 7;
const P_ROT_SPEED: usize = 8;
const P_BOX_SCALE: usize = 9;
const P_CAM_DIST: usize = 10;
const P_CAM_ORBIT: usize = 11;
const P_CAM_TILT: usize = 12;
const P_CAM_FOV: usize = 13;

// ─── Uniform structs (must match WGSL exactly, 16-byte aligned) ─────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ComputeUniforms {
    time: f32,
    instance_count: u32,
    noise_scale: f32,
    anim_speed: f32,
    morph: f32,
    base_radius: f32,
    height_scale: f32,
    taper: f32,
    torus_radius: f32,
    petal_amp: f32,
    rot_speed: f32,
    box_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_pos: [f32; 4],
    light_color: [f32; 4],
    shadow_info: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowUniforms {
    light_view_proj: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowMatrix {
    light_vp: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    max_radius: f32,
    direction: f32,
    width: f32,
    height: f32,
}

// ─── Generator ──────────────────────────────────────────────────────

pub struct DigitalPlantsGenerator {
    // Compute
    compute_pipeline: manifold_gpu::GpuComputePipeline,
    instance_buf: manifold_gpu::GpuBuffer,

    // Shadow
    shadow_pipeline: manifold_gpu::GpuRenderPipeline,
    shadow_depth_stencil: manifold_gpu::GpuDepthStencilState,
    shadow_map: manifold_gpu::GpuTexture,
    shadow_color_dummy: manifold_gpu::GpuTexture,

    // Render
    render_pipeline: manifold_gpu::GpuRenderPipeline,
    render_depth_stencil: manifold_gpu::GpuDepthStencilState,
    depth_texture: Option<manifold_gpu::GpuTexture>,
    shadow_sampler: manifold_gpu::GpuSampler,

    // Blur
    blur_pipeline: manifold_gpu::GpuComputePipeline,
    blur_temp: Option<manifold_gpu::GpuTexture>,

    // State
    depth_width: u32,
    depth_height: u32,
}

const NOISE_COMMON: &str = include_str!("shaders/noise_common.wgsl");

impl DigitalPlantsGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        // ── Compute pipeline (noise_common prepended) ──
        let compute_source = format!(
            "{}\n{}",
            NOISE_COMMON,
            include_str!("shaders/digital_plants_compute.wgsl"),
        );
        let compute_pipeline = device.create_compute_pipeline(
            &compute_source,
            "cs_main",
            "DigitalPlants Compute",
        );

        let instance_buf =
            device.create_buffer_shared(INSTANCE_COUNT as u64 * INSTANCE_STRIDE);

        // ── Shadow pipeline ──
        let shadow_pipeline = device.create_render_pipeline_depth(
            include_str!("shaders/digital_plants_shadow.wgsl"),
            "vs_shadow",
            "fs_shadow",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            1,
            "DigitalPlants Shadow",
        );

        let shadow_depth_stencil =
            device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                compare: manifold_gpu::GpuCompareFunction::Less,
                write_enabled: true,
            });

        let shadow_map = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "DigitalPlants Shadow",
            mip_levels: 1,
        });

        let shadow_color_dummy = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "DigitalPlants ShadowColor",
            mip_levels: 1,
        });

        // ── Main render pipeline ──
        let render_pipeline = device.create_render_pipeline_depth(
            include_str!("shaders/digital_plants_render.wgsl"),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            1,
            "DigitalPlants Render",
        );

        let render_depth_stencil =
            device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                compare: manifold_gpu::GpuCompareFunction::Less,
                write_enabled: true,
            });

        let shadow_sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Nearest,
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            compare: Some(manifold_gpu::GpuCompareFunction::LessEqual),
        });

        // ── Blur pipeline (reuse galactic_rock_blur.wgsl) ──
        let blur_pipeline = device.create_compute_pipeline(
            include_str!("shaders/galactic_rock_blur.wgsl"),
            "cs_main",
            "DigitalPlants Blur",
        );

        Self {
            compute_pipeline,
            instance_buf,
            shadow_pipeline,
            shadow_depth_stencil,
            shadow_map,
            shadow_color_dummy,
            render_pipeline,
            render_depth_stencil,
            depth_texture: None,
            shadow_sampler,
            blur_pipeline,
            blur_temp: None,
            depth_width: 0,
            depth_height: 0,
        }
    }

    fn ensure_depth_texture(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        width: u32,
        height: u32,
    ) {
        if self.depth_width == width
            && self.depth_height == height
            && self.depth_texture.is_some()
        {
            return;
        }
        self.depth_texture = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "DigitalPlants Depth",
            mip_levels: 1,
        }));
        self.blur_temp = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "DigitalPlants BlurTemp",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }
}

impl Generator for DigitalPlantsGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::DIGITAL_PLANTS
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let width = ctx.width;
        let height = ctx.height;
        self.ensure_depth_texture(gpu.device, width, height);

        let noise_scale = ctx.params[P_NOISE_SCALE];
        let anim_speed = ctx.params[P_ANIM_SPEED];
        let morph = ctx.params[P_MORPH];
        let base_radius = ctx.params[P_BASE_RADIUS];
        let height_scale = ctx.params[P_HEIGHT];
        let taper = ctx.params[P_TAPER];
        let torus_radius = ctx.params[P_TORUS_RADIUS];
        let petal_amp = ctx.params[P_PETAL_AMP];
        let rot_speed = ctx.params[P_ROT_SPEED];
        let box_scale = ctx.params[P_BOX_SCALE];
        let cam_dist = ctx.params[P_CAM_DIST].max(0.05);
        let cam_orbit = ctx.params[P_CAM_ORBIT].to_radians();
        let cam_tilt = ctx.params[P_CAM_TILT].to_radians();
        let cam_fov = ctx.params[P_CAM_FOV].to_radians().max(0.1);

        // ── Phase 1: Compute instance simulation ──
        let compute_uniforms = ComputeUniforms {
            time: ctx.time as f32,
            instance_count: INSTANCE_COUNT,
            noise_scale,
            anim_speed,
            morph,
            base_radius,
            height_scale,
            taper,
            torus_radius,
            petal_amp,
            rot_speed,
            box_scale,
        };

        let wg = INSTANCE_COUNT.div_ceil(256);
        gpu.native_enc.dispatch_compute(
            &self.compute_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&compute_uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.instance_buf,
                    offset: 0,
                },
            ],
            [wg, 1, 1],
            "DigitalPlants Compute",
        );

        // ── Camera setup (orbit controls) ──
        let target_pos = [0.0f32, 0.0, 0.0];
        let eye = [
            cam_dist * cam_tilt.cos() * cam_orbit.sin(),
            cam_dist * cam_tilt.sin(),
            cam_dist * cam_tilt.cos() * cam_orbit.cos(),
        ];
        let up = [0.0f32, 1.0, 0.0];
        let aspect = ctx.aspect;
        let view = mesh_pipeline::look_at_rh(eye, target_pos, up);
        let proj = mesh_pipeline::perspective_rh(cam_fov, aspect, 0.005, 50.0);
        let view_proj = mesh_pipeline::mat4_mul(proj, view);

        // ── Light position (single high-angle light) ──
        let light_radius = 4.0;
        let light_height = 3.0;
        let light_pos = [light_radius * 0.7, light_height, light_radius * 0.7];

        // ── Phase 2: Shadow pass ──
        let shadow_extent = 4.0;
        let light_view =
            mesh_pipeline::look_at_rh(light_pos, target_pos, up);
        let light_proj = mesh_pipeline::ortho_rh(
            -shadow_extent,
            shadow_extent,
            -shadow_extent,
            shadow_extent,
            0.1,
            50.0,
        );
        let light_vp = mesh_pipeline::mat4_mul(light_proj, light_view);

        let shadow_uniforms = ShadowUniforms {
            light_view_proj: light_vp,
        };

        gpu.native_enc.draw_instanced_depth(
            &self.shadow_pipeline,
            &self.shadow_color_dummy,
            &self.shadow_map,
            &self.shadow_depth_stencil,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&shadow_uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.instance_buf,
                    offset: 0,
                },
            ],
            36,
            INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Clear,
            "DigitalPlants Shadow",
        );

        // ── Phase 3: Main render ──
        let render_uniforms = RenderUniforms {
            view_proj,
            camera_pos: [eye[0], eye[1], eye[2], 0.0],
            light_pos: [light_pos[0], light_pos[1], light_pos[2], 0.0],
            light_color: [1.0, 0.95, 0.9, 2.0], // warm white, intensity 2.0
            shadow_info: [SHADOW_MAP_SIZE as f32, 0.0, 0.0, 0.0],
        };

        let shadow_mat = ShadowMatrix { light_vp };

        let depth_tex = self.depth_texture.as_ref().unwrap();

        gpu.native_enc.draw_instanced_depth(
            &self.render_pipeline,
            target,
            depth_tex,
            &self.render_depth_stencil,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&render_uniforms),
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.instance_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&shadow_mat),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.shadow_map,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 4,
                    sampler: &self.shadow_sampler,
                },
            ],
            36,
            INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Clear,
            "DigitalPlants Render",
        );

        // ── Phase 4: Optional blur ──
        let blur_radius = 3.0; // subtle fixed blur for organic softness
        if blur_radius > 0.5 {
            let blur_temp = self.blur_temp.as_ref().unwrap();
            let wg_x = width.div_ceil(16);
            let wg_y = height.div_ceil(16);

            // Pass 1: horizontal
            let blur_h = BlurUniforms {
                max_radius: blur_radius,
                direction: 0.0,
                width: width as f32,
                height: height as f32,
            };
            gpu.native_enc.dispatch_compute(
                &self.blur_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&blur_h),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: target,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 2,
                        texture: blur_temp,
                    },
                ],
                [wg_x, wg_y, 1],
                "DigitalPlants BlurH",
            );

            // Pass 2: vertical
            let blur_v = BlurUniforms {
                max_radius: blur_radius,
                direction: 1.0,
                width: width as f32,
                height: height as f32,
            };
            gpu.native_enc.dispatch_compute(
                &self.blur_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&blur_v),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: blur_temp,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 2,
                        texture: target,
                    },
                ],
                [wg_x, wg_y, 1],
                "DigitalPlants BlurV",
            );
        }

        ctx.anim_progress
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.ensure_depth_texture(device, width, height);
    }
}
