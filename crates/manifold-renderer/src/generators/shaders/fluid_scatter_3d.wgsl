// 3D volumetric scatter + projected 2D scatter for FluidSimulation3D.
// 4 entry points:
//   splat_3d: per-particle atomic deposit into 3D accumulator
//   resolve_3d: resolve 3D accumulator to density volume + self-clear
//   splat_projected: per-particle 3D->2D camera projection into display accumulator
//   resolve_display: resolve display accumulator to 2D density RT

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

// ── Splat 3D ──

struct Splat3DUniforms {
    active_count: u32,
    vol_res: u32,
    base_energy: f32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> accum_3d: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Splat3DUniforms;

@compute @workgroup_size(256, 1, 1)
fn splat_3d(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.active_count {
        return;
    }

    let p = particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    let vr = params.vol_res;
    let coord = vec3<u32>(
        u32(fract(p.position.x + 1.0) * f32(vr)) % vr,
        u32(fract(p.position.y + 1.0) * f32(vr)) % vr,
        u32(fract(p.position.z + 1.0) * f32(vr)) % vr,
    );
    let idx = coord.z * vr * vr + coord.y * vr + coord.x;

    let res_factor = (f32(vr) / 128.0) * (f32(vr) / 128.0);
    let energy = u32(params.base_energy * res_factor * 4096.0 + 0.5);
    atomicAdd(&accum_3d[idx], energy);
}

// ── Resolve 3D ──

struct Resolve3DUniforms {
    vol_res: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read_write> resolve_accum: array<atomic<u32>>;
@group(0) @binding(1) var density_volume: texture_storage_3d<r32float, write>;
@group(0) @binding(2) var<uniform> resolve_params: Resolve3DUniforms;

@compute @workgroup_size(4, 4, 4)
fn resolve_3d(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = resolve_params.vol_res;
    if id.x >= vr || id.y >= vr || id.z >= vr {
        return;
    }

    let idx = id.z * vr * vr + id.y * vr + id.x;
    let val = atomicLoad(&resolve_accum[idx]);
    let density = f32(val) / 4096.0;

    textureStore(density_volume, vec3<i32>(i32(id.x), i32(id.y), i32(id.z)), vec4<f32>(density, 0.0, 0.0, 1.0));
    atomicStore(&resolve_accum[idx], 0u);
}

// ── Splat Projected (3D -> 2D camera projection) ──

struct ProjectedUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    container: f32,
    cam_dist: f32,
    cam_tilt: f32,
    time_speed: f32,
    aspect: f32,
    base_energy: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> proj_particles: array<Particle>;
@group(0) @binding(1) var<storage, read_write> display_accum: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> proj_params: ProjectedUniforms;

@compute @workgroup_size(256, 1, 1)
fn splat_projected(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= proj_params.active_count {
        return;
    }

    let p = proj_particles[id.x];
    if p.life <= 0.0 {
        return;
    }

    let angle = proj_params.time_speed;
    let tilt = proj_params.cam_tilt;
    let cam_dist = proj_params.cam_dist;

    let cam_pos = vec3<f32>(
        cos(angle) * cam_dist * cos(tilt),
        sin(tilt) * cam_dist,
        sin(angle) * cam_dist * cos(tilt),
    );
    let cam_fwd = normalize(-cam_pos);
    // Avoid degenerate cross when cam_fwd is near (0,1,0)
    let world_up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(dot(cam_fwd, vec3<f32>(0.0, 1.0, 0.0))) > 0.99);
    let cam_right = normalize(cross(world_up, cam_fwd));
    let cam_up = cross(cam_fwd, cam_right);

    var screen_uv: vec2<f32>;

    if proj_params.container > 0.5 {
        // Perspective projection
        let world_pos = p.position - 0.5;
        let rel = world_pos - cam_pos;
        let view_z = dot(rel, cam_fwd);
        if view_z <= 0.001 {
            return;
        }
        screen_uv = vec2<f32>(
            dot(rel, cam_right) / (view_z * proj_params.aspect) + 0.5,
            dot(rel, cam_up) / view_z + 0.5,
        );
    } else {
        // Orthographic projection
        let world_pos = p.position - 0.5;
        screen_uv = vec2<f32>(
            dot(world_pos, cam_right) + 0.5,
            dot(world_pos, cam_up) + 0.5,
        );
    }

    if screen_uv.x < 0.0 || screen_uv.x >= 1.0 || screen_uv.y < 0.0 || screen_uv.y >= 1.0 {
        return;
    }

    let coord = vec2<u32>(
        u32(screen_uv.x * f32(proj_params.disp_w)) % proj_params.disp_w,
        u32(screen_uv.y * f32(proj_params.disp_h)) % proj_params.disp_h,
    );
    let idx = coord.y * proj_params.disp_w + coord.x;

    let energy = u32(proj_params.base_energy * 4096.0 + 0.5);
    atomicAdd(&display_accum[idx], energy);
}

// ── Resolve Display ──

struct ResolveDisplayUniforms {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> resolve_disp_accum: array<atomic<u32>>;
@group(0) @binding(1) var display_density_out: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> resolve_disp_params: ResolveDisplayUniforms;

@compute @workgroup_size(16, 16, 1)
fn resolve_display(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= resolve_disp_params.width || id.y >= resolve_disp_params.height {
        return;
    }

    let idx = id.y * resolve_disp_params.width + id.x;
    let val = atomicLoad(&resolve_disp_accum[idx]);
    let density = f32(val) / 4096.0;

    textureStore(display_density_out, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(density, 0.0, 0.0, 1.0));
    atomicStore(&resolve_disp_accum[idx], 0u);
}
