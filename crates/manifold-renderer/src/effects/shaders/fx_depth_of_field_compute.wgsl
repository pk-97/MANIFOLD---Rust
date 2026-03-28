// Depth of Field — 3 modes (Tilt-Shift, Radial, Depth), 4 compute passes.
//
// Pass 0 (mode 0): CoC generation + bilinear downsample to half-res.
//                   CoC stored in alpha channel.
// Pass 1 (mode 1): Horizontal separable Gaussian blur at half-res.
//                   Variable-width kernel driven by CoC alpha.
// Pass 2 (mode 2): Vertical separable Gaussian blur at half-res.
//                   Variable-width kernel driven by CoC alpha.
// Pass 3 (mode 3): Composite — upsample blurred half-res, blend with
//                   sharp full-res original using recomputed CoC.

struct Uniforms {
    mode: u32,           // 0=CoC+Down, 1=HBlur, 2=VBlur, 3=Composite
    focus_mode: u32,     // 0=TiltShift, 1=Radial, 2=Depth
    amount: f32,         // overall effect intensity (dry/wet)
    focus_y: f32,        // focus Y position [0,1] (tilt-shift & depth)
    focus_x: f32,        // focus X position [0,1] (radial center)
    focus_width: f32,    // in-focus band/radius [0.01, 0.5]
    blur_strength: f32,  // max blur kernel spread [0,1]
    tilt_angle: f32,     // rotation in radians (tilt-shift only)
    quality: u32,        // 0=9tap, 1=17tap, 2=25tap
    texel_size_x: f32,   // 1.0 / source width
    texel_size_y: f32,   // 1.0 / source height
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex_a: texture_2d<f32>;
@group(0) @binding(2) var source_tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// ─── Gaussian kernel weights ──────────────────────────────────────────
// Precomputed normalized Gaussian kernels for 3 quality levels.
// All kernels are symmetric — we store center + positive side only.

// Quality 0: 9-tap (sigma ≈ 2.0)
const K9_0: f32 = 0.16501;
const K9_1: f32 = 0.15019;
const K9_2: f32 = 0.11325;
const K9_3: f32 = 0.07076;
const K9_4: f32 = 0.03664;

// Quality 1: 17-tap (sigma ≈ 4.0) — same as halation
const K17_0: f32 = 0.10315;
const K17_1: f32 = 0.09998;
const K17_2: f32 = 0.09103;
const K17_3: f32 = 0.07786;
const K17_4: f32 = 0.06257;
const K17_5: f32 = 0.04723;
const K17_6: f32 = 0.03350;
const K17_7: f32 = 0.02232;
const K17_8: f32 = 0.01396;

// Quality 2: 25-tap (sigma ≈ 6.0)
const K25_0:  f32 = 0.07087;
const K25_1:  f32 = 0.06947;
const K25_2:  f32 = 0.06540;
const K25_3:  f32 = 0.05917;
const K25_4:  f32 = 0.05148;
const K25_5:  f32 = 0.04307;
const K25_6:  f32 = 0.03465;
const K25_7:  f32 = 0.02680;
const K25_8:  f32 = 0.01995;
const K25_9:  f32 = 0.01428;
const K25_10: f32 = 0.00983;
const K25_11: f32 = 0.00651;
const K25_12: f32 = 0.00415;

// ─── Circle of Confusion ──────────────────────────────────────────────

fn compute_coc_tilt_shift(uv: vec2<f32>) -> f32 {
    // Rotate UV around focus point by tilt angle
    let center = vec2<f32>(0.5, uniforms.focus_y);
    let delta = uv - center;
    let cos_a = cos(uniforms.tilt_angle);
    let sin_a = sin(uniforms.tilt_angle);
    // Project onto the perpendicular axis of the tilt line
    let rotated_y = -delta.x * sin_a + delta.y * cos_a;
    // Distance from focus band center, normalized by focus width
    let dist = abs(rotated_y) / max(uniforms.focus_width, 0.001);
    // Smooth ramp: 0 inside focus band, 1 far away
    return smoothstep(0.0, 1.0, dist - 0.5);
}

fn compute_coc_radial(uv: vec2<f32>) -> f32 {
    let center = vec2<f32>(uniforms.focus_x, uniforms.focus_y);
    // Correct for aspect ratio so the focus region is circular
    let aspect = uniforms.texel_size_y / max(uniforms.texel_size_x, 0.00001);
    var delta = uv - center;
    delta.x *= aspect;
    let dist = length(delta) / max(uniforms.focus_width, 0.001);
    return smoothstep(0.0, 1.0, dist - 0.5);
}

fn compute_coc_depth(uv: vec2<f32>) -> f32 {
    // Sample depth from source_b (depth texture, grayscale in R channel)
    let depth = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0).r;
    // Distance from focus depth
    let dist = abs(depth - uniforms.focus_y) / max(uniforms.focus_width, 0.001);
    return smoothstep(0.0, 1.0, dist - 0.5);
}

fn compute_coc(uv: vec2<f32>) -> f32 {
    var coc: f32;
    if uniforms.focus_mode == 0u {
        coc = compute_coc_tilt_shift(uv);
    } else if uniforms.focus_mode == 1u {
        coc = compute_coc_radial(uv);
    } else {
        coc = compute_coc_depth(uv);
    }
    return clamp(coc * uniforms.blur_strength, 0.0, 1.0);
}

// ─── Blur helpers ─────────────────────────────────────────────────────

// Sample with CoC-weighted contribution to prevent sharp edges bleeding
// into blurry regions (scatter-as-gather).
fn weighted_sample(uv: vec2<f32>, center_coc: f32) -> vec4<f32> {
    let s = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
    // Neighbor's CoC — only allow it to contribute if IT is also blurry,
    // or if the center pixel is blurry (prevents foreground leaking).
    let neighbor_coc = s.a;
    let weight = select(
        step(center_coc, neighbor_coc), // only if neighbor >= center
        1.0,                             // center is very blurry, accept all
        center_coc > 0.5
    );
    return vec4<f32>(s.rgb * weight, weight);
}

// 9-tap separable blur along `dir`, variable width from CoC alpha.
fn blur_9tap(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let center = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
    let coc = center.a;
    if coc < 0.005 {
        return center;  // In focus — skip blur entirely
    }
    let step_size = coc * 6.0 + 1.0;
    let d = dir * step_size;

    var acc = center.rgb * K9_0;
    var w_acc = K9_0;

    var s: vec4<f32>;
    s = weighted_sample(uv + d,       coc); acc += s.rgb * K9_1; w_acc += s.a * K9_1;
    s = weighted_sample(uv - d,       coc); acc += s.rgb * K9_1; w_acc += s.a * K9_1;
    s = weighted_sample(uv + d * 2.0, coc); acc += s.rgb * K9_2; w_acc += s.a * K9_2;
    s = weighted_sample(uv - d * 2.0, coc); acc += s.rgb * K9_2; w_acc += s.a * K9_2;
    s = weighted_sample(uv + d * 3.0, coc); acc += s.rgb * K9_3; w_acc += s.a * K9_3;
    s = weighted_sample(uv - d * 3.0, coc); acc += s.rgb * K9_3; w_acc += s.a * K9_3;
    s = weighted_sample(uv + d * 4.0, coc); acc += s.rgb * K9_4; w_acc += s.a * K9_4;
    s = weighted_sample(uv - d * 4.0, coc); acc += s.rgb * K9_4; w_acc += s.a * K9_4;

    return vec4<f32>(acc / max(w_acc, 0.001), coc);
}

// 17-tap separable blur along `dir`.
fn blur_17tap(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let center = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
    let coc = center.a;
    if coc < 0.005 {
        return center;
    }
    let step_size = coc * 6.0 + 1.0;
    let d = dir * step_size;

    var acc = center.rgb * K17_0;
    var w_acc = K17_0;

    var s: vec4<f32>;
    s = weighted_sample(uv + d,       coc); acc += s.rgb * K17_1; w_acc += s.a * K17_1;
    s = weighted_sample(uv - d,       coc); acc += s.rgb * K17_1; w_acc += s.a * K17_1;
    s = weighted_sample(uv + d * 2.0, coc); acc += s.rgb * K17_2; w_acc += s.a * K17_2;
    s = weighted_sample(uv - d * 2.0, coc); acc += s.rgb * K17_2; w_acc += s.a * K17_2;
    s = weighted_sample(uv + d * 3.0, coc); acc += s.rgb * K17_3; w_acc += s.a * K17_3;
    s = weighted_sample(uv - d * 3.0, coc); acc += s.rgb * K17_3; w_acc += s.a * K17_3;
    s = weighted_sample(uv + d * 4.0, coc); acc += s.rgb * K17_4; w_acc += s.a * K17_4;
    s = weighted_sample(uv - d * 4.0, coc); acc += s.rgb * K17_4; w_acc += s.a * K17_4;
    s = weighted_sample(uv + d * 5.0, coc); acc += s.rgb * K17_5; w_acc += s.a * K17_5;
    s = weighted_sample(uv - d * 5.0, coc); acc += s.rgb * K17_5; w_acc += s.a * K17_5;
    s = weighted_sample(uv + d * 6.0, coc); acc += s.rgb * K17_6; w_acc += s.a * K17_6;
    s = weighted_sample(uv - d * 6.0, coc); acc += s.rgb * K17_6; w_acc += s.a * K17_6;
    s = weighted_sample(uv + d * 7.0, coc); acc += s.rgb * K17_7; w_acc += s.a * K17_7;
    s = weighted_sample(uv - d * 7.0, coc); acc += s.rgb * K17_7; w_acc += s.a * K17_7;
    s = weighted_sample(uv + d * 8.0, coc); acc += s.rgb * K17_8; w_acc += s.a * K17_8;
    s = weighted_sample(uv - d * 8.0, coc); acc += s.rgb * K17_8; w_acc += s.a * K17_8;

    return vec4<f32>(acc / max(w_acc, 0.001), coc);
}

// 25-tap separable blur along `dir`.
fn blur_25tap(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let center = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
    let coc = center.a;
    if coc < 0.005 {
        return center;
    }
    let step_size = coc * 6.0 + 1.0;
    let d = dir * step_size;

    var acc = center.rgb * K25_0;
    var w_acc = K25_0;

    var s: vec4<f32>;
    s = weighted_sample(uv + d,        coc); acc += s.rgb * K25_1;  w_acc += s.a * K25_1;
    s = weighted_sample(uv - d,        coc); acc += s.rgb * K25_1;  w_acc += s.a * K25_1;
    s = weighted_sample(uv + d * 2.0,  coc); acc += s.rgb * K25_2;  w_acc += s.a * K25_2;
    s = weighted_sample(uv - d * 2.0,  coc); acc += s.rgb * K25_2;  w_acc += s.a * K25_2;
    s = weighted_sample(uv + d * 3.0,  coc); acc += s.rgb * K25_3;  w_acc += s.a * K25_3;
    s = weighted_sample(uv - d * 3.0,  coc); acc += s.rgb * K25_3;  w_acc += s.a * K25_3;
    s = weighted_sample(uv + d * 4.0,  coc); acc += s.rgb * K25_4;  w_acc += s.a * K25_4;
    s = weighted_sample(uv - d * 4.0,  coc); acc += s.rgb * K25_4;  w_acc += s.a * K25_4;
    s = weighted_sample(uv + d * 5.0,  coc); acc += s.rgb * K25_5;  w_acc += s.a * K25_5;
    s = weighted_sample(uv - d * 5.0,  coc); acc += s.rgb * K25_5;  w_acc += s.a * K25_5;
    s = weighted_sample(uv + d * 6.0,  coc); acc += s.rgb * K25_6;  w_acc += s.a * K25_6;
    s = weighted_sample(uv - d * 6.0,  coc); acc += s.rgb * K25_6;  w_acc += s.a * K25_6;
    s = weighted_sample(uv + d * 7.0,  coc); acc += s.rgb * K25_7;  w_acc += s.a * K25_7;
    s = weighted_sample(uv - d * 7.0,  coc); acc += s.rgb * K25_7;  w_acc += s.a * K25_7;
    s = weighted_sample(uv + d * 8.0,  coc); acc += s.rgb * K25_8;  w_acc += s.a * K25_8;
    s = weighted_sample(uv - d * 8.0,  coc); acc += s.rgb * K25_8;  w_acc += s.a * K25_8;
    s = weighted_sample(uv + d * 9.0,  coc); acc += s.rgb * K25_9;  w_acc += s.a * K25_9;
    s = weighted_sample(uv - d * 9.0,  coc); acc += s.rgb * K25_9;  w_acc += s.a * K25_9;
    s = weighted_sample(uv + d * 10.0, coc); acc += s.rgb * K25_10; w_acc += s.a * K25_10;
    s = weighted_sample(uv - d * 10.0, coc); acc += s.rgb * K25_10; w_acc += s.a * K25_10;
    s = weighted_sample(uv + d * 11.0, coc); acc += s.rgb * K25_11; w_acc += s.a * K25_11;
    s = weighted_sample(uv - d * 11.0, coc); acc += s.rgb * K25_11; w_acc += s.a * K25_11;
    s = weighted_sample(uv + d * 12.0, coc); acc += s.rgb * K25_12; w_acc += s.a * K25_12;
    s = weighted_sample(uv - d * 12.0, coc); acc += s.rgb * K25_12; w_acc += s.a * K25_12;

    return vec4<f32>(acc / max(w_acc, 0.001), coc);
}

fn blur_pass(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    if uniforms.quality == 0u {
        return blur_9tap(uv, dir);
    } else if uniforms.quality == 2u {
        return blur_25tap(uv, dir);
    }
    return blur_17tap(uv, dir);
}

// ─── Main entry point ─────────────────────────────────────────────────

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    var color: vec4<f32>;

    if uniforms.mode == 0u {
        // ── Pass 0: CoC + bilinear downsample ──────────────────────
        // Sample source at full-res (bilinear downsample via textureSampleLevel)
        let src = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let coc = compute_coc(uv);
        color = vec4<f32>(src.rgb, coc);

    } else if uniforms.mode == 1u {
        // ── Pass 1: Horizontal blur (half-res) ────────────────────
        let dx = vec2<f32>(uniforms.texel_size_x, 0.0);
        color = blur_pass(uv, dx);

    } else if uniforms.mode == 2u {
        // ── Pass 2: Vertical blur (half-res) ──────────────────────
        let dy = vec2<f32>(0.0, uniforms.texel_size_y);
        color = blur_pass(uv, dy);

    } else {
        // ── Pass 3: Composite (full-res) ──────────────────────────
        // source_a = original full-res, source_b = blurred half-res
        let sharp = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let blurred = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0);
        // CoC rides in blurred alpha — slightly smoothed from blur passes
        // which gives us nice anti-aliased focus transitions for free.
        let coc = blurred.a;
        let mixed = mix(sharp.rgb, blurred.rgb, coc * uniforms.amount);
        color = vec4<f32>(mixed, sharp.a);
    }

    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
