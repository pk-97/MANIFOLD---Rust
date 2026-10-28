// The wet map's helpers (`manifold_node_engine::scene::wet_map`), shared by
// the node that marks it (`node.wet_map`) and the scene's wet pass that draws
// it. Composed after the layout constants `wet_map::layout()` writes.
//
// One array of u32 words: a header, a merge hash of WD_HASH_SLOTS slots, then
// two halves of WD_CAPACITY decals of WD_DECAL_WORDS words; the header names
// the half in use. The header's first four words are Metal's draw arguments
// for the decals in use (six vertices, the count, 0, 0), so the wet pass
// draws them with no count on the CPU. A hash slot holds a decal's index + 1,
// 0 when empty, WD_CLAIMING while a contact writes the decal it claimed.
//
// A decal is a soft round wet mark where the water touched a surface: its
// object-space centre (xyz), its surface normal (octahedral, two snorm16),
// the display time of its last touch (touch key) and how much water hit it
// (WD_HIT a contact, full at WD_FULL).

const WD_DRAW_VERTICES: u32 = 0u;
const WD_COUNT: u32 = 1u;
const WD_NOW: u32 = 4u;
const WD_DRYING: u32 = 5u;
const WD_ACTIVE: u32 = 6u;
const WD_REFUSED: u32 = 7u;
const WD_NEXT_COUNT: u32 = 8u;
const WD_RADIUS: u32 = 9u;
const WD_MERGE: u32 = 10u;

const WD_POSITION: u32 = 0u;
const WD_NORMAL: u32 = 3u;
const WD_TOUCH: u32 = 4u;
const WD_AMOUNT: u32 = 5u;

const WD_CLAIMING: u32 = 0xffffffffu;
// One contact wets a decal a quarter; four make it full.
const WD_HIT: u32 = 64u;
const WD_FULL: u32 = 256u;

// The first word of hash slot `slot`.
fn wd_hash_word(slot: u32) -> u32 {
    return WD_HEADER_WORDS + slot;
}

// The first word of decal `index` in half `half`.
fn wd_decal_word(half: u32, index: u32) -> u32 {
    return WD_HEADER_WORDS + WD_HASH_SLOTS + (half * WD_CAPACITY + index) * WD_DECAL_WORDS;
}

// The hash slot a merge cell starts probing at (Teschner et al. 2003).
fn wd_cell_slot(cell: vec3<i32>) -> u32 {
    let h = (u32(cell.x) * 73856093u) ^ (u32(cell.y) * 19349663u) ^ (u32(cell.z) * 83492791u);
    return h & (WD_HASH_SLOTS - 1u);
}

// The touch key of a display time: its f32 bits plus one, so 0 stays "never".
// Non-negative floats order like their bits; time below zero marks as zero.
fn wd_touch_key(now: f32) -> u32 {
    return bitcast<u32>(max(now, 0.0)) + 1u;
}

fn wd_touch_time(key: u32) -> f32 {
    return bitcast<f32>(key - 1u);
}

// A unit normal as one word: octahedral (Meyer et al. 2010), two snorm16.
fn wd_normal_word(n: vec3<f32>) -> u32 {
    let p = n.xy / (abs(n.x) + abs(n.y) + abs(n.z));
    let sign_xy = select(vec2<f32>(-1.0), vec2<f32>(1.0), p >= vec2<f32>(0.0));
    let folded = select(p, (vec2<f32>(1.0) - abs(p.yx)) * sign_xy, n.z < 0.0);
    return pack2x16snorm(folded);
}

fn wd_normal(word: u32) -> vec3<f32> {
    let f = unpack2x16snorm(word);
    var n = vec3<f32>(f, 1.0 - abs(f.x) - abs(f.y));
    let t = max(-n.z, 0.0);
    n.x = n.x + select(t, -t, n.x >= 0.0);
    n.y = n.y + select(t, -t, n.y >= 0.0);
    return normalize(n);
}

// How wet a decal is, 0 to 1: how much water hit it, dried by
// exp(-age * drying / WD_DRY_SECONDS) since its last touch.
fn wd_strength(touch: u32, amount: u32, now: f32, drying: f32) -> f32 {
    if touch == 0u {
        return 0.0;
    }
    let age = max(now - wd_touch_time(touch), 0.0);
    return min(f32(amount) / f32(WD_FULL), 1.0) * exp(-age * drying / WD_DRY_SECONDS);
}

// A decal's soft edge: full out to half its radius, then a smoothstep to 0 at
// its radius. Contacts leave decals at most about 0.7 merge distances from any
// point of a wetted patch and the radius is 1.5 of them, so overlapping decals
// read as one even film with a soft rim, not a lattice of dots.
fn wd_falloff(distance: f32, radius: f32) -> f32 {
    return 1.0 - smoothstep(0.5 * radius, radius, distance);
}
