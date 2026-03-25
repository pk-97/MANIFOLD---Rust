// Linear EDR -> ST.2084 PQ transfer function — compute dispatch variant.
// Identical math to linear_to_pq.wgsl. Only the input/output mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

struct Uniforms {
    paper_white: f32,  // EDR 1.0 = this many nits (typically 200)
    max_nits: f32,     // PQ ceiling (typically 10000)
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(t_source, s_source, uv, 0.0);

    // EDR values: 1.0 = SDR white (paper_white nits).
    // Convert EDR -> absolute nits -> normalize to 10,000-nit PQ domain.
    let nits = clamp(src.rgb * u.paper_white, vec3<f32>(0.0), vec3<f32>(u.max_nits));
    let pq = linear_to_pq(nits / 10000.0);

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(pq, src.a));
}
