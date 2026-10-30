// node.motion_blur pass 3: per-pixel gather along the tile neighbourhood's
// longest smear (Jimenez 2014, "Next Generation Post Processing in Call of
// Duty: Advanced Warfare").
//
// Taps sit in mirrored pairs at +/- offset. Odd pairs follow the pixel's own
// smear; even pairs follow the neighbourhood's longest smear when the pixel
// is slow beside it, turning to the pixel's own as it nears that speed. A
// pixel whose motion differs from the fastest thing nearby still streaks its
// own way.
// Each tap weighs in by depth: a nearer tap counts where its own smear
// reaches this pixel (a moving foreground covers the background), and a
// farther tap counts where this pixel's smear reaches it (a moving
// foreground turns see-through at its edges). Uncovered weight keeps the
// centre colour, so a still pixel next to a moving object only takes the
// share of the shutter the object covers it.
//
// Where the whole neighbourhood moves as one (a camera turn, a distant pan)
// every tap would weigh the same, so the pixel averages plain colour taps
// along its own smear and skips the depth and smear reads.
//
// A fixed per-pixel offset staggers the taps so a long smear reads as a
// smooth streak, not stacked copies, and shifts each pixel's tile lookup by
// up to a quarter tile so tile edges dissolve instead of drawing a grid. It
// never changes between frames, so the result doesn't flicker.
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var in_tex: texture_2d<f32>;
@group(0) @binding(2) var velocity_tex: texture_2d<f32>;
@group(0) @binding(3) var prep: texture_2d<f32>;
@group(0) @binding(4) var neighbors: texture_2d<f32>;
@group(0) @binding(5) var samp: sampler;
@group(0) @binding(6) var output_tex: texture_storage_2d<rgba16float, write>;

// Most tap pairs per pixel. Short smears use fewer, about one pair per 2 px.
const MAX_PAIRS: u32 = 16u;
// Relative depth gap (fraction of the nearer depth) at which a tap counts as
// fully in front or behind: 0.5 / 20 = 2.5%. Relative, so any scene scale works.
const DEPTH_SCALE: f32 = 20.0;
// A neighbourhood moves as one when its shortest smear is at least this
// share of its longest.
const UNIFORM_SHARE: f32 = 0.9;

// x: the tap is behind the centre and the centre's smear reaches it.
// y: the tap is in front and its own smear reaches the centre.
// Equal depths split evenly, so a surface moving as one averages its taps.
// `ic`/`is` are inverse depths: (zs - zc) / min(zc, zs) = (ic - is) / min(ic, is).
fn tap_weight(ic: f32, is: f32, offset: f32, sc: f32, ss: f32) -> f32 {
    let rel = (ic - is) / min(ic, is);
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
    let jitter = pixel_jitter(id.xy) - 0.5;
    let shift = vec2<f32>(jitter, pixel_jitter(id.yx + vec2<u32>(17u, 31u)) - 0.5) * f32(TILE) * 0.5;
    let ndims = textureDimensions(neighbors);
    let tile = clamp(vec2<i32>(floor((vec2<f32>(id.xy) + shift) / f32(TILE))), vec2<i32>(0), vec2<i32>(ndims) - 1);
    let n = textureLoad(neighbors, tile, 0);
    let vmax = n.rg;
    let vmax_len = length(vmax);
    // Nothing near this pixel moves a visible amount.
    if vmax_len < 0.5 {
        textureStore(output_tex, vec2<i32>(id.xy), center);
        return;
    }
    let own_ndc = textureLoad(velocity_tex, own_texel(uv, textureDimensions(velocity_tex)), 0).rg;
    let own = spread(own_ndc, u.dims, u.shutter_scale, u.max_blur_px);
    let sc = length(own);

    if n.b >= vmax_len * UNIFORM_SHARE {
        let pairs = clamp(u32(ceil(sc * 0.5)), 1u, MAX_PAIRS);
        let step_uv = own / u.dims;
        var acc = vec4<f32>(0.0);
        for (var i = 0u; i < pairs; i++) {
            let t = (f32(i) + 0.5 + jitter) / f32(pairs);
            acc += textureSampleLevel(in_tex, samp, uv + step_uv * t, 0.0);
            acc += textureSampleLevel(in_tex, samp, uv - step_uv * t, 0.0);
        }
        textureStore(output_tex, vec2<i32>(id.xy), acc / f32(2u * pairs));
        return;
    }

    let pdims = textureDimensions(prep);
    let ic = textureLoad(prep, vec2<i32>(id.xy), 0).g;
    // Taps are mirrored, so a smear and its reverse sample the same line:
    // flip the pixel's own direction onto the longest smear's side. Even
    // pairs turn from the longest smear toward the pixel's own as its share
    // grows, so a pixel moving nearly as fast never takes a neighbour's
    // direction, and the switch between tiles' longest smears leaves no seam.
    let max_dir = vmax / vmax_len;
    let own_unit = select(max_dir, own / sc, sc >= 0.5);
    let own_dir = select(own_unit, -own_unit, dot(own_unit, max_dir) < 0.0);
    let even_dir = normalize(mix(max_dir, own_dir, smoothstep(0.25, 0.75, sc / vmax_len)));
    let even_step = even_dir / u.dims;
    let odd_step = own_dir / u.dims;
    let pairs = clamp(u32(ceil(vmax_len * 0.5)), 1u, MAX_PAIRS);

    var acc = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = 0u; i < pairs; i++) {
        let offset = (f32(i) + 0.5 + jitter) / f32(pairs) * vmax_len;
        let step_uv = select(even_step, odd_step, (i & 1u) == 1u) * offset;
        let uv1 = uv + step_uv;
        let uv2 = uv - step_uv;
        let t1 = textureLoad(prep, own_texel(uv1, pdims), 0).rg;
        let t2 = textureLoad(prep, own_texel(uv2, pdims), 0).rg;
        var w1 = tap_weight(ic, t1.y, offset, sc, t1.x);
        var w2 = tap_weight(ic, t2.y, offset, sc, t2.x);
        // The pair takes the weight of its nearer, faster tap. This rebuilds
        // the background a moving foreground uncovers on its trailing side.
        // Nearer means the larger inverse depth.
        let mirror = vec2<bool>(t1.y < t2.y, t2.x > t1.x);
        w1 = select(w1, w2, all(mirror));
        w2 = select(w1, w2, any(mirror));
        acc += w1 * textureSampleLevel(in_tex, samp, uv1, 0.0);
        acc += w2 * textureSampleLevel(in_tex, samp, uv2, 0.0);
        total += w1 + w2;
    }
    let n_taps = f32(2u * pairs);
    textureStore(output_tex, vec2<i32>(id.xy), acc / n_taps + (1.0 - total / n_taps) * center);
}
