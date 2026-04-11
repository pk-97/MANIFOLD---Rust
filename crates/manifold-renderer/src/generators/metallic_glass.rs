//! Metallic Glass — Y2K aesthetic feedback-displacement generator.
//!
//! Pipeline (5 passes per frame):
//!   1. Blend:   Simplex noise + abs_diff with previous frame × decay → feedback_b
//!   2. Blur H:  Separable Gaussian horizontal → blur_temp
//!   3. Blur V:  Separable Gaussian vertical → feedback_a (completes swap)
//!   4. Process:  Sobel edge + 45° mirror + height/metallic remap → processed
//!   5. Render:  300×300 displaced grid + Cook-Torrance PBR + procedural IBL → target

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::mesh_pipeline;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::METALLIC_GLASS,
        create: |device| Box::new(MetallicGlassGenerator::new(device)),
    }
}

const GRID_SIZE: u32 = 500;
const GRID_QUADS: u32 = GRID_SIZE - 1;
const VERTEX_COUNT: u32 = GRID_QUADS * GRID_QUADS * 6; // 1,494,006

// Parameter indices (must match generator_definition_registry order)
const P_FEEDBACK: usize = 0;
const P_NOISE_SCALE: usize = 1;
const P_NOISE_SPEED: usize = 2;
const P_EDGE_STR: usize = 3;
const P_MIRROR: usize = 4;
const P_DISPLACE: usize = 5;
const P_ROUGHNESS: usize = 6;
const P_LIGHT_INT: usize = 7;
const P_CAM_DIST: usize = 8;
const P_CAM_ORBIT: usize = 9;
const P_CAM_TILT: usize = 10;
const P_CAM_FOV: usize = 11;
const P_LOOK_Y: usize = 12;

// ─── Uniform structs (must match WGSL exactly, 16-byte aligned) ─────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendUniforms {
    time: f32,
    noise_scale: f32,
    noise_speed: f32,
    feedback_decay: f32,
    width: f32,
    height: f32,
    _pad0: f32,
    _pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    direction: f32,
    width: f32,
    height: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProcessUniforms {
    edge_strength: f32,
    mirror_angle: f32,
    width: f32,
    height: f32,
    temporal_blend: f32,  // 0.0 = frozen, 1.0 = no smoothing
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderUniforms {
    view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 4],
    light_pos: [f32; 4],
    light_color: [f32; 4],
    material: [f32; 4],
    grid_info: [f32; 4],
}

// ─── Generator ──────────────────────────────────────────────────────

pub struct MetallicGlassGenerator {
    // Compute pipelines
    blend_pipeline: manifold_gpu::GpuComputePipeline,
    blur_pipeline: manifold_gpu::GpuComputePipeline,
    process_pipeline: manifold_gpu::GpuComputePipeline,
    envmap_pipeline: manifold_gpu::GpuComputePipeline,

    // Render pipeline
    render_pipeline: manifold_gpu::GpuRenderPipeline,
    depth_stencil: manifold_gpu::GpuDepthStencilState,
    sampler: manifold_gpu::GpuSampler,

    // Pre-baked HDR environment map (512×256 equirectangular)
    env_map: manifold_gpu::GpuTexture,

    // Ping-pong feedback textures (persistent across frames)
    feedback_a: Option<manifold_gpu::GpuTexture>,
    feedback_b: Option<manifold_gpu::GpuTexture>,

    // Temporary textures
    blur_temp: Option<manifold_gpu::GpuTexture>,
    processed_a: Option<manifold_gpu::GpuTexture>,
    processed_b: Option<manifold_gpu::GpuTexture>,
    use_processed_a: bool,
    depth_texture: Option<manifold_gpu::GpuTexture>,

    // State
    tex_width: u32,
    tex_height: u32,
    frame_count: u64,
}

impl MetallicGlassGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let blend_pipeline = device.create_compute_pipeline(
            include_str!("shaders/metallic_glass_blend.wgsl"),
            "cs_main",
            "MetallicGlass Blend",
        );

        let blur_pipeline = device.create_compute_pipeline(
            include_str!("shaders/metallic_glass_blur.wgsl"),
            "cs_main",
            "MetallicGlass Blur",
        );

        let process_pipeline = device.create_compute_pipeline(
            include_str!("shaders/metallic_glass_process.wgsl"),
            "cs_main",
            "MetallicGlass Process",
        );

        let envmap_pipeline = device.create_compute_pipeline(
            include_str!("shaders/metallic_glass_envmap.wgsl"),
            "cs_main",
            "MetallicGlass EnvMap",
        );

        // Pre-bake the HDR environment map (512×256 equirectangular)
        let env_map = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: 512,
            height: 256,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
            label: "MetallicGlass EnvMap",
            mip_levels: 1,
        });

        let render_pipeline = device.create_render_pipeline_depth(
            include_str!("shaders/metallic_glass_render.wgsl"),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None, // opaque geometry
            1,
            "MetallicGlass Render",
        );

        let depth_stencil =
            device.create_depth_stencil_state(&manifold_gpu::GpuDepthStencilDesc {
                compare: manifold_gpu::GpuCompareFunction::Less,
                write_enabled: true,
            });

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            min_filter: manifold_gpu::GpuFilterMode::Linear,
            mag_filter: manifold_gpu::GpuFilterMode::Linear,
            mip_filter: manifold_gpu::GpuFilterMode::Nearest,
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            compare: None,
        });

        Self {
            blend_pipeline,
            blur_pipeline,
            process_pipeline,
            envmap_pipeline,
            render_pipeline,
            depth_stencil,
            sampler,
            env_map,
            feedback_a: None,
            feedback_b: None,
            blur_temp: None,
            processed_a: None,
            processed_b: None,
            use_processed_a: true,
            depth_texture: None,
            tex_width: 0,
            tex_height: 0,
            frame_count: 0,
        }
    }

    fn ensure_textures(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        if self.tex_width == width && self.tex_height == height && self.feedback_a.is_some() {
            return;
        }

        let make_rw = |label: &str| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width,
                height,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::SHADER_WRITE,
                label,
                mip_levels: 1,
            })
        };

        self.feedback_a = Some(make_rw("MetallicGlass FeedbackA"));
        self.feedback_b = Some(make_rw("MetallicGlass FeedbackB"));
        self.blur_temp = Some(make_rw("MetallicGlass BlurTemp"));
        self.processed_a = Some(make_rw("MetallicGlass ProcessedA"));
        self.processed_b = Some(make_rw("MetallicGlass ProcessedB"));
        self.use_processed_a = true;

        self.depth_texture = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width,
            height,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Depth32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET,
            label: "MetallicGlass Depth",
            mip_levels: 1,
        }));

        self.tex_width = width;
        self.tex_height = height;
        self.frame_count = 0;
    }
}

impl Generator for MetallicGlassGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::METALLIC_GLASS
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let width = ctx.width;
        let height = ctx.height;
        self.ensure_textures(gpu.device, width, height);

        // Bake the HDR environment map on the first frame
        if self.frame_count == 0 {
            gpu.native_enc.dispatch_compute(
                &self.envmap_pipeline,
                &[manifold_gpu::GpuBinding::Texture {
                    binding: 0,
                    texture: &self.env_map,
                }],
                [512u32.div_ceil(16), 256u32.div_ceil(16), 1],
                "MetallicGlass EnvMap Bake",
            );
        }

        let feedback_decay = ctx.params[P_FEEDBACK];
        let noise_scale = ctx.params[P_NOISE_SCALE];
        let noise_speed = ctx.params[P_NOISE_SPEED];
        let edge_str = ctx.params[P_EDGE_STR];
        let mirror_angle = ctx.params[P_MIRROR].to_radians();
        let displacement = ctx.params[P_DISPLACE];
        let roughness = ctx.params[P_ROUGHNESS];
        let light_int = ctx.params[P_LIGHT_INT];
        let cam_dist = ctx.params[P_CAM_DIST].max(0.1);
        let cam_orbit = ctx.params[P_CAM_ORBIT].to_radians();
        let cam_tilt = ctx.params[P_CAM_TILT].to_radians();
        let cam_fov = ctx.params[P_CAM_FOV].to_radians().max(0.1);
        let look_y = ctx.params[P_LOOK_Y];

        let wg_x = width.div_ceil(16);
        let wg_y = height.div_ceil(16);

        let fb_a = self.feedback_a.as_ref().unwrap();
        let fb_b = self.feedback_b.as_ref().unwrap();
        let blur_temp = self.blur_temp.as_ref().unwrap();

        // Ping-pong processed textures for temporal smoothing.
        // Write to one, read previous from the other, then swap.
        let (proc_write, proc_read) = if self.use_processed_a {
            (
                self.processed_a.as_ref().unwrap(),
                self.processed_b.as_ref().unwrap(),
            )
        } else {
            (
                self.processed_b.as_ref().unwrap(),
                self.processed_a.as_ref().unwrap(),
            )
        };

        // ── Pass 1: Feedback blend ──
        // Read feedback_a (previous frame), write to feedback_b
        let blend_uniforms = BlendUniforms {
            time: ctx.time as f32,
            noise_scale,
            noise_speed,
            feedback_decay,
            width: width as f32,
            height: height as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            &self.blend_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&blend_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: fb_a,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: fb_b,
                },
            ],
            [wg_x, wg_y, 1],
            "MetallicGlass Blend",
        );

        // ── Pass 2: Blur horizontal (feedback_b → blur_temp) ──
        let blur_h = BlurUniforms {
            direction: 0.0,
            width: width as f32,
            height: height as f32,
            _pad: 0.0,
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
                    texture: fb_b,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: blur_temp,
                },
            ],
            [wg_x, wg_y, 1],
            "MetallicGlass BlurH",
        );

        // ── Pass 3: Blur vertical (blur_temp → feedback_a) ──
        // This completes the ping-pong: feedback_a now has the current frame's
        // blurred feedback, ready to be read next frame.
        let blur_v = BlurUniforms {
            direction: 1.0,
            width: width as f32,
            height: height as f32,
            _pad: 0.0,
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
                    texture: fb_a,
                },
            ],
            [wg_x, wg_y, 1],
            "MetallicGlass BlurV",
        );

        // ── Pass 4: Mirror + height/metallic + temporal blend ──
        // Writes to proc_write, blends with proc_read (previous frame).
        let process_uniforms = ProcessUniforms {
            edge_strength: edge_str,
            mirror_angle,
            width: width as f32,
            height: height as f32,
            temporal_blend: 0.15, // blend 15% new, 85% previous → stable
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            &self.process_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&process_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: fb_a,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: proc_write,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: proc_read,
                },
            ],
            [wg_x, wg_y, 1],
            "MetallicGlass Process",
        );

        // Swap processed ping-pong for next frame
        self.use_processed_a = !self.use_processed_a;

        // ── Pass 5: Render displaced grid ──
        let target_pos = [0.0f32, look_y, 0.0];
        let eye = [
            cam_dist * cam_tilt.cos() * cam_orbit.sin(),
            cam_dist * cam_tilt.sin() + look_y,
            cam_dist * cam_tilt.cos() * cam_orbit.cos(),
        ];
        let up = [0.0f32, 1.0, 0.0];
        let aspect = ctx.aspect;
        let view = mesh_pipeline::look_at_rh(eye, target_pos, up);
        let proj = mesh_pipeline::perspective_rh(cam_fov, aspect, 0.01, 50.0);
        let view_proj = mesh_pipeline::mat4_mul(proj, view);

        // Light positioned at 45° angle to catch displacement edges
        let light_pos = [-2.0f32, 2.0, 5.0, 0.0];

        let render_uniforms = RenderUniforms {
            view_proj,
            camera_pos: [eye[0], eye[1], eye[2], 0.0],
            light_pos,
            light_color: [1.0, 1.0, 1.0, light_int],
            material: [1.0, roughness, displacement, 0.0], // metallic=1.0 always
            grid_info: [GRID_SIZE as f32, 1.0 / width as f32, aspect, 0.0],
        };

        let depth_tex = self.depth_texture.as_ref().unwrap();

        gpu.native_enc.draw_instanced_depth(
            &self.render_pipeline,
            target,
            depth_tex,
            &self.depth_stencil,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&render_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: proc_write,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &self.env_map,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 4,
                    sampler: &self.sampler,
                },
            ],
            VERTEX_COUNT,
            1, // single instance (the grid itself)
            manifold_gpu::GpuLoadAction::Clear,
            "MetallicGlass Render",
        );

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.ensure_textures(device, width, height);
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        // Force texture re-creation to clear feedback state
        self.feedback_a = None;
        self.feedback_b = None;
        self.processed_a = None;
        self.processed_b = None;
        self.use_processed_a = true;
        self.tex_width = 0;
        self.tex_height = 0;
        self.frame_count = 0;
    }
}
