// node.motion_blur pass 3: per-pixel gather along the tile neighbourhood's
// longest smear (Jimenez 2014, "Next Generation Post Processing in Call of
// Duty: Advanced Warfare").
//
// Taps sit in mirrored pairs at +/- offset along that smear. Each tap weighs
// in by depth: a nearer tap counts where its own smear reaches this pixel (a
// moving foreground covers the background), and a farther tap counts where
// this pixel's smear reaches it (a moving foreground turns see-through at its
// edges). Uncovered weight keeps the centre colour, so a still pixel next to
// a moving object only takes the share of the shutter the object covers it.
//
// A fixed per-pixel offset staggers the taps so a long smear reads as a
// smooth streak, not stacked copies. It never changes between frames, so the
// result doesn't flicker.
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var in_tex: texture_2d<f32>;
@group(0) @binding(2) var velocity_tex: texture_2d<f32>;
@group(0) @binding(3) var depth_tex: texture_2d<f32>;
@group(0) @binding(4) var neighbors: texture_2d<f32>;
@group(0) @binding(5) var samp: sampler;
@group(0) @binding(6) var output_tex: texture_storage_2d<rgba16float, write>;

// Most tap pairs per pixel. Short smears use fewer, about one pair per 2 px.
const MAX_PAIRS: u32 = 8u;
// Relative depth gap (fraction of the nearer depth) at which a tap counts as
// fully in front or behind: 0.5 / 20 = 2.5%. Relative, so any scene scale works.
const DEPTH_SCALE: f32 = 20.0;

fn linear_depth(uv: vec2<f32>) -> f32 {
    let raw = textureLoad(depth_tex, own_texel(uv, textureDimensions(depth_tex)), 0).r;
    return linearize_depth(raw, u.near, u.far);
}

fn spread_len(uv: vec2<f32>) -> f32 {
    let ndc = textureLoad(velocity_tex, own_texel(uv, textureDimensions(velocity_tex)), 0).rg;
    return length(spread(ndc, u.dims, u.shutter_scale, u.max_blur_px));
}

// x: the tap is behind the centre and the centre's smear reaches it.
// y: the tap is in front and its own smear reaches the centre.
// Equal depths split evenly, so a surface moving as one averages its taps.
fn tap_weight(zc: f32, zs: f32, offset: f32, sc: f32, ss: f32) -> f32 {
    let rel = (zs - zc) / min(zc, zs);
    let depth_cmp = clamp(vec2<f32>(0.5) + vec2<f32>(DEPTH_SCALE, -DEPTH_SCALE) * rel, vec2<f32>(0.0), vec2<f32>(1.0));
    let spread_cmp = clamp(vec2<f32>(sc, ss) - offset + 1.0, vec2<f32>(0.0), vec2<f32>(1.0));
    return dot(depth_cmp, spread_cmp);
}

// Interleaved gradient noise in [0, 1).
fn pixel_jitter(p: vec2<u32>) -> f32 {
    return fract(52.9829189 * fract(dot(vec2<f32>(p), vec2<f32>(0.06711056, 0.00583715))));
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if any(id.xy >= dims) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / u.dims;
    let center = textureSampleLevel(in_tex, samp, uv, 0.0);
    let ndims = textureDimensions(neighbors);
    let vmax = textureLoad(neighbors, vec2<i32>(min(id.xy / TILE, ndims - 1u)), 0).rg;
    let vmax_len = length(vmax);
    // Nothing near this pixel moves a visible amount.
    if vmax_len < 0.5 {
        textureStore(output_tex, vec2<i32>(id.xy), center);
        return;
    }
    let zc = linear_depth(uv);
    let sc = spread_len(uv);
    let step_uv = vmax / vmax_len / u.dims;
    let pairs = clamp(u32(ceil(vmax_len * 0.5)), 1u, MAX_PAIRS);
    let jitter = pixel_jitter(id.xy) - 0.5;

    var acc = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = 0u; i < pairs; i++) {
        let offset = (f32(i) + 0.5 + jitter) / f32(pairs) * vmax_len;
        let uv1 = uv + step_uv * offset;
        let uv2 = uv - step_uv * offset;
        let z1 = linear_depth(uv1);
        let z2 = linear_depth(uv2);
        let s1 = spread_len(uv1);
        let s2 = spread_len(uv2);
        var w1 = tap_weight(zc, z1, offset, sc, s1);
        var w2 = tap_weight(zc, z2, offset, sc, s2);
        // The pair takes the weight of its nearer, faster tap. This rebuilds
        // the background a moving foreground uncovers on its trailing side.
        let mirror = vec2<bool>(z1 > z2, s2 > s1);
        w1 = select(w1, w2, all(mirror));
        w2 = select(w1, w2, any(mirror));
        acc += w1 * textureSampleLevel(in_tex, samp, uv1, 0.0);
        acc += w2 * textureSampleLevel(in_tex, samp, uv2, 0.0);
        total += w1 + w2;
    }
    let n = f32(2u * pairs);
    textureStore(output_tex, vec2<i32>(id.xy), acc / n + (1.0 - total / n) * center);
}
