// node.tone_map — fusable body (freeze §12). HDR -> display tone mapping,
// four curves (Narkowicz ACES / Hill ACES / AgX / Khronos PBR Neutral) and
// four output modes (SDR / PQ / EDR / EDR passthrough). Helpers verbatim
// from aces_tonemap_compute.wgsl (the parity oracle, kept as
// shaders/tone_map.wgsl via the primitive's original include path — see
// `../../effects/shaders/aces_tonemap_compute.wgsl`), with the uniform
// reads (`u.curve`, `u.mode`, ...) rebound to the body's own params.
// PARAMS order: [exposure, curve, mode, paper_white, max_nits].

// ── Narkowicz ACES (2015) ──────────────────────────────────────────────

fn aces_narkowicz_raw(x: vec3<f32>) -> vec3<f32> {
    let a: f32 = 2.51;
    let b: f32 = 0.03;
    let c: f32 = 2.43;
    let d: f32 = 0.59;
    let e: f32 = 0.14;
    return (x * (a * x + b)) / (x * (c * x + d) + e);
}

fn aces_narkowicz(x: vec3<f32>) -> vec3<f32> {
    return saturate(aces_narkowicz_raw(x));
}

// ── Hill ACES (RRT+ODT fit in AP1) ────────────────────────────────────

// sRGB -> XYZ -> D65_2_D60 -> AP1 -> RRT saturation (concatenated)
const TONE_MAP_ACES_INPUT: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.59719, 0.07600, 0.02840),
    vec3<f32>(0.35458, 0.90834, 0.13383),
    vec3<f32>(0.04823, 0.01566, 0.83777),
);

// ODT saturation -> AP1 -> XYZ -> D60_2_D65 -> sRGB (concatenated)
const TONE_MAP_ACES_OUTPUT: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.60475, -0.10208, -0.00327),
    vec3<f32>(-0.53108,  1.10813, -0.07276),
    vec3<f32>(-0.07367, -0.00605,  1.07602),
);

fn rrt_and_odt_fit(v: vec3<f32>) -> vec3<f32> {
    let a = v * (v + 0.0245786) - 0.000090537;
    let b = v * (0.983729 * v + 0.4329510) + 0.238081;
    return a / b;
}

fn aces_hill_raw(color: vec3<f32>) -> vec3<f32> {
    var c = TONE_MAP_ACES_INPUT * color;
    c = rrt_and_odt_fit(c);
    c = TONE_MAP_ACES_OUTPUT * c;
    return c;
}

fn aces_hill(color: vec3<f32>) -> vec3<f32> {
    return saturate(aces_hill_raw(color));
}

// ── AgX (Troy Sobotka) ────────────────────────────────────────────────

// sRGB linear -> Rec.2020 linear
const TONE_MAP_SRGB_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.6274, 0.0691, 0.0164),
    vec3<f32>(0.3293, 0.9195, 0.0880),
    vec3<f32>(0.0433, 0.0113, 0.8956),
);

// Rec.2020 linear -> sRGB linear
const TONE_MAP_REC2020_TO_SRGB: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876,  1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083,  1.1187),
);

// AgX inset matrix (operates in Rec.2020 space)
const TONE_MAP_AGX_INSET: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.856627153315983, 0.137318972929847, 0.11189821299995),
    vec3<f32>(0.0951212405381588, 0.761241990602591, 0.0767994186031903),
    vec3<f32>(0.0482516061458583, 0.101439036467562, 0.811302368396859),
);

// AgX outset matrix (inverse)
const TONE_MAP_AGX_OUTSET: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.1271005818144368, -0.1413297634984383, -0.14132976349843826),
    vec3<f32>(-0.11060664309660323, 1.157823702216272, -0.11060664309660294),
    vec3<f32>(-0.016493938717834573, -0.016493938717834257, 1.2519364065950405),
);

const TONE_MAP_AGX_MIN_EV: f32 = -12.47393;
const TONE_MAP_AGX_MAX_EV: f32 = 4.026069;

// 6th-order polynomial approximation of the AgX sigmoid contrast curve
fn agx_default_contrast(x: vec3<f32>) -> vec3<f32> {
    let x2 = x * x;
    let x4 = x2 * x2;
    return 15.5 * x4 * x2
         - 40.14 * x4 * x
         + 31.96 * x4
         - 6.868 * x2 * x
         + 0.4298 * x2
         + 0.1191 * x
         - 0.00232;
}

fn agx_tonemap(color: vec3<f32>) -> vec3<f32> {
    // sRGB -> Rec.2020 -> AgX inset
    var c = TONE_MAP_AGX_INSET * (TONE_MAP_SRGB_TO_REC2020 * color);

    // Log2 encoding
    c = max(c, vec3<f32>(1e-10));
    c = log2(c);
    c = (c - TONE_MAP_AGX_MIN_EV) / (TONE_MAP_AGX_MAX_EV - TONE_MAP_AGX_MIN_EV);
    c = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));

    // Sigmoid contrast
    c = agx_default_contrast(c);

    // AgX outset -> linearize -> Rec.2020 -> sRGB
    c = TONE_MAP_AGX_OUTSET * c;
    c = pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
    c = TONE_MAP_REC2020_TO_SRGB * c;

    return saturate(c);
}

fn agx_tonemap_raw(color: vec3<f32>) -> vec3<f32> {
    var c = TONE_MAP_AGX_INSET * (TONE_MAP_SRGB_TO_REC2020 * color);

    c = max(c, vec3<f32>(1e-10));
    c = log2(c);
    c = (c - TONE_MAP_AGX_MIN_EV) / (TONE_MAP_AGX_MAX_EV - TONE_MAP_AGX_MIN_EV);
    c = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));

    c = agx_default_contrast(c);

    c = TONE_MAP_AGX_OUTSET * c;
    c = pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
    c = TONE_MAP_REC2020_TO_SRGB * c;

    return c;
}

// ── Khronos PBR Neutral ───────────────────────────────────────────────
// Reference: https://github.com/KhronosGroup/ToneMapping

fn khronos_pbr_neutral(color: vec3<f32>) -> vec3<f32> {
    let start_compression: f32 = 0.8 - 0.04;
    let desaturation: f32 = 0.15;

    var x = min(color.r, min(color.g, color.b));
    var offset: f32;
    if x < 0.08 {
        offset = x - 6.25 * x * x;
    } else {
        offset = 0.04;
    }
    var c = color - offset;

    let peak = max(c.r, max(c.g, c.b));
    if peak < start_compression {
        return c;
    }

    let d = 1.0 - start_compression;
    let new_peak = 1.0 - d * d / (peak + d - start_compression);
    c *= new_peak / peak;

    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(c, vec3<f32>(new_peak), vec3<f32>(g));
}

// ── Curve dispatch ─────────────────────────────────────────────────────

fn tonemap_sdr(hdr: vec3<f32>, curve: u32) -> vec3<f32> {
    if curve == 1u {
        return aces_hill(hdr);
    } else if curve == 2u {
        return agx_tonemap(hdr);
    } else if curve == 3u {
        return saturate(khronos_pbr_neutral(hdr));
    }
    return aces_narkowicz(hdr);
}

fn tonemap_raw(hdr: vec3<f32>, curve: u32) -> vec3<f32> {
    if curve == 1u {
        return aces_hill_raw(hdr);
    } else if curve == 2u {
        return agx_tonemap_raw(hdr);
    } else if curve == 3u {
        return khronos_pbr_neutral(hdr);
    }
    return aces_narkowicz_raw(hdr);
}

// ── PQ encoding ────────────────────────────────────────────────────────

fn linear_to_pq(L: vec3<f32>) -> vec3<f32> {
    let m1: f32 = 0.1593017578125;
    let m2: f32 = 78.84375;
    let c1: f32 = 0.8359375;
    let c2: f32 = 18.8515625;
    let c3: f32 = 18.6875;
    let Ym1: vec3<f32> = pow(max(L, vec3<f32>(0.0)), vec3<f32>(m1));
    return pow((c1 + c2 * Ym1) / (1.0 + c3 * Ym1), vec3<f32>(m2));
}

fn body(
    c: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    exposure: f32,
    curve: u32,
    mode: u32,
    paper_white: f32,
    max_nits: f32,
) -> vec4<f32> {
    let hdr = c.rgb * exposure;

    var result: vec3<f32>;

    if mode == 1u {
        // PQ output (export pipeline).
        let mapped = tonemap_raw(hdr, curve);
        let nits = clamp(mapped * paper_white, vec3<f32>(0.0), vec3<f32>(max_nits));
        result = linear_to_pq(nits / 10000.0);
    } else if mode == 2u {
        // HDR display-linear output (macOS EDR).
        let mapped = tonemap_raw(hdr, curve);
        let nits = clamp(mapped * paper_white, vec3<f32>(0.0), vec3<f32>(max_nits));
        result = nits / max(paper_white, 1.0);
    } else if mode == 3u {
        // EDR passthrough — no curve compression, linear values directly to EDR.
        let peak = max_nits / max(paper_white, 1.0);
        let edr = max(hdr, vec3<f32>(0.0));
        let knee = peak * 0.8;
        let compressed = knee + (peak - knee) * tanh((edr - knee) / (peak - knee));
        let below = vec3<f32>(f32(edr.r < knee), f32(edr.g < knee), f32(edr.b < knee));
        result = mix(compressed, edr, below);
    } else {
        // SDR output (default).
        result = tonemap_sdr(hdr, curve);
    }

    return vec4<f32>(result, c.a);
}
