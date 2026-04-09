//! Galactic Rock — procedural spiral rock formation generator.
//!
//! Pipeline:
//!   1. Compute: 100K particle simulation (distribute → noise → twist → FBM → rotation)
//!   2. Shadow: depth-only render from each light's perspective (2x)
//!   3. Render: instanced cubes with PBR + PCF shadow sampling
//!   4. Blur: luma-masked separable Gaussian for macro DOF

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::mesh_pipeline;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;
use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::GALACTIC_ROCK,
        display_name: "Galactic Rock",
        is_line_based: false,
        available: true,
        osc_prefix: "galacticRock",
        legacy_discriminant: Some(22),
        params: &[
            ParamSpec::continuous("Speed", 0.0, 5.0, 1.0, "F2", "speed"),
            ParamSpec::continuous("Wave Amp", 0.0, 0.5, 0.1, "F3", "waveAmp"),
            ParamSpec::continuous("Wave Freq", 0.1, 2.0, 0.5, "F2", "waveFreq"),
            ParamSpec::continuous("Twist", 0.0, 20.0, 10.0, "F1", "twist"),
            ParamSpec::continuous("Grain", 0.0, 0.01, 0.001, "F4", "grain"),
            ParamSpec::continuous("Roughness", 0.0, 1.0, 0.5, "F2", "roughness"),
            ParamSpec::continuous("Light Int", 0.1, 10.0, 2.5, "F1", "lightInt"),
            ParamSpec::continuous("Blur", 0.0, 20.0, 10.0, "F0", "blur"),
            ParamSpec::continuous("Cam Dist", 0.1, 10.0, 0.8, "F2", "camDist"),
            ParamSpec::continuous("Cam Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),
            ParamSpec::continuous("Cam Tilt", -90.0, 90.0, 10.0, "F0", "camTilt"),
            ParamSpec::continuous("Cam FOV", 20.0, 120.0, 60.0, "F0", "camFov"),
            ParamSpec::continuous("Look Y", -2.0, 2.0, 0.0, "F2", "lookY"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
        ],
        string_params: &[],
    }
}
inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::GALACTIC_ROCK,
        create: |device| Box::new(GalacticRockGenerator::new(device)),
    }
}

const INSTANCE_COUNT: u32 = 100_000;
const INSTANCE_STRIDE: u64 = 32; // 2 × vec4<f32>
const SHADOW_MAP_SIZE: u32 = 2048;

// Parameter indices (must match generator_definition_registry order)
const P_SPEED: usize = 0;
const P_WAVE_AMP: usize = 1;
const P_WAVE_FREQ: usize = 2;
const P_TWIST: usize = 3;
const P_GRAIN: usize = 4;
const P_ROUGHNESS: usize = 5;
const P_LIGHT_INT: usize = 6;
const P_BLUR: usize = 7;
const P_CAM_DIST: usize = 8;
const P_CAM_ORBIT: usize = 9;
const P_CAM_TILT: usize = 10;
const P_CAM_FOV: usize = 11;
const P_LOOK_Y: usize = 12;
const P_SCALE: usize = 13;

// ─── Uniform structs (must match WGSL exactly, 16-byte aligned) ─────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ComputeUniforms {
    time: f32,
    instance_count: u32,
    speed: f32,
    wave_amp: f32,
    wave_freq: f32,
    twist_amount: f32,
    grain_amp: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light0_pos: [f32; 4],
    light1_pos: [f32; 4],
    light0_color: [f32; 4],
    light1_color: [f32; 4],
    ambient_color: [f32; 4],
    material: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowUniforms {
    light_view_proj: [[f32; 4]; 4],
    _pad: [[f32; 4]; 7],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowMatrices {
    light0_vp: [[f32; 4]; 4],
    light1_vp: [[f32; 4]; 4],
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

pub struct GalacticRockGenerator {
    // Compute
    compute_pipeline: manifold_gpu::GpuComputePipeline,
    instance_buf: manifold_gpu::GpuBuffer,

    // Shadow
    shadow_pipeline: manifold_gpu::GpuRenderPipeline,
    shadow_depth_stencil: manifold_gpu::GpuDepthStencilState,
    shadow_map_0: manifold_gpu::GpuTexture,
    shadow_map_1: manifold_gpu::GpuTexture,
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

impl GalacticRockGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        // ── Compute pipeline ──
        let compute_pipeline = device.create_compute_pipeline(
            include_str!("shaders/galactic_rock_compute.wgsl"),
            "cs_main",
            "GalacticRock Compute",
        );

        let instance_buf =
            device.create_buffer_shared(INSTANCE_COUNT as u64 * INSTANCE_STRIDE);

        // ── Shadow pipeline ──
        let shadow_pipeline = device.create_render_pipeline_depth(
            include_str!("shaders/galactic_rock_shadow.wgsl"),
            "vs_shadow",
            "fs_shadow",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            1,
            "GalacticRock Shadow",
        );

        let shadow_depth_stencil =
            device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                compare: manifold_gpu::GpuCompareFunction::Less,
                write_enabled: true,
            });

        let shadow_map_0 = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "GalacticRock Shadow0",
            mip_levels: 1,
        });
        let shadow_map_1 = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET
                | manifold_gpu::GpuTextureUsage::SHADER_READ,
            label: "GalacticRock Shadow1",
            mip_levels: 1,
        });
        // Color target for shadow passes — must match shadow map dimensions.
        // Color output is discarded (DontCare store) but Metal requires matching sizes.
        let shadow_color_dummy = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: SHADOW_MAP_SIZE,
            height: SHADOW_MAP_SIZE,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "GalacticRock ShadowColor",
            mip_levels: 1,
        });

        // ── Main render pipeline ──
        let render_pipeline = device.create_render_pipeline_depth(
            include_str!("shaders/galactic_rock_render.wgsl"),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            1,
            "GalacticRock Render",
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

        // ── Blur pipeline ──
        let blur_pipeline = device.create_compute_pipeline(
            include_str!("shaders/galactic_rock_blur.wgsl"),
            "cs_main",
            "GalacticRock Blur",
        );

        Self {
            compute_pipeline,
            instance_buf,
            shadow_pipeline,
            shadow_depth_stencil,
            shadow_map_0,
            shadow_map_1,
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
        if self.depth_width == width && self.depth_height == height && self.depth_texture.is_some()
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
            label: "GalacticRock Depth",
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
            label: "GalacticRock BlurTemp",
            mip_levels: 1,
        }));
        self.depth_width = width;
        self.depth_height = height;
    }

    /// Render shadow map from a light's perspective.
    fn render_shadow_pass(
        &self,
        gpu: &mut manifold_gpu::GpuEncoder,
        shadow_map: &manifold_gpu::GpuTexture,
        light_vp: [[f32; 4]; 4],
        label: &str,
    ) {
        let uniforms = ShadowUniforms {
            light_view_proj: light_vp,
            _pad: [[0.0; 4]; 7],
        };

        gpu.draw_instanced_depth(
            &self.shadow_pipeline,
            &self.shadow_color_dummy,
            shadow_map,
            &self.shadow_depth_stencil,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
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
            label,
        );
    }
}

impl Generator for GalacticRockGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::GALACTIC_ROCK
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

        let speed = ctx.params[P_SPEED];
        let wave_amp = ctx.params[P_WAVE_AMP];
        let wave_freq = ctx.params[P_WAVE_FREQ];
        let twist = ctx.params[P_TWIST];
        let grain = ctx.params[P_GRAIN];
        let roughness = ctx.params[P_ROUGHNESS];
        let light_int = ctx.params[P_LIGHT_INT];
        let blur_radius = ctx.params[P_BLUR];
        let cam_dist = ctx.params[P_CAM_DIST].max(0.05);
        let cam_orbit = ctx.params[P_CAM_ORBIT].to_radians();
        let cam_tilt = ctx.params[P_CAM_TILT].to_radians();
        let cam_fov = ctx.params[P_CAM_FOV].to_radians().max(0.1);
        let look_y = ctx.params[P_LOOK_Y];
        let scale = ctx.params[P_SCALE].max(0.01);

        // ── Phase 1: Compute particle simulation ──
        let compute_uniforms = ComputeUniforms {
            time: ctx.time as f32,
            instance_count: INSTANCE_COUNT,
            speed,
            wave_amp,
            wave_freq,
            twist_amount: twist,
            grain_amp: grain,
            _pad: 0.0,
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
            "GalacticRock Compute",
        );

        // ── Camera setup (orbit controls) ──
        // Orbit: horizontal angle around Y axis. Tilt: vertical angle.
        // Camera orbits around the look-at target at the given distance.
        let target_pos = [0.0f32, look_y, 0.0];
        let eye = [
            cam_dist * cam_tilt.cos() * cam_orbit.sin(),
            cam_dist * cam_tilt.sin() + look_y,
            cam_dist * cam_tilt.cos() * cam_orbit.cos(),
        ];
        let up = [0.0f32, 1.0, 0.0];
        let aspect = ctx.aspect;
        let view = mesh_pipeline::look_at_rh(eye, target_pos, up);
        let proj = mesh_pipeline::perspective_rh(cam_fov, aspect, 0.005, 50.0);
        let view_proj = mesh_pipeline::mat4_mul(proj, view);

        // ── Light positions (opposing high angles, static) ──
        // Two point lights at steep angles for maximum shadow depth in crevices.
        let light_radius = 3.0;
        let light_height = 2.5;
        let light0_pos = [
            light_radius * 0.7,
            light_height,
            light_radius * 0.7,
        ];
        let light1_pos = [
            -light_radius * 0.7,
            -light_height * 0.4,
            -light_radius * 0.7,
        ];

        // ── Phase 2: Shadow passes ──
        let shadow_extent = 3.0 * scale;
        let light0_view = mesh_pipeline::look_at_rh(light0_pos, target_pos, up);
        let light0_proj = ortho_rh(-shadow_extent, shadow_extent, -shadow_extent, shadow_extent, 0.1, 50.0);
        let light0_vp = mesh_pipeline::mat4_mul(light0_proj, light0_view);

        let light1_view = mesh_pipeline::look_at_rh(light1_pos, target_pos, up);
        let light1_proj = ortho_rh(-shadow_extent, shadow_extent, -shadow_extent, shadow_extent, 0.1, 50.0);
        let light1_vp = mesh_pipeline::mat4_mul(light1_proj, light1_view);

        self.render_shadow_pass(gpu.native_enc, &self.shadow_map_0, light0_vp, "Shadow0");
        self.render_shadow_pass(gpu.native_enc, &self.shadow_map_1, light1_vp, "Shadow1");

        // ── Phase 3: Main render ──
        let render_uniforms = RenderUniforms {
            view_proj,
            camera_pos: [eye[0], eye[1], eye[2], 0.0],
            light0_pos: [light0_pos[0], light0_pos[1], light0_pos[2], 0.0],
            light1_pos: [light1_pos[0], light1_pos[1], light1_pos[2], 0.0],
            light0_color: [1.0, 0.95, 0.9, light_int],
            light1_color: [0.9, 0.93, 1.0, light_int],
            ambient_color: [0.5, 0.5, 0.5, 0.02],
            material: [0.0, roughness, SHADOW_MAP_SIZE as f32, 0.0],
        };

        let shadow_mats = ShadowMatrices {
            light0_vp,
            light1_vp,
        };

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
                    data: bytemuck::bytes_of(&shadow_mats),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.shadow_map_0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: &self.shadow_map_1,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 5,
                    sampler: &self.shadow_sampler,
                },
            ],
            36,
            INSTANCE_COUNT,
            manifold_gpu::GpuLoadAction::Clear,
            "GalacticRock Render",
        );

        // ── Phase 4: Luma blur (2-pass separable) ──
        if blur_radius > 0.5 {
            let blur_temp = self.blur_temp.as_ref().unwrap();

            // Pass 1: horizontal blur (target → blur_temp)
            let blur_h = BlurUniforms {
                max_radius: blur_radius,
                direction: 0.0,
                width: width as f32,
                height: height as f32,
            };
            let wg_x = width.div_ceil(16);
            let wg_y = height.div_ceil(16);
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
                "GalacticRock BlurH",
            );

            // Pass 2: vertical blur (blur_temp → target)
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
                "GalacticRock BlurV",
            );
        }

        ctx.anim_progress
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.ensure_depth_texture(device, width, height);
    }
}

// ─── Orthographic projection (right-handed, depth [0,1]) ────────────

fn ortho_rh(
    left: f32,
    right: f32,
    bottom: f32,
    top: f32,
    z_near: f32,
    z_far: f32,
) -> [[f32; 4]; 4] {
    let rml = right - left;
    let tmb = top - bottom;
    let fmn = z_far - z_near;
    [
        [2.0 / rml, 0.0, 0.0, 0.0],
        [0.0, 2.0 / tmb, 0.0, 0.0],
        [0.0, 0.0, -1.0 / fmn, 0.0],
        [
            -(right + left) / rml,
            -(top + bottom) / tmb,
            -z_near / fmn,
            1.0,
        ],
    ]
}
