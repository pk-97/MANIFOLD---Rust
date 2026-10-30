// The wet map: soft wet marks (decals) where the water touched a surface,
// kept between frames (composed after the world-tile read side and the map's
// layout, `manifold_node_engine::scene::wet_map::layout()`).
//
// Passes, a barrier between each:
//   cs_begin       one thread: the draw's instance, time, drying, radius and
//                  merge distance into the header
//   cs_clear_hash  a thread a hash slot, before a compaction: every slot empty
//   cs_compact     a thread a decal, oldest first: a decal touched within
//                  `keep` seconds moves to the other half and back into the
//                  hash
//   cs_finish      one thread: the other half becomes the one in use
//   cs_mark        a thread a particle: a particle within two radii of a solid
//                  finds the decal of the merge cell and facing it touches
//                  through the hash and wets it again, or starts a new one
// With a Sand wired, its packed grains are a surface too:
//   cs_sand_clear  a thread a display element: no sand
//   cs_sand        a thread a grain: its rest volume shared trilinearly onto
//                  the display nodes round it
//   cs_mark_sand   cs_mark against the nearer of the solid and the sand
// Drying is computed where a decal is drawn, from the age of its last touch,
// so nothing runs over the decals while the water is still.
//
// The decals in a half run oldest first. Once a half is full a new decal
// overwrites the oldest, as the whitewater pool does its oldest particles:
// the ring's front counts the slots overwritten since the last compaction,
// from the half's first, and compaction moves those, the newest, behind the
// rest. An overwritten decal's hash slot goes stale: a contact that finds it
// sees another decal's key there and probes on, and compaction rebuilds the
// hash.

struct Params {
    // The display time and the drying rate, copied into the header.
    now: f32,
    drying: f32,
    // A decal's radius and the distance within which a contact wets an
    // existing decal instead of starting one, metres.
    radius: f32,
    merge: f32,
    // The particle slots.
    capacity: u32,
    // The display pool and cell size the solid is stored under.
    pool_tiles: u32,
    cell: f32,
    // A compaction keeps the decals touched within this many seconds.
    keep: f32,
    // The water a contact adds to a decal: its share of WD_FULL for the
    // display time since the last run.
    hit: u32,
    // A wired Sand's grains at binding 5, 0 with none.
    sand: u32,
    pad1: u32,
    pad2: u32,
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
@group(0) @binding(4) var<storage, read_write> map: array<atomic<u32>>;
// A wired Sand's grains this frame, and their volume share at each display
// node in SAND_FIXED units of a whole cell.
@group(0) @binding(5) var<storage, read> sand: array<WetParticle>;
@group(0) @binding(6) var<storage, read_write> sand_share: array<atomic<u32>>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// A particle marks the surface it is within this many radii of.
const WET_REACH_RADII: f32 = 2.0;
// The solid reads 1e30 in a tile no slot holds; capped so a trilinear mix
// stays finite.
const WET_SOLID_FAR: f32 = 1.0e4;
// Fixed-point units of one whole display cell of sand in `sand_share`.
const SAND_FIXED: f32 = 65536.0;
// Hash slots a contact or a compacted decal probes from its cell's slot.
const WET_PROBES: u32 = 8u;
// A contact wets a decal whose normal is within about 45 degrees of its own.
const WET_MERGE_AGREE: f32 = 0.7;

@compute @workgroup_size(1)
fn cs_begin() {
    atomicStore(&map[WD_DRAW_INSTANCES], 1u);
    atomicStore(&map[WD_NOW], bitcast<u32>(params.now));
    atomicStore(&map[WD_DRYING], bitcast<u32>(params.drying));
    atomicStore(&map[WD_RADIUS], bitcast<u32>(params.radius));
    atomicStore(&map[WD_MERGE], bitcast<u32>(params.merge));
}

@compute @workgroup_size(64)
fn cs_clear_hash(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= WD_HASH_SLOTS {
        return;
    }
    atomicStore(&map[wd_hash_word(gid.x)], 0u);
}

fn wet_position(word: u32) -> vec3<f32> {
    return vec3<f32>(
        bitcast<f32>(atomicLoad(&map[word + WD_POSITION])),
        bitcast<f32>(atomicLoad(&map[word + WD_POSITION + 1u])),
        bitcast<f32>(atomicLoad(&map[word + WD_POSITION + 2u])),
    );
}

// The decals in the half in use.
fn wet_count() -> u32 {
    return min(atomicLoad(&map[WD_DRAW_VERTICES]) / WD_DECAL_VERTICES, WD_CAPACITY);
}

// A surface's facing: the axis its normal is nearest, 0 x, 1 y, 2 z, plus 3
// when the normal points down that axis.
fn wet_facing(n: vec3<f32>) -> u32 {
    let a = abs(n);
    var axis = 2u;
    if a.x >= a.y && a.x >= a.z {
        axis = 0u;
    } else if a.y >= a.z {
        axis = 1u;
    }
    return axis + select(0u, 3u, n[axis] < 0.0);
}

// Point x's coordinates across facing's axis: the next axis, then the one
// after.
fn wet_across(x: vec3<f32>, facing: u32) -> vec2<f32> {
    let axis = facing % 3u;
    return vec2<f32>(x[(axis + 1u) % 3u], x[(axis + 2u) % 3u]);
}

// The merge cell holding point x across facing's axis.
fn wet_cell(x: vec3<f32>, facing: u32) -> vec2<i32> {
    return vec2<i32>(floor(wet_across(x, facing) / params.merge));
}

// The hash slot a decal's key starts probing at.
fn wet_home(cell: vec2<i32>, facing: u32) -> u32 {
    return wd_cell_slot(vec3<i32>(cell, i32(facing)));
}

// Where a decal starts for a contact at x on a surface of normal n: halfway
// from x to the middle of its merge cell across facing's axis, on the
// surface's plane through x. Every decal then sits in the middle half of its
// cell, so a wetted patch reads as one even film, while a lone splash's mark
// stays within a quarter cell of where it fell, off any grid.
fn wet_site(x: vec3<f32>, n: vec3<f32>, facing: u32) -> vec3<f32> {
    let axis = facing % 3u;
    let a = (axis + 1u) % 3u;
    let b = (axis + 2u) % 3u;
    let middle = (vec2<f32>(wet_cell(x, facing)) + vec2<f32>(0.5)) * params.merge;
    let shift = 0.5 * (middle - wet_across(x, facing));
    var site = x;
    site[a] = x[a] + shift.x;
    site[b] = x[b] + shift.y;
    site[axis] = x[axis] - (shift.x * n[a] + shift.y * n[b]) / n[axis];
    return site;
}

// The slot of a new decal: the half's next free one, or once the half is full
// its oldest decal, from the ring's front on.
fn wet_new_slot() -> u32 {
    let index = atomicAdd(&map[WD_DRAW_VERTICES], WD_DECAL_VERTICES) / WD_DECAL_VERTICES;
    if index < WD_CAPACITY {
        return index;
    }
    // Full: the count goes back to what the half holds.
    atomicSub(&map[WD_DRAW_VERTICES], WD_DECAL_VERTICES);
    atomicAdd(&map[WD_OVERWRITTEN], 1u);
    return atomicAdd(&map[WD_FRONT], 1u) % WD_CAPACITY;
}

@compute @workgroup_size(64)
fn cs_compact(@builtin(global_invocation_id) gid: vec3<u32>) {
    let half = atomicLoad(&map[WD_ACTIVE]);
    let count = wet_count();
    if gid.x >= count {
        return;
    }
    // Oldest first: the ring's front slots hold the newest decals, so they
    // go last.
    let i = (gid.x + atomicLoad(&map[WD_FRONT]) % count) % count;
    let source = wd_decal_word(half, i);
    let touch = atomicLoad(&map[source + WD_TOUCH]);
    if touch == 0u || params.now - wd_touch_time(touch) > params.keep {
        return;
    }
    let j = atomicAdd(&map[WD_NEXT_COUNT], 1u);
    let to = wd_decal_word(1u - half, j);
    for (var k = 0u; k < WD_DECAL_WORDS; k = k + 1u) {
        atomicStore(&map[to + k], atomicLoad(&map[source + k]));
    }
    let facing = atomicLoad(&map[source + WD_FACING]);
    let home = wet_home(wet_cell(wet_position(source), facing), facing);
    for (var probe = 0u; probe < WET_PROBES; probe = probe + 1u) {
        let slot = wd_hash_word((home + probe) & (WD_HASH_SLOTS - 1u));
        if atomicCompareExchangeWeak(&map[slot], 0u, j + 1u).exchanged {
            return;
        }
    }
}

@compute @workgroup_size(1)
fn cs_finish() {
    atomicStore(&map[WD_DRAW_VERTICES], atomicLoad(&map[WD_NEXT_COUNT]) * WD_DECAL_VERTICES);
    atomicStore(&map[WD_NEXT_COUNT], 0u);
    atomicStore(&map[WD_FRONT], 0u);
    atomicStore(&map[WD_ACTIVE], 1u - atomicLoad(&map[WD_ACTIVE]));
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

// The sand's distance at x the way `wet_solid_at` reads the solid's: a node
// is cell * (1 - 2 share) from the sand's surface, -1 cell packed, +1 cell
// with no grain.
fn wet_sand_at(x: vec3<f32>) -> WetSolid {
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
        let share = f32(atomicLoad(&sand_share[ts_cell_at(base + o)])) / SAND_FIXED;
        let v = params.cell * (1.0 - 2.0 * min(share, 1.0));
        value = value + wx * wy * wz * v;
        slope = slope + v * toward * vec3<f32>(wy * wz, wx * wz, wx * wy);
    }
    return WetSolid(value, slope / params.cell);
}

@compute @workgroup_size(64)
fn cs_sand_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x < ts_pool_len() {
        atomicStore(&sand_share[gid.x], 0u);
    }
}

@compute @workgroup_size(64)
fn cs_sand(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.sand {
        return;
    }
    let grain = sand[gid.x].position_radius;
    let r = grain.w / params.cell;
    if !(r > 0.0) {
        return;
    }
    let g = grain.xyz / params.cell + vec3<f32>(0.5);
    let base = vec3<i32>(floor(g));
    let home = ts_cell_at(base);
    // A grain whose own tile the display does not hold is far from the water.
    if home == ts_canon() {
        return;
    }
    ts_home(home);
    let f = g - floor(g);
    let share = SAND_FIXED * 4.18879 * r * r * r;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, corner >> 2u);
        let idx = ts_cell_at(base + vec3<i32>(o));
        if idx == ts_canon() {
            continue;
        }
        let w = select(1.0 - f, f, o == vec3<u32>(1u));
        atomicAdd(&sand_share[idx], u32(share * w.x * w.y * w.z + 0.5));
    }
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
    wet_mark(p, wet_solid_at(p.xyz));
}

@compute @workgroup_size(64)
fn cs_mark_sand(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    let p = particles[i].position_radius;
    if !(p.w > 0.0) {
        return;
    }
    var near = wet_solid_at(p.xyz);
    let grains = wet_sand_at(p.xyz);
    if grains.distance < near.distance {
        near = grains;
    }
    wet_mark(p, near);
}

// A particle at p (xyz, radius w) within two radii of the surface `near`
// describes wets the decal it touches.
fn wet_mark(p: vec4<f32>, near: WetSolid) {
    let steep = length(near.gradient);
    if near.distance > WET_REACH_RADII * p.w || !(steep > 0.0) {
        return;
    }
    // The point of the surface the particle touches: back along the solid's
    // normal by its distance.
    let normal = near.gradient / steep;
    let contact = p.xyz - near.distance * normal;
    let facing = wet_facing(normal);
    let cell = wet_cell(contact, facing);
    let touch = wd_touch_key(params.now);
    let half = atomicLoad(&map[WD_ACTIVE]);
    let home = wet_home(cell, facing);
    for (var probe = 0u; probe < WET_PROBES; probe = probe + 1u) {
        let slot = wd_hash_word((home + probe) & (WD_HASH_SLOTS - 1u));
        var held = atomicLoad(&map[slot]);
        if held == 0u {
            // Claim the empty slot, then start a decal behind it.
            let claim = atomicCompareExchangeWeak(&map[slot], 0u, WD_CLAIMING);
            if !claim.exchanged {
                held = claim.old_value;
            } else {
                let index = wet_new_slot();
                let word = wd_decal_word(half, index);
                let site = wet_site(contact, normal, facing);
                atomicStore(&map[word + WD_POSITION], bitcast<u32>(site.x));
                atomicStore(&map[word + WD_POSITION + 1u], bitcast<u32>(site.y));
                atomicStore(&map[word + WD_POSITION + 2u], bitcast<u32>(site.z));
                atomicStore(&map[word + WD_NORMAL], wd_normal_word(normal));
                atomicStore(&map[word + WD_TOUCH], touch);
                atomicStore(&map[word + WD_AMOUNT], params.hit);
                atomicStore(&map[word + WD_FACING], facing);
                atomicStore(&map[slot], index + 1u);
                return;
            }
        }
        // Another contact is writing this slot's decal this frame: the next
        // frame's contact wets it.
        if held == WD_CLAIMING {
            return;
        }
        // The slot's decal takes the touch when it has this contact's key, its
        // plane runs within a merge distance of the contact and it faces this
        // contact's way within about 45 degrees.
        if held != 0u {
            let word = wd_decal_word(half, held - 1u);
            let centre = wet_position(word);
            let same_key = atomicLoad(&map[word + WD_FACING]) == facing
                && all(wet_cell(centre, facing) == cell);
            let n = wd_normal(atomicLoad(&map[word + WD_NORMAL]));
            let on_plane = abs(dot(contact - centre, n)) < params.merge;
            if same_key && on_plane && dot(n, normal) > WET_MERGE_AGREE {
                atomicMax(&map[word + WD_TOUCH], touch);
                if atomicLoad(&map[word + WD_AMOUNT]) < WD_FULL {
                    atomicAdd(&map[word + WD_AMOUNT], params.hit);
                }
                return;
            }
        }
    }
}
