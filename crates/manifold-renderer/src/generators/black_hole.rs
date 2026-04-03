// Schwarzschild black hole generator — cinematic Interstellar-style.
//
// Two-pass compute:
//   1. Deflection map bake — volumetric geodesic trace, only on camera/param change.
//   2. Display — samples deflection map, applies cinematic disk shading.

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const ROTATE: usize = 3;
const STEPS: usize = 4;
const DISK_INNER: usize = 5;
const DISK_OUTER: usize = 6;
const DISK_GLOW: usize = 7;
const SCALE: usize = 8;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DeflectionUniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    rotate_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    uv_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

pub struct BlackHoleGenerator {
    deflection_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,

    deflection_map: Option<manifold_gpu::GpuTexture>,
    deflection_map2: Option<manifold_gpu::GpuTexture>,
    defl_w: u32,
    defl_h: u32,

    last_cam_dist: f32,
    last_tilt: f32,
    last_rotate: f32,
    last_scale: f32,
    last_steps: f32,
    last_disk_inner: f32,
    last_disk_outer: f32,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let deflection_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_deflection.wgsl"),
            "cs_main",
            "BlackHole Deflection",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_display.wgsl"),
            "cs_main",
            "BlackHole Display",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        Self {
            deflection_pipeline,
            display_pipeline,
            sampler,
            deflection_map: None,
            deflection_map2: None,
            defl_w: 0,
            defl_h: 0,
            last_cam_dist: f32::MIN,
            last_tilt: f32::MIN,
            last_rotate: f32::MIN,
            last_scale: f32::MIN,
            last_steps: f32::MIN,
            last_disk_inner: f32::MIN,
            last_disk_outer: f32::MIN,
        }
    }

    fn ensure_deflection_maps(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.deflection_map.is_some() && self.defl_w == w && self.defl_h == h {
            return;
        }
        self.defl_w = w;
        self.defl_h = h;
        let make = |label| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba32Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label,
            })
        };
        self.deflection_map = Some(make("BlackHole Deflection1"));
        self.deflection_map2 = Some(make("BlackHole Deflection2"));
        self.last_cam_dist = f32::MIN;
    }

    fn needs_rebake(&self, cd: f32, t: f32, r: f32, s: f32, st: f32, di: f32, do_: f32) -> bool {
        const EPS: f32 = 0.001;
        (self.last_cam_dist - cd).abs() > EPS
            || (self.last_tilt - t).abs() > EPS
            || (self.last_rotate - r).abs() > EPS
            || (self.last_scale - s).abs() > EPS
            || (self.last_steps - st).abs() > 0.5
            || (self.last_disk_inner - di).abs() > EPS
            || (self.last_disk_outer - do_).abs() > EPS
    }
}

impl Generator for BlackHoleGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::BLACK_HOLE
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        if ctx.param_count == 0 {
            return ctx.anim_progress;
        }

        let speed = param(ctx, SPEED, 0.3);
        let cam_dist = param(ctx, CAM_DIST, 20.0);
        let tilt_deg = param(ctx, TILT, 75.0);
        let rotate_deg = param(ctx, ROTATE, 0.0);
        let steps = param(ctx, STEPS, 200.0).round();
        let disk_inner = param(ctx, DISK_INNER, 3.0);
        let disk_outer = param(ctx, DISK_OUTER, 10.0);
        let disk_glow = param(ctx, DISK_GLOW, 2.0);
        let scale = param(ctx, SCALE, 1.0);

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let tilt_rad = tilt_deg.to_radians();
        let rotate_rad = rotate_deg.to_radians();
        let orbit_angle = ctx.time as f32 * speed * 0.3;

        self.ensure_deflection_maps(gpu.device, ctx.width, ctx.height);

        // ── Pass 1: Deflection Map (only on param change) ──
        if self.needs_rebake(
            cam_dist, tilt_rad, rotate_rad, uv_scale, steps, disk_inner, disk_outer,
        ) {
            let defl_uniforms = DeflectionUniforms {
                aspect: ctx.aspect,
                cam_dist,
                tilt_rad,
                rotate_rad,
                steps,
                disk_inner,
                disk_outer,
                uv_scale,
            };
            gpu.native_enc.dispatch_compute(
                &self.deflection_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&defl_uniforms),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: self.deflection_map.as_ref().unwrap(),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 2,
                        texture: self.deflection_map2.as_ref().unwrap(),
                    },
                ],
                [self.defl_w.div_ceil(16), self.defl_h.div_ceil(16), 1],
                "BlackHole Deflection",
            );
            self.last_cam_dist = cam_dist;
            self.last_tilt = tilt_rad;
            self.last_rotate = rotate_rad;
            self.last_scale = uv_scale;
            self.last_steps = steps;
            self.last_disk_inner = disk_inner;
            self.last_disk_outer = disk_outer;
        }

        // ── Pass 2: Display ──
        let display_uniforms = DisplayUniforms {
            time_val: ctx.time as f32,
            disk_inner,
            disk_outer,
            disk_glow,
            orbit_angle,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.display_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&display_uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: self.deflection_map.as_ref().unwrap(),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 2,
                    texture: self.deflection_map2.as_ref().unwrap(),
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 3,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlackHole Display",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Deflection maps are at 1/4 res — recreated in ensure_deflection_maps
    }

    fn internal_resolution_scale(&self) -> f32 {
        0.75
    }
}
