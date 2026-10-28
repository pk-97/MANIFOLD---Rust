// The wet map: a top-down map over the water's object-space XZ of where and
// when the water touched a surface (composed after the world-tile read side
// and the map's layout, `manifold_node_engine::scene::wet_map::LAYOUT`).
//
// Passes, a barrier between each:
//   cs_place  one thread: places the map round the water's extent when the
//             node starts over or the water leaves it, writes the header and
//             the clear's group count (none when the map stays put)
//   cs_clear  indirect, a thread a texel: every texel untouched
//   cs_mark   a thread a particle: a particle within two radii of a solid
//             stamps the texels round the surface point it touches with the
//             display time, widens their touched heights and sets the
//             height's bin for a wall or an up-facing surface
// Drying is computed where the map is read, from the age of the last touch,
// so nothing runs over the map while the water is still.

struct Params {
    // The display time and the drying rate, copied into the header.
    now: f32,
    drying: f32,
    // The texel size the cell asks for, metres.
    base_texel: f32,
    // 1 when the node starts over: the map is placed again and cleared.
    restart: u32,
    // Texels a side, and the particle slots.
    side: u32,
    capacity: u32,
    // The display pool and cell size the solid is stored under.
    pool_tiles: u32,
    cell: f32,
}

struct WetParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> particles: array<WetParticle>;
// The display solid's distance on the display tiles, negative inside.
@group(0) @binding(2) var<storage, read> solid: array<f32>;
@group(0) @binding(3) var<storage, read> tiles: array<u32>;
// The held tiles' node bounds in object space: min xyz, 0, max xyz, 0.
@group(0) @binding(4) var<storage, read> extent: array<f32>;
@group(0) @binding(5) var<storage, read_write> map: array<atomic<u32>>;
// The clear's threadgroups: x, y, z.
@group(0) @binding(6) var<storage, read_write> clear_groups: array<u32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// The texel sizes a placed map takes, metres: 5 mm keeps a splash's edge
// sharp at close range, 50 mm makes the map 51 m across.
const WET_MIN_TEXEL: f32 = 0.005;
const WET_MAX_TEXEL: f32 = 0.05;
// A particle marks the surface it is within this many radii of.
const WET_REACH_RADII: f32 = 2.0;
// The solid reads 1e30 in a tile no slot holds; capped so a trilinear mix
// stays finite.
const WET_SOLID_FAR: f32 = 1.0e4;
// The clear's threadgroup side (cs_clear is 8 x 8).
const WET_CLEAR_GROUP: u32 = 8u;

fn wet_header_f32(word: u32) -> f32 {
    return bitcast<f32>(atomicLoad(&map[word]));
}

// Whether the map at `origin` with sides `span` should move for an extent
// from `lo` to `hi` (XZ). Per axis: an extent the map can hold must lie inside
// it; a wider one keeps the map while its middle stays within a quarter map of
// the map's middle, so a sea wider than the map does not move it every frame.
fn wet_leaves(lo: vec2<f32>, hi: vec2<f32>, origin: vec2<f32>, span: f32) -> bool {
    let fits = (hi - lo) <= vec2<f32>(span);
    let inside = lo >= origin & hi <= origin + vec2<f32>(span);
    let middle = 0.5 * (lo + hi);
    let near = abs(middle - (origin + vec2<f32>(0.5 * span))) <= vec2<f32>(0.25 * span);
    return any(select(!near, !inside, fits));
}

@compute @workgroup_size(1)
fn cs_place() {
    let lo = vec3<f32>(extent[0], extent[1], extent[2]);
    let hi = vec3<f32>(extent[4], extent[5], extent[6]);
    let held = all(lo <= hi);
    let texel = wet_header_f32(WM_TEXEL);
    let origin = vec2<f32>(wet_header_f32(WM_ORIGIN_X), wet_header_f32(WM_ORIGIN_Z));
    let side = f32(params.side);
    var place = params.restart != 0u || !(texel > 0.0);
    if held && !place {
        place = wet_leaves(lo.xz, hi.xz, origin, side * texel);
    }
    if place && held {
        // The finest texel at which the map spans twice the water, centred on
        // it: the water can spread half its size each way before it leaves.
        let size = max(hi.x - lo.x, hi.z - lo.z);
        let fit = clamp(max(params.base_texel, 2.0 * size / side), WET_MIN_TEXEL, WET_MAX_TEXEL);
        let middle = 0.5 * (lo.xz + hi.xz);
        let corner = floor((middle - vec2<f32>(0.5 * side * fit)) / fit) * fit;
        atomicStore(&map[WM_ORIGIN_X], bitcast<u32>(corner.x));
        atomicStore(&map[WM_ORIGIN_Z], bitcast<u32>(corner.y));
        atomicStore(&map[WM_TEXEL], bitcast<u32>(fit));
    }
    atomicStore(&map[WM_SIDE], params.side);
    atomicStore(&map[WM_NOW], bitcast<u32>(params.now));
    atomicStore(&map[WM_DRYING], bitcast<u32>(params.drying));
    let groups = select(0u, params.side / WET_CLEAR_GROUP, place);
    clear_groups[0] = groups;
    clear_groups[1] = groups;
    clear_groups[2] = 1u;
}

@compute @workgroup_size(8, 8)
fn cs_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.side || gid.y >= params.side {
        return;
    }
    let word = wm_texel_word(gid.xy, params.side);
    for (var k = 0u; k < WM_TEXEL_WORDS; k = k + 1u) {
        atomicStore(&map[word + k], 0u);
    }
}

// The solid's distance at object-space point x, trilinear on the display
// nodes round it (node p sits at (p - 1/2) cells), and its gradient there,
// per metre.
struct WetSolid {
    distance: f32,
    gradient: vec3<f32>,
}

fn wet_solid_at(x: vec3<f32>) -> WetSolid {
    let g = x / params.cell + vec3<f32>(0.5);
    let lower = floor(g);
    let f = g - lower;
    let base = vec3<i32>(lower);
    var value = 0.0;
    var slope = vec3<f32>(0.0);
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<i32>(i32(corner & 1u), i32((corner >> 1u) & 1u), i32((corner >> 2u) & 1u));
        let wx = select(1.0 - f.x, f.x, o.x == 1);
        let wy = select(1.0 - f.y, f.y, o.y == 1);
        let wz = select(1.0 - f.z, f.z, o.z == 1);
        let toward = select(vec3<f32>(-1.0), vec3<f32>(1.0), o == vec3<i32>(1));
        let v = min(solid[ts_cell_at(base + o)], WET_SOLID_FAR);
        value = value + wx * wy * wz * v;
        slope = slope + v * toward * vec3<f32>(wy * wz, wx * wz, wx * wy);
    }
    return WetSolid(value, slope / params.cell);
}

@compute @workgroup_size(64)
fn cs_mark(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    let p = particles[i].position_radius;
    if !(p.w > 0.0) {
        return;
    }
    let texel = wet_header_f32(WM_TEXEL);
    if !(texel > 0.0) {
        return;
    }
    let near = wet_solid_at(p.xyz);
    if near.distance > WET_REACH_RADII * p.w {
        return;
    }
    // The point of the surface the particle touches: back along the solid's
    // normal by its distance. A wall's face is one line of texels seen from
    // above, so the stamp lands on it, not a particle's width in front, and
    // covers the texels either side of it.
    let steep = length(near.gradient);
    var contact = p.xyz;
    var up = false;
    if steep > 0.0 {
        let normal = near.gradient / steep;
        contact = p.xyz - near.distance * normal;
        up = normal.y > WM_UP_FACING;
    }
    let origin = vec2<f32>(wet_header_f32(WM_ORIGIN_X), wet_header_f32(WM_ORIGIN_Z));
    let home = vec2<i32>(floor((contact.xz - origin) / texel));
    let touch = wm_touch_key(params.now);
    let low = ~wm_height_key(contact.y);
    let high = wm_height_key(contact.y);
    let bit = wm_height_bit(contact.y, WM_HEIGHT_MARGIN_TEXELS * texel, up);
    let side = i32(params.side);
    for (var dz = 0; dz < WM_STAMP; dz = dz + 1) {
        for (var dx = 0; dx < WM_STAMP; dx = dx + 1) {
            let t = home + vec2<i32>(dx, dz) - vec2<i32>(WM_STAMP / 2);
            if any(t < vec2<i32>(0)) || any(t >= vec2<i32>(side)) {
                continue;
            }
            let word = wm_texel_word(vec2<u32>(t), params.side);
            atomicMax(&map[word], touch);
            atomicMax(&map[word + 1u], low);
            atomicMax(&map[word + 2u], high);
            atomicOr(&map[word + 3u], bit);
        }
    }
}
