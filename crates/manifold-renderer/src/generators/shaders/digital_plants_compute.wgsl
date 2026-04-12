// Digital Plants — Compute: topology morphing (cylinder ↔ torus).
//
// noise_common.wgsl is prepended via string concatenation at pipeline creation.
// Provides: simplex3d, fbm, hash_u32, random_rotation.
//
// 400x400 grid = 160,000 instances. Each thread processes one instance:
//   UV mapping → noise displacement → cylinder wrap → torus wrap → morph blend.

struct Uniforms {
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
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> instances: array<Instance>;

const GRID_SIZE: u32 = 400u;
const TAU: f32 = 6.283185307;
const PI: f32 = 3.141592654;

fn rotate_x(p: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec3(p.x, c * p.y - s * p.z, s * p.y + c * p.z);
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.instance_count { return; }

    // ── UV mapping: 2D grid → [0, 1]² ──
    let col = idx % GRID_SIZE;
    let row = idx / GRID_SIZE;
    let uv = vec2<f32>(
        (f32(col) + 0.5) / f32(GRID_SIZE),
        (f32(row) + 0.5) / f32(GRID_SIZE),
    );

    // ── Noise displacement ──
    // Primary displacement: 3D simplex FBM evaluated at grid position.
    let sample_pos = vec3<f32>(uv.x * u.noise_scale, uv.y * u.noise_scale, 0.0);
    let raw_disp = fbm(sample_pos);
    let disp = max(raw_disp, 0.0); // clamp negative values

    // Animation layer: secondary noise driven by time, masked by height.
    // Tips (high v) move more than base (low v).
    let anim_sample = vec3<f32>(
        uv.x * u.noise_scale * 2.0,
        uv.y * u.noise_scale * 2.0,
        u.time * u.anim_speed,
    );
    let anim_noise = simplex3d(anim_sample) * 0.3;
    let height_mask = uv.y; // 0 at base, 1 at tip
    let anim_disp = anim_noise * height_mask;

    let total_disp = disp + anim_disp;

    // ── Cylinder topology (morph = 0) ──
    let theta = uv.x * TAU;
    let cos_theta = cos(theta);
    let sin_theta = sin(theta);

    // Vertical position
    let y_cyl = (uv.y - 0.5) * u.height_scale;

    // Taper: radius narrows toward top. Fades out as morph increases.
    let taper_factor = pow(1.0 - uv.y, u.taper) * (1.0 - u.morph);
    let r_cyl = u.base_radius * (0.1 + 0.9 * taper_factor) + total_disp * 0.3;

    let pos_cyl = vec3<f32>(
        r_cyl * cos_theta,
        y_cyl,
        r_cyl * sin_theta,
    );

    // ── Torus topology (morph = 1) ──
    let phi = uv.y * TAU;
    let cos_phi = cos(phi);
    let sin_phi = sin(phi);

    // Petal displacement: high-frequency noise on torus surface normals.
    let petal_sample = vec3<f32>(uv.x * 8.0, uv.y * 8.0, u.time * u.anim_speed * 0.5);
    let petal_noise = fbm(petal_sample);
    let petal_disp = u.petal_amp * petal_noise;

    let r_tube = u.base_radius + total_disp * 0.2 + petal_disp;
    let R = u.torus_radius;

    var pos_tor = vec3<f32>(
        (R + r_tube * cos_phi) * cos_theta,
        r_tube * sin_phi,
        (R + r_tube * cos_phi) * sin_theta,
    );

    // Continuous folding rotation around X-axis (flower animation)
    let fold_angle = u.time * u.rot_speed;
    pos_tor = rotate_x(pos_tor, fold_angle);

    // ── Morph blend ──
    let pos = mix(pos_cyl, pos_tor, u.morph);

    // ── Per-instance rotation ──
    // Deterministic base rotation + subtle animated component
    let base_rot = random_rotation(idx);
    let static_jitter = vec3<f32>(
        (hash_u32(idx * 7u + 3u) - 0.5) * 0.2,
        (hash_u32(idx * 7u + 5u) - 0.5) * 0.2,
        (hash_u32(idx * 7u + 7u) - 0.5) * 0.2,
    );
    let rot = base_rot + static_jitter + vec3<f32>(u.time * u.rot_speed * 0.2, 0.0, 0.0);

    // ── Write output ──
    instances[idx] = Instance(
        vec4<f32>(pos, u.box_scale),
        vec4<f32>(rot, 0.0),
    );
}
