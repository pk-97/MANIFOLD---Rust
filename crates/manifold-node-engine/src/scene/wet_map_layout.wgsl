// The wet map's layout (`manifold_node_engine::scene::wet_map`), shared by the
// node that marks it (`node.wet_map`) and the look that reads it
// (`look_wet_surface.wgsl`). One array of u32 words over the water's
// object-space XZ: an eight-word header, then four words a texel, row-major
// along x.
//
// Header: origin x and z (f32 bits, the low corner of texel 0), the texel
// size (f32 bits, 0 while the map has not been placed), the side in texels,
// the display time of the last mark (f32 bits) and the drying rate (f32 bits).
// Texel: the touch key, the lowest height touched (inverted height key), the
// highest height touched (height key), and the heights touched as bits, one
// bin of WM_HEIGHT_MARGIN_TEXELS texels a bit, wrapping every 16 bins, walls
// in the low half and up-facing surfaces in the high half. A
// zeroed texel is untouched, so dry. A touch stamps the WM_STAMP x WM_STAMP
// texels round the point it touched, so the bilinear read is full over it
// and fades over one texel past it.

const WM_HEADER_WORDS: u32 = 8u;
const WM_TEXEL_WORDS: u32 = 4u;
const WM_ORIGIN_X: u32 = 0u;
const WM_ORIGIN_Z: u32 = 1u;
const WM_TEXEL: u32 = 2u;
const WM_SIDE: u32 = 3u;
const WM_NOW: u32 = 4u;
const WM_DRYING: u32 = 5u;
// A touch stamps this many texels a side, centred on the texel it lands in.
const WM_STAMP: i32 = 3;

// A floor the water left dries to 1/e in this many seconds at Drying 1, so a
// splash fades in about half a minute on stage. A real 0.1 mm film takes about
// half an hour (FAO-56 evaporation), which reads as never drying.
const WM_DRY_SECONDS: f32 = 15.0;

// The height bins, and how far past a touched height wetness fades out, in
// texels.
const WM_HEIGHT_MARGIN_TEXELS: f32 = 2.0;

// The touch key of a display time: its f32 bits plus one, so 0 stays "never".
// Non-negative floats order like their bits; time below zero marks as zero.
fn wm_touch_key(now: f32) -> u32 {
    return bitcast<u32>(max(now, 0.0)) + 1u;
}

fn wm_touch_time(key: u32) -> f32 {
    return bitcast<f32>(key - 1u);
}

// A height as a u32 that orders like the float: positive floats get the sign
// bit set, negative floats are inverted.
fn wm_height_key(y: f32) -> u32 {
    let bits = bitcast<u32>(y);
    return select(bits | 0x80000000u, ~bits, (bits & 0x80000000u) != 0u);
}

fn wm_height(key: u32) -> f32 {
    return bitcast<f32>(select(~key, key & 0x7fffffffu, (key & 0x80000000u) != 0u));
}

// The first word of texel t.
fn wm_texel_word(t: vec2<u32>, side: u32) -> u32 {
    return WM_HEADER_WORDS + WM_TEXEL_WORDS * (t.y * side + t.x);
}

// A surface faces up when its normal is within 60 degrees of up: floors,
// tops and ramps. Up-facing touches and the rest keep separate height bins,
// so water landing on a box's top leaves the face under its edge dry.
const WM_UP_FACING: f32 = 0.5;

// The bit of height y's bin, bins `bin` metres tall wrapping every 16: the
// low half of the word for touches on walls, the high half for touches on
// up-facing surfaces.
fn wm_height_bit(y: f32, bin: f32, up: bool) -> u32 {
    return (1u << u32(i32(floor(y / bin)) & 15)) << select(0u, 16u, up);
}

// How much a texel speaks for height y on a surface facing up or not, 0 to 1,
// fading over `margin` (the bin height) so no edge is a step: full between
// its lowest and highest touched heights, fading to nothing `margin` past
// them; and full within a bin of a touched bin of the surface's kind, read
// linearly between bin centres.
fn wm_height_weight(low: u32, high: u32, bins: u32, y: f32, margin: f32, up: bool) -> f32 {
    let outside = max(wm_height(~low) - y, y - wm_height(high));
    let band = clamp(1.0 - outside / margin, 0.0, 1.0);
    let kind = (bins >> select(0u, 16u, up)) & 0xffffu;
    let near = (kind | (kind << 1u) | (kind >> 15u) | (kind >> 1u) | (kind << 15u)) & 0xffffu;
    let q = y / margin - 0.5;
    let below = floor(q);
    let i = u32(i32(below) & 15);
    let lower = f32((near >> i) & 1u);
    let upper = f32((near >> ((i + 1u) & 15u)) & 1u);
    return band * mix(lower, upper, q - below);
}

// A touched texel's wetness, 0 to 1: exp(-age * drying / WM_DRY_SECONDS)
// since the last touch.
fn wm_wetness(touch: u32, now: f32, drying: f32) -> f32 {
    let age = max(now - wm_touch_time(touch), 0.0);
    return exp(-age * drying / WM_DRY_SECONDS);
}
