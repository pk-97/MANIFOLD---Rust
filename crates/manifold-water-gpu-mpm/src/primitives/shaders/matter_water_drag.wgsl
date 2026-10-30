// node.matter_state — the water lifts and drags the sand: each live point
// under water loses its row's lift share of its gravity for its share under
// water (`water_lift`), then moves toward the water's velocity there, at its
// row's drag rate: the rate the water is slowed by the sand
// (`SAND_WATER_DRAG` at a full cell) times water's density over the sand's,
// so the push is equal and opposite. The rows are node.matter_material's
// table, one per slot (affine_z.w), lift and rate at floats 10 and 11. Exact over the frame's
// simulated time (exponential), so no rate overshoots. The point also keeps
// how wet it is (f_y.w: soaked to its share under water, drying by
// exp(-t / dry) after) and its share under water now (f_z.w), which make
// damp sand sticky (`matter_wet_cohesion`). One thread a point; the push is
// the water's from the last frame, one vec4 a point in point order (xyz the
// water's velocity, w the share under water).

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
    // Simulated seconds this frame.
    dt: f32,
    // Seconds wet sand takes to dry to 1/e, the wet look's.
    dry: f32,
    _pad0: u32,
    // The domain's gravity, m/s².
    gravity: vec3<f32>,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read> push: array<vec4<f32>>;
// Composed after matter_material.wgsl for `matter_slot`.
@group(0) @binding(3) var<storage, read> materials: array<f32>;

@compute @workgroup_size(256)
fn water_drag(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= min(params.count, arrayLength(&points)) || points[i].id == 0u { return; }
    // A point past the push (grains born since) is under no water.
    var water = vec4<f32>(0.0);
    if i < arrayLength(&push) {
        water = push[i];
    }
    let under = clamp(water.w, 0.0, 1.0);
    points[i].f_y.w = max(under, points[i].f_y.w * exp(-params.dt / params.dry));
    points[i].f_z.w = under;
    if under > 0.0 {
        let row = matter_slot(points[i].affine_z.w) * 16u;
        let lift = materials[row + 10u];
        let rate = materials[row + 11u];
        let lifted = points[i].velocity - params.gravity * (lift * under * params.dt);
        let keep = exp(-rate * under * params.dt);
        points[i].velocity = water.xyz + (lifted - water.xyz) * keep;
    }
}
