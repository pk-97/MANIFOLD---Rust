// Digital Plants — Compute: topology morphing (cylinder <-> torus).
//
// noise_common.wgsl is prepended via string concatenation at pipeline creation.
// Provides: simplex3d, fbm, hash_u32, random_rotation.
//
// 400x400 grid = 160,000 instances.  Each thread processes one instance:
//   UV mapping -> noise displacement -> cylinder wrap -> torus wrap -> morph blend.

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

// Mirror-repeat ramp with ease-in/ease-out interpolation.
// Triangle wave 0 -> 1 -> 0 over [0, 1], smoothstepped.
fn mirror_ramp(t: f32) -> f32 {
    let tri = 1.0 - abs(t * 2.0 - 1.0);
    return smoothstep(0.0, 1.0, tri);
}

// ── Cylinder / Stem position ───────────────────────────────────────────
//
// Spec Stage 2 noise overrides: scaled-down period (higher freq), harmonics = 0
// (single simplex, no FBM), exponent = 2.0.

fn compute_cylinder_pos(uv: vec2<f32>) -> vec3<f32> {
    // Stem noise overrides: scaled-down period (higher freq = denser features),
    // single octave (harmonics=0), exponent 2.0.
    let stem_freq = u.noise_scale * 2.0;
    let sample_pos = vec3<f32>(uv.x * stem_freq, uv.y * stem_freq, 0.0);
    let raw_disp = simplex3d(sample_pos);
    let disp = pow(max(raw_disp, 0.0), 2.0);

    // Animation layer: secondary noise driven by time.
    // Masked by displacement height — movement at tips only (spec: "matte").
    let anim_sample = vec3<f32>(
        uv.x * stem_freq * 2.0,
        uv.y * stem_freq * 2.0,
        u.time * u.anim_speed,
    );
    let anim_noise = simplex3d(anim_sample) * 0.3;
    let height_mask = clamp(disp, 0.0, 1.0);
    let anim_disp = anim_noise * height_mask;

    // Mirror-repeat ramp modulation: ease-in/ease-out on uv.y.
    let ramp = mirror_ramp(uv.y);
    let total_disp = (disp + anim_disp) * ramp;

    // Cylinder wrapping: theta = uv.x * 2pi.
    let theta = uv.x * TAU;
    let cos_theta = cos(theta);
    let sin_theta = sin(theta);

    // Taper: convex power function narrows radius toward top (Christmas tree).
    // Disabled in torus mode via (1 - morph).
    let taper_factor = pow(1.0 - uv.y, u.taper) * (1.0 - u.morph);

    // Master noise control: 0.3 global scalar on displacement intensity.
    let r = u.base_radius * taper_factor + total_disp * 0.3;

    // Y position: centered at origin for camera framing.
    let y = (uv.y - 0.5) * u.height_scale;

    var pos = vec3<f32>(
        r * cos_theta,
        y,
        r * sin_theta,
    );

    // High-frequency low-amplitude detail noise for organic texture.
    let detail_freq = 20.0;
    let detail_amp = 0.01;
    let ds = vec3<f32>(uv.x * detail_freq, uv.y * detail_freq, 0.3);
    pos.x += simplex3d(ds) * detail_amp;
    pos.y += simplex3d(ds + vec3(100.0, 0.0, 0.0)) * detail_amp;
    pos.z += simplex3d(ds + vec3(0.0, 100.0, 0.0)) * detail_amp;

    return pos;
}

// ── Torus / Flower position ────────────────────────────────────────────
//
// Spec Stage 3: disable taper/ramp, wrap into torus, apply extreme petal
// displacement along outward normals, continuous fold rotation, micro-movements.

fn compute_torus_pos(uv: vec2<f32>) -> vec3<f32> {
    let theta = uv.x * TAU;
    let phi = uv.y * TAU;
    let cos_theta = cos(theta);
    let sin_theta = sin(theta);
    let cos_phi = cos(phi);
    let sin_phi = sin(phi);

    // Base torus: major radius R, tube radius r.
    let r_tube = u.base_radius;
    let R = u.torus_radius;

    var pos = vec3<f32>(
        (R + r_tube * cos_phi) * cos_theta,
        r_tube * sin_phi,
        (R + r_tube * cos_phi) * sin_theta,
    );

    // Outward normal direction on torus surface.
    let normal_outward = vec3<f32>(
        cos_phi * cos_theta,
        sin_phi,
        cos_phi * sin_theta,
    );

    // Petal displacement: low-frequency FBM, static (no time).
    // Amplitude 60-80 fractures the torus into petal clusters.
    let petal_sample = vec3<f32>(uv.x * 2.0, uv.y * 2.0, 0.5);
    let petal_noise = fbm(petal_sample);
    let petal_disp = u.petal_amp * petal_noise;
    pos += normal_outward * petal_disp;

    // Continuous fold rotation around X-axis (spec: petal animation).
    let fold_angle = u.time * u.rot_speed;
    pos = rotate_x(pos, fold_angle);

    // Micro-movement noise: subtle time-driven organic motion.
    let micro_freq = 3.0;
    let micro_amp = 0.02;
    let ms = vec3<f32>(
        uv.x * micro_freq + u.time * 0.2,
        uv.y * micro_freq,
        u.time * 0.15,
    );
    pos.x += simplex3d(ms) * micro_amp;
    pos.y += simplex3d(ms + vec3(50.0, 0.0, 0.0)) * micro_amp;
    pos.z += simplex3d(ms + vec3(0.0, 50.0, 0.0)) * micro_amp;

    return pos;
}

// ────────────────────────────────────────────────────────────────────────

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.instance_count { return; }

    // UV mapping: 2D grid -> [0, 1]^2.
    let col = idx % GRID_SIZE;
    let row = idx / GRID_SIZE;
    let uv = vec2<f32>(
        (f32(col) + 0.5) / f32(GRID_SIZE),
        (f32(row) + 0.5) / f32(GRID_SIZE),
    );

    // Morph blend between cylinder and torus topologies.
    let pos_cyl = compute_cylinder_pos(uv);
    let pos_tor = compute_torus_pos(uv);
    let pos = mix(pos_cyl, pos_tor, u.morph);

    // Per-instance static rotation jitter +/-0.1 for visual density.
    let rot = vec3<f32>(
        (hash_u32(idx * 3u + 0u) - 0.5) * 0.2,
        (hash_u32(idx * 3u + 1u) - 0.5) * 0.2,
        (hash_u32(idx * 3u + 2u) - 0.5) * 0.2,
    );

    // Box scale: increased with morph so flower petals form solid surfaces.
    let scale = u.box_scale * (1.0 + u.morph * 3.0);

    instances[idx] = Instance(
        vec4<f32>(pos, scale),
        vec4<f32>(rot, 0.0),
    );
}
