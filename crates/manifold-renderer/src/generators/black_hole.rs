// Schwarzschild black hole generator.
//
// Two-pass architecture:
//   1. Deflection map bake — traces geodesics, stores ray termination data.
//      Only runs when camera/disk parameters change (amortized cost).
//   2. Display — samples deflection map, applies dynamic disk coloring.
//      Runs every frame (cheap: one texture read per pixel).
//
// Schwarzschild geometry is rotationally symmetric around y, so the
// deflection map is baked at orbit_angle=0 and the display shader
// offsets disk angles by the current orbit rotation.

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

// Parameter indices (must match generator_definition_registry.rs)
const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const STEPS: usize = 3;
const DISK_INNER: usize = 4;
const DISK_OUTER: usize = 5;
const DISK_GLOW: usize = 6;
const SCALE: usize = 7;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

// ── Uniform structs ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DeflectionUniforms {
    aspect: f32,
    cam_dist: f32,
    tilt_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    uv_scale: f32,
    _pad0: f32,
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

    // Deflection map (lazy-init, resolution-dependent)
    deflection_map: Option<manifold_gpu::GpuTexture>,
    defl_w: u32,
    defl_h: u32,

    // Dirty tracking — only rebake when these change
    last_cam_dist: f32,
    last_tilt: f32,
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
            defl_w: 0,
            defl_h: 0,
            last_cam_dist: f32::MIN,
            last_tilt: f32::MIN,
            last_scale: f32::MIN,
            last_steps: f32::MIN,
            last_disk_inner: f32::MIN,
            last_disk_outer: f32::MIN,
        }
    }

    fn ensure_deflection_map(&mut self, device: &manifold_gpu::GpuDevice, w: u32, h: u32) {
        if self.deflection_map.is_some() && self.defl_w == w && self.defl_h == h {
            return;
        }
        self.defl_w = w;
        self.defl_h = h;
        self.deflection_map = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba32Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "BlackHole DeflectionMap",
        }));
        // Force rebake on next render
        self.last_cam_dist = f32::MIN;
    }

    fn needs_rebake(
        &self,
        cam_dist: f32,
        tilt: f32,
        scale: f32,
        steps: f32,
        disk_inner: f32,
        disk_outer: f32,
    ) -> bool {
        const EPS: f32 = 0.001;
        (self.last_cam_dist - cam_dist).abs() > EPS
            || (self.last_tilt - tilt).abs() > EPS
            || (self.last_scale - scale).abs() > EPS
            || (self.last_steps - steps).abs() > 0.5
            || (self.last_disk_inner - disk_inner).abs() > EPS
            || (self.last_disk_outer - disk_outer).abs() > EPS
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
        let steps = param(ctx, STEPS, 200.0).round();
        let disk_inner = param(ctx, DISK_INNER, 3.0);
        let disk_outer = param(ctx, DISK_OUTER, 10.0);
        let disk_glow = param(ctx, DISK_GLOW, 2.0);
        let scale = param(ctx, SCALE, 1.0);

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let tilt_rad = tilt_deg.to_radians();
        let orbit_angle = ctx.time as f32 * speed * 0.3;

        // Ensure deflection map exists at render resolution
        self.ensure_deflection_map(gpu.device, ctx.width, ctx.height);

        // ── Pass 1: Deflection Map (only on param change) ──
        if self.needs_rebake(cam_dist, tilt_rad, uv_scale, steps, disk_inner, disk_outer) {
            let defl_uniforms = DeflectionUniforms {
                aspect: ctx.aspect,
                cam_dist,
                tilt_rad,
                steps,
                disk_inner,
                disk_outer,
                uv_scale,
                _pad0: 0.0,
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
                ],
                [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
                "BlackHole Deflection",
            );

            self.last_cam_dist = cam_dist;
            self.last_tilt = tilt_rad;
            self.last_scale = uv_scale;
            self.last_steps = steps;
            self.last_disk_inner = disk_inner;
            self.last_disk_outer = disk_outer;
        }

        // ── Pass 2: Display (every frame — cheap texture lookup) ──
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
            "BlackHole Display",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, device: &manifold_gpu::GpuDevice, width: u32, height: u32) {
        self.ensure_deflection_map(device, width, height);
    }

    fn internal_resolution_scale(&self) -> f32 {
        0.75
    }
}
