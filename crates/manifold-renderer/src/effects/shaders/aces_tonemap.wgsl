// ACES filmic tonemapping — mechanical translation of ACESTonemap.shader.
// Narkowicz 2015 fit. Three modes selected by uniform:
//   0 = SDR (clamped ACES -> sRGB)
//   1 = PQ  (unclamped ACES -> ST.2084 perceptual quantizer, for export)
//   2 = EDR (unclamped ACES -> display-linear, for macOS EDR)

struct Uniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32,
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

// ACES filmic tonemapping curve (Narkowicz 2015 fit).
// Unclamped variant — returns values potentially > 1.0 for HDR paths.
fn aces_film_raw(x: vec3<f32>) -> vec3<f32> {
    let a: f32 = 2.51;
    let b: f32 = 0.03;
    let c: f32 = 2.43;
    let d: f32 = 0.59;
    let e: f32 = 0.14;
    return (x * (a * x + b)) / (x * (c * x + d) + e);
}

// SDR helper — clamps to [0,1]. Matches Unity ACESFilm().
fn aces_film(x: vec3<f32>) -> vec3<f32> {
    return saturate(aces_film_raw(x));
}

// ST 2084 PQ OETF — encodes linear luminance to perceptual quantizer for HDR10.
// Matches Unity LinearToPQ().
fn linear_to_pq(L: vec3<f32>) -> vec3<f32> {
    let m1: f32 = 0.1593017578125;
    let m2: f32 = 78.84375;
    let c1: f32 = 0.8359375;
    let c2: f32 = 18.8515625;
    let c3: f32 = 18.6875;
    let Ym1: vec3<f32> = pow(max(L, vec3<f32>(0.0)), vec3<f32>(m1));
    return pow((c1 + c2 * Ym1) / (1.0 + c3 * Ym1), vec3<f32>(m2));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(t_source, s_source, in.uv);
    let hdr = src.rgb * u.exposure;

    var result: vec3<f32>;

    if (u.mode == 1u) {
        // Pass 1: HDR PQ output (export pipeline).
        // Unclamped ACES — values > 1.0 carry highlight detail.
        let mapped = aces_film_raw(hdr);
        // Scene 1.0 (ACES midtone) -> paper_white nits.
        // Highlights above 1.0 extend toward max_nits, then hard-cap.
        let nits = clamp(mapped * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
        // Normalize to 10,000-nit PQ domain and encode.
        result = linear_to_pq(nits / 10000.0);
    } else if (u.mode == 2u) {
        // Pass 2: HDR display-linear output (macOS EDR).
        // Unclamped ACES — preserves highlight separation.
        let mapped = aces_film_raw(hdr);
        // Scene 1.0 -> paper_white nits -> EDR 1.0 (SDR white).
        // Highlights extend up to max_nits / paper_white in EDR units.
        let nits = clamp(mapped * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
        result = nits / max(u.paper_white, 1.0);
    } else {
        // Pass 0: SDR output (default).
        // Clamped ACES -> [0,1] for sRGB display.
        result = aces_film(hdr);
    }

    return vec4<f32>(result, src.a);
}
