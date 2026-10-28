// The wet map: soft wet marks (decals) where the water touched a surface,
// kept between frames (composed after the world-tile read side and the map's
// layout, `manifold_node_engine::scene::wet_map::layout()`).
//
// Passes, a barrier between each:
//   cs_begin       one thread: the draw arguments, time, drying, radius and
//                  merge distance into the header
//   cs_clear_hash  a thread a hash slot, before a compaction: every slot empty
//   cs_compact     a thread a decal: a decal touched within `keep` seconds
//                  moves to the other half and back into the hash
//   cs_finish      one thread: the other half becomes the one in use
//   cs_mark        a thread a particle: a particle within two radii of a solid
//                  finds the decal at the surface point it touches through the
//                  hash and wets it again, or starts a new one
// Drying is computed where a decal is drawn, from the age of its last touch,
// so nothing runs over the decals while the water is still.

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

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// A particle marks the surface it is within this many radii of.
const WET_REACH_RADII: f32 = 2.0;
// The solid reads 1e30 in a tile no slot holds; capped so a trilinear mix
// stays finite.
const WET_SOLID_FAR: f32 = 1.0e4;
// Hash slots a contact or a compacted decal probes from its cell's slot.
const WET_PROBES: u32 = 8u;
// A contact wets a decal whose normal is within about 45 degrees of its own.
const WET_MERGE_AGREE: f32 = 0.7;

@compute @workgroup_size(1)
fn cs_begin() {
    atomicStore(&map[WD_DRAW_VERTICES], 6u);
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

// The merge cell holding point x.
fn wet_cell(x: vec3<f32>) -> vec3<i32> {
    return vec3<i32>(floor(x / params.merge));
}

@compute @workgroup_size(64)
fn cs_compact(@builtin(global_invocation_id) gid: vec3<u32>) {
    let half = atomicLoad(&map[WD_ACTIVE]);
    let i = gid.x;
    if i >= min(atomicLoad(&map[WD_COUNT]), WD_CAPACITY) {
        return;
    }
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
    let home = wd_cell_slot(wet_cell(wet_position(source)));
    for (var probe = 0u; probe < WET_PROBES; probe = probe + 1u) {
        let slot = wd_hash_word((home + probe) & (WD_HASH_SLOTS - 1u));
        if atomicCompareExchangeWeak(&map[slot], 0u, j + 1u).exchanged {
            return;
        }
    }
}

@compute @workgroup_size(1)
fn cs_finish() {
    atomicStore(&map[WD_COUNT], atomicLoad(&map[WD_NEXT_COUNT]));
    atomicStore(&map[WD_NEXT_COUNT], 0u);
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
    let near = wet_solid_at(p.xyz);
    let steep = length(near.gradient);
    if near.distance > WET_REACH_RADII * p.w || !(steep > 0.0) {
        return;
    }
    // The point of the surface the particle touches: back along the solid's
    // normal by its distance.
    let normal = near.gradient / steep;
    let contact = p.xyz - near.distance * normal;
    let touch = wd_touch_key(params.now);
    let half = atomicLoad(&map[WD_ACTIVE]);
    let home = wd_cell_slot(wet_cell(contact));
    for (var probe = 0u; probe < WET_PROBES; probe = probe + 1u) {
        let slot = wd_hash_word((home + probe) & (WD_HASH_SLOTS - 1u));
        var held = atomicLoad(&map[slot]);
        if held == 0u {
            // Claim the empty slot, then start a decal behind it.
            let claim = atomicCompareExchangeWeak(&map[slot], 0u, WD_CLAIMING);
            if !claim.exchanged {
                held = claim.old_value;
            } else {
                let index = atomicAdd(&map[WD_COUNT], 1u);
                if index >= WD_CAPACITY {
                    // Full: the count goes back to what the halves hold.
                    atomicSub(&map[WD_COUNT], 1u);
                    atomicAdd(&map[WD_REFUSED], 1u);
                    atomicStore(&map[slot], 0u);
                    return;
                }
                let word = wd_decal_word(half, index);
                atomicStore(&map[word + WD_POSITION], bitcast<u32>(contact.x));
                atomicStore(&map[word + WD_POSITION + 1u], bitcast<u32>(contact.y));
                atomicStore(&map[word + WD_POSITION + 2u], bitcast<u32>(contact.z));
                atomicStore(&map[word + WD_NORMAL], wd_normal_word(normal));
                atomicStore(&map[word + WD_TOUCH], touch);
                atomicStore(&map[word + WD_AMOUNT], WD_HIT);
                atomicStore(&map[slot], index + 1u);
                return;
            }
        }
        // Another contact is writing this slot's decal this frame: the next
        // frame's contact wets it.
        if held == WD_CLAIMING {
            return;
        }
        if held != 0u {
            let word = wd_decal_word(half, held - 1u);
            let close = distance(wet_position(word), contact) < params.merge;
            if close && dot(wd_normal(atomicLoad(&map[word + WD_NORMAL])), normal) > WET_MERGE_AGREE {
                atomicMax(&map[word + WD_TOUCH], touch);
                if atomicLoad(&map[word + WD_AMOUNT]) < WD_FULL {
                    atomicAdd(&map[word + WD_AMOUNT], WD_HIT);
                }
                return;
            }
        }
    }
}
