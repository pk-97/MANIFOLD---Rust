// node.water_on_sand — the water at each grain of a Sand, from the water's
// display level set and velocity on the display tiles (trilinear at the
// grain): the water's velocity there and how much of the grain is under
// water, one vec4 a grain in the grains' order. Composed after the
// world-tile read side.

struct Params {
    // The grains wired, at most the push's records.
    sand: u32,
    // The display pool and cell size the fields are stored under.
    pool_tiles: u32,
    cell: f32,
    pad: u32,
}

struct Grain {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> sand: array<Grain>;
// Metres, negative inside the water; one value a pool element.
@group(0) @binding(2) var<storage, read> levelset: array<f32>;
// Metres per second, three values a pool element.
@group(0) @binding(3) var<storage, read> velocity: array<f32>;
@group(0) @binding(4) var<storage, read> tiles: array<u32>;
// xyz the water's velocity, w the share of the grain under water.
@group(0) @binding(5) var<storage, read_write> push: array<vec4<f32>>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

@compute @workgroup_size(64)
fn cs_push(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= min(params.sand, arrayLength(&push)) {
        return;
    }
    let grain = sand[i].position_radius;
    // Node p sits at (p - 1/2) cells, the low corner of cell p.
    let g = grain.xyz / params.cell + vec3<f32>(0.5);
    let base = vec3<i32>(floor(g));
    let home = ts_cell_at(base);
    // A removed grain, or one whose own tile the display does not hold, is
    // under no water.
    if !(grain.w > 0.0) || home == ts_canon() {
        push[i] = vec4<f32>(0.0);
        return;
    }
    ts_home(home);
    let f = g - floor(g);
    var phi = 0.0;
    var v = vec3<f32>(0.0);
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, corner >> 2u);
        let e = ts_cell_at(base + vec3<i32>(o));
        let w3 = select(1.0 - f, f, o == vec3<u32>(1u));
        let w = w3.x * w3.y * w3.z;
        // The canonical element reads three blob reaches out and still.
        phi = phi + w * levelset[e];
        v = v + w * vec3<f32>(velocity[3u * e], velocity[3u * e + 1u], velocity[3u * e + 2u]);
    }
    // Wholly under water a half cell inside the surface, dry a half cell out.
    let under = clamp(0.5 - phi / params.cell, 0.0, 1.0);
    push[i] = vec4<f32>(v, under);
}
