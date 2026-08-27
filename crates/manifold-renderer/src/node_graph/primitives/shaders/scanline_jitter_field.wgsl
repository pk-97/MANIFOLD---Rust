// node.scanline_jitter_field — per-row random horizontal-offset field.
// Pure generator (reads its own dims). Emits one texture:
//   offset_out : R/G = signed UV shift per scanline row/band, gated
//                so only a fraction of rows tear (A=1). Slide mode also
//                spreads bands across the slice axis (Shear: shifted
//                windows; Split: translated strips) and writes B as the
//                coverage mask (1 = covered, 0 = Split-mode gap). Feed
//                into node.remap (Relative mode), alone or summed with
//                other offset fields.
//
// Scanline-jitter math is verbatim from the old fused fx_glitch /
// node.glitch_displace, except the row index is taken from the original
// uv.y (the fused pass took it from the block-displaced uv.y — a weak
// coupling now dropped so each field is a pure function of the source UV).

struct Uniforms {
    amount: f32,
    scanline: f32,
    speed: f32,
    motion: i32,
    bands: f32,
    spread: f32,
    spread_mode: i32,
    angle: f32,
    time: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var offset_out: texture_storage_2d<rgba16float, write>;

fn hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

// Value-noise field (123.34/456.21/45.32 hash) — smooth, in [0,1]. Matches
// node.noise's Value type; drives Slide.
fn value_hash(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p = p + dot(p, p + 45.32);
    return fract(p.x * p.y);
}
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let uu = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(value_hash(i),                       value_hash(i + vec2<f32>(1.0, 0.0)), uu.x),
        mix(value_hash(i + vec2<f32>(0.0, 1.0)), value_hash(i + vec2<f32>(1.0, 1.0)), uu.x),
        uu.y
    );
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(offset_out);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let res = vec2<f32>(dims);

    if u.motion == 1 {
        // Slide — smooth, ungated, every band drifts. speed=2 → website 0.13.
        // bands = 0 → no rows (offset 0, full coverage): a downstream
        // flow/domain warp carries the motion instead of slicing the image
        // into per-row tears.
        if u.bands <= 0.0 {
            textureStore(offset_out, vec2<i32>(id.xy), vec4<f32>(0.0, 0.0, 1.0, 1.0));
            return;
        }
        // The whole slice frame rotates by `angle` degrees around the canvas
        // centre: bands quantise across the rotated y, the slide runs along
        // the rotated x, the spread pushes across the rotated y.
        let rad = u.angle * 0.01745329;
        let ca = cos(rad);
        let sa = sin(rad);
        let cuv = uv - vec2<f32>(0.5, 0.5);
        let ruv = vec2<f32>(ca * cuv.x + sa * cuv.y, -sa * cuv.x + ca * cuv.y) + vec2<f32>(0.5, 0.5);
        let t = u.time * u.speed * 0.065;
        var band = floor(ruv.y * u.bands);
        var offset_y = 0.0;
        var coverage = 1.0;
        if u.spread_mode == 1 {
            // Split — translate the strips themselves apart from the centre;
            // gaps emit coverage 0 so a downstream masked_mix composites
            // them as transparent. u inverts the strip translation: output
            // row ruv.y pulls the strip content from u.
            let su = (ruv.y + 0.5 * u.spread) / (1.0 + u.spread);
            band = floor(su * u.bands);
            let f = fract(su * u.bands);
            let edge = 0.5 * u.spread / (1.0 + u.spread);
            coverage = step(edge, f) * step(f, 1.0 - edge);
            offset_y = su - ruv.y;
        } else {
            // Shear — resample each band's window shifted across the slice
            // axis; the gap between adjacent bands is `spread` band-heights.
            offset_y = ((band + 0.5) / u.bands - 0.5) * u.spread;
        }
        let n = value_noise(vec2<f32>(band, t));
        let offset_x = (n - 0.5) * u.amount * 0.05;
        let off = vec2<f32>(ca * offset_x - sa * offset_y, sa * offset_x + ca * offset_y);
        textureStore(offset_out, vec2<i32>(id.xy), vec4<f32>(off.x, off.y, coverage, 1.0));
        return;
    }

    // Tear (default) — byte-identical to the original VHS jolt.
    let t = floor(u.time * u.speed * 12.0);
    let scanline_row = floor(uv.y * res.y);
    let scan_hash = hash1(scanline_row + t * 7.31);
    let scan_mask = step(1.0 - u.scanline * u.amount * 0.3, scan_hash);
    let scan_shift = (hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * u.amount * 0.08;
    let offset_x = scan_shift * scan_mask;

    textureStore(offset_out, vec2<i32>(id.xy), vec4<f32>(offset_x, 0.0, 1.0, 1.0));
}
