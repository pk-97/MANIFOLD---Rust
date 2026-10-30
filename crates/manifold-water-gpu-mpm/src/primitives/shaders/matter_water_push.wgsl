// node.matter_water_push — the water pushes the Matter once a tick, at the
// first substep of each tick. The water is the liquid step's as its last tick
// left it: its pooled faces and distance on its world tiles, read at each
// point (FLIP's MAC trilinear for the velocity, the distance trilinear at
// cell centres for the share under water, 1 half a cell inside the surface,
// 0 half a cell out). A live point under water loses its row's lift share of
// its gravity for its share under water (`water_lift`), then moves toward the
// water's velocity there at its row's drag rate: the rate the water is slowed
// by the sand (`SAND_WATER_DRAG` at a full cell) times water's density over
// the sand's, so the push is equal and opposite. The rows are
// node.matter_material's table, one per slot (affine_z.w), lift and rate at
// floats 10 and 11. Exact over the tick (exponential), so no rate overshoots.
// The point also keeps how wet it is (f_y.w: soaked to its share under water,
// drying by exp(-t / dry) after) and its share under water now (f_z.w), which
// make damp sand sticky (`matter_wet_cohesion`). One thread a point. Composed
// after the world-tile read side, liquid_faces.wgsl, whitewater_common.wgsl
// and matter_material.wgsl.

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

struct FaceSample {
    face_velocity: vec4<f32>,
    face_weight: vec4<f32>,
}

struct Params {
    count: u32,
    // The water's pool and cell, metres.
    pool_tiles: u32,
    cell: f32,
    // Simulated seconds of the tick.
    dt: f32,
    // The domain's gravity, m/s².
    gravity: vec3<f32>,
    // Seconds wet sand takes to dry to 1/e, the wet look's.
    dry: f32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read> faces: array<FaceSample>;
// Metres, negative inside the water; the canonical element is air.
@group(0) @binding(3) var<storage, read> distance: array<f32>;
@group(0) @binding(4) var<storage, read> tiles: array<u32>;
@group(0) @binding(5) var<storage, read> materials: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// The water's velocity and the share under water at grid position q.
fn mw_water(q: vec3<f32>) -> vec4<f32> {
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let face = faces[ts_face(base + vec3<i32>(ww_corner(corner)))];
            sum = sum + ww_corner_weight(f, corner) * lf_face_velocity(face.face_velocity, face.face_weight, axis);
        }
        v[axis] = sum;
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * distance[ts_cell_at(base + vec3<i32>(ww_corner(corner)))];
    }
    return vec4<f32>(v, clamp(0.5 - d / params.cell, 0.0, 1.0));
}

@compute @workgroup_size(256)
fn water_push(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= min(params.count, arrayLength(&points)) || points[i].id == 0u { return; }
    let q = ww_cell_position(points[i].position, params.cell);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    // A point whose cell the water's tiles do not hold is under no water.
    var water = vec4<f32>(0.0);
    if ts_cell_stored(cell) {
        water = mw_water(q);
    }
    let under = water.w;
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
