// node.terrain — a height map of noise carved by simulated rain, laid out as
// the grid of mesh vertices that draws it.
//
// `body_main` writes the land's smooth body into `snapshot`; `base_main`
// carves gullies down its slopes and adds fine detail, writing the noise
// height of every cell into `base`, in units of Height (about [-1, 1]), and
// how far down a gully floor each cell lies into `drain`.
// `erode_main` runs raindrops over base + erosion: each drop
// rolls downhill, picks up soil where it speeds up and lays it down where it
// slows or climbs (Hans Beyer, "Implementation of a method for hydraulic
// erosion", 2015, as Sebastian Lague's Coding Adventure tuned it, 2019). The
// soil it moves is summed into `erosion` with integer atomics, so drops on
// every thread write at once, and each batch reads the land from a snapshot
// taken before it, so every run carves alike. Drops running at once can all fill the same
// hollow, so after each batch the loose soil slides (`snapshot_main`, then
// `slide_main`): soil left steeper than its angle of repose moves down to
// the neighbours below it. Each drop also adds its water to `flow` on every
// cell it crosses. `fade_main` scales the erosion and flow when the land
// under them changes. `vertices_main` writes each cell as a vertex with its
// normal, and in its second UV set where water runs (gully floors, and
// channels the drops ran) and where soil settled, each 0 to 1, for Covers
// and Copies On Surface to read (`cover_rule.wgsl`), and how much of the sky
// the land around hides (`sky_blocked`), which darkens its sky light.

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
    // Raindrops fallen so far per cell, which `flow` is measured against.
    rain: f32,
    _pad0: u32,
}

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    // The share of the sky the land around hides (`sky_blocked`).
    sky_blocked: f32,
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
@group(0) @binding(5) var<storage, read_write> flow: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read_write> drain: array<f32>;

// Erosion is held in 1/EROSION_FIX of Height: fine enough that a drop's
// share on one cell never rounds away, with room for ±128.
const EROSION_FIX: f32 = 16777216.0;
// Flow is held in 1/FLOW_FIX of a whole drop's water.
const FLOW_FIX: f32 = 256.0;
// A cell's water, as a share of what an average cell takes, from which it
// starts to count as a channel, and the doublings past that to a full one.
// Drops die within LIFETIME cells, so they mark rills, not rivers.
const FLOW_START: f32 = 1.5;
const FLOW_DOUBLINGS: f32 = 4.0;
// Metres of laid soil that count as a full deposit.
const DEPOSIT_FULL: f32 = 4.0;
// Steps an average drop runs before it stops or leaves the map, which
// scales `rain` into the water an average cell takes.
const AVERAGE_STEPS: f32 = 32.0;

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

// Broad rolling ground, about [-1, 1], and the share of it that rises
// into mountains.
fn terrain_rolling(xz: vec2<f32>) -> vec2<f32> {
    var p = xz / (2.0 * max(u.feature_size, 1e-3));
    var rolling = 0.0;
    var amplitude = 1.0;
    for (var octave = 0u; octave < 2u; octave++) {
        rolling += amplitude * terrain_noise(p, u.seed + 0x68e31da4u + octave).x;
        amplitude *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    rolling /= 1.5;
    return vec2<f32>(rolling, smoothstep(-0.3, 0.5, rolling));
}

// The land's body at `xz` metres: the rolling ground, and where it rises,
// mountain ranges along the ridge lines of one octave (1 - |noise|, the
// crest rounded over its top tenth so gullies can't notch a knife edge
// into teeth), with
// smooth shoulders under them from the next two, each scaled by the crest
// beneath it. Ridged octaves finer than the first would line identical
// peaks up along every crest, so gullies and detail take over from here.
fn terrain_body(xz: vec2<f32>, spacing: f32) -> f32 {
    let ground = terrain_rolling(xz);
    var p = xz / max(u.feature_size, 1e-3);
    var wavelength = u.feature_size;
    var amplitude = 1.0;
    var massif = 0.0;
    var crest = 1.0;
    for (var octave = 0u; octave < 3u; octave++) {
        let fade = terrain_fade(wavelength, spacing);
        let n = terrain_noise(p, u.seed + octave * 0x9e3779b9u).x;
        let r = select(0.5 + 0.5 * n, 1.0 - sqrt(n * n + 0.01), octave == 0u);
        massif += fade * amplitude * r * crest;
        if octave == 0u {
            crest = clamp(r * 1.5, 0.0, 1.0);
        }
        amplitude *= u.roughness;
        wavelength *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    return 0.5 * ground.x + ground.y * (0.6 * massif - 0.3);
}

// Gully noise: the octave of stripes water would cut down a slope (Clay
// John's "Eroded Terrain Noise", 2018, as Rune Skovbo Johansen's "Fast and
// Gorgeous Erosion Filter", 2026, refines it). Each cell of the octave's
// grid holds a pivot at a random place; round it, stripes run straight
// downhill along `down`, a cosine across them. The cells' stripes blend by
// distance, and the blend is pushed back out to full strength unless the
// cells cancel, so gullies stay deep without looping round on themselves.
// `q` is in the octave's cells, one stripe a cell. Returns the stripe height
// in [-1, 1] (ridges +1, gully floors -1) and its gradient in q.
fn gully_octave(q: vec2<f32>, down: vec2<f32>, seed: u32) -> vec3<f32> {
    let across = vec2<f32>(-down.y, down.x);
    let home = vec2<i32>(floor(q));
    var wave = vec2<f32>(0.0);
    var total = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let cell = home + vec2<i32>(x, y);
            let pivot = vec2<f32>(cell) + vec2<f32>(terrain_hash(cell, seed), terrain_hash(cell, seed ^ 0x5bd1e995u));
            let offset = q - pivot;
            let w = exp(-2.0 * dot(offset, offset));
            let phase = 6.2831853 * dot(offset, across);
            wave += w * vec2<f32>(cos(phase), sin(phase));
            total += w;
        }
    }
    wave /= total;
    let strength = length(wave);
    wave *= min(2.0 * strength, 1.0) / max(strength, 1e-6);
    return vec3<f32>(wave.x, -6.2831853 * wave.y * across);
}

// How deep gullies cut: each octave's gully floors sink this share of
// their own width below the ridges between them, in Height units per
// Feature Size, on the steepest ground.
const GULLY_DEPTH: f32 = 0.5;
// The slope, in Height units per Feature Size, past which gullies cut at
// full depth; flat ground keeps none.
const GULLY_SLOPE: f32 = 1.0;
// Along a range's crest gullies fade out, from this share of the ridge
// octave's height to the top, so they start below it as channels do and
// the crest stays one line instead of a row of notches.
const CREST_START: f32 = 0.6;
const CREST_END: f32 = 0.85;
// The widest gullies, as a share of Feature Size; each octave after is half
// as wide, down to what the grid can hold.
const GULLY_WIDTH: f32 = 0.5;

// Gullies carved down the body's slopes at `xz` metres, where the body has
// gradient `slope` (Height units a metre): the carving in Height units, and
// how far down a gully floor the point lies, 0 on open ground and ridges, 1
// at the floor of gullies cut full depth. Each octave
// follows the slope the octaves before it left, so gullies branch, and cuts
// by how steep the ground is, so valley floors and summits stay whole.
// Water only takes rock away: the ridges between gullies stay at the body's
// height and the floors sink, so no fins or spikes rise above the land.
fn terrain_gullies(xz: vec2<f32>, spacing: f32, slope: vec2<f32>) -> vec2<f32> {
    let feature_size = max(u.feature_size, 1e-3);
    let ridge = terrain_noise(xz / feature_size, u.seed).x;
    let open = 1.0 - smoothstep(CREST_START, CREST_END, 1.0 - sqrt(ridge * ridge + 0.01));
    var gradient = slope;
    var carved = 0.0;
    var floor_depth = 0.0;
    var deepest = 0.0;
    var wavelength = feature_size * GULLY_WIDTH;
    for (var octave = 0u; octave < 8u; octave++) {
        let fade = terrain_fade(wavelength, spacing);
        if fade <= 0.0 {
            break;
        }
        let steep = length(gradient) * feature_size;
        let t = min(steep / GULLY_SLOPE, 1.0);
        let reach = 1.0 - (1.0 - t) * (1.0 - t);
        let down = -gradient / max(length(gradient), 1e-9);
        let g = gully_octave(xz / wavelength, down, u.seed + 0x51ed270bu + octave);
        let amplitude = fade * reach * open * GULLY_DEPTH * wavelength / feature_size;
        carved += 0.5 * amplitude * (g.x - 1.0);
        gradient += 0.5 * amplitude * g.yz / wavelength;
        floor_depth += amplitude * 0.5 * (1.0 - g.x);
        deepest += fade * GULLY_DEPTH * wavelength / feature_size;
        wavelength *= 0.5;
    }
    return vec2<f32>(carved, floor_depth / max(deepest, 1e-6));
}

// Fine detail at `xz` metres, fading on steep ground (Quilez's "Elevated",
// 2009), and lower in low ground, where it rolls gently.
fn terrain_detail(xz: vec2<f32>, spacing: f32) -> f32 {
    let mountain = terrain_rolling(xz).y;
    var p = TERRAIN_TURN * TERRAIN_TURN * TERRAIN_TURN * (xz / max(u.feature_size, 1e-3)) * 8.0;
    var wavelength = u.feature_size * 0.125;
    var amplitude = u.roughness * u.roughness * u.roughness;
    var slope = vec2<f32>(0.0);
    var detail = 0.0;
    for (var octave = 3u; octave < 16u; octave++) {
        let fade = terrain_fade(wavelength, spacing);
        if (fade <= 0.0) {
            break;
        }
        let n = terrain_noise(p, u.seed + octave * 0x9e3779b9u);
        slope += n.yz;
        detail += fade * amplitude * n.x / (1.0 + dot(slope, slope));
        amplitude *= u.roughness;
        wavelength *= 0.5;
        p = TERRAIN_TURN * p * 2.0;
    }
    return detail * mix(0.35, 1.0, mountain);
}

fn cell_spacing() -> f32 {
    return u.size / f32(u.res - 1u);
}

// A cell's place in metres, the map spanning `size` around the origin.
fn cell_xz(c: vec2<u32>) -> vec2<f32> {
    return (vec2<f32>(c) / f32(u.res - 1u) - 0.5) * u.size;
}

@compute @workgroup_size(256)
fn body_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.res * u.res {
        return;
    }
    let c = vec2<u32>(i % u.res, i / u.res);
    snapshot[i] = terrain_body(cell_xz(c), cell_spacing());
}

// The body at cell `c`, clamped to the map.
fn body_at(c: vec2<i32>) -> f32 {
    let k = clamp(c, vec2<i32>(0), vec2<i32>(i32(u.res) - 1));
    return snapshot[u32(k.y) * u.res + u32(k.x)];
}

@compute @workgroup_size(256)
fn base_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.res * u.res {
        return;
    }
    let c = vec2<i32>(i32(i % u.res), i32(i / u.res));
    let spacing = cell_spacing();
    let last = i32(u.res) - 1;
    // The body's slope from the cells on either side, one-sided at the edge.
    let x0 = max(c.x - 1, 0);
    let x1 = min(c.x + 1, last);
    let y0 = max(c.y - 1, 0);
    let y1 = min(c.y + 1, last);
    let slope = vec2<f32>(
        (body_at(vec2<i32>(x1, c.y)) - body_at(vec2<i32>(x0, c.y))) / (f32(max(x1 - x0, 1)) * spacing),
        (body_at(vec2<i32>(c.x, y1)) - body_at(vec2<i32>(c.x, y0))) / (f32(max(y1 - y0, 1)) * spacing),
    );
    let xz = cell_xz(vec2<u32>(c));
    let gullies = terrain_gullies(xz, spacing, slope);
    base[i] = snapshot[i] + gullies.x + terrain_detail(xz, spacing);
    drain[i] = gullies.y;
}

@compute @workgroup_size(256)
fn fade_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x < u.res * u.res {
        let kept = f32(atomicLoad(&erosion[id.x])) * u.keep;
        atomicStore(&erosion[id.x], i32(round(kept)));
        atomicStore(&flow[id.x], u32(round(f32(atomicLoad(&flow[id.x])) * u.keep)));
    }
}

// Height in Height units at cell `c`, clamped to the map, as the batch's
// snapshot holds it: drops running at once never read each other's changes,
// so the carving is the same on every run.
fn cell_height(c: vec2<i32>) -> f32 {
    let last = i32(u.res) - 1;
    let k = clamp(c, vec2<i32>(0), vec2<i32>(last));
    return snapshot[u32(k.y) * u.res + u32(k.x)];
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
        atomicAdd(&flow[u32(cell.y) * u.res + u32(cell.x)], u32(water * FLOW_FIX));
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
    // The soil as the snapshot found it, not as neighbours sliding at once
    // leave it.
    let h = snapshot[i];
    let soil = h - base[i];
    if soil <= 0.0 {
        return;
    }
    let c = vec2<i32>(i32(i % u.res), i32(i / u.res));
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

// Where water ran through cell `j`, 0 to 1: none at or below FLOW_START
// times an average cell's water, full FLOW_DOUBLINGS doublings past it.
fn flow_mark(j: u32) -> f32 {
    let average = max(u.rain, 1e-6) * AVERAGE_STEPS;
    let share = f32(atomicLoad(&flow[j])) / FLOW_FIX / average;
    return clamp(log2(max(share / FLOW_START, 1e-6)) / FLOW_DOUBLINGS, 0.0, 1.0);
}

// Where the rain laid soil on cell `j`, 0 to 1, full at DEPOSIT_FULL metres.
fn deposit_mark(j: u32) -> f32 {
    let soil = f32(atomicLoad(&erosion[j])) / EROSION_FIX * u.height;
    return clamp(soil / DEPOSIT_FULL, 0.0, 1.0);
}

// Sky blocking: from a vertex, SKY_DIRECTIONS ways along the grid, out to
// the last of SKY_STEPS cells. Per way, the sky lost is sin² of how far the
// highest point rises above the ground's own slope there, so a steady slope
// loses nothing and a valley floor or the foot of a cliff loses a lot; the
// vertex keeps the mean.
const SKY_DIRECTIONS: u32 = 8u;
const SKY_STEPS: array<i32, 12> = array<i32, 12>(1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64);

fn sky_blocked(col: i32, row: i32, here: vec3<f32>, normal: vec3<f32>) -> f32 {
    let ways = array<vec2<i32>, 8>(
        vec2<i32>(1, 0), vec2<i32>(1, 1), vec2<i32>(0, 1), vec2<i32>(-1, 1),
        vec2<i32>(-1, 0), vec2<i32>(-1, -1), vec2<i32>(0, -1), vec2<i32>(1, -1),
    );
    let last = i32(u.res) - 1;
    var blocked = 0.0;
    for (var d = 0u; d < SKY_DIRECTIONS; d++) {
        let way = ways[d];
        let flat = normalize(vec2<f32>(way));
        // The ground's own rise along this way, as a sine.
        let rise = -dot(normal.xz, flat) / max(normal.y, 1e-3);
        let ground = rise / sqrt(1.0 + rise * rise);
        var highest = ground;
        for (var k = 0; k < 12; k++) {
            let c = vec2<i32>(col, row) + way * SKY_STEPS[k];
            if any(c < vec2<i32>(0)) || any(c > vec2<i32>(last)) {
                break;
            }
            let to = vertex_position(c.x, c.y) - here;
            highest = max(highest, to.y / length(to));
        }
        let above = sin(max(asin(highest) - asin(ground), 0.0));
        blocked += above * above;
    }
    return blocked / f32(SKY_DIRECTIONS);
}

// Each grid point's height as drawn, row by row, into the snapshot: the
// ground a Collider reads back.
@compute @workgroup_size(256)
fn heights_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.res * u.res {
        return;
    }
    snapshot[i] = vertices[i].position.y;
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
    let here = vertex_position(col, row);
    vertices[i] = MeshVertex(
        here,
        0.0,
        normal,
        sky_blocked(col, row, here, normal),
        vec2<f32>(f32(col), f32(row)) / f32(u.res - 1u),
        vec2<f32>(max(drain[i], flow_mark(i)), deposit_mark(i)),
        vec4<f32>(0.0),
        vec4<f32>(1.0),
    );
}
