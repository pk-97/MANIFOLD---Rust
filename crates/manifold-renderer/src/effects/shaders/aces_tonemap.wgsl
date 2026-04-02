// Tonemapping — fragment shader variant (UI thread wgpu path).
// Supports multiple curves (Narkowicz ACES, Hill ACES, AgX) and multiple
// output modes (SDR, PQ, EDR, EDR passthrough).

struct Uniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32,  // 0 = SDR, 1 = PQ, 2 = EDR, 3 = EDR passthrough
    curve: u32, // 0 = Narkowicz, 1 = Hill, 2 = AgX
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

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

const ACES_INPUT: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.59719, 0.07600, 0.02840),
    vec3<f32>(0.35458, 0.90834, 0.13383),
    vec3<f32>(0.04823, 0.01566, 0.83777),
);

const ACES_OUTPUT: mat3x3<f32> = mat3x3<f32>(
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
    var c = ACES_INPUT * color;
    c = rrt_and_odt_fit(c);
    c = ACES_OUTPUT * c;
    return c;
}

fn aces_hill(color: vec3<f32>) -> vec3<f32> {
    return saturate(aces_hill_raw(color));
}

// ── AgX (Troy Sobotka) ────────────────────────────────────────────────

const SRGB_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.6274, 0.0691, 0.0164),
    vec3<f32>(0.3293, 0.9195, 0.0880),
    vec3<f32>(0.0433, 0.0113, 0.8956),
);

const REC2020_TO_SRGB: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.6605, -0.1246, -0.0182),
    vec3<f32>(-0.5876,  1.1329, -0.1006),
    vec3<f32>(-0.0728, -0.0083,  1.1187),
);

const AGX_INSET: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.856627153315983, 0.137318972929847, 0.11189821299995),
    vec3<f32>(0.0951212405381588, 0.761241990602591, 0.0767994186031903),
    vec3<f32>(0.0482516061458583, 0.101439036467562, 0.811302368396859),
);

const AGX_OUTSET: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.1271005818144368, -0.1413297634984383, -0.14132976349843826),
    vec3<f32>(-0.11060664309660323, 1.157823702216272, -0.11060664309660294),
    vec3<f32>(-0.016493938717834573, -0.016493938717834257, 1.2519364065950405),
);

const AGX_MIN_EV: f32 = -12.47393;
const AGX_MAX_EV: f32 = 4.026069;

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
    var c = AGX_INSET * (SRGB_TO_REC2020 * color);
    c = max(c, vec3<f32>(1e-10));
    c = log2(c);
    c = (c - AGX_MIN_EV) / (AGX_MAX_EV - AGX_MIN_EV);
    c = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    c = agx_default_contrast(c);
    c = AGX_OUTSET * c;
    c = pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
    c = REC2020_TO_SRGB * c;
    return saturate(c);
}

fn agx_tonemap_raw(color: vec3<f32>) -> vec3<f32> {
    var c = AGX_INSET * (SRGB_TO_REC2020 * color);
    c = max(c, vec3<f32>(1e-10));
    c = log2(c);
    c = (c - AGX_MIN_EV) / (AGX_MAX_EV - AGX_MIN_EV);
    c = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    c = agx_default_contrast(c);
    c = AGX_OUTSET * c;
    c = pow(max(c, vec3<f32>(0.0)), vec3<f32>(2.2));
    c = REC2020_TO_SRGB * c;
    return c;
}

// ── Curve dispatch ─────────────────────────────────────────────────────

fn tonemap_sdr(hdr: vec3<f32>) -> vec3<f32> {
    if u.curve == 1u {
        return aces_hill(hdr);
    } else if u.curve == 2u {
        return agx_tonemap(hdr);
    }
    return aces_narkowicz(hdr);
}

fn tonemap_raw(hdr: vec3<f32>) -> vec3<f32> {
    if u.curve == 1u {
        return aces_hill_raw(hdr);
    } else if u.curve == 2u {
        return agx_tonemap_raw(hdr);
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

// ── Main ───────────────────────────────────────────────────────────────

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(t_source, s_source, in.uv);
    let hdr = src.rgb * u.exposure;

    var result: vec3<f32>;

    if (u.mode == 1u) {
        let mapped = tonemap_raw(hdr);
        let nits = clamp(mapped * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
        result = linear_to_pq(nits / 10000.0);
    } else if (u.mode == 2u) {
        let mapped = tonemap_raw(hdr);
        let nits = clamp(mapped * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
        result = nits / max(u.paper_white, 1.0);
    } else if (u.mode == 3u) {
        let peak = u.max_nits / max(u.paper_white, 1.0);
        let mapped = tonemap_raw(hdr);
        let edr = max(mapped * peak, vec3<f32>(0.0));
        let knee = peak * 0.8;
        let compressed = knee + (peak - knee) * tanh((edr - knee) / (peak - knee));
        let below = vec3<f32>(f32(edr.r < knee), f32(edr.g < knee), f32(edr.b < knee));
        result = mix(compressed, edr, below);
    } else {
        result = tonemap_sdr(hdr);
    }

    return vec4<f32>(result, src.a);
}
