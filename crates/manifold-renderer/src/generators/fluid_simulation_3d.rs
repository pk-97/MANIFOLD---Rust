// Volumetric 3D particle compute fluid simulation.
// Line-by-line translation of FluidSimulation3DGenerator.cs.
//
// 7 passes per frame (steps 1-4 amortized to alternate frames):
//   [alternate frames, forced on snap:]
//   3D Scatter (2 compute) -> 3D Blur Density (3 compute passes: X, Y, Z)
//   -> GradientCurl3D (compute) -> 3D Blur VectorField (3 compute passes: X, Y, Z)
//   [every frame:]
//   Simulate3D (compute) -> ProjectedScatter (2 compute) -> Display (compute)

use super::compute_common::Particle;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;
use std::f32::consts::PI;

use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::FLUID_SIMULATION_3D,
        create: |device| Box::new(FluidSimulation3DGenerator::new(device)),
    }
}

// Parameter indices — must match generator_definition_registry.rs (21 params)
const FLOW: usize = 0;
const FEATHER: usize = 1;
const CURL: usize = 2;
const TURBULENCE: usize = 3;
const SPEED: usize = 4;
const CONTRAST: usize = 5;
#[allow(dead_code)]
const SCALE: usize = 6;
const PARTICLES: usize = 7;
const SNAP: usize = 8;
const SNAP_MODE: usize = 9;
const PARTICLE_SIZE: usize = 10;
const ANTI_CLUMP: usize = 11;
const INJECT_FORCE: usize = 12;
const CONTAINER: usize = 13;
const CTR_SCALE: usize = 14;
const VOL_RES: usize = 15;
const CAM_DIST: usize = 16;
const CAM_DIST_DEFAULT: f32 = 3.0;
const ROT_X: usize = 17;
const ROT_Y: usize = 18;
const ROT_Z: usize = 19;
const FLATTEN: usize = 20;

const MAX_PARTICLES: u32 = 8_000_000;
const BAKE_GROUP_SIZE: u32 = 8;
const PATTERN_COUNT: u32 = 7;
const SNAP_DECAY_RATE: f32 = 12.0;
const INJECT_DURATION_SECS: f32 = 0.5;
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0;
const THREAD_GROUP_SIZE: u32 = 256;
const FORCE_SCALE: f32 = 500.0;

const DENSITY_3D_FORMAT: manifold_gpu::GpuTextureFormat =
    manifold_gpu::GpuTextureFormat::Rgba16Float;
const VECTOR_3D_FORMAT: manifold_gpu::GpuTextureFormat =
    manifold_gpu::GpuTextureFormat::Rgba16Float;
const DISPLAY_DENSITY_FORMAT: manifold_gpu::GpuTextureFormat =
    manifold_gpu::GpuTextureFormat::Rgba16Float;

const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

fn vol_res_from_param(val: f32) -> u32 {
    let idx = (val + 0.5) as u32;
    match idx {
        0 => 64,
        2 => 256,
        _ => 128,
    }
}

fn active_count_from_param(val: f32) -> u32 {
    ((val * 1_000_000.0).round() as i32).clamp(100_000, MAX_PARTICLES as i32) as u32
}

fn compute_camera_vectors_euler(
    rot_x: f32,
    rot_y: f32,
    rot_z: f32,
    cam_dist: f32,
) -> ([f32; 3], [f32; 3], [f32; 3], [f32; 3]) {
    let ax = rot_x * PI;
    let ay = rot_y * PI;
    let az = rot_z * PI;
    let (sin_x, cos_x) = (ax.sin(), ax.cos());
    let (sin_y, cos_y) = (ay.sin(), ay.cos());
    let cam_pos = [
        sin_y * cos_x * cam_dist,
        -sin_x * cam_dist,
        cos_y * cos_x * cam_dist,
    ];
    let cam_fwd = normalize3([-cam_pos[0], -cam_pos[1], -cam_pos[2]]);
    let right_raw = [cos_y, 0.0, -sin_y];
    let up_raw = [sin_y * sin_x, cos_x, cos_y * sin_x];
    let cos_z = az.cos();
    let sin_z = az.sin();
    let cam_right = [
        right_raw[0] * cos_z + up_raw[0] * sin_z,
        right_raw[1] * cos_z + up_raw[1] * sin_z,
        right_raw[2] * cos_z + up_raw[2] * sin_z,
    ];
    let cam_up = [
        -right_raw[0] * sin_z + up_raw[0] * cos_z,
        -right_raw[1] * sin_z + up_raw[1] * cos_z,
        -right_raw[2] * sin_z + up_raw[2] * cos_z,
    ];
    (cam_pos, cam_fwd, cam_right, cam_up)
}

fn compute_cam_fwd_euler(rot_x: f32, rot_y: f32) -> [f32; 3] {
    let ax = rot_x * PI;
    let ay = rot_y * PI;
    normalize3([-(ay.sin() * ax.cos()), ax.sin(), -(ay.cos() * ax.cos())])
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        return [0.0, 1.0, 0.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

// ── Uniform structs ──

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat3DUniforms {
    active_count: u32,
    vol_res: u32,
    vol_depth: u32,
    scaled_energy: u32,
    _pad: [u32; 24],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Resolve3DUniforms {
    vol_res: u32,
    vol_depth: u32,
    _pad0: u32,
    _pad1: u32,
    _pad: [u32; 24],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Blur3DUniforms {
    vol_res: u32,
    axis: u32,
    radius: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientCurl3DUniforms {
    vol_res: u32,
    vol_depth: u32,
    _pad0: u32,
    _pad1: u32,
    curl_strength: f32,
    slope_strength: f32,
    ref_axis_x: f32,
    ref_axis_y: f32,
    ref_axis_z: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Sim3DUniforms {
    active_count: u32,
    frame_count: u32,
    use_vector_field: u32,
    container: u32,
    ctr_scale: f32,
    speed: f32,
    turbulence: f32,
    anti_clump: f32,
    diffusion: f32,
    respawn_rate: f32,
    dense_respawn: f32,
    flatten: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad0: f32,
    inject_index: i32,
    inject_force: f32,
    inject_phase: f32,
    time2: f32,
    dt: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectedUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    ortho: u32,
    scaled_energy: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    cam_pos_x: f32,
    cam_pos_y: f32,
    cam_pos_z: f32,
    _pad3: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad4: f32,
    cam_right_x: f32,
    cam_right_y: f32,
    cam_right_z: f32,
    _pad5: f32,
    cam_up_x: f32,
    cam_up_y: f32,
    cam_up_z: f32,
    _pad6: f32,
    aspect: f32,
    _pad7: f32,
    _pad8: f32,
    _pad9: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedUniforms {
    active_count: u32,
    pattern_type: u32,
    trigger_count: u32,
    _pad0: u32,
    container: u32,
    ctr_scale: f32,
    flatten: f32,
    _pad1: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    uv_scale: f32,
    _pad0: f32,
}

// Compile-time layout assertions
const _: () = assert!(std::mem::size_of::<Splat3DUniforms>() == 112);
const _: () = assert!(std::mem::size_of::<Resolve3DUniforms>() == 112);
const _: () = assert!(std::mem::size_of::<Blur3DUniforms>() == 16);
const _: () = assert!(std::mem::size_of::<GradientCurl3DUniforms>() == 48);
const _: () = assert!(std::mem::size_of::<Sim3DUniforms>() == 96);
const _: () = assert!(std::mem::size_of::<ProjectedUniforms>() == 112);
const _: () = assert!(std::mem::size_of::<SeedUniforms>() == 48);
const _: () = assert!(std::mem::size_of::<DisplayUniforms>() == 16);

pub struct FluidSimulation3DGenerator {
    // Compute pipelines
    splat_3d_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_3d_pipeline: manifold_gpu::GpuComputePipeline,
    blur_scalar_pipeline: manifold_gpu::GpuComputePipeline,
    blur_vector_pipeline: manifold_gpu::GpuComputePipeline,
    gradient_curl_pipeline: manifold_gpu::GpuComputePipeline,
    simulate_pipeline: manifold_gpu::GpuComputePipeline,
    seed_pipeline: manifold_gpu::GpuComputePipeline,
    splat_projected_pipeline: manifold_gpu::GpuComputePipeline,
    resolve_display_pipeline: manifold_gpu::GpuComputePipeline,
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
    blur_sampler: manifold_gpu::GpuSampler,

    // GPU resources (lazy-init)
    particle_buffer: Option<manifold_gpu::GpuBuffer>,
    accum_3d: Option<manifold_gpu::GpuBuffer>,
    display_accum: Option<manifold_gpu::GpuBuffer>,
    density_volume: Option<manifold_gpu::GpuTexture>,
    density_blur_temp: Option<manifold_gpu::GpuTexture>,
    vector_volume: Option<manifold_gpu::GpuTexture>,
    vector_blur_temp: Option<manifold_gpu::GpuTexture>,
    display_density_tex: Option<manifold_gpu::GpuTexture>,

    // State
    active_count: u32,
    vol_res: u32,
    disp_w: u32,
    disp_h: u32,
    frame_count: u64,
    initialized: bool,
    last_trigger_count: u32,
    snap_envelope: f32,
    active_snap_mode: i32,
    inject_zone_index: i32,
    inject_elapsed: f32,
    next_inject_zone: i32,
}

impl FluidSimulation3DGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let splat_3d_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter_3d.wgsl"),
            "splat_3d",
            "FluidSim3D Splat3D",
        );
        let resolve_3d_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter_3d.wgsl"),
            "resolve_3d",
            "FluidSim3D Resolve3D",
        );
        let blur_scalar_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_blur_3d.wgsl"),
            "blur_scalar",
            "FluidSim3D BlurScalar",
        );
        let blur_vector_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_blur_3d.wgsl"),
            "blur_vector",
            "FluidSim3D BlurVector",
        );
        let gradient_curl_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_gradient_curl_3d.wgsl"),
            "main",
            "FluidSim3D GradientCurl3D",
        );
        let simulate_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_simulate_3d.wgsl"),
            "main",
            "FluidSim3D Simulate3D",
        );
        let seed_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_simulate_3d.wgsl"),
            "seed_pattern",
            "FluidSim3D SeedPattern",
        );
        let splat_projected_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter_3d.wgsl"),
            "splat_projected",
            "FluidSim3D SplatProjected",
        );
        let resolve_display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_scatter_3d.wgsl"),
            "resolve_display",
            "FluidSim3D ResolveDisplay",
        );
        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/fluid_display_compute.wgsl"),
            "cs_main",
            "FluidSim3D Display",
        );

        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());
        let blur_sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
            address_mode_u: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_v: manifold_gpu::GpuAddressMode::Repeat,
            address_mode_w: manifold_gpu::GpuAddressMode::Repeat,
            ..Default::default()
        });

        Self {
            splat_3d_pipeline,
            resolve_3d_pipeline,
            blur_scalar_pipeline,
            blur_vector_pipeline,
            gradient_curl_pipeline,
            simulate_pipeline,
            seed_pipeline,
            splat_projected_pipeline,
            resolve_display_pipeline,
            display_pipeline,
            sampler,
            blur_sampler,
            particle_buffer: None,
            accum_3d: None,
            display_accum: None,
            density_volume: None,
            density_blur_temp: None,
            vector_volume: None,
            vector_blur_temp: None,
            display_density_tex: None,
            active_count: 0,
            vol_res: 0,
            disp_w: 0,
            disp_h: 0,
            frame_count: 0,
            initialized: false,
            last_trigger_count: u32::MAX,
            snap_envelope: 0.0,
            active_snap_mode: 0,
            inject_zone_index: -1,
            inject_elapsed: 0.0,
            next_inject_zone: 0,
        }
    }

    fn init_particles_gpu(&mut self, gpu: &mut GpuEncoder) {
        let particle_buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = gpu.device.create_buffer(particle_buf_size);
        self.particle_buffer = Some(particle_buffer);
        self.initialized = true;
        // pattern 255 = random fill, same as 2D
        self.dispatch_seed_pattern(gpu, 255, 42, 0, 0.8, 0.0, [0.0, 0.0, 1.0]);
    }

    fn ensure_volume_resources(&mut self, device: &manifold_gpu::GpuDevice, vol_res: u32) {
        if self.accum_3d.is_some() && self.vol_res == vol_res {
            return;
        }
        self.vol_res = vol_res;

        let accum_3d_size = (vol_res as u64).pow(3) * 4;
        self.accum_3d = Some(device.create_buffer(accum_3d_size));

        let make_vol = |fmt, label| {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: vol_res,
                height: vol_res,
                depth: vol_res,
                format: fmt,
                dimension: manifold_gpu::GpuTextureDimension::D3,
                usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
                label,
                mip_levels: 1,
            })
        };
        self.density_volume = Some(make_vol(DENSITY_3D_FORMAT, "FluidSim3D DensityVolume"));
        self.density_blur_temp = Some(make_vol(DENSITY_3D_FORMAT, "FluidSim3D DensityBlurTemp"));
        self.vector_volume = Some(make_vol(VECTOR_3D_FORMAT, "FluidSim3D VectorVolume"));
        self.vector_blur_temp = Some(make_vol(VECTOR_3D_FORMAT, "FluidSim3D VectorBlurTemp"));
        self.frame_count = 0;
    }

    fn ensure_display_resources(&mut self, device: &manifold_gpu::GpuDevice, dw: u32, dh: u32) {
        if self.display_accum.is_some() && self.disp_w == dw && self.disp_h == dh {
            return;
        }
        self.disp_w = dw;
        self.disp_h = dh;

        let display_accum_size = (dw as u64) * (dh as u64) * 4;
        self.display_accum = Some(device.create_buffer(display_accum_size));
        self.display_density_tex = Some(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: dw,
            height: dh,
            depth: 1,
            format: DISPLAY_DENSITY_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "FluidSim3D DisplayDensity",
            mip_levels: 1,
        }));
    }

    fn dispatch_seed_pattern(
        &self,
        gpu: &mut GpuEncoder,
        pattern: u32,
        trigger_count: u32,
        container: u32,
        ctr_scale: f32,
        flatten: f32,
        cam_fwd: [f32; 3],
    ) {
        if self.particle_buffer.is_none() {
            return;
        }
        let uniforms = SeedUniforms {
            active_count: self.active_count,
            pattern_type: pattern,
            trigger_count,
            _pad0: 0,
            container,
            ctr_scale,
            flatten,
            _pad1: 0.0,
            cam_fwd_x: cam_fwd[0],
            cam_fwd_y: cam_fwd[1],
            cam_fwd_z: cam_fwd[2],
            _pad2: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.seed_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: self.particle_buffer.as_ref().unwrap(),
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [self.active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
            "FluidSim3D SeedPattern",
        );
    }
}

impl Generator for FluidSimulation3DGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::FLUID_SIMULATION_3D
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        let flow = param(ctx, FLOW, -0.01);
        let feather = param(ctx, FEATHER, 20.0);
        let curl = param(ctx, CURL, 85.0);
        let mut turbulence = param(ctx, TURBULENCE, 0.001);
        let speed = param(ctx, SPEED, 1.0);
        let contrast = param(ctx, CONTRAST, 3.5);
        let particles_param = param(ctx, PARTICLES, 2.0);
        let snap = param(ctx, SNAP, 0.0);
        let snap_mode_f = param(ctx, SNAP_MODE, 0.0);
        let particle_size = param(ctx, PARTICLE_SIZE, 3.0);
        let anti_clump = param(ctx, ANTI_CLUMP, 20.0);
        let inject_force_p = param(ctx, INJECT_FORCE, 0.005);
        let container_f = param(ctx, CONTAINER, 0.0);
        let ctr_scale = param(ctx, CTR_SCALE, 0.8);
        let vol_res_param = param(ctx, VOL_RES, 1.0);
        let cam_dist = param(ctx, CAM_DIST, CAM_DIST_DEFAULT);
        let rot_x = param(ctx, ROT_X, 0.0);
        let rot_y = param(ctx, ROT_Y, 0.0);
        let rot_z = param(ctx, ROT_Z, 0.0);
        let flatten = param(ctx, FLATTEN, 0.0);

        // Anti-clump drives diffusion kick (wander replacement)
        let diffusion = (anti_clump / 60.0) * 0.05;

        let container_type = container_f.round() as u32;
        let snap_mode = snap_mode_f.round() as i32;
        let active_count = active_count_from_param(particles_param);
        let desired_vol_res = vol_res_from_param(vol_res_param);
        // Lock display resolution at full size (density_res = 1.0)
        let desired_dw = ctx.width;
        let desired_dh = ctx.height;

        self.active_count = active_count;

        if !self.initialized {
            self.init_particles_gpu(gpu);
        }
        self.ensure_volume_resources(gpu.device, desired_vol_res);
        self.ensure_display_resources(gpu.device, desired_dw, desired_dh);
        let vol_res = self.vol_res;
        let dw = self.disp_w;
        let dh = self.disp_h;

        let cam_fwd_sim = compute_cam_fwd_euler(rot_x, rot_y);
        let snap_active = snap > 0.5;

        if ctx.trigger_count != self.last_trigger_count {
            let should_snap = snap_active && self.last_trigger_count != u32::MAX;
            self.last_trigger_count = ctx.trigger_count;
            if should_snap {
                self.snap_envelope = 1.0;
                self.active_snap_mode = snap_mode.clamp(0, 4);
                if self.active_snap_mode == 3 {
                    self.dispatch_seed_pattern(
                        gpu,
                        ctx.trigger_count % PATTERN_COUNT,
                        ctx.trigger_count,
                        container_type,
                        ctr_scale,
                        flatten,
                        cam_fwd_sim,
                    );
                    // Skip the full pipeline on the seed frame to avoid a
                    // GPU pipeline stall from back-to-back particle buffer
                    // write→read. New positions render next frame.
                    self.frame_count += 1;
                    return ctx.anim_progress;
                } else if self.active_snap_mode == 4 {
                    self.inject_zone_index = self.next_inject_zone;
                    self.inject_elapsed = 0.0;
                    self.next_inject_zone = (self.next_inject_zone + 1) % 4;
                }
            }
        }

        if self.snap_envelope > 0.001 {
            self.snap_envelope *= (-SNAP_DECAY_RATE * ctx.dt).exp();
        } else {
            self.snap_envelope = 0.0;
        }

        if self.snap_envelope > 0.0 && self.active_snap_mode == 0 {
            turbulence *= 1.0 + 9.0 * self.snap_envelope;
        }

        if self.inject_zone_index >= 0 {
            self.inject_elapsed += ctx.dt;
            if self.inject_elapsed >= INJECT_DURATION_SECS {
                self.inject_zone_index = -1;
            }
        }
        let inject_phase = if self.inject_zone_index >= 0 {
            self.inject_elapsed / INJECT_DURATION_SECS
        } else {
            0.0
        };
        let inject_force_val = if self.inject_zone_index >= 0 {
            inject_force_p
        } else {
            0.0
        };

        let mut cur_flow = flow;
        let mut curl_angle = curl;
        if self.snap_envelope > 0.0 {
            match self.active_snap_mode {
                1 => {
                    curl_angle += 180.0 * self.snap_envelope;
                }
                2 => {
                    cur_flow += (-cur_flow - cur_flow) * self.snap_envelope;
                }
                _ => {}
            }
        }

        let angle_rad = curl_angle.to_radians();
        let curl_strength = cur_flow * FORCE_SCALE * angle_rad.sin();
        let slope_strength = cur_flow * FORCE_SCALE * angle_rad.cos();
        let t_ref = ctx.time as f32 * 0.3;
        let ref_axis = normalize3([
            (t_ref * 1.0).sin(),
            (t_ref * 0.7).cos(),
            (t_ref * 0.5).sin(),
        ]);

        let volume_frame_parity = self.frame_count.is_multiple_of(2);
        let update_volume = volume_frame_parity || self.snap_envelope > 0.01;
        let base_blur_radius = feather.round() as i32;
        let res_scale = vol_res as f32 / 640.0;
        let scaled_radius = ((base_blur_radius as f32 * res_scale).round() as i32).max(1);

        let scatter_res_scale = (vol_res as f32 / 128.0) * (vol_res as f32 / 128.0);
        let scatter_3d_energy = 0.005 * (1_000_000.0 / active_count as f32) * scatter_res_scale;
        let scaled_energy_3d = (scatter_3d_energy * 4096.0 + 0.5) as u32;
        let proj_energy = 0.005 * particle_size / 3.0 * (1_000_000.0 / active_count as f32);
        let scaled_energy_proj = (proj_energy * 4096.0 + 0.5) as u32;
        let (cam_pos, cam_fwd, cam_right, cam_up) =
            compute_camera_vectors_euler(rot_x, rot_y, rot_z, cam_dist);
        let ortho: u32 = if container_type == 0 { 1 } else { 0 };

        let particle_buf = self.particle_buffer.as_ref().unwrap();
        let density_vol = self.density_volume.as_ref().unwrap();
        let density_temp = self.density_blur_temp.as_ref().unwrap();
        let vector_vol = self.vector_volume.as_ref().unwrap();
        let vector_temp = self.vector_blur_temp.as_ref().unwrap();

        // STEPS 1-4: Volume pipeline (alternate frames)
        if update_volume {
            let accum_3d = self.accum_3d.as_ref().unwrap();
            let wg3d = vol_res.div_ceil(BAKE_GROUP_SIZE);

            // Splat 3D
            let splat_uni = Splat3DUniforms {
                active_count,
                vol_res,
                vol_depth: vol_res,
                scaled_energy: scaled_energy_3d,
                _pad: [0; 24],
            };
            gpu.native_enc.dispatch_compute(
                &self.splat_3d_pipeline,
                &[
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 0,
                        buffer: particle_buf,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 1,
                        buffer: accum_3d,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 2,
                        data: bytemuck::bytes_of(&splat_uni),
                    },
                ],
                [active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
                "FluidSim3D Splat3D",
            );

            // Resolve 3D
            let resolve_uni = Resolve3DUniforms {
                vol_res,
                vol_depth: vol_res,
                _pad0: 0,
                _pad1: 0,
                _pad: [0; 24],
            };
            gpu.native_enc.dispatch_compute(
                &self.resolve_3d_pipeline,
                &[
                    manifold_gpu::GpuBinding::Buffer {
                        binding: 0,
                        buffer: accum_3d,
                        offset: 0,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: density_vol,
                    },
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 2,
                        data: bytemuck::bytes_of(&resolve_uni),
                    },
                ],
                [wg3d, wg3d, wg3d],
                "FluidSim3D Resolve3D",
            );

            // Blur Density (X, Y, Z)
            let wg_blur = vol_res.div_ceil(4);
            let blur_x = Blur3DUniforms {
                vol_res,
                axis: 0,
                radius: scaled_radius as f32,
                _pad: 0,
            };
            let blur_y = Blur3DUniforms {
                vol_res,
                axis: 1,
                radius: scaled_radius as f32,
                _pad: 0,
            };

            gpu.native_enc.dispatch_compute(
                &self.blur_scalar_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&blur_x),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: density_vol,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.blur_sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: density_temp,
                    },
                ],
                [wg_blur, wg_blur, wg_blur],
                "FluidSim3D BlurScalar X",
            );

            gpu.native_enc.dispatch_compute(
                &self.blur_scalar_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&blur_y),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: density_temp,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.blur_sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: density_vol,
                    },
                ],
                [wg_blur, wg_blur, wg_blur],
                "FluidSim3D BlurScalar Y",
            );

            if vol_res >= 8 {
                let blur_z = Blur3DUniforms {
                    vol_res,
                    axis: 2,
                    radius: scaled_radius as f32,
                    _pad: 0,
                };
                gpu.native_enc.dispatch_compute(
                    &self.blur_scalar_pipeline,
                    &[
                        manifold_gpu::GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&blur_z),
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 1,
                            texture: density_vol,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 2,
                            sampler: &self.blur_sampler,
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 3,
                            texture: density_temp,
                        },
                    ],
                    [wg_blur, wg_blur, wg_blur],
                    "FluidSim3D BlurScalar Z",
                );
            }

            // GradientCurl
            let blurred_density = if vol_res >= 8 {
                density_temp
            } else {
                density_vol
            };
            let gc_uni = GradientCurl3DUniforms {
                vol_res,
                vol_depth: vol_res,
                _pad0: 0,
                _pad1: 0,
                curl_strength,
                slope_strength,
                ref_axis_x: ref_axis[0],
                ref_axis_y: ref_axis[1],
                ref_axis_z: ref_axis[2],
                _pad2: 0.0,
                _pad3: 0.0,
                _pad4: 0.0,
            };
            gpu.native_enc.dispatch_compute(
                &self.gradient_curl_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&gc_uni),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: blurred_density,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 2,
                        texture: vector_vol,
                    },
                ],
                [wg3d, wg3d, wg3d],
                "FluidSim3D GradientCurl3D",
            );

            // Blur Vector Field (X, Y, Z)
            gpu.native_enc.dispatch_compute(
                &self.blur_vector_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&blur_x),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: vector_vol,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.blur_sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: vector_temp,
                    },
                ],
                [wg_blur, wg_blur, wg_blur],
                "FluidSim3D BlurVector X",
            );

            gpu.native_enc.dispatch_compute(
                &self.blur_vector_pipeline,
                &[
                    manifold_gpu::GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&blur_y),
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 1,
                        texture: vector_temp,
                    },
                    manifold_gpu::GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.blur_sampler,
                    },
                    manifold_gpu::GpuBinding::Texture {
                        binding: 3,
                        texture: vector_vol,
                    },
                ],
                [wg_blur, wg_blur, wg_blur],
                "FluidSim3D BlurVector Y",
            );

            if vol_res >= 8 {
                let blur_z = Blur3DUniforms {
                    vol_res,
                    axis: 2,
                    radius: scaled_radius as f32,
                    _pad: 0,
                };
                gpu.native_enc.dispatch_compute(
                    &self.blur_vector_pipeline,
                    &[
                        manifold_gpu::GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&blur_z),
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 1,
                            texture: vector_vol,
                        },
                        manifold_gpu::GpuBinding::Sampler {
                            binding: 2,
                            sampler: &self.blur_sampler,
                        },
                        manifold_gpu::GpuBinding::Texture {
                            binding: 3,
                            texture: vector_temp,
                        },
                    ],
                    [wg_blur, wg_blur, wg_blur],
                    "FluidSim3D BlurVector Z",
                );
            }
        }

        // STEP 5: Simulate
        let vector_field = if vol_res >= 8 {
            vector_temp
        } else {
            vector_vol
        };
        let blurred_density = if vol_res >= 8 {
            density_temp
        } else {
            density_vol
        };

        let sim_uni = Sim3DUniforms {
            active_count,
            frame_count: self.frame_count as u32,
            use_vector_field: 1,
            container: container_type,
            ctr_scale,
            speed,
            turbulence,
            anti_clump,
            diffusion,
            respawn_rate: 0.0,
            dense_respawn: 0.0,
            flatten,
            cam_fwd_x: cam_fwd_sim[0],
            cam_fwd_y: cam_fwd_sim[1],
            cam_fwd_z: cam_fwd_sim[2],
            _pad0: 0.0,
            inject_index: self.inject_zone_index,
            inject_force: inject_force_val,
            inject_phase,
            time2: ctx.time as f32,
            dt: ctx.dt,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.simulate_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: particle_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: vector_field,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: blurred_density,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 4,
                    data: bytemuck::bytes_of(&sim_uni),
                },
            ],
            [active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
            "FluidSim3D Simulate",
        );

        // STEP 6: Projected scatter
        let display_accum = self.display_accum.as_ref().unwrap();
        let display_density = self.display_density_tex.as_ref().unwrap();

        let proj_uni = ProjectedUniforms {
            active_count,
            disp_w: dw,
            disp_h: dh,
            ortho,
            scaled_energy: scaled_energy_proj,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
            cam_pos_x: cam_pos[0],
            cam_pos_y: cam_pos[1],
            cam_pos_z: cam_pos[2],
            _pad3: 0.0,
            cam_fwd_x: cam_fwd[0],
            cam_fwd_y: cam_fwd[1],
            cam_fwd_z: cam_fwd[2],
            _pad4: 0.0,
            cam_right_x: cam_right[0],
            cam_right_y: cam_right[1],
            cam_right_z: cam_right[2],
            _pad5: 0.0,
            cam_up_x: cam_up[0],
            cam_up_y: cam_up[1],
            cam_up_z: cam_up[2],
            _pad6: 0.0,
            aspect: ctx.aspect,
            _pad7: 0.0,
            _pad8: 0.0,
            _pad9: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.splat_projected_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: particle_buf,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Buffer {
                    binding: 1,
                    buffer: display_accum,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&proj_uni),
                },
            ],
            [active_count.div_ceil(THREAD_GROUP_SIZE), 1, 1],
            "FluidSim3D SplatProjected",
        );

        // Resolve Display
        let resolve_disp_uni = Resolve3DUniforms {
            vol_res: dw,
            vol_depth: dh,
            _pad0: 0,
            _pad1: 0,
            _pad: [0; 24],
        };
        gpu.native_enc.dispatch_compute(
            &self.resolve_display_pipeline,
            &[
                manifold_gpu::GpuBinding::Buffer {
                    binding: 0,
                    buffer: display_accum,
                    offset: 0,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: display_density,
                },
                manifold_gpu::GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&resolve_disp_uni),
                },
            ],
            [dw.div_ceil(16), dh.div_ceil(16), 1],
            "FluidSim3D ResolveDisplay",
        );

        // STEP 7: Display
        let area_scale = (dw as f32 * dh as f32) / SCATTER_REFERENCE_AREA;
        let intensity = 3.0 * area_scale;
        let display_uni = DisplayUniforms {
            intensity,
            contrast,
            uv_scale: 1.0,
            _pad0: 0.0,
        };
        gpu.native_enc.dispatch_compute(
            &self.display_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&display_uni),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: display_density,
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
            "FluidSim3D Display",
        );

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        self.display_accum = None;
        self.display_density_tex = None;
        self.disp_w = 0;
        self.disp_h = 0;
    }

    fn internal_resolution_scale(&self) -> f32 {
        1.0
    }

    fn reset_state(&mut self, _device: &manifold_gpu::GpuDevice) {
        self.initialized = false;
        self.frame_count = 0;
        self.particle_buffer = None;
        self.accum_3d = None;
        self.display_accum = None;
        self.density_volume = None;
        self.density_blur_temp = None;
        self.vector_volume = None;
        self.vector_blur_temp = None;
        self.display_density_tex = None;
        self.disp_w = 0;
        self.disp_h = 0;
        self.snap_envelope = 0.0;
        self.inject_elapsed = 0.0;
    }
}
