// Linear EDR → ST.2084 PQ transfer function.
// Pure encoding — no creative transform. Takes the final display-linear
// compositor output (post-tonemap, post-effects) and encodes it for
// HDR10 HEVC delivery. The viewer's PQ decoder inverts this to recover
// the exact linear values the user saw on their HDR display.

struct Uniforms {
    paper_white: f32,  // EDR 1.0 = this many nits (typically 200)
    max_nits: f32,     // PQ ceiling (typically 10000)
    _pad0: f32,
    _pad1: f32,
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

// ST 2084 PQ OETF — encodes linear luminance [0..1] (normalized to 10000 nits)
// to perceptual quantizer for HDR10.
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

    // EDR values: 1.0 = SDR white (paper_white nits).
    // Convert EDR → absolute nits → normalize to 10,000-nit PQ domain.
    let nits = clamp(src.rgb * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
    let pq = linear_to_pq(nits / 10000.0);

    return vec4<f32>(pq, src.a);
}
