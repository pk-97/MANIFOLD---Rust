// Oily Fluid — 4-pass reaction/advection fluid feedback generator.
//
// Faithful port of Bileam Tschepe's "red oily fluid" TouchDesigner tutorial.
// The simulation is driven by an internal frame counter so it continues to
// evolve while playback is stopped (decoupled from `ctx.time`).
//
// Per-frame dispatch sequence (7 compute dispatches):
//   1. cs_downsample      velocity(front) → blur_scratch_a (quarter-res box)
//   2. cs_main (blur H)   blur_scratch_a → blur_scratch_b
//   3. cs_main (blur V)   blur_scratch_b → blur_scratch_c  (final blurred vel)
//   4. cs_velocity        color/velocity/blurred → velocity(back)
//   5. cs_color           color(front) + velocity(back) + inline noise → color(back)
//   6. cs_render          color(back) + velocity(back) → target
//   7. swap() on both ping-pong states; internal_frame += 1
//
// Shader source is composed from particle_common.wgsl (for wang_hash and
// simplex_noise_3d) + oily_fluid.wgsl.

use super::stateful_base::StatefulState;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use crate::render_target::RenderTarget;
use manifold_core::GeneratorTypeId;

// Parameter indices (must match generator_definition_registry order).
const SPEED: usize = 0;
const FEEDBACK: usize = 1;
const NOISE_INJECT: usize = 2;
const VEL_DAMP: usize = 3;
const CURL: usize = 4;
const RELIEF: usize = 5;
const CHROMA: usize = 6;
const CONTRAST: usize = 7;
const HUE: usize = 8;
const SATURATION: usize = 9;
const BRIGHTNESS: usize = 10;
const VEL_DISP: usize = 11;
const COL_DISP: usize = 12;
const MODE: usize = 13;

const STATE_FORMAT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
const BLUR_RADIUS: f32 = 12.0; // spec: filter size 12
const BLUR_PRESHRINK: u32 = 4; // spec: pre-shrink 4

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

// ── Uniform structs (all 32 bytes to satisfy Naga multi-entry-point rule) ──

// All uniform structs are 48 bytes (12 f32s) to satisfy Naga's
// multi-entry-point same-size-at-same-binding rule.

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DownsampleUniforms {
    src_width: f32,
    src_height: f32,
    dst_width: f32,
    dst_height: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
    _pad6: f32,
    _pad7: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VelocityUniforms {
    width: f32,
    height: f32,
    grad_attenuation: f32,
    velocity_damping: f32,
    self_advect_scale: f32,
    vel_disp: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorUniforms {
    width: f32,
    height: f32,
    feedback_retention: f32,
    noise_injection: f32,
    noise_time: f32,
    aspect: f32,
    col_disp: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderUniforms {
    width: f32,
    height: f32,
    normal_z_scale: f32,
    chroma: f32,
    contrast: f32,
    hue_shift: f32,
    saturation: f32,
    brightness: f32,
    mode: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

// Matches gaussian_blur_compute.wgsl's BlurUniforms exactly (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurUniforms {
    direction_x: f32,
    direction_y: f32,
    radius: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

pub struct OilyFluidGenerator {
    // Pipelines
    downsample_pipeline: manifold_gpu::GpuComputePipeline,
    blur_pipeline: manifold_gpu::GpuComputePipeline,
    velocity_pipeline: manifold_gpu::GpuComputePipeline,
    color_pipeline: manifold_gpu::GpuComputePipeline,
    render_pipeline: manifold_gpu::GpuComputePipeline,

    // Samplers
    sampler_repeat: manifold_gpu::GpuSampler,
    sampler_clamp: manifold_gpu::GpuSampler,

    // Persistent simulation state (lazy-init)
    color_state: Option<StatefulState>,
    velocity_state: Option<StatefulState>,
    blur_scratch_a: Option<RenderTarget>,
    blur_scratch_b: Option<RenderTarget>,
    blur_scratch_c: Option<RenderTarget>,

    // Resolution tracking
    sim_width: u32,
    sim_height: u32,

    // Independent frame counter (drives noise time, decoupled from transport)
    internal_frame: u64,
}

impl OilyFluidGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let common = include_str!("shaders/particle_common.wgsl");
        let oily = include_str!("shaders/oily_fluid.wgsl");
        let composed = format!("{}\n{}", common, oily);

        let downsample_pipeline =
            device.create_compute_pipeline(&composed, "cs_downsample", "OilyFluid Downsample");
        let velocity_pipeline =
            device.create_compute_pipeline(&composed, "cs_velocity", "OilyFluid Velocity");
        let color_pipeline =
            device.create_compute_pipeline(&composed, "cs_color", "OilyFluid Color");
        let render_pipeline =
            device.create_compute_pipeline(&composed, "cs_render", "OilyFluid Render");

        // Reuse the shared Gaussian blur compute shader — same binary serves
        // both horizontal and vertical passes via its direction uniform.
        let blur_src = include_str!("shaders/gaussian_blur_compute.wgsl");
        let blur_pipeline =
            device.create_compute_pipeline(blur_src, "cs_main", "OilyFluid Blur");

        let sampler_repeat = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_v: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_w: manifold_gpu::GpuAddressMode::Repeat,
            ..Default::default()
        });
        let sampler_clamp = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_v: manifold_gpu::GpuAddressMode::ClampToEdge,
            address_mode_w: manifold_gpu::GpuAddressMode::ClampToEdge,
            ..Default::default()
        });

        Self {
            downsample_pipeline,
            blur_pipeline,
            velocity_pipeline,
            color_pipeline,
            render_pipeline,
            sampler_repeat,
            sampler_clamp,
            color_state: None,
            velocity_state: None,
            blur_scratch_a: None,
            blur_scratch_b: None,
            blur_scratch_c: None,
            sim_width: 0,
            sim_height: 0,
            internal_frame: 0,
        }
    }

    fn ensure_resources(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.color_state.is_some() && self.sim_width == w && self.sim_height == h {
            return;
        }
        self.sim_width = w;
        self.sim_height = h;

        self.color_state = Some(StatefulState::new(
            device,
            w,
            h,
            STATE_FORMAT,
            "OilyFluid Color",
        ));
        self.velocity_state = Some(StatefulState::new(
            device,
            w,
            h,
            STATE_FORMAT,
            "OilyFluid Velocity",
        ));

        let qw = w.div_ceil(BLUR_PRESHRINK).max(1);
        let qh = h.div_ceil(BLUR_PRESHRINK).max(1);

        self.blur_scratch_a = Some(RenderTarget::new(
            device,
            qw,
            qh,
            STATE_FORMAT,
            "OilyFluid Blur Scratch A",
        ));
        self.blur_scratch_b = Some(RenderTarget::new(
            device,
            qw,
            qh,
            STATE_FORMAT,
            "OilyFluid Blur Scratch B",
        ));
        self.blur_scratch_c = Some(RenderTarget::new(
            device,
            qw,
            qh,
            STATE_FORMAT,
            "OilyFluid Blur Scratch C",
        ));
    }
}

impl Generator for OilyFluidGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::OILY_FLUID
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        self.ensure_resources(gpu.device, ctx.width, ctx.height);

        // Advance internal frame counter — decoupled from transport so the
        // sim continues to simmer when playback is stopped.
        self.internal_frame = self.internal_frame.wrapping_add(1);

        // Read params (defaults match the TD reference exactly).
        let speed = param(ctx, SPEED, 1.0);
        let feedback = param(ctx, FEEDBACK, 0.998);
        let noise_injection = param(ctx, NOISE_INJECT, 0.002);
        let vel_damp = param(ctx, VEL_DAMP, 0.98);
        let curl = param(ctx, CURL, 0.2);
        let relief = param(ctx, RELIEF, 0.5);
        let chroma = param(ctx, CHROMA, 2.0);
        let contrast = param(ctx, CONTRAST, 1.4);
        let hue_shift = param(ctx, HUE, 0.0);
        let saturation = param(ctx, SATURATION, 1.0);
        let brightness = param(ctx, BRIGHTNESS, 1.0);
        let vel_disp = param(ctx, VEL_DISP, 1.0);
        let col_disp = param(ctx, COL_DISP, 1.0);
        let mode = param(ctx, MODE, 0.0);

        let fw = ctx.width as f32;
        let fh = ctx.height as f32;

        let color_state = self.color_state.as_ref().unwrap();
        let velocity_state = self.velocity_state.as_ref().unwrap();
        let scratch_a = self.blur_scratch_a.as_ref().unwrap();
        let scratch_b = self.blur_scratch_b.as_ref().unwrap();
        let scratch_c = self.blur_scratch_c.as_ref().unwrap();

        let qw = scratch_a.width;
        let qh = scratch_a.height;
        let inv_qw = 1.0 / qw as f32;
        let inv_qh = 1.0 / qh as f32;

        // ── 1. Downsample velocity(front) → scratch_a ──
        let down_u = DownsampleUniforms {
            src_width: fw,
            src_height: fh,
            dst_width: qw as f32,
            dst_height: qh as f32,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
            _pad4: 0.0,
            _pad5: 0.0,
            _pad6: 0.0,
            _pad7: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.downsample_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&down_u),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: velocity_state.read_texture(),
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler_clamp,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &scratch_a.texture,
                },
            ],
            [qw.div_ceil(16), qh.div_ceil(16), 1],
            "OilyFluid Downsample",
        );

        // ── 2. Horizontal blur: scratch_a → scratch_b ──
        let blur_h = BlurUniforms {
            direction_x: 1.0,
            direction_y: 0.0,
            radius: BLUR_RADIUS,
            texel_x: inv_qw,
            texel_y: inv_qh,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
                    texture: &scratch_a.texture,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler_clamp,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &scratch_b.texture,
                },
            ],
            [qw.div_ceil(16), qh.div_ceil(16), 1],
            "OilyFluid Blur H",
        );

        // ── 3. Vertical blur: scratch_b → scratch_c ──
        let blur_v = BlurUniforms {
            direction_x: 0.0,
            direction_y: 1.0,
            radius: BLUR_RADIUS,
            texel_x: inv_qw,
            texel_y: inv_qh,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
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
                    texture: &scratch_b.texture,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler_clamp,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &scratch_c.texture,
                },
            ],
            [qw.div_ceil(16), qh.div_ceil(16), 1],
            "OilyFluid Blur V",
        );

        // ── 4. Velocity update: write to velocity_state.write() ──
        let vel_u = VelocityUniforms {
            width: fw,
            height: fh,
            grad_attenuation: curl,
            velocity_damping: vel_damp,
            self_advect_scale: 0.5,
            vel_disp,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
            _pad4: 0.0,
            _pad5: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.velocity_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&vel_u),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: color_state.read_texture(),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: velocity_state.read_texture(),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: &scratch_c.texture,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 4,
                    sampler: &self.sampler_repeat,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 5,
                    texture: velocity_state.write_texture(),
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "OilyFluid Velocity",
        );

        // ── 5. Color feedback: write to color_state.write() ──
        // Note: reads velocity_state.write() — the just-updated velocity.
        let noise_time = (self.internal_frame as f32) * 0.01 * speed;
        let col_u = ColorUniforms {
            width: fw,
            height: fh,
            feedback_retention: feedback,
            noise_injection,
            noise_time,
            aspect: ctx.aspect,
            col_disp,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
            _pad4: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.color_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&col_u),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: color_state.read_texture(),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: velocity_state.write_texture(),
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler_repeat,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: color_state.write_texture(),
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "OilyFluid Color",
        );

        // ── 6. Render: write to target ──
        let rnd_u = RenderUniforms {
            width: fw,
            height: fh,
            normal_z_scale: relief,
            chroma,
            contrast,
            hue_shift,
            saturation,
            brightness,
            mode,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.render_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&rnd_u),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: color_state.write_texture(),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: velocity_state.write_texture(),
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler_clamp,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "OilyFluid Render",
        );

        // ── 7. Swap ping-pong states ──
        self.color_state.as_mut().unwrap().swap();
        self.velocity_state.as_mut().unwrap().swap();

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Invalidate resources — `ensure_resources()` rebuilds at next render.
        self.color_state = None;
        self.velocity_state = None;
        self.blur_scratch_a = None;
        self.blur_scratch_b = None;
        self.blur_scratch_c = None;
        self.sim_width = 0;
        self.sim_height = 0;
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.color_state = None;
        self.velocity_state = None;
        self.blur_scratch_a = None;
        self.blur_scratch_b = None;
        self.blur_scratch_c = None;
        self.sim_width = 0;
        self.sim_height = 0;
        self.internal_frame = 0;
    }
}
