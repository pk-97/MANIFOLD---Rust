// Hand-fused ColorGrade reference (design build-sequence: the first REAL
// fusion target, after the Gain warm-up). The ColorGrade preset is a 7-atom
// pointwise run — gain -> saturation -> hue_saturation -> contrast -> colorize
// -> mix(with the original source) -> clamp — folded into ONE kernel: read the
// source once, run the whole chain in f32 registers, write once.
//
// Every stage is a VERBATIM transcription of its atom's shader (value-level
// parity — gain.wgsl / saturation.wgsl / hue_saturation.wgsl / contrast.wgsl /
// colorize.wgsl / mix.wgsl / clamp_texture.wgsl). The two HSV helpers are
// bit-identical between hue_saturation and colorize, so one copy serves both.
// The ONLY intended divergence from the unfused chain is precision: the chain
// rounds RGB to f16 after every pass; this rounds once on write. The oracle's
// two-sided tolerance absorbs that drift (design §11.D).
//
// Source is read with textureLoad (exact texel); the atoms sample at pixel
// centers, which returns the identical texel for a same-dimension texture.

struct U {
    gain: f32,        // node.gain
    sat_s: f32,       // node.saturation (luma lerp)
    hue_deg: f32,     // node.hue_saturation
    sat_h: f32,
    val_h: f32,
    contrast: f32,    // node.contrast
    col_amount: f32,  // node.colorize
    col_hue: f32,
    col_sat: f32,
    col_focus: f32,
    mix_amount: f32,  // node.mix
    mix_mode: u32,
    clamp_min: f32,   // node.clamp_texture
    clamp_max: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

// Sam Hocevar branchless RGB<->HSV — identical in hue_saturation.wgsl and
// colorize.wgsl, transcribed once.
fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.b, c.g, K.w, K.z), vec4<f32>(c.g, c.b, K.x, K.y), step(c.b, c.g));
    let q = mix(vec4<f32>(p.x, p.y, p.w, c.r), vec4<f32>(c.r, p.y, p.z, p.x), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x, c.x, c.x) + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

fn overlay_channel(a: f32, b: f32) -> f32 {
    if a < 0.5 {
        return 2.0 * a * b;
    }
    return 1.0 - 2.0 * (1.0 - a) * (1.0 - b);
}

fn safe_div(a: f32, b: f32) -> f32 {
    if abs(b) < 1.0e-6 {
        return 0.0;
    }
    return a / b;
}

fn blend_rgb(a: vec3<f32>, b: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 0u: { return b; }
        case 1u: { return 1.0 - (1.0 - a) * (1.0 - b); }
        case 2u: { return a + b; }
        case 3u: { return max(a, b); }
        case 4u: { return a * b; }
        case 5u: { return abs(a - b); }
        case 6u: {
            return vec3<f32>(
                overlay_channel(a.x, b.x),
                overlay_channel(a.y, b.y),
                overlay_channel(a.z, b.z),
            );
        }
        case 7u: {
            return vec3<f32>(safe_div(a.x, b.x), safe_div(a.y, b.y), safe_div(a.z, b.z));
        }
        default: { return b; }
    }
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(dst);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let coord = vec2<i32>(i32(id.x), i32(id.y));
    let src = textureLoad(src_tex, coord, 0);
    let src_rgb = src.rgb;

    // node.gain — rgb * gain.
    var c = src_rgb * u.gain;

    // node.saturation — lerp luma grayscale <-> colour.
    let luma_s = dot(c, LUMA);
    c = mix(vec3<f32>(luma_s), c, u.sat_s);

    // node.hue_saturation — HSV rotate/scale (clamp negatives before HSV).
    var hsv = rgb2hsv(max(c, vec3<f32>(0.0)));
    hsv.x = fract(hsv.x + u.hue_deg / 360.0);
    hsv.y = clamp(hsv.y * u.sat_h, 0.0, 1.0);
    hsv.z = hsv.z * u.val_h;
    c = hsv2rgb(hsv);

    // node.contrast — pivot around 0.5.
    c = (c - vec3<f32>(0.5)) * u.contrast + vec3<f32>(0.5);

    // node.colorize — tint toward a hue, masked by brightness*neutrality*focus.
    {
        let colorize = clamp(u.col_amount, 0.0, 1.0);
        let tint_h = fract(u.col_hue / 360.0);
        let tint_hsv = vec3<f32>(tint_h, clamp(u.col_sat, 0.0, 1.0), 1.0);
        let tint_rgb = hsv2rgb(tint_hsv);
        let graded_luma = dot(c, LUMA);
        let graded_sat = rgb2hsv(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))).y;
        let highlight_mask = smoothstep(0.18, 0.95, graded_luma);
        let neutral_mask = 1.0 - smoothstep(0.10, 0.80, graded_sat);
        let focus = clamp(u.col_focus, 0.0, 1.0);
        let element_mask = mix(1.0, highlight_mask * neutral_mask, focus);
        let tinted = tint_rgb * graded_luma;
        c = mix(c, tinted, colorize * element_mask);
    }

    // node.mix — a = original source, b = graded chain; blend then crossfade.
    let blended = blend_rgb(src_rgb, c, u.mix_mode);
    c = mix(src_rgb, blended, u.mix_amount);

    // node.clamp_texture — saturate to [min, max].
    c = clamp(c, vec3<f32>(u.clamp_min), vec3<f32>(u.clamp_max));

    textureStore(dst, coord, vec4<f32>(c, src.a));
}
