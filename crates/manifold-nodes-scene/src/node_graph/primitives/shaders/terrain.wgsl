// node.terrain — a height map of noise carved by simulated rain, laid out as
// the grid of mesh vertices that draws it.
//
// `base_main` writes the noise height of every cell, in units of Height
// (about [-1, 1]). `erode_main` runs raindrops over base + erosion: each drop
// rolls downhill, picks up soil where it speeds up and lays it down where it
// slows or climbs (Hans Beyer, "Implementation of a method for hydraulic
// erosion", 2015, as Sebastian Lague's Coding Adventure tuned it, 2019). The
// soil it moves is summed into `erosion` with integer atomics, so drops on
// every thread write at once. Drops running at once can all fill the same
// hollow, so after each batch the loose soil slides (`snapshot_main`, then
// `slide_main`): soil left steeper than its angle of repose moves down to
// the neighbours below it. `fade_main` scales the erosion when the land under
// it changes. `vertices_main` writes each cell as a vertex with its normal.

struct TerrainUniforms {
    // Cells along a side.
    res: u32,
    // This dispatch's raindrops, and the index of its first (its randomness).
    droplets: u32,
    first: u32,
    seed: u32,
    size: f32,
    height: f32,
    feature_size: f32,
    roughness: f32,
    // What share of the erosion `fade_main` keeps.
    keep: f32,
    // The steepest rise loose soil holds, in Height units a cell.
    repose: f32,
    _pad0: u32,
    _pad1: u32,
}

struct MeshVertex {
    position: vec3<f32>,
    normal: vec3<f32>,
    uv: vec2<f32>,
    uv1: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: TerrainUniforms;
@group(0) @binding(1) var<storage, read_write> base: array<f32>;
@group(0) @binding(2) var<storage, read_write> erosion: array<atomic<i32>>;
@group(0) @binding(3) var<storage, read_write> vertices: array<MeshVertex>;
@group(0) @binding(4) var<storage, read_write> snapshot: array<f32>;

// Erosion is held in 1/EROSION_FIX of Height: fine enough that a drop's
// share on one cell never rounds away, with room for ±128.
const EROSION_FIX: f32 = 16777216.0;

// The raindrop, in Lague's units: heights of a [0, 1] map 256 cells across.
// INERTIA: how much of its heading a drop keeps each step. CAPACITY: soil a
// drop can carry per unit of fall, speed and water; MIN_CAPACITY what it can
// always carry, so drops on flat ground still move some. ERODE and DEPOSIT:
// the share of the gap to capacity taken or laid each step. EVAPORATE: water
// lost each step. GRAVITY: speed gained per unit of fall. RADIUS: cells a
// drop wears around it, so drops wear channels rather than pits. LIFETIME:
// steps per drop.
const INERTIA: f32 = 0.05;
const CAPACITY: f32 = 4.0;
const MIN_CAPACITY: f32 = 0.01;
const ERODE: f32 = 0.3;
const DEPOSIT: f32 = 0.3;
const EVAPORATE: f32 = 0.01;
const GRAVITY: f32 = 4.0;
const RADIUS: i32 = 3;
const LIFETIME: u32 = 64u;

const TERRAIN_TURN = mat2x2<f32>(vec2<f32>(0.8, 0.6), vec2<f32>(-0.6, 0.8));

fn terrain_hash(cell: vec2<i32>, seed: u32) -> f32 {
    var h = (bitcast<u32>(cell.x) * 0x8da6b343u) ^ (bitcast<u32>(cell.y) * 0xd8163841u) ^ (seed * 0xcb1ab31fu);
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return f32(h) * (1.0 / 4294967296.0);
}

// Value noise in [-1, 1] with quintic smoothing, and its gradient.
fn terrain_noise(p: vec2<f32>, seed: u32) -> vec3<f32> {
    let cell = vec2<i32>(floor(p));
    let w = fract(p);
    let s = w * w * w * (w * (w * 6.0 - 15.0) + 10.0);
    let ds = 30.0 * w * w * (w * (w - 2.0) + 1.0);
    let a = terrain_hash(cell, seed);
    let b = terrain_hash(cell + vec2<i32>(1, 0), seed);
    let c = terrain_hash(cell + vec2<i32>(0, 1), seed);
    let d = terrain_hash(cell + vec2<i32>(1, 1), seed);
    let k1 = b - a;
    let k2 = c - a;
    let k4 = a - b - c + d;
    let value = -1.0 + 2.0 * (a + k1 * s.x + k2 * s.y + k4 * s.x * s.y);
    let gradient = 2.0 * ds * vec2<f32>(k1 + k4 * s.y, k2 + k4 * s.x);
    return vec3<f32>(value, gradient);
}

// How much of an octave of `wavelength` metres cells `spacing` apart can
// hold: whole above four cells, none below two, so it never aliases.
fn terrain_fade(wavelength: f32, spacing: f32) -> f32 {
    return clamp(wavelength / (2.0 * spacing) - 1.0, 0.0, 1.0);
}

// The noise height at `xz` metres, about [-1, 1]. Broad rolling ground
// carries the height; where it rises, ridged octaves (1 - |noise|, each
// octave's share following the crest under it: Musgrave's ridged
// multifractal) raise crests; then detail octaves that fade on steep ground
// (Quilez's "Elevated", 2009) and in low ground. The rain does the rest.
fn terrain_noise_height(xz: vec2<f32>, spacing: f32) -> f32 {
    let feature_size = max(u.feature_size, 1e-3);
    let seed = u.seed;
    var p = xz / (2.0 * feature_size);
    var rolling = 0.0;
    var amplitude = 1.0;
    for (var octave = 0u; octave < 2u; octave++) {
        rolling += amplitude * terrain_noise(p, seed + 0x68e31da4u + octave).x;
        amplitude *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    rolling /= 1.5;
    let mountain = smoothstep(-0.3, 0.5, rolling);
    p = xz / feature_size;
    var wavelength = feature_size;
    amplitude = 1.0;
    var ridges = 0.0;
    var crest = 1.0;
    var slope = vec2<f32>(0.0);
    for (var octave = 0u; octave < 3u; octave++) {
        let fade = terrain_fade(wavelength, spacing);
        let n = terrain_noise(p, seed + octave * 0x9e3779b9u);
        // The widest octave stays a broad ridge; finer ones sharpen.
        var r = 1.0 - abs(n.x);
        r = select(r, r * r, octave > 0u);
        ridges += fade * amplitude * r * crest;
        crest = clamp(r * 1.5, 0.0, 1.0);
        slope += n.yz;
        amplitude *= u.roughness;
        wavelength *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    var detail = 0.0;
    for (var octave = 3u; octave < 16u; octave++) {
        let fade = terrain_fade(wavelength, spacing);
        if (fade <= 0.0) {
            break;
        }
        let n = terrain_noise(p, seed + octave * 0x9e3779b9u);
        slope += n.yz;
        detail += fade * amplitude * n.x / (1.0 + dot(slope, slope));
        amplitude *= u.roughness;
        wavelength *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    return 0.5 * rolling + mountain * (0.6 * ridges - 0.3) + detail * mix(0.05, 1.0, mountain);
}

fn cell_spacing() -> f32 {
    return u.size / f32(u.res - 1u);
}

// A cell's place in metres, the map spanning `size` around the origin.
fn cell_xz(c: vec2<u32>) -> vec2<f32> {
    return (vec2<f32>(c) / f32(u.res - 1u) - 0.5) * u.size;
}

@compute @workgroup_size(256)
fn base_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.res * u.res {
        return;
    }
    let c = vec2<u32>(i % u.res, i / u.res);
    base[i] = terrain_noise_height(cell_xz(c), cell_spacing());
}

@compute @workgroup_size(256)
fn fade_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < u.res * u.res {
        let kept = f32(atomicLoad(&erosion[id.x])) * u.keep;
        atomicStore(&erosion[id.x], i32(round(kept)));
    }
}

// Height in Height units at cell `c`, clamped to the map.
fn cell_height(c: vec2<i32>) -> f32 {
    let last = i32(u.res) - 1;
    let k = clamp(c, vec2<i32>(0), vec2<i32>(last));
    let i = u32(k.y) * u.res + u32(k.x);
    return base[i] + f32(atomicLoad(&erosion[i])) / EROSION_FIX;
}

// Height and gradient (per cell) at `p` in cells, bilinear.
fn height_and_gradient(p: vec2<f32>) -> vec3<f32> {
    let c = vec2<i32>(floor(p));
    let f = p - floor(p);
    let h00 = cell_height(c);
    let h10 = cell_height(c + vec2<i32>(1, 0));
    let h01 = cell_height(c + vec2<i32>(0, 1));
    let h11 = cell_height(c + vec2<i32>(1, 1));
    let gx = (h10 - h00) * (1.0 - f.y) + (h11 - h01) * f.y;
    let gy = (h01 - h00) * (1.0 - f.x) + (h11 - h10) * f.x;
    let h = h00 * (1.0 - f.x) * (1.0 - f.y) + h10 * f.x * (1.0 - f.y) + h01 * (1.0 - f.x) * f.y + h11 * f.x * f.y;
    return vec3<f32>(h, gx, gy);
}

fn in_map(c: vec2<i32>) -> bool {
    return all(c >= vec2<i32>(0)) && all(c < vec2<i32>(i32(u.res)));
}

fn add_erosion(c: vec2<i32>, amount: f32) {
    if !in_map(c) {
        return;
    }
    atomicAdd(&erosion[u32(c.y) * u.res + u32(c.x)], i32(round(amount * EROSION_FIX)));
}

fn brush_weight(dx: i32, dy: i32) -> f32 {
    return max(f32(RADIUS) - length(vec2<f32>(f32(dx), f32(dy))), 0.0);
}

@compute @workgroup_size(256)
fn erode_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= u.droplets {
        return;
    }
    let n = i32(u.first + id.x);
    let span = f32(u.res - 3u);
    var pos = vec2<f32>(
        1.0 + terrain_hash(vec2<i32>(n, 17), u.seed) * span,
        1.0 + terrain_hash(vec2<i32>(n, 91), u.seed) * span,
    );
    // Lague's map spans 1 in height over 256 cells; ours about 2 over `res`.
    let to_lague = f32(u.res) / 512.0;
    var brush_total = 0.0;
    for (var dy = -RADIUS; dy <= RADIUS; dy++) {
        for (var dx = -RADIUS; dx <= RADIUS; dx++) {
            brush_total += brush_weight(dx, dy);
        }
    }
    var dir = vec2<f32>(0.0);
    var speed = 1.0;
    var water = 1.0;
    var sediment = 0.0;
    for (var step = 0u; step < LIFETIME; step++) {
        let cell = vec2<i32>(floor(pos));
        let f = pos - floor(pos);
        let here = height_and_gradient(pos);
        dir = dir * INERTIA - here.yz * to_lague * (1.0 - INERTIA);
        let len = length(dir);
        if len < 1e-9 {
            break;
        }
        dir /= len;
        pos += dir;
        if any(pos < vec2<f32>(1.0)) || any(pos > vec2<f32>(f32(u.res) - 2.0)) {
            break;
        }
        let fall = (height_and_gradient(pos).x - here.x) * to_lague;
        let capacity = max(-fall * speed * water * CAPACITY, MIN_CAPACITY);
        if sediment > capacity || fall > 0.0 {
            // Uphill it fills the pit behind it, at most; else it lays the
            // share of its load past capacity. On the four cells round its
            // old place, by how near each is.
            var lay = (sediment - capacity) * DEPOSIT;
            if fall > 0.0 {
                lay = min(fall, sediment);
            }
            sediment -= lay;
            let amount = lay / to_lague;
            add_erosion(cell, amount * (1.0 - f.x) * (1.0 - f.y));
            add_erosion(cell + vec2<i32>(1, 0), amount * f.x * (1.0 - f.y));
            add_erosion(cell + vec2<i32>(0, 1), amount * (1.0 - f.x) * f.y);
            add_erosion(cell + vec2<i32>(1, 1), amount * f.x * f.y);
        } else {
            // It takes soil, never more than its fall, from the cells within
            // RADIUS of its old cell, most from the nearest, and none from
            // below where it flows to: water can't dig a hole deeper than
            // its own path, so a slope can't run away into a trench.
            let take = min((capacity - sediment) * ERODE, -fall);
            let share = take / to_lague / brush_total;
            let lowest = here.x + fall / to_lague;
            var taken = 0.0;
            for (var dy = -RADIUS; dy <= RADIUS; dy++) {
                for (var dx = -RADIUS; dx <= RADIUS; dx++) {
                    let k = cell + vec2<i32>(dx, dy);
                    let w = brush_weight(dx, dy);
                    if w > 0.0 && in_map(k) {
                        let t = min(share * w, max(cell_height(k) - lowest, 0.0));
                        add_erosion(k, -t);
                        taken += t;
                    }
                }
            }
            sediment += taken * to_lague;
        }
        speed = sqrt(max(speed * speed - fall * GRAVITY, 0.0));
        water *= 1.0 - EVAPORATE;
    }
}

@compute @workgroup_size(256)
fn snapshot_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i < u.res * u.res {
        snapshot[i] = base[i] + f32(atomicLoad(&erosion[i])) / EROSION_FIX;
    }
}

const NEIGHBOURS = array<vec2<i32>, 8>(
    vec2<i32>(-1, -1), vec2<i32>(0, -1), vec2<i32>(1, -1), vec2<i32>(-1, 0),
    vec2<i32>(1, 0), vec2<i32>(-1, 1), vec2<i32>(0, 1), vec2<i32>(1, 1),
);

// A cell's loose soil (what the rain laid on it) over the neighbours it
// stands steeper than repose above: half the steepest excess moves, shared
// by how far past repose each neighbour is (thermal erosion, Musgrave,
// Kolb and Mace 1989), never more soil than the cell holds.
@compute @workgroup_size(256)
fn slide_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.res * u.res {
        return;
    }
    let soil = f32(atomicLoad(&erosion[i])) / EROSION_FIX;
    if soil <= 0.0 {
        return;
    }
    let c = vec2<i32>(i32(i % u.res), i32(i / u.res));
    let h = snapshot[i];
    var excess: array<f32, 8>;
    var total = 0.0;
    var steepest = 0.0;
    for (var k = 0; k < 8; k++) {
        let n = c + NEIGHBOURS[k];
        var e = 0.0;
        if in_map(n) {
            let d = h - snapshot[u32(n.y) * u.res + u32(n.x)];
            e = max(d - u.repose * length(vec2<f32>(NEIGHBOURS[k])), 0.0);
        }
        excess[k] = e;
        total += e;
        steepest = max(steepest, e);
    }
    if total <= 0.0 {
        return;
    }
    let moved = min(0.5 * steepest, soil);
    for (var k = 0; k < 8; k++) {
        if excess[k] > 0.0 {
            add_erosion(c + NEIGHBOURS[k], moved * excess[k] / total);
        }
    }
    add_erosion(c, -moved);
}

// A grid point's place, its column and row clamped to the map.
fn vertex_position(col: i32, row: i32) -> vec3<f32> {
    let last = i32(u.res) - 1;
    let c = vec2<u32>(vec2<i32>(clamp(col, 0, last), clamp(row, 0, last)));
    let j = c.y * u.res + c.x;
    let h = base[j] + f32(atomicLoad(&erosion[j])) / EROSION_FIX;
    let xz = cell_xz(c);
    return vec3<f32>(xz.x, h * u.height, xz.y);
}

@compute @workgroup_size(256)
fn vertices_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.res * u.res {
        return;
    }
    let col = i32(i % u.res);
    let row = i32(i / u.res);
    // The normal from the neighbours on either side across and along the
    // grid, up where they collapse.
    let dx = vertex_position(col + 1, row) - vertex_position(col - 1, row);
    let dy = vertex_position(col, row + 1) - vertex_position(col, row - 1);
    let n = cross(dy, dx);
    let len = length(n);
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    if len >= 1e-8 {
        normal = n / len;
    }
    vertices[i] = MeshVertex(
        vertex_position(col, row),
        normal,
        vec2<f32>(f32(col), f32(row)) / f32(u.res - 1u),
        vec2<f32>(0.0),
        vec4<f32>(0.0),
        vec4<f32>(1.0),
    );
}
