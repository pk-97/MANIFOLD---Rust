// Volumetric 3D particle compute fluid simulation.
// Line-by-line translation of FluidSimulation3DGenerator.cs.
//
// 7 passes per frame (steps 1-4 amortized to alternate frames):
//   [alternate frames, forced on snap:]
//   3D Scatter (2 compute) -> 3D Blur Density (3 compute passes: X, Y, Z)
//   -> GradientCurl3D (compute) -> 3D Blur VectorField (3 compute passes: X, Y, Z)
//   [every frame:]
//   Simulate3D (compute) -> ProjectedScatter (2 compute) -> Display (fragment)

use std::f32::consts::PI;
use manifold_core::GeneratorType;
use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::render_target::RenderTarget;
use super::compute_common::Particle;

// Parameter indices — must match types.rs param_defs (26 params)
// Unity: SLOPE=0, BLUR=1, ROTATION=2, NOISE=3, SPEED=4, CONTRAST=5, INVERT=6,
//        SCALE=7, PARTICLES=8, SNAP=9, SNAP_MODE=10, SPLAT_SIZE=11, DENSITY_RES=12,
//        DENSITY_NOISE=13, DIFFUSION=14, REFRESH=15, DENSITY_REFRESH=16,
//        COLOR_MODE=17, COLOR_BRIGHT=18, INJECT_FORCE=19,
//        CONTAINER=20, CONTAINER_SCALE=21, VOLUME_RES=22, CAM_DIST=23,
//        ROT_X=24, ROT_Y=25, ROT_Z=26, FLATTEN=27
// DIVERGENCE: Unity has auto-orbit + CAM_TILT; Rust uses manual Rotate X/Y/Z sliders
const FLOW:          usize = 0;   // SLOPE
const FEATHER:       usize = 1;   // BLUR
const CURL:          usize = 2;   // ROTATION
const TURBULENCE:    usize = 3;   // NOISE
const SPEED:         usize = 4;
const CONTRAST:      usize = 5;
const INVERT:        usize = 6;
#[allow(dead_code)]  // Scale handled via camera distance in 3D; defined for param index completeness
const SCALE:         usize = 7;
const PARTICLES:     usize = 8;
const SNAP:          usize = 9;
const SNAP_MODE:     usize = 10;
const PARTICLE_SIZE: usize = 11;  // SPLAT_SIZE
const FIELD_RES:     usize = 12;  // DENSITY_RES
const ANTI_CLUMP:    usize = 13;  // DENSITY_NOISE
const WANDER:        usize = 14;  // DIFFUSION
const RESPAWN:       usize = 15;  // REFRESH
const DENSE_RESPAWN: usize = 16;  // DENSITY_REFRESH
const COLOR:         usize = 17;  // COLOR_MODE
const COLOR_BRIGHT:  usize = 18;
const INJECT_FORCE:  usize = 19;
const CONTAINER:     usize = 20;
const CTR_SCALE:     usize = 21;
const VOL_RES:       usize = 22;
const CAM_DIST:      usize = 23;
const CAM_DIST_DEFAULT: f32 = 3.0;
const ROT_X:         usize = 24;
const ROT_Y:         usize = 25;
const ROT_Z:         usize = 26;
const FLATTEN:       usize = 27;

const MAX_PARTICLES: u32 = 8_000_000;  // Unity: MAX_PARTICLES = 8_000_000 (FM-11)
const BAKE_GROUP_SIZE: u32 = 8;        // Unity: BAKE_GROUP_SIZE = 8
const PATTERN_COUNT: u32 = 7;          // Unity: PATTERN_COUNT = 7

// Unity: SNAP_DECAY_RATE = 12f — exponential decay rate for snap envelope (~200ms to near-zero)
const SNAP_DECAY_RATE: f32 = 12.0;

// Unity: INJECT_FRAMES_PER_ZONE = 120 — ~2 sec at 60fps
const INJECT_FRAMES_PER_ZONE: i32 = 120;

// Unity: SCATTER_REFERENCE_AREA = 1920 * 1080 (from ComputeParticleGeneratorBase)
const SCATTER_REFERENCE_AREA: f32 = 1920.0 * 1080.0;

// Unity: THREAD_GROUP_SIZE = 256 (from ComputeParticleGeneratorBase)
const THREAD_GROUP_SIZE: u32 = 256;

// Unity: FORCE_SCALE = 500f (from DispatchGradientCurl constant)
const FORCE_SCALE: f32 = 500.0;

// Texture formats:
// Unity densityVolume:   RHalf (R16Float). R16Float lacks STORAGE_BINDING on Metal.
//   Use Rgba16Float: supports STORAGE_BINDING + filterable textureSample (matching Unity's
//   bilinear-filtered SampleLevel). Same 16-bit precision as Unity RHalf.
// Unity vectorFieldVolume: ARGBHalf -> Rgba16Float (filterable, storage OK on Metal)
// Unity displayDensityRT:  RFloat -> use Rgba16Float (filterable for display fragment).
const DENSITY_3D_FORMAT:  wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const VECTOR_3D_FORMAT:   wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DISPLAY_DENSITY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

const PARTICLE_SIZE_BYTES: u64 = std::mem::size_of::<Particle>() as u64;

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 { ctx.params[idx] } else { default }
}

// Unity GetVolumeRes: 0->64, 1->128 (default), 2->256
fn vol_res_from_param(val: f32) -> u32 {
    let idx = (val + 0.5) as u32;
    match idx {
        0 => 64,
        2 => 256,
        _ => 128,
    }
}

// Unity GetActiveParticleCount: Clamp(RoundToInt(millions * 1_000_000), 100_000, MAX_PARTICLES)
fn active_count_from_param(val: f32) -> u32 {
    ((val * 1_000_000.0).round() as i32).clamp(100_000, MAX_PARTICLES as i32) as u32
}

// Compute camera vectors from Euler rotation sliders (Rotate X / Y / Z).
// DIVERGENCE: Unity uses auto-orbit (ctx.Time * speed * 0.25) + CAM_TILT.
// Rust uses user-controlled Euler angles for decoupled 3D rotation.
//
// rot_x/rot_y/rot_z are in [-1, 1] mapped to [-π, π].
// Application order: Y (horizontal spin) → X (vertical tilt) → Z (roll).
// This gives intuitive turntable-like control:
//   Rotate Y = turntable (spin object left/right)
//   Rotate X = tilt (nod object up/down)
//   Rotate Z = roll (tilt object sideways)
fn compute_camera_vectors_euler(rot_x: f32, rot_y: f32, rot_z: f32, cam_dist: f32) -> (
    [f32; 3], // cam_pos
    [f32; 3], // cam_fwd
    [f32; 3], // cam_right
    [f32; 3], // cam_up
) {
    let ax = rot_x * PI;  // pitch
    let ay = rot_y * PI;  // yaw
    let az = rot_z * PI;  // roll

    // Camera position on sphere: start at [0, 0, cam_dist], apply Y then X rotation
    let cam_pos = [
        ay.sin() * ax.cos() * cam_dist,
        -ax.sin() * cam_dist,
        ay.cos() * ax.cos() * cam_dist,
    ];

    let cam_fwd = normalize3([-cam_pos[0], -cam_pos[1], -cam_pos[2]]);

    // Derive right/up before roll
    let world_up: [f32; 3] = if cam_fwd[1].abs() > 0.999 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };

    let right_raw = normalize3(cross3(world_up, cam_fwd));
    let up_raw    = cross3(cam_fwd, right_raw);

    // Apply Z roll to right/up vectors
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

// Camera forward for simulation flatten uniform — same as the projection camera forward.
fn compute_cam_fwd_euler(rot_x: f32, rot_y: f32) -> [f32; 3] {
    let ax = rot_x * PI;
    let ay = rot_y * PI;
    normalize3([
        -(ay.sin() * ax.cos()),
        ax.sin(),
        -(ay.cos() * ax.cos()),
    ])
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0]*v[0] + v[1]*v[1] + v[2]*v[2]).sqrt();
    if len < 1e-10 { return [0.0, 1.0, 0.0]; }
    [v[0]/len, v[1]/len, v[2]/len]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1]*b[2] - a[2]*b[1],
        a[2]*b[0] - a[0]*b[2],
        a[0]*b[1] - a[1]*b[0],
    ]
}

// ── Uniform structs ──
// All must be 16-byte aligned (pad to vec4 boundaries).

// Splat3DUniforms — matches fluid_scatter_3d.wgsl Splat3DUniforms
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat3DUniforms {
    active_count:  u32,
    vol_res:       u32,
    vol_depth:     u32,
    scaled_energy: u32,  // precomputed: uint(energy * 4096 + 0.5)
}

// Resolve3DUniforms — matches fluid_scatter_3d.wgsl Resolve3DUniforms
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Resolve3DUniforms {
    vol_res:   u32,
    vol_depth: u32,
    _pad0:     u32,
    _pad1:     u32,
}

// Blur3DUniforms — matches fluid_blur_3d.wgsl BlurUniforms { vol_res, axis, radius: f32, _pad }
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Blur3DUniforms {
    vol_res: u32,
    axis:    u32,    // 0=X, 1=Y, 2=Z
    radius:  f32,    // integer radius passed as f32 (shader casts with i32(radius))
    _pad:    u32,
}

// GradientCurl3DUniforms — matches fluid_gradient_curl_3d.wgsl (after DIFF-14 fix)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientCurl3DUniforms {
    vol_res:        u32,
    vol_depth:      u32,
    _pad0:          u32,
    _pad1:          u32,
    curl_strength:  f32,  // flow * FORCE_SCALE * sin(curl_angle_rad), precomputed C#-side
    slope_strength: f32,  // flow * FORCE_SCALE * cos(curl_angle_rad), precomputed C#-side
    ref_axis_x:     f32,  // normalized rotating ref axis from ctx.Time * 0.3
    ref_axis_y:     f32,
    ref_axis_z:     f32,
    _pad2:          f32,
    _pad3:          f32,
    _pad4:          f32,
}

// Sim3DUniforms — matches fluid_simulate_3d.wgsl SimUniforms (after DIFF-2 fixes)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Sim3DUniforms {
    active_count:     u32,
    frame_count:      u32,
    use_vector_field: u32,
    container:        u32,   // container type (0=none, 1=box, 2=sphere, 3=torus)
    ctr_scale:        f32,
    speed:            f32,
    turbulence:       f32,   // _NoiseAmplitude
    anti_clump:       f32,   // _AntiClump
    wander:           f32,   // _Diffusion
    respawn_rate:     f32,   // _RefreshRate
    dense_respawn:    f32,   // _DensityRefreshScale
    flatten:          f32,
    // camera forward, precomputed C#-side (DIFF-1, DIFF-2)
    cam_fwd_x:        f32,
    cam_fwd_y:        f32,
    cam_fwd_z:        f32,
    // injection uniforms (matching Unity SetSimulationParams)
    color_mode:       u32,
    inject_index:     i32,   // -1 = injection off
    inject_force:     f32,
    inject_phase:     f32,
    time2:            f32,   // ctx.Time (_Time2)
    _pad0:            f32,
    _pad1:            f32,
    _pad2:            f32,
    // 24 bytes above rounded to 96 total (6 * 16)
}

// ProjectedUniforms — matches fluid_scatter_3d.wgsl ProjectedUniforms (camera vectors precomputed)
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectedUniforms {
    active_count:   u32,
    disp_w:         u32,
    disp_h:         u32,
    ortho:          u32,   // 0=perspective, 1=ortho
    scaled_energy:  u32,   // precomputed: uint(energy * 4096 + 0.5)
    _pad0:          u32,
    _pad1:          u32,
    _pad2:          u32,
    // camera vectors from compute_camera_vectors (16-byte rows)
    cam_pos_x:    f32, cam_pos_y:   f32, cam_pos_z:   f32, _pad3: f32,
    cam_fwd_x:    f32, cam_fwd_y:   f32, cam_fwd_z:   f32, _pad4: f32,
    cam_right_x:  f32, cam_right_y: f32, cam_right_z: f32, _pad5: f32,
    cam_up_x:     f32, cam_up_y:    f32, cam_up_z:    f32, _pad6: f32,
    aspect:       f32,
    _pad7:        f32,
    _pad8:        f32,
    _pad9:        f32,
}

// SeedUniforms — matches fluid_simulate_3d.wgsl SeedUniforms
// DIFF-12: add trigger_count field
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedUniforms {
    active_count:  u32,
    pattern_type:  u32,
    trigger_count: u32,   // DIFF-12: triggerCount passed for seed = triggerCount * 7919
    _pad0:         u32,
    container:     u32,
    ctr_scale:     f32,
    flatten:       f32,
    _pad1:         f32,
    cam_fwd_x:     f32,
    cam_fwd_y:     f32,
    cam_fwd_z:     f32,
    _pad2:         f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayUniforms {
    intensity:    f32,
    contrast:     f32,
    invert:       f32,
    uv_scale:     f32,
    color_mode:   f32,
    color_bright: f32,
    _pad0:        f32,
    _pad1:        f32,
}

pub struct FluidSimulation3DGenerator {
    // Compute pipelines — 3D volume
    splat_3d_pipeline:           wgpu::ComputePipeline,
    splat_3d_bgl:                wgpu::BindGroupLayout,
    resolve_3d_pipeline:         wgpu::ComputePipeline,
    resolve_3d_bgl:              wgpu::BindGroupLayout,
    blur_scalar_pipeline:        wgpu::ComputePipeline,
    blur_scalar_bgl:             wgpu::BindGroupLayout,
    blur_vector_pipeline:        wgpu::ComputePipeline,
    blur_vector_bgl:             wgpu::BindGroupLayout,
    gradient_curl_3d_pipeline:   wgpu::ComputePipeline,
    gradient_curl_3d_bgl:        wgpu::BindGroupLayout,

    // Compute pipelines — simulation + projected scatter
    simulate_3d_pipeline:        wgpu::ComputePipeline,
    simulate_3d_bgl:             wgpu::BindGroupLayout,
    seed_pattern_pipeline:       wgpu::ComputePipeline,
    seed_pattern_bgl:            wgpu::BindGroupLayout,
    splat_projected_pipeline:    wgpu::ComputePipeline,
    splat_projected_bgl:         wgpu::BindGroupLayout,
    resolve_display_pipeline:    wgpu::ComputePipeline,
    resolve_display_bgl:         wgpu::BindGroupLayout,

    // Display fragment pipeline
    display_pipeline:            wgpu::RenderPipeline,
    display_bgl:                 wgpu::BindGroupLayout,

    // GPU resources (lazy-init, released on vol_res change)
    particle_buffer:   Option<wgpu::Buffer>,
    accum_3d:          Option<wgpu::Buffer>,
    display_accum:     Option<wgpu::Buffer>,
    density_volume:    Option<Volume3D>,
    density_blur_temp: Option<Volume3D>,
    vector_volume:     Option<Volume3D>,
    vector_blur_temp:  Option<Volume3D>,
    display_density_rt: Option<RenderTarget>,   // display density: Rgba16Float (filterable for display)

    // Uniform buffers
    splat_3d_uniform_buf:           wgpu::Buffer,
    resolve_3d_uniform_buf:         wgpu::Buffer,
    // 6 blur uniform buffers — 3 scalar (X,Y,Z) + 3 vector (X,Y,Z) per volume frame.
    // Each compute dispatch needs its own buffer (queue.write_buffer not flushed until submit).
    blur_3d_uniform_bufs:           [wgpu::Buffer; 6],
    gradient_curl_3d_uniform_buf:   wgpu::Buffer,
    sim_3d_uniform_buf:             wgpu::Buffer,
    seed_uniform_buf:               wgpu::Buffer,
    projected_uniform_buf:          wgpu::Buffer,
    resolve_display_uniform_buf:    wgpu::Buffer,
    display_uniform_buf:            wgpu::Buffer,

    sampler_3d:  wgpu::Sampler,
    sampler_display: wgpu::Sampler,  // for display fragment (linear clamp)

    // State
    active_count:       u32,
    vol_res:            u32,
    disp_w:             u32,
    disp_h:             u32,
    frame_count:        u32,
    initialized:        bool,

    // Snap state machine (mirrors Unity Render / SetSimulationParams)
    last_trigger_count:   u32,   // u32::MAX = initial (sentinel)
    snap_envelope:        f32,
    active_snap_mode:     i32,

    // Color injection state machine (mirrors Unity SetSimulationParams)
    last_color_mode:         i32,
    inject_zone_index:       i32,   // -1 = off
    inject_frames_remaining: i32,
    next_inject_zone:        i32,
}

/// 3D volume texture with view.
struct Volume3D {
    _texture: wgpu::Texture,
    view:     wgpu::TextureView,
    _res:     u32,
}

impl Volume3D {
    fn new(device: &wgpu::Device, res: u32, format: wgpu::TextureFormat, label: &str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: res,
                height: res,
                depth_or_array_layers: res,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { _texture: texture, view, _res: res }
    }
}

impl FluidSimulation3DGenerator {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        // Sampler for 3D textures (linear clamp — matches Unity filterMode=Bilinear, wrapMode=Clamp)
        let sampler_3d = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim3D 3D Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let sampler_display = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("FluidSim3D Display Sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // ── Scatter 3D shader ──
        let scatter_3d_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Scatter3D Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fluid_scatter_3d.wgsl").into()),
        });

        // Splat 3D BGL: particles (ro), accum (rw), uniforms
        let splat_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Splat3D BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let splat_3d_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &splat_3d_bgl, "splat_3d", "FluidSim3D Splat3D");

        // Resolve 3D BGL: accum (rw atomic), density_volume (storage write r32float), uniforms
        let resolve_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Resolve3D BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(1, wgpu::ShaderStages::COMPUTE, DENSITY_3D_FORMAT),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let resolve_3d_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &resolve_3d_bgl, "resolve_3d", "FluidSim3D Resolve3D");

        // Splat projected BGL: particles (ro), display_accum (rw), uniforms
        let splat_projected_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D SplatProjected BGL"),
            entries: &[
                bgl_storage_ro(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_rw(1, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let splat_projected_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &splat_projected_bgl, "splat_projected", "FluidSim3D SplatProjected");

        // Resolve display BGL: display_accum (rw), display_density_out (storage r32float... wait Rgba16Float), uniforms
        // Display density is Rgba16Float (filterable for display fragment shader)
        let resolve_display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D ResolveDisplay BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_2d(1, wgpu::ShaderStages::COMPUTE, DISPLAY_DENSITY_FORMAT),
                bgl_uniform(2, wgpu::ShaderStages::COMPUTE),
            ],
        });
        // The resolve_display entry point in scatter shader writes r32float, but our RT is Rgba16Float.
        // We need a separate resolve shader that writes Rgba16Float. For now we use the same
        // entry point — but the storage format must match. Let's use Rgba16Float storage in the shader.
        // This is consistent with our DISPLAY_DENSITY_FORMAT.
        let resolve_display_pipeline = create_compute_pipeline(device, &scatter_3d_shader, &resolve_display_bgl, "resolve_display", "FluidSim3D ResolveDisplay");

        // ── Blur 3D shader ──
        let blur_3d_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Blur3D Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fluid_blur_3d.wgsl").into()),
        });

        // Blur scalar BGL: uniforms, density in (textureLoad, no filtering needed), density out (storage Rgba16Float)
        let blur_scalar_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D BlurScalar BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d_unfilterable(1, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(2, wgpu::ShaderStages::COMPUTE, DENSITY_3D_FORMAT),
            ],
        });
        let blur_scalar_pipeline = create_compute_pipeline(device, &blur_3d_shader, &blur_scalar_bgl, "blur_scalar", "FluidSim3D BlurScalar");

        // Blur vector BGL: uniforms, vector in (filterable Rgba16Float), vector out (storage Rgba16Float)
        let blur_vector_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D BlurVector BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d(1, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(2, wgpu::ShaderStages::COMPUTE, VECTOR_3D_FORMAT),
            ],
        });
        let blur_vector_pipeline = create_compute_pipeline(device, &blur_3d_shader, &blur_vector_bgl, "blur_vector", "FluidSim3D BlurVector");

        // ── Gradient+Curl 3D pipeline ──
        let gradient_curl_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D GradientCurl3D Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fluid_gradient_curl_3d.wgsl").into()),
        });

        let gradient_curl_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D GradientCurl3D BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::COMPUTE),
                bgl_texture_3d_unfilterable(1, wgpu::ShaderStages::COMPUTE),
                bgl_storage_texture_3d(2, wgpu::ShaderStages::COMPUTE, VECTOR_3D_FORMAT),
            ],
        });
        let gradient_curl_3d_pipeline = create_compute_pipeline(device, &gradient_curl_shader, &gradient_curl_3d_bgl, "main", "FluidSim3D GradientCurl3D");

        // ── Simulate 3D pipeline ──
        let sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Simulate3D Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fluid_simulate_3d.wgsl").into()),
        });

        let simulate_3d_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Simulate3D BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),             // particles rw
                bgl_texture_3d(1, wgpu::ShaderStages::COMPUTE),             // vector field (Rgba16Float, filterable)
                bgl_sampler(2, wgpu::ShaderStages::COMPUTE),                // linear clamp sampler
                bgl_texture_3d(3, wgpu::ShaderStages::COMPUTE),             // density (Rgba16Float, filterable — textureSampleLevel)
                bgl_uniform(4, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let simulate_3d_pipeline = create_compute_pipeline(device, &sim_shader, &simulate_3d_bgl, "main", "FluidSim3D Simulate3D");

        // ── Seed pattern pipeline (in the simulate shader) ──
        let seed_pattern_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D SeedPattern BGL"),
            entries: &[
                bgl_storage_rw(0, wgpu::ShaderStages::COMPUTE),
                bgl_uniform(1, wgpu::ShaderStages::COMPUTE),
            ],
        });
        let seed_pattern_pipeline = create_compute_pipeline(device, &sim_shader, &seed_pattern_bgl, "seed_pattern", "FluidSim3D SeedPattern");

        // ── Display fragment pipeline ──
        let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FluidSim3D Display Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fluid_display.wgsl").into()),
        });

        // fluid_display.wgsl: uniform, density tex (filterable), sampler, color tex, color sampler
        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("FluidSim3D Display BGL"),
            entries: &[
                bgl_uniform(0, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(1, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(2, wgpu::ShaderStages::FRAGMENT),
                bgl_texture_filterable(3, wgpu::ShaderStages::FRAGMENT),
                bgl_sampler(4, wgpu::ShaderStages::FRAGMENT),
            ],
        });

        let display_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("FluidSim3D Display Layout"),
            bind_group_layouts: &[&display_bgl],
            immediate_size: 0,
        });

        let display_pipeline = create_fragment_pipeline(device, &display_shader, &display_layout, target_format, "FluidSim3D Display");

        // ── Uniform buffers ──
        let splat_3d_uniform_buf         = create_uniform_buffer(device, std::mem::size_of::<Splat3DUniforms>(), "FluidSim3D Splat3D Uniforms");
        let resolve_3d_uniform_buf       = create_uniform_buffer(device, std::mem::size_of::<Resolve3DUniforms>(), "FluidSim3D Resolve3D Uniforms");
        let blur_3d_uniform_bufs = std::array::from_fn(|i| {
            create_uniform_buffer(device, std::mem::size_of::<Blur3DUniforms>(), &format!("FluidSim3D Blur3D Uniforms {i}"))
        });
        let gradient_curl_3d_uniform_buf = create_uniform_buffer(device, std::mem::size_of::<GradientCurl3DUniforms>(), "FluidSim3D GradientCurl3D Uniforms");
        let sim_3d_uniform_buf           = create_uniform_buffer(device, std::mem::size_of::<Sim3DUniforms>(), "FluidSim3D Sim3D Uniforms");
        let seed_uniform_buf             = create_uniform_buffer(device, std::mem::size_of::<SeedUniforms>(), "FluidSim3D Seed Uniforms");
        let projected_uniform_buf        = create_uniform_buffer(device, std::mem::size_of::<ProjectedUniforms>(), "FluidSim3D Projected Uniforms");
        let resolve_display_uniform_buf  = create_uniform_buffer(device, std::mem::size_of::<Resolve3DUniforms>(), "FluidSim3D ResolveDisplay Uniforms");
        let display_uniform_buf          = create_uniform_buffer(device, std::mem::size_of::<DisplayUniforms>(), "FluidSim3D Display Uniforms");

        Self {
            splat_3d_pipeline,
            splat_3d_bgl,
            resolve_3d_pipeline,
            resolve_3d_bgl,
            blur_scalar_pipeline,
            blur_scalar_bgl,
            blur_vector_pipeline,
            blur_vector_bgl,
            gradient_curl_3d_pipeline,
            gradient_curl_3d_bgl,
            simulate_3d_pipeline,
            simulate_3d_bgl,
            seed_pattern_pipeline,
            seed_pattern_bgl,
            splat_projected_pipeline,
            splat_projected_bgl,
            resolve_display_pipeline,
            resolve_display_bgl,
            display_pipeline,
            display_bgl,
            particle_buffer:      None,
            accum_3d:             None,
            display_accum:        None,
            density_volume:       None,
            density_blur_temp:    None,
            vector_volume:        None,
            vector_blur_temp:     None,
            display_density_rt:   None,
            splat_3d_uniform_buf,
            resolve_3d_uniform_buf,
            blur_3d_uniform_bufs,
            gradient_curl_3d_uniform_buf,
            sim_3d_uniform_buf,
            seed_uniform_buf,
            projected_uniform_buf,
            resolve_display_uniform_buf,
            display_uniform_buf,
            sampler_3d,
            sampler_display,
            active_count:        0,
            vol_res:             0,
            disp_w:              0,
            disp_h:              0,
            frame_count:         0,
            initialized:         false,
            last_trigger_count:  u32::MAX,  // initial sentinel (Unity: lastTriggerCount = -1)
            snap_envelope:       0.0,
            active_snap_mode:    0,
            last_color_mode:     0,
            inject_zone_index:   -1,
            inject_frames_remaining: 0,
            next_inject_zone:    0,
        }
    }

    /// Unity ComputeParticleGeneratorBase.Initialize: create and seed particle buffer.
    /// Called once; buffer is always MAX_PARTICLES. activeCount is dispatch-only.
    /// Unity NEVER recreates the particle buffer when the count slider changes.
    fn init_particles(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Unity: particleBuffer = new ComputeBuffer(ParticleCount, PARTICLE_STRIDE)
        // ParticleCount = MAX_PARTICLES = 8_000_000 (constant, not slider-driven)
        let particle_buf_size = MAX_PARTICLES as u64 * PARTICLE_SIZE_BYTES;
        let particle_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("FluidSim3D Particle Buffer"),
            size:  particle_buf_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Unity InitParticles: seeded RNG(42), matching Unity lines 201-228
        let mut rng_state: u64 = 42;
        let particle_data: Vec<Particle> = (0..MAX_PARTICLES as usize).map(|_| {
            let px = lcg_next_f32(&mut rng_state);
            let py = lcg_next_f32(&mut rng_state);
            let pz = lcg_next_f32(&mut rng_state);
            Particle {
                position: [px, py, pz],
                _pad0:    0.0,
                velocity: [0.0, 0.0, 0.0],
                life:     1.0,
                age:      -1.0,
                _pad1:    [0.0, 0.0, 0.0],
                color:    [0.005, 0.005, 0.005, 1.0],
            }
        }).collect();
        queue.write_buffer(&particle_buffer, 0, bytemuck::cast_slice(&particle_data));

        self.particle_buffer = Some(particle_buffer);
        self.initialized     = true;
    }

    /// Unity EnsureVolumeResources: recreate 3D volumes + accumulator only when vol_res changes.
    /// Does NOT touch particles or display resources.
    fn ensure_volume_resources(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, vol_res: u32) {
        if self.accum_3d.is_some() && self.vol_res == vol_res {
            return;
        }

        self.vol_res = vol_res;

        // 3D accumulator: vol_res^3 * 4 bytes (uint per voxel)
        let accum_3d_size = (vol_res as u64).pow(3) * 4;
        let accum_3d = device.create_buffer(&wgpu::BufferDescriptor {
            label:  Some("FluidSim3D Accum3D"),
            size:   accum_3d_size,
            usage:  wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&accum_3d, 0, &vec![0u8; accum_3d_size as usize]);

        // 3D volumes: density (R32Float), vector field (Rgba16Float), ping-pong temps
        let density_volume    = Volume3D::new(device, vol_res, DENSITY_3D_FORMAT, "FluidSim3D DensityVolume");
        let density_blur_temp = Volume3D::new(device, vol_res, DENSITY_3D_FORMAT, "FluidSim3D DensityBlurTemp");
        let vector_volume     = Volume3D::new(device, vol_res, VECTOR_3D_FORMAT,  "FluidSim3D VectorVolume");
        let vector_blur_temp  = Volume3D::new(device, vol_res, VECTOR_3D_FORMAT,  "FluidSim3D VectorBlurTemp");

        self.accum_3d          = Some(accum_3d);
        self.density_volume    = Some(density_volume);
        self.density_blur_temp = Some(density_blur_temp);
        self.vector_volume     = Some(vector_volume);
        self.vector_blur_temp  = Some(vector_blur_temp);
        self.frame_count       = 0;
    }

    /// Unity EnsureDisplayResources: recreate 2D display accumulator + density RT only when
    /// display dimensions change (from field_res or output size). Does NOT touch particles or volumes.
    fn ensure_display_resources(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, dw: u32, dh: u32) {
        if self.display_accum.is_some() && self.disp_w == dw && self.disp_h == dh {
            return;
        }

        self.disp_w = dw;
        self.disp_h = dh;

        // 2D display accumulator: dw * dh * 4 bytes
        let display_accum_size = (dw as u64) * (dh as u64) * 4;
        let display_accum = device.create_buffer(&wgpu::BufferDescriptor {
            label:  Some("FluidSim3D DisplayAccum"),
            size:   display_accum_size,
            usage:  wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&display_accum, 0, &vec![0u8; display_accum_size as usize]);

        // Display density RT: Rgba16Float (filterable for display fragment)
        let display_density_rt = RenderTarget::new(device, dw, dh, DISPLAY_DENSITY_FORMAT, "FluidSim3D DisplayDensity");

        self.display_accum      = Some(display_accum);
        self.display_density_rt = Some(display_density_rt);
    }

    // ── 3D blur scalar (R32Float density) ──
    // Translation of Unity BlurScalar3D: X->temp, Y->src, Z->temp (result in temp).
    // Workgroup: (res+7)/8 per axis (BAKE_GROUP_SIZE=8).
    fn dispatch_blur_scalar(
        &self,
        device:    &wgpu::Device,
        queue:     &wgpu::Queue,
        encoder:   &mut wgpu::CommandEncoder,
        radius:    i32,
        src_view:  &wgpu::TextureView,
        dst_view:  &wgpu::TextureView,
        axis:      u32,
        buf_index: usize,
    ) {
        let uniforms = Blur3DUniforms {
            vol_res: self.vol_res,
            axis,
            radius:  radius as f32,
            _pad:    0,
        };
        queue.write_buffer(&self.blur_3d_uniform_bufs[buf_index], 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("FluidSim3D BlurScalar BG"),
            layout:  &self.blur_scalar_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.blur_3d_uniform_bufs[buf_index].as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(dst_view) },
            ],
        });

        // Blur shader uses @workgroup_size(4,4,4); dispatch (res+3)/4 per axis
        let wg = (self.vol_res + 3) / 4;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D BlurScalar"), timestamp_writes: None });
        pass.set_pipeline(&self.blur_scalar_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(wg, wg, wg);
    }

    // ── 3D blur vector (Rgba16Float vector field) ──
    fn dispatch_blur_vector(
        &self,
        device:    &wgpu::Device,
        queue:     &wgpu::Queue,
        encoder:   &mut wgpu::CommandEncoder,
        radius:    i32,
        src_view:  &wgpu::TextureView,
        dst_view:  &wgpu::TextureView,
        axis:      u32,
        buf_index: usize,
    ) {
        let uniforms = Blur3DUniforms {
            vol_res: self.vol_res,
            axis,
            radius:  radius as f32,
            _pad:    0,
        };
        queue.write_buffer(&self.blur_3d_uniform_bufs[buf_index], 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("FluidSim3D BlurVector BG"),
            layout:  &self.blur_vector_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.blur_3d_uniform_bufs[buf_index].as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(dst_view) },
            ],
        });

        // Blur shader uses @workgroup_size(4,4,4); dispatch (res+3)/4 per axis
        let wg = (self.vol_res + 3) / 4;
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D BlurVector"), timestamp_writes: None });
        pass.set_pipeline(&self.blur_vector_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(wg, wg, wg);
    }
}

impl Generator for FluidSimulation3DGenerator {
    fn generator_type(&self) -> GeneratorType {
        GeneratorType::FluidSimulation3D
    }

    fn render(
        &mut self,
        device:  &wgpu::Device,
        queue:   &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target:  &wgpu::TextureView,
        ctx:     &GeneratorContext,
    ) -> f32 {
        // ── Read params (matching Unity Render) ──
        let flow           = param(ctx, FLOW,          -0.01);
        let feather        = param(ctx, FEATHER,        20.0);
        let curl           = param(ctx, CURL,           85.0);
        let mut turbulence = param(ctx, TURBULENCE,    0.001);
        let speed          = param(ctx, SPEED,          1.0);
        let contrast       = param(ctx, CONTRAST,       3.5);
        let invert         = param(ctx, INVERT,         0.0);
        let particles_param = param(ctx, PARTICLES,     2.0);
        let snap           = param(ctx, SNAP,           0.0);
        let snap_mode_f    = param(ctx, SNAP_MODE,      0.0);
        let particle_size  = param(ctx, PARTICLE_SIZE,  3.0);
        let field_res      = param(ctx, FIELD_RES,      0.5);
        let anti_clump     = param(ctx, ANTI_CLUMP,    20.0);
        let wander         = param(ctx, WANDER,        0.01);
        let respawn        = param(ctx, RESPAWN,       0.001);
        let dense_respawn  = param(ctx, DENSE_RESPAWN, 0.05);
        let color_mode_f   = param(ctx, COLOR,          0.0);
        let color_bright   = param(ctx, COLOR_BRIGHT,   2.0);
        let inject_force_p = param(ctx, INJECT_FORCE,  0.005);
        let container_f    = param(ctx, CONTAINER,      0.0);
        let ctr_scale      = param(ctx, CTR_SCALE,      0.8);
        let vol_res_param  = param(ctx, VOL_RES,        1.0);
        let cam_dist       = param(ctx, CAM_DIST,       CAM_DIST_DEFAULT);
        let rot_x          = param(ctx, ROT_X,          0.0);
        let rot_y          = param(ctx, ROT_Y,          0.0);
        let rot_z          = param(ctx, ROT_Z,          0.0);
        let flatten        = param(ctx, FLATTEN,        0.0);

        let container_type  = container_f.round() as u32;
        let color_mode      = color_mode_f.round() as u32;
        let snap_mode       = snap_mode_f.round() as i32;

        // Unity: Clamp(RoundToInt(millions * 1_000_000), 100_000, MAX_PARTICLES)
        let active_count    = active_count_from_param(particles_param);
        let desired_vol_res = vol_res_from_param(vol_res_param);

        // Unity: displayResW = Max(16, RoundToInt(rt.width * fieldRes))
        let desired_dw = ((ctx.width  as f32 * field_res).round() as u32).max(16);
        let desired_dh = ((ctx.height as f32 * field_res).round() as u32).max(16);

        self.active_count = active_count;

        // Unity: particles created once in Initialize(), never recreated for param changes.
        // Buffer is always MAX_PARTICLES; activeCount is dispatch-only.
        if !self.initialized {
            self.init_particles(device, queue);
        }

        // Unity EnsureVolumeResources: only recreate when vol_res changes
        self.ensure_volume_resources(device, queue, desired_vol_res);

        // Unity EnsureDisplayResources: only recreate when display dims change
        self.ensure_display_resources(device, queue, desired_dw, desired_dh);
        let vol_res      = self.vol_res;
        let dw           = self.disp_w;
        let dh           = self.disp_h;

        // ── Camera vectors from Euler rotation sliders ──
        // DIVERGENCE: Unity uses auto-orbit (ctx.Time * speed * 0.25) + CAM_TILT.
        // Rust uses manual Rotate X/Y/Z sliders for user-controlled 3D rotation.
        let cam_fwd_sim = compute_cam_fwd_euler(rot_x, rot_y);

        // ── Snap envelope (Unity Render lines 818-852) ──
        let snap_active = snap > 0.5;

        if ctx.trigger_count != self.last_trigger_count {
            // Unity: bool shouldSnap = snap && lastTriggerCount >= 0  (initial = -1)
            // In Rust: initial sentinel = u32::MAX, so treat MAX as "not yet seen"
            let should_snap = snap_active && self.last_trigger_count != u32::MAX;
            self.last_trigger_count = ctx.trigger_count;

            if should_snap {
                self.snap_envelope    = 1.0;
                self.active_snap_mode = snap_mode.clamp(0, 4);

                if self.active_snap_mode == 3 {
                    // Seed pattern: dispatch SeedPatternKernel
                    self.dispatch_seed_pattern(
                        queue, encoder, device,
                        (snap_mode as u32) % PATTERN_COUNT,
                        ctx.trigger_count,
                        container_type,
                        ctr_scale,
                        flatten,
                        cam_fwd_sim,
                    );
                } else if self.active_snap_mode == 4 {
                    // Color injection trigger (only if color mode active)
                    if color_mode > 0 {
                        self.inject_zone_index      = self.next_inject_zone;
                        self.inject_frames_remaining = INJECT_FRAMES_PER_ZONE;
                        self.next_inject_zone        = (self.next_inject_zone + 1) % 4;
                    }
                }
            }
        }

        // Decay envelope: Exp(-SNAP_DECAY_RATE * deltaTime) (Unity line 849-852)
        if self.snap_envelope > 0.001 {
            self.snap_envelope *= (-SNAP_DECAY_RATE * ctx.dt).exp();
        } else {
            self.snap_envelope = 0.0;
        }

        // ── SetSimulationParams: snap mode 0 = turbulence spike ──
        // Unity line 238-239: if (snapEnvelope > 0f && activeSnapMode == 0) turbulence *= ...
        if self.snap_envelope > 0.0 && self.active_snap_mode == 0 {
            turbulence *= 1.0 + 9.0 * self.snap_envelope;
        }

        // ── Color mode state machine (Unity SetSimulationParams lines 265-290) ──
        let color_mode_i = color_mode as i32;
        if color_mode_i == 0 && self.last_color_mode > 0 {
            self.inject_zone_index       = -1;
            self.inject_frames_remaining = 0;
            self.next_inject_zone        = 0;
        }
        self.last_color_mode = color_mode_i;

        // Advance injection countdown
        if self.inject_zone_index >= 0 {
            self.inject_frames_remaining -= 1;
            if self.inject_frames_remaining <= 0 {
                self.inject_zone_index = -1;
            }
        }

        let inject_phase = if self.inject_zone_index >= 0 {
            1.0 - self.inject_frames_remaining as f32 / INJECT_FRAMES_PER_ZONE as f32
        } else {
            0.0
        };
        let inject_force_val = if self.inject_zone_index >= 0 { inject_force_p } else { 0.0 };

        // ── Snap parameter overrides for GradientCurl (Unity DispatchGradientCurl lines 745-751) ──
        let mut cur_flow = flow;
        let mut curl_angle = curl;
        if self.snap_envelope > 0.0 {
            match self.active_snap_mode {
                1 => { curl_angle += 180.0 * self.snap_envelope; }    // rotation flip
                2 => { cur_flow = cur_flow + (-cur_flow - cur_flow) * self.snap_envelope; } // Lerp(flow, -flow, envelope)
                _ => {}
            }
        }

        // ── GradientCurl precomputed values (Unity DispatchGradientCurl lines 763-766) ──
        let angle_rad      = curl_angle.to_radians();
        let curl_strength  = cur_flow * FORCE_SCALE * angle_rad.sin();
        let slope_strength = cur_flow * FORCE_SCALE * angle_rad.cos();

        // Reference axis: ctx.Time * 0.3 (DIFF-8: NOT raw ctx.time)
        let t_ref = ctx.time * 0.3;
        let ref_axis = normalize3([
            (t_ref * 1.0).sin(),
            (t_ref * 0.7).cos(),
            (t_ref * 0.5).sin(),
        ]);

        // ── Temporal amortization (Unity lines 867-899) ──
        // Toggle parity every frame; force update when snap is active.
        // Unity: volumeFrameParity = !volumeFrameParity; updateVolume = parity || snapEnvelope > 0.01f
        let volume_frame_parity = self.frame_count % 2 == 0;
        let update_volume = volume_frame_parity || self.snap_envelope > 0.01;

        // ── Blur radius (Unity lines 882-885) ──
        // baseBlurRadius = RoundToInt(layer.GetGenParam(BLUR))
        // scaledRadius   = Max(1, RoundToInt(baseBlurRadius * volumeRes / 640.0))
        // DIFF-9: same radius for BOTH density and vector blur (not half for vector)
        let base_blur_radius = feather.round() as i32;
        let res_scale        = vol_res as f32 / 640.0;
        let scaled_radius    = ((base_blur_radius as f32 * res_scale).round() as i32).max(1);

        // ── 3D Scatter energy (DIFF-3) ──
        // Unity Dispatch3DScatter lines 547-549:
        //   resScale = (res/128)^2
        //   energy   = 0.005 * (1_000_000 / activeCount) * resScale
        //   NO particle_size factor for 3D scatter
        let scatter_res_scale  = (vol_res as f32 / 128.0) * (vol_res as f32 / 128.0);
        let scatter_3d_energy  = 0.005 * (1_000_000.0 / active_count as f32) * scatter_res_scale;
        let scaled_energy_3d   = (scatter_3d_energy * 4096.0 + 0.5) as u32;

        // ── Projected scatter energy (Unity DispatchProjectedScatter lines 606-608) ──
        // energy = 0.005 * splatSize / 3.0 * (1_000_000 / activeCount)
        let proj_energy      = 0.005 * particle_size / 3.0 * (1_000_000.0 / active_count as f32);
        let scaled_energy_proj = (proj_energy * 4096.0 + 0.5) as u32;

        // ── Camera vectors for projected scatter ──
        let (cam_pos, cam_fwd, cam_right, cam_up) =
            compute_camera_vectors_euler(rot_x, rot_y, rot_z, cam_dist);

        // Ortho when no container (Unity line 613)
        let ortho: u32 = if container_type == 0 { 1 } else { 0 };

        // ════════════════════════════════════════════════════════════════
        // STEP 1+2+3+4: Volume pipeline (alternate frames + snap force)
        // Unity lines 870-899
        // ════════════════════════════════════════════════════════════════
        if update_volume {
            // ── Pass 1a: Splat 3D ──
            {
                let splat_uniforms = Splat3DUniforms {
                    active_count,
                    vol_res,
                    vol_depth: vol_res,     // QuantizeDepth returns full res
                    scaled_energy: scaled_energy_3d,
                };
                queue.write_buffer(&self.splat_3d_uniform_buf, 0, bytemuck::bytes_of(&splat_uniforms));

                let particle_buffer = self.particle_buffer.as_ref().unwrap();
                let accum_3d = self.accum_3d.as_ref().unwrap();

                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label:   Some("FluidSim3D Splat3D BG"),
                    layout:  &self.splat_3d_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: accum_3d.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.splat_3d_uniform_buf.as_entire_binding() },
                    ],
                });
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D Splat3D"), timestamp_writes: None });
                pass.set_pipeline(&self.splat_3d_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups((active_count + THREAD_GROUP_SIZE - 1) / THREAD_GROUP_SIZE, 1, 1);
            }

            // ── Pass 1b: Resolve 3D ──
            {
                let resolve_uniforms = Resolve3DUniforms {
                    vol_res,
                    vol_depth: vol_res,
                    _pad0: 0,
                    _pad1: 0,
                };
                queue.write_buffer(&self.resolve_3d_uniform_buf, 0, bytemuck::bytes_of(&resolve_uniforms));

                let accum_3d = self.accum_3d.as_ref().unwrap();
                let density_vol = self.density_volume.as_ref().unwrap();

                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label:   Some("FluidSim3D Resolve3D BG"),
                    layout:  &self.resolve_3d_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: accum_3d.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&density_vol.view) },
                        wgpu::BindGroupEntry { binding: 2, resource: self.resolve_3d_uniform_buf.as_entire_binding() },
                    ],
                });
                let wg = (vol_res + BAKE_GROUP_SIZE - 1) / BAKE_GROUP_SIZE;
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D Resolve3D"), timestamp_writes: None });
                pass.set_pipeline(&self.resolve_3d_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(wg, wg, wg);
            }

            // ── Pass 2: 3D Blur Density — separable X, Y, Z ──
            // Unity BlurScalar3D: X: src->temp, Y: temp->src, Z: src->temp (result in temp)
            // (Z skipped if depth < 8, but depth == vol_res so always runs when vol_res >= 8)
            {
                let density_vol  = self.density_volume.as_ref().unwrap();
                let density_temp = self.density_blur_temp.as_ref().unwrap();
                // X: density_volume -> density_blur_temp
                self.dispatch_blur_scalar(device, queue, encoder, scaled_radius, &density_vol.view, &density_temp.view, 0, 0);
            }
            {
                let density_vol  = self.density_volume.as_ref().unwrap();
                let density_temp = self.density_blur_temp.as_ref().unwrap();
                // Y: density_blur_temp -> density_volume
                self.dispatch_blur_scalar(device, queue, encoder, scaled_radius, &density_temp.view, &density_vol.view, 1, 1);
            }
            if vol_res >= 8 {
                let density_vol  = self.density_volume.as_ref().unwrap();
                let density_temp = self.density_blur_temp.as_ref().unwrap();
                // Z: density_volume -> density_blur_temp (result in blur_temp)
                self.dispatch_blur_scalar(device, queue, encoder, scaled_radius, &density_vol.view, &density_temp.view, 2, 2);
                // After Z blur, blurred density is in density_blur_temp (Unity BlurScalar3D returns temp)
            }
            // else: no Z blur, result is in density_volume (copied to temp in Unity via Graphics.CopyTexture)
            // For simplicity here, if vol_res < 8 we use density_volume as blurred source.

            // ── Pass 3: Gradient + Curl ──
            // Blurred density: in density_blur_temp (Z done) or density_volume (no Z)
            {
                let blurred_density_view = if vol_res >= 8 {
                    &self.density_blur_temp.as_ref().unwrap().view
                } else {
                    &self.density_volume.as_ref().unwrap().view
                };
                let vector_vol = self.vector_volume.as_ref().unwrap();

                let gradient_uniforms = GradientCurl3DUniforms {
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
                queue.write_buffer(&self.gradient_curl_3d_uniform_buf, 0, bytemuck::bytes_of(&gradient_uniforms));

                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label:   Some("FluidSim3D GradientCurl3D BG"),
                    layout:  &self.gradient_curl_3d_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: self.gradient_curl_3d_uniform_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(blurred_density_view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&vector_vol.view) },
                    ],
                });
                let wg = (vol_res + BAKE_GROUP_SIZE - 1) / BAKE_GROUP_SIZE;
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D GradientCurl3D"), timestamp_writes: None });
                pass.set_pipeline(&self.gradient_curl_3d_pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(wg, wg, wg);
            }

            // ── Pass 4: 3D Blur Vector Field — separable X, Y, Z ──
            // Unity BlurVector3D: same pattern as BlurScalar3D, same radius (DIFF-9: NOT half)
            {
                let vector_vol  = self.vector_volume.as_ref().unwrap();
                let vector_temp = self.vector_blur_temp.as_ref().unwrap();
                // X: vector_volume -> vector_blur_temp
                self.dispatch_blur_vector(device, queue, encoder, scaled_radius, &vector_vol.view, &vector_temp.view, 0, 3);
            }
            {
                let vector_vol  = self.vector_volume.as_ref().unwrap();
                let vector_temp = self.vector_blur_temp.as_ref().unwrap();
                // Y: vector_blur_temp -> vector_volume
                self.dispatch_blur_vector(device, queue, encoder, scaled_radius, &vector_temp.view, &vector_vol.view, 1, 4);
            }
            if vol_res >= 8 {
                let vector_vol  = self.vector_volume.as_ref().unwrap();
                let vector_temp = self.vector_blur_temp.as_ref().unwrap();
                // Z: vector_volume -> vector_blur_temp (result in blur_temp)
                self.dispatch_blur_vector(device, queue, encoder, scaled_radius, &vector_vol.view, &vector_temp.view, 2, 5);
            }
        }

        // ════════════════════════════════════════════════════════════════
        // STEP 5: Simulate — particles sample blurred vector field + integrate
        // Unity lines 906-926
        // ════════════════════════════════════════════════════════════════
        {
            let sim_uniforms = Sim3DUniforms {
                active_count,
                frame_count: self.frame_count,
                use_vector_field: 1,   // always 1 when volume pipeline active
                container: container_type,
                ctr_scale,
                speed,
                turbulence,
                anti_clump,
                wander,
                respawn_rate:  respawn,
                dense_respawn,
                flatten,
                cam_fwd_x: cam_fwd_sim[0],
                cam_fwd_y: cam_fwd_sim[1],
                cam_fwd_z: cam_fwd_sim[2],
                color_mode,
                inject_index: self.inject_zone_index,
                inject_force: inject_force_val,
                inject_phase,
                time2: ctx.time,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            };
            queue.write_buffer(&self.sim_3d_uniform_buf, 0, bytemuck::bytes_of(&sim_uniforms));

            // Vector field: from vector_blur_temp (result after Z blur, or vector_volume if no Z)
            // Density: from density_blur_temp (result after Z blur, or density_volume if no Z)
            let vector_field_view = if vol_res >= 8 {
                &self.vector_blur_temp.as_ref().unwrap().view
            } else {
                &self.vector_volume.as_ref().unwrap().view
            };
            let density_view = if vol_res >= 8 {
                &self.density_blur_temp.as_ref().unwrap().view
            } else {
                &self.density_volume.as_ref().unwrap().view
            };

            let particle_buffer = self.particle_buffer.as_ref().unwrap();

            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label:   Some("FluidSim3D Simulate3D BG"),
                layout:  &self.simulate_3d_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(vector_field_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler_3d) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(density_view) },
                    wgpu::BindGroupEntry { binding: 4, resource: self.sim_3d_uniform_buf.as_entire_binding() },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D Simulate3D"), timestamp_writes: None });
            pass.set_pipeline(&self.simulate_3d_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((active_count + THREAD_GROUP_SIZE - 1) / THREAD_GROUP_SIZE, 1, 1);
        }

        // ════════════════════════════════════════════════════════════════
        // STEP 6: Projected scatter — 3D particles -> 2D screen density
        // Unity lines 936-940 (DispatchProjectedScatter)
        // ════════════════════════════════════════════════════════════════

        // ── Pass 6a: Splat Projected ──
        {
            let proj_uniforms = ProjectedUniforms {
                active_count,
                disp_w: dw,
                disp_h: dh,
                ortho,
                scaled_energy: scaled_energy_proj,
                _pad0: 0,
                _pad1: 0,
                _pad2: 0,
                cam_pos_x: cam_pos[0], cam_pos_y: cam_pos[1], cam_pos_z: cam_pos[2], _pad3: 0.0,
                cam_fwd_x: cam_fwd[0], cam_fwd_y: cam_fwd[1], cam_fwd_z: cam_fwd[2], _pad4: 0.0,
                cam_right_x: cam_right[0], cam_right_y: cam_right[1], cam_right_z: cam_right[2], _pad5: 0.0,
                cam_up_x: cam_up[0], cam_up_y: cam_up[1], cam_up_z: cam_up[2], _pad6: 0.0,
                aspect: ctx.aspect,
                _pad7: 0.0,
                _pad8: 0.0,
                _pad9: 0.0,
            };
            queue.write_buffer(&self.projected_uniform_buf, 0, bytemuck::bytes_of(&proj_uniforms));

            let particle_buffer = self.particle_buffer.as_ref().unwrap();
            let display_accum   = self.display_accum.as_ref().unwrap();

            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label:   Some("FluidSim3D SplatProjected BG"),
                layout:  &self.splat_projected_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: particle_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: display_accum.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.projected_uniform_buf.as_entire_binding() },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D SplatProjected"), timestamp_writes: None });
            pass.set_pipeline(&self.splat_projected_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((active_count + THREAD_GROUP_SIZE - 1) / THREAD_GROUP_SIZE, 1, 1);
        }

        // ── Pass 6b: Resolve Display ──
        // Re-use Resolve3DUniforms with vol_res=dw, vol_depth=dh for the display resolve
        {
            let resolve_disp_uniforms = Resolve3DUniforms {
                vol_res: dw,
                vol_depth: dh,
                _pad0: 0,
                _pad1: 0,
            };
            queue.write_buffer(&self.resolve_display_uniform_buf, 0, bytemuck::bytes_of(&resolve_disp_uniforms));

            let display_accum      = self.display_accum.as_ref().unwrap();
            let display_density_rt = self.display_density_rt.as_ref().unwrap();

            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label:   Some("FluidSim3D ResolveDisplay BG"),
                layout:  &self.resolve_display_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: display_accum.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&display_density_rt.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: self.resolve_display_uniform_buf.as_entire_binding() },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D ResolveDisplay"), timestamp_writes: None });
            pass.set_pipeline(&self.resolve_display_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            // Unity: resolveGroupsX = CeilToInt(w/16f), resolveGroupsY = CeilToInt(h/16f)
            pass.dispatch_workgroups((dw + 15) / 16, (dh + 15) / 16, 1);
        }

        // ════════════════════════════════════════════════════════════════
        // STEP 7: Display — FluidParticleDisplay tone mapping
        // Unity lines 959-980
        // Note: NO 2D blur of projected density (DIFF-26: Unity doesn't do this)
        // ════════════════════════════════════════════════════════════════
        {
            // Unity: areaScale = (displayResW * displayResH) / SCATTER_REFERENCE_AREA
            //        intensity  = 3f * areaScale
            let area_scale = (dw as f32 * dh as f32) / SCATTER_REFERENCE_AREA;
            let intensity  = 3.0 * area_scale;

            let display_uniforms = DisplayUniforms {
                intensity,
                contrast,
                invert,
                uv_scale: 1.0,  // 3D fluid sim: no UV scaling (1.0 = identity)
                color_mode: color_mode as f32,
                color_bright,
                _pad0: 0.0,
                _pad1: 0.0,
            };
            queue.write_buffer(&self.display_uniform_buf, 0, bytemuck::bytes_of(&display_uniforms));

            let display_density_rt = self.display_density_rt.as_ref().unwrap();

            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label:   Some("FluidSim3D Display BG"),
                layout:  &self.display_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.display_uniform_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&display_density_rt.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler_display) },
                    // Dummy color texture/sampler (FluidSim3D handles color mode 0 only here)
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&display_density_rt.view) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sampler_display) },
                ],
            });

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FluidSim3D Display Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           target,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes:         None,
                occlusion_query_set:      None,
                multiview_mask:           None,
            });
            pass.set_pipeline(&self.display_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..3, 0..1);
        }

        self.frame_count += 1;
        ctx.anim_progress
    }

    fn resize(&mut self, _device: &wgpu::Device, _width: u32, _height: u32) {
        // Invalidate display resources (output dimensions changed) but keep particles alive.
        // Volume resources are also invalidated since display depends on output size.
        // Unity: Resize only releases display RTs, not particle buffer.
        self.display_accum      = None;
        self.display_density_rt = None;
        self.disp_w = 0;
        self.disp_h = 0;
    }
}

impl FluidSimulation3DGenerator {
    // ── SeedPatternKernel dispatch (Unity SeedParticlePattern) ──
    // Unity lines 1052-1071
    fn dispatch_seed_pattern(
        &self,
        queue:          &wgpu::Queue,
        encoder:        &mut wgpu::CommandEncoder,
        device:         &wgpu::Device,
        pattern:        u32,
        trigger_count:  u32,
        container:      u32,
        ctr_scale:      f32,
        flatten:        f32,
        cam_fwd:        [f32; 3],
    ) {
        if self.particle_buffer.is_none() { return; }

        let uniforms = SeedUniforms {
            active_count:  self.active_count,
            pattern_type:  pattern,
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
        queue.write_buffer(&self.seed_uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:   Some("FluidSim3D SeedPattern BG"),
            layout:  &self.seed_pattern_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.particle_buffer.as_ref().unwrap().as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.seed_uniform_buf.as_entire_binding() },
            ],
        });

        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("FluidSim3D SeedPattern"), timestamp_writes: None });
        pass.set_pipeline(&self.seed_pattern_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups((self.active_count + THREAD_GROUP_SIZE - 1) / THREAD_GROUP_SIZE, 1, 1);
    }
}

// ── Simple LCG matching C# System.Random(42) (approximate) ──
// C# System.Random uses a subtractive generator. We use a simple LCG for particle init
// which is close enough (positions are random, exact values don't matter for init).
fn lcg_next_f32(state: &mut u64) -> f32 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let hi = (*state >> 33) as u32;
    hi as f32 / 4294967296.0
}

// ── Helper: bind group layout entries ──

fn bgl_uniform(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn bgl_storage_rw(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn bgl_storage_ro(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn bgl_texture_3d(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false },
        count: None,
    }
}

fn bgl_texture_3d_unfilterable(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: false }, view_dimension: wgpu::TextureViewDimension::D3, multisampled: false },
        count: None,
    }
}

fn bgl_texture_filterable(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

fn bgl_sampler(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn bgl_storage_texture_3d(binding: u32, visibility: wgpu::ShaderStages, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format, view_dimension: wgpu::TextureViewDimension::D3 },
        count: None,
    }
}

fn bgl_storage_texture_2d(binding: u32, visibility: wgpu::ShaderStages, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding, visibility,
        ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format, view_dimension: wgpu::TextureViewDimension::D2 },
        count: None,
    }
}

fn create_uniform_buffer(device: &wgpu::Device, size: usize, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size:  size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_compute_pipeline(
    device:      &wgpu::Device,
    shader:      &wgpu::ShaderModule,
    bgl:         &wgpu::BindGroupLayout,
    entry_point: &str,
    label:       &str,
) -> wgpu::ComputePipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(&format!("{label} Layout")),
        bind_group_layouts: &[bgl],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        module: shader,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

fn create_fragment_pipeline(
    device:        &wgpu::Device,
    shader:        &wgpu::ShaderModule,
    layout:        &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    label:         &str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label:  Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format:     target_format,
                blend:      None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
        depth_stencil:  None,
        multisample:    wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache:          None,
    })
}
