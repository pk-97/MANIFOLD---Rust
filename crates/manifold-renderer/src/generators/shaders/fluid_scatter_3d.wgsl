// 3D volumetric scatter + projected 2D scatter for FluidSimulation3D.
// Line-by-line translation of FluidDensityScatter3D.compute.
//
// 4 entry points:
//   splat_3d:        per-particle atomic deposit into 3D accumulator
//   resolve_3d:      resolve 3D accumulator to density volume + self-clear
//   splat_projected: per-particle 3D->2D camera projection into display accumulator
//   resolve_display: resolve display accumulator to 2D density RT (R32Float)

// Particle layout (64 bytes, WGSL vec3 implicit padding):
// position(0-11)|pad(12-15)|velocity(16-27)|life(28)|age(32)|pad(36-47)|color(48-63)
struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    color:    vec4<f32>,
};

// ── Splat 3D ──
// FluidDensityScatter3D.compute SplatKernel3D
// Unity energy: (R/128)^2 * 0.005 * (1e6 / activeCount), scaled by 4096, precomputed on CPU.

struct Splat3DUniforms {
    active_count:   u32,
    vol_res:        u32,
    vol_depth:      u32,
    scaled_energy:  u32,  // precomputed: uint(energy * 4096 + 0.5)
};

@group(0) @binding(0) var<storage, read>       particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> accum_3d: array<atomic<u32>>;
@group(0) @binding(2) var<uniform>             splat_params: Splat3DUniforms;

@compute @workgroup_size(256, 1, 1)
fn splat_3d(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= splat_params.active_count {
        return;
    }

    let p = particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    // Nearest voxel + toroidal wrap (XY use vol_res, Z uses vol_depth)
    let vr = splat_params.vol_res;
    let vd = splat_params.vol_depth;
    let coord = vec3<u32>(
        u32(p.position.x * f32(vr)) % vr,
        u32(p.position.y * f32(vr)) % vr,
        u32(p.position.z * f32(vd)) % vd,
    );
    let idx = coord.z * vr * vr + coord.y * vr + coord.x;
    atomicAdd(&accum_3d[idx], splat_params.scaled_energy);
}

// ── Resolve 3D ──
// Unity ResolveKernel3D: fixed-point -> float density + self-clear

struct Resolve3DUniforms {
    vol_res:   u32,
    vol_depth: u32,
    _pad0:     u32,
    _pad1:     u32,
};

@group(0) @binding(0) var<storage, read_write> resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var density_volume: texture_storage_3d<r32float, write>;
@group(0) @binding(2) var<uniform> resolve_params: Resolve3DUniforms;

@compute @workgroup_size(8, 8, 8)
fn resolve_3d(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = resolve_params.vol_res;
    let vd = resolve_params.vol_depth;
    if id.x >= vr || id.y >= vr || id.z >= vd {
        return;
    }

    let idx = id.z * vr * vr + id.y * vr + id.x;
    let raw_val = atomicLoad(&resolve_accum[idx]);

    // Fixed-point -> float: float(rawVal) / FIXED_POINT_MULTIPLIER
    let density = f32(raw_val) / 4096.0;
    textureStore(density_volume, vec3<i32>(i32(id.x), i32(id.y), i32(id.z)), vec4<f32>(density, 0.0, 0.0, 1.0));

    // Self-clearing: zero for next frame
    atomicStore(&resolve_accum[idx], 0u);
}

// ── Splat Projected (3D -> 2D camera projection) ──
// Translation of SplatProjected3D kernel in FluidDensityScatter3D.compute.
// Camera vectors precomputed on CPU and passed as uniforms (DIFF-1).

struct ProjectedUniforms {
    active_count:  u32,
    disp_w:        u32,
    disp_h:        u32,
    ortho:         u32,   // 0 = perspective (containers), 1 = ortho (no container)
    scaled_energy: u32,   // precomputed: uint(energy * 4096 + 0.5)
    _pad0:         u32,
    _pad1:         u32,
    _pad2:         u32,
    // camera vectors precomputed on CPU (Unity DispatchProjectedScatter)
    cam_pos_x: f32,  cam_pos_y: f32,  cam_pos_z: f32,  _pad3: f32,
    cam_fwd_x: f32,  cam_fwd_y: f32,  cam_fwd_z: f32,  _pad4: f32,
    cam_right_x: f32, cam_right_y: f32, cam_right_z: f32, _pad5: f32,
    cam_up_x: f32,   cam_up_y: f32,   cam_up_z: f32,   _pad6: f32,
    aspect: f32,
    _pad7:  f32,
    _pad8:  f32,
    _pad9:  f32,
};

@group(0) @binding(0) var<storage, read>       proj_particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> display_accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform>             proj_params: ProjectedUniforms;

// Project 3D particle to screen UV — matches ProjectParticle() in Unity scatter shader.
fn project_particle(position: vec3<f32>) -> vec2<f32> {
    let world_pos = position - 0.5;
    let cam_pos   = vec3<f32>(proj_params.cam_pos_x, proj_params.cam_pos_y, proj_params.cam_pos_z);
    let cam_fwd   = vec3<f32>(proj_params.cam_fwd_x, proj_params.cam_fwd_y, proj_params.cam_fwd_z);
    let cam_right = vec3<f32>(proj_params.cam_right_x, proj_params.cam_right_y, proj_params.cam_right_z);
    let cam_up    = vec3<f32>(proj_params.cam_up_x, proj_params.cam_up_y, proj_params.cam_up_z);

    if proj_params.ortho != 0u {
        // Orthographic: frac() wraps toroidally so edges connect seamlessly.
        // Unity line 98-99: frac(dot(worldPos, _CamRight) + 0.5), frac(dot(worldPos, _CamUp) + 0.5)
        return vec2<f32>(
            fract(dot(world_pos, cam_right) + 0.5),
            fract(dot(world_pos, cam_up)    + 0.5),
        );
    } else {
        // Perspective: geometrically correct for containers.
        let rel    = world_pos - cam_pos;
        let view_z = dot(rel, cam_fwd);
        if view_z <= 0.001 {
            return vec2<f32>(-1.0, -1.0);  // behind camera — cull
        }
        return vec2<f32>(
            dot(rel, cam_right) / (view_z * proj_params.aspect) + 0.5,
            dot(rel, cam_up)    / view_z + 0.5,
        );
    }
}

@compute @workgroup_size(256, 1, 1)
fn splat_projected(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= proj_params.active_count {
        return;
    }

    let p = proj_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    let screen_uv = project_particle(p.position.xyz);

    // Ortho never culls (toroidal). Perspective culls out-of-bounds.
    if proj_params.ortho == 0u {
        if screen_uv.x < 0.0 || screen_uv.x >= 1.0 || screen_uv.y < 0.0 || screen_uv.y >= 1.0 {
            return;
        }
    }

    let coord = vec2<u32>(
        min(u32(screen_uv.x * f32(proj_params.disp_w)), proj_params.disp_w - 1u),
        min(u32(screen_uv.y * f32(proj_params.disp_h)), proj_params.disp_h - 1u),
    );
    let idx = coord.y * proj_params.disp_w + coord.x;
    atomicAdd(&display_accum[idx], proj_params.scaled_energy);
}

// ── Resolve Display ──
// Unity ResolveDisplay2D: uint -> float density RT + self-clear.
// Unity uses RFloat, but R32Float is not filterable on Metal for the display fragment shader.
// We use Rgba16Float for the display RT so the fragment shader can filter it (KNOWN_DIVERGENCES).

@group(0) @binding(0) var<storage, read_write> resolve_disp_accum: array<atomic<u32>>;
@group(0) @binding(1) var display_density_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> resolve_disp_params: Resolve3DUniforms;  // reuse vol_res as width, vol_depth as height

@compute @workgroup_size(16, 16, 1)
fn resolve_display(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = resolve_disp_params.vol_res;
    let h = resolve_disp_params.vol_depth;
    if id.x >= w || id.y >= h {
        return;
    }

    let idx = id.y * w + id.x;
    let raw_val = atomicLoad(&resolve_disp_accum[idx]);
    let density = f32(raw_val) / 4096.0;

    textureStore(display_density_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(density, 0.0, 0.0, 1.0));

    // Self-clearing
    atomicStore(&resolve_disp_accum[idx], 0u);
}
