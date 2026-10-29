// node.matter_state — the water drags the sand: each live point under water
// moves toward the water's velocity there, at the rate the water is slowed
// by the sand (`SAND_WATER_DRAG` at a full cell) times water's density over
// the sand's, so the push is equal and opposite. Exact over the frame's
// simulated time (exponential), so no rate overshoots. One thread a point;
// the push is the water's from the last frame, one vec4 a point in point
// order (xyz the water's velocity, w the share under water).

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    f_x: vec4<f32>,
    f_y: vec4<f32>,
    f_z: vec4<f32>,
    stress_diag: vec4<f32>,
    stress_off: vec4<f32>,
}

struct Params {
    count: u32,
    // Per second at a point wholly under water.
    rate: f32,
    // Simulated seconds this frame.
    dt: f32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read> push: array<vec4<f32>>;

@compute @workgroup_size(256)
fn water_drag(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= min(min(params.count, arrayLength(&points)), arrayLength(&push)) { return; }
    let water = push[i];
    if points[i].id == 0u || !(water.w > 0.0) { return; }
    let keep = exp(-params.rate * water.w * params.dt);
    points[i].velocity = water.xyz + (points[i].velocity - water.xyz) * keep;
}
