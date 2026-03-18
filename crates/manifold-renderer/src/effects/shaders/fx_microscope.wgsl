// Mechanical port of MicroscopeEffect.shader.
// Same logic, same variables, same constants, same pass structure.
//
// Passes (selected by uniforms.mode):
//   0 = HBlur   (MicroscopeEffect.shader Pass 0)
//   1 = VBlur   (MicroscopeEffect.shader Pass 1)
//   2 = EdgeDetect (MicroscopeEffect.shader Pass 2)
//   3 = Composite  (MicroscopeEffect.shader Pass 3)
//
// Bind group layout:
//   @group(0) @binding(0) var<uniform> uniforms: MicroscopeUniforms;
//   @group(0) @binding(1) var main_tex: texture_2d<f32>;    // _MainTex
//   @group(0) @binding(2) var tex_sampler: sampler;
//   @group(0) @binding(3) var blur_tex: texture_2d<f32>;    // _BlurTex  (composite pass)
//   @group(0) @binding(4) var edge_tex: texture_2d<f32>;    // _EdgeTex  (composite pass)

struct MicroscopeUniforms {
    // offset 0
    mode: u32,          // 0=HBlur, 1=VBlur, 2=Edge, 3=Composite
    amount: f32,        // _Amount       p0
    zoom: f32,          // _Zoom         p1
    focus: f32,         // _Focus        p2
    // offset 16
    dof: f32,           // _DOF          p3
    aberration: f32,    // _Aberration   p4
    illumination: f32,  // _Illumination p5 (cast to int in shader)
    structure: f32,     // _Structure    p6
    // offset 32
    distortion: f32,    // _Distortion   p7
    drift: f32,         // _Drift        p8
    noise: f32,         // _Noise        p9
    dust: f32,          // _Dust         p10
    // offset 48
    texel_x: f32,       // _TexelSize.x = 1/width
    texel_y: f32,       // _TexelSize.y = 1/height
    texel_z: f32,       // _TexelSize.z = width
    texel_w: f32,       // _TexelSize.w = height
    // offset 64
    time: f32,          // _Time.y (ctx.time)
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: MicroscopeUniforms;
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var blur_tex: texture_2d<f32>;
@group(0) @binding(4) var edge_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// MicroscopeEffect.shader — vert function
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    // Flip Y: Unity UV origin is bottom-left, wgpu is top-left
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// MicroscopeEffect.shader — luminance() helper
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// MicroscopeEffect.shader — hash11()
fn hash11(p_in: f32) -> f32 {
    var p = fract(p_in * 0.1031);
    p = p * (p + 33.33);
    p = p * (p + p);
    return fract(p);
}

// MicroscopeEffect.shader — hash21()
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 = p3 + dot(p3, vec3<f32>(p3.y, p3.z, p3.x) + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// MicroscopeEffect.shader — 13-tap gaussian weights (sigma ~2.5, normalized)
// static const float gaussWeights[7] = { 0.1974, 0.1748, 0.1210, 0.0656, 0.0277, 0.0092, 0.0024 }
const GAUSS_W0: f32 = 0.1974;
const GAUSS_W1: f32 = 0.1748;
const GAUSS_W2: f32 = 0.1210;
const GAUSS_W3: f32 = 0.0656;
const GAUSS_W4: f32 = 0.0277;
const GAUSS_W5: f32 = 0.0092;
const GAUSS_W6: f32 = 0.0024;

// MicroscopeEffect.shader Pass 0 — fragHBlur
fn frag_hblur(uv: vec2<f32>) -> vec4<f32> {
    let spread = uniforms.dof * 6.0 + uniforms.structure * 3.0;
    let texel = vec2<f32>(uniforms.texel_x, 0.0) * spread;

    var sum = textureSample(main_tex, tex_sampler, uv).rgb * GAUSS_W0;

    // t=1
    let o1 = texel * 1.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o1).rgb * GAUSS_W1;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o1).rgb * GAUSS_W1;
    // t=2
    let o2 = texel * 2.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o2).rgb * GAUSS_W2;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o2).rgb * GAUSS_W2;
    // t=3
    let o3 = texel * 3.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o3).rgb * GAUSS_W3;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o3).rgb * GAUSS_W3;
    // t=4
    let o4 = texel * 4.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o4).rgb * GAUSS_W4;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o4).rgb * GAUSS_W4;
    // t=5
    let o5 = texel * 5.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o5).rgb * GAUSS_W5;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o5).rgb * GAUSS_W5;
    // t=6
    let o6 = texel * 6.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o6).rgb * GAUSS_W6;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o6).rgb * GAUSS_W6;

    return vec4<f32>(sum, 1.0);
}

// MicroscopeEffect.shader Pass 1 — fragVBlur
fn frag_vblur(uv: vec2<f32>) -> vec4<f32> {
    let spread = uniforms.dof * 6.0 + uniforms.structure * 3.0;
    let texel = vec2<f32>(0.0, uniforms.texel_y) * spread;

    var sum = textureSample(main_tex, tex_sampler, uv).rgb * GAUSS_W0;

    let o1 = texel * 1.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o1).rgb * GAUSS_W1;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o1).rgb * GAUSS_W1;
    let o2 = texel * 2.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o2).rgb * GAUSS_W2;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o2).rgb * GAUSS_W2;
    let o3 = texel * 3.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o3).rgb * GAUSS_W3;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o3).rgb * GAUSS_W3;
    let o4 = texel * 4.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o4).rgb * GAUSS_W4;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o4).rgb * GAUSS_W4;
    let o5 = texel * 5.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o5).rgb * GAUSS_W5;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o5).rgb * GAUSS_W5;
    let o6 = texel * 6.0;
    sum = sum + textureSample(main_tex, tex_sampler, uv + o6).rgb * GAUSS_W6;
    sum = sum + textureSample(main_tex, tex_sampler, uv - o6).rgb * GAUSS_W6;

    return vec4<f32>(sum, 1.0);
}

// MicroscopeEffect.shader Pass 2 — fragEdge / sampleLum
fn sample_lum(uv: vec2<f32>, offset: vec2<f32>) -> f32 {
    return luminance(textureSample(main_tex, tex_sampler,
        uv + offset * vec2<f32>(uniforms.texel_x, uniforms.texel_y)).rgb);
}

fn frag_edge(uv: vec2<f32>) -> vec4<f32> {
    let tl = sample_lum(uv, vec2<f32>(-1.0, -1.0));
    let tc = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let tr = sample_lum(uv, vec2<f32>( 1.0, -1.0));
    let ml = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let mr = sample_lum(uv, vec2<f32>( 1.0,  0.0));
    let bl = sample_lum(uv, vec2<f32>(-1.0,  1.0));
    let bc = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let br = sample_lum(uv, vec2<f32>( 1.0,  1.0));

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;

    let edge = sqrt(gx * gx + gy * gy);
    return vec4<f32>(edge, edge, edge, 1.0);
}

// MicroscopeEffect.shader Pass 3 — fragComposite
fn frag_composite(uv: vec2<f32>) -> vec4<f32> {
    let orig_uv = uv;
    var cur_uv = uv;

    // --- 1. Specimen drift (Lissajous pattern) ---
    // MicroscopeEffect.shader lines 212-216
    let t = uniforms.time;
    let drift_offset = vec2<f32>(
        sin(t * 0.31 + 1.7) + sin(t * 0.17) * 0.6,
        cos(t * 0.23 + 0.5) + cos(t * 0.13) * 0.6
    ) * uniforms.drift * 0.015;
    cur_uv = cur_uv + drift_offset;

    // --- 2. Zoom (center-based) ---
    // MicroscopeEffect.shader lines 219-221
    var centered = cur_uv - 0.5;
    centered = centered / max(uniforms.zoom, 1.0);
    cur_uv = centered + 0.5;

    // --- 3. Barrel distortion ---
    // MicroscopeEffect.shader lines 224-227
    var dc = cur_uv * 2.0 - 1.0;
    let r2 = dot(dc, dc);
    dc = dc * (1.0 + uniforms.distortion * 0.5 * r2);
    cur_uv = dc * 0.5 + 0.5;

    // --- 4. Chromatic aberration (radial) ---
    // MicroscopeEffect.shader lines 230-237
    let ca_dir = cur_uv - 0.5;
    let ca_offset = ca_dir * uniforms.aberration * 0.04;

    let r_ch = textureSample(main_tex, tex_sampler, cur_uv + ca_offset).r;
    let g_ch = textureSample(main_tex, tex_sampler, cur_uv).g;
    let b_ch = textureSample(main_tex, tex_sampler, cur_uv - ca_offset).b;
    let sharp = vec3<f32>(r_ch, g_ch, b_ch);

    // --- 5. Sample blurred at transformed UVs ---
    // MicroscopeEffect.shader lines 240-243
    let blur_r = textureSample(blur_tex, tex_sampler, cur_uv + ca_offset).r;
    let blur_g = textureSample(blur_tex, tex_sampler, cur_uv).g;
    let blur_b = textureSample(blur_tex, tex_sampler, cur_uv - ca_offset).b;
    let blurred = vec3<f32>(blur_r, blur_g, blur_b);

    // --- 6. DOF mixing (radial focus) ---
    // MicroscopeEffect.shader lines 246-259
    let dist = length(orig_uv - 0.5) * 2.0;
    let focus_band = max(0.05, 0.5 - uniforms.dof * 0.4);
    let focus_mask = clamp(
        1.0 - smoothstep(
            uniforms.focus - focus_band,
            uniforms.focus + focus_band,
            abs(dist - uniforms.focus)
        ),
        0.0, 1.0
    );

    var dof_result: vec3<f32>;
    if uniforms.dof > 0.001 {
        dof_result = mix(blurred, sharp, focus_mask);
    } else {
        dof_result = sharp;
    }

    // --- 7. Structure enhancement (unsharp mask) ---
    // MicroscopeEffect.shader lines 262-263
    let detail = sharp - blurred;
    dof_result = dof_result + detail * uniforms.structure * 2.5;

    // --- 8. Illumination modes ---
    // MicroscopeEffect.shader lines 266-295
    let edge = textureSample(edge_tex, tex_sampler, cur_uv).r;
    let mode = i32(round(uniforms.illumination));

    var illuminated = dof_result;

    if mode == 1 {
        // Dark-field: edges glow on black background
        // MicroscopeEffect.shader lines 272-275
        let edge_intensity = smoothstep(0.05, 0.3, edge);
        illuminated = dof_result * edge_intensity * 3.0;
    } else if mode == 2 {
        // Phase contrast: edge halos with warm tint
        // MicroscopeEffect.shader lines 279-284
        let halo = smoothstep(0.02, 0.2, edge);
        illuminated = dof_result + halo * vec3<f32>(0.8, 0.65, 0.45) * 0.7;
        let anti_edge = 1.0 - smoothstep(0.0, 0.15, edge) * 0.3;
        illuminated = illuminated * anti_edge;
    } else if mode == 3 {
        // Fluorescence: isolate bright channels on dark background
        // MicroscopeEffect.shader lines 288-295
        let lum = luminance(dof_result);
        var fluor = clamp(dof_result - 0.12, vec3<f32>(0.0), vec3<f32>(1.0)) * 2.5;
        let fluor_lum = luminance(fluor);
        fluor = mix(vec3<f32>(fluor_lum), fluor, 1.8);
        illuminated = fluor * (1.0 - lum * 0.2);
    }

    // --- 9. CCD sensor noise ---
    // MicroscopeEffect.shader lines 298-301
    let noise_uv = orig_uv * vec2<f32>(uniforms.texel_z, uniforms.texel_w);
    let noise_seed = floor(uniforms.time * 30.0);
    let n = hash21(noise_uv + noise_seed * 7.13);
    illuminated = illuminated + (n - 0.5) * uniforms.noise * 0.2;

    // --- 10. Lens dust (procedural, fixed in screen space) ---
    // MicroscopeEffect.shader lines 304-319
    var dust_mask = 0.0;
    // d=0
    var dust_pos = vec2<f32>(hash11(0.0 * 127.1 + 31.7), hash11(0.0 * 269.5 + 83.3));
    var dust_size = hash11(0.0 * 419.2 + 57.1) * 0.025 + 0.008;
    var dust_dist = length(orig_uv - dust_pos);
    var dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(0.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=1
    dust_pos = vec2<f32>(hash11(1.0 * 127.1 + 31.7), hash11(1.0 * 269.5 + 83.3));
    dust_size = hash11(1.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(1.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=2
    dust_pos = vec2<f32>(hash11(2.0 * 127.1 + 31.7), hash11(2.0 * 269.5 + 83.3));
    dust_size = hash11(2.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(2.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=3
    dust_pos = vec2<f32>(hash11(3.0 * 127.1 + 31.7), hash11(3.0 * 269.5 + 83.3));
    dust_size = hash11(3.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(3.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=4
    dust_pos = vec2<f32>(hash11(4.0 * 127.1 + 31.7), hash11(4.0 * 269.5 + 83.3));
    dust_size = hash11(4.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(4.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=5
    dust_pos = vec2<f32>(hash11(5.0 * 127.1 + 31.7), hash11(5.0 * 269.5 + 83.3));
    dust_size = hash11(5.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(5.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=6
    dust_pos = vec2<f32>(hash11(6.0 * 127.1 + 31.7), hash11(6.0 * 269.5 + 83.3));
    dust_size = hash11(6.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(6.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=7
    dust_pos = vec2<f32>(hash11(7.0 * 127.1 + 31.7), hash11(7.0 * 269.5 + 83.3));
    dust_size = hash11(7.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(7.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=8
    dust_pos = vec2<f32>(hash11(8.0 * 127.1 + 31.7), hash11(8.0 * 269.5 + 83.3));
    dust_size = hash11(8.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(8.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=9
    dust_pos = vec2<f32>(hash11(9.0 * 127.1 + 31.7), hash11(9.0 * 269.5 + 83.3));
    dust_size = hash11(9.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(9.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=10
    dust_pos = vec2<f32>(hash11(10.0 * 127.1 + 31.7), hash11(10.0 * 269.5 + 83.3));
    dust_size = hash11(10.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(10.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);
    // d=11
    dust_pos = vec2<f32>(hash11(11.0 * 127.1 + 31.7), hash11(11.0 * 269.5 + 83.3));
    dust_size = hash11(11.0 * 419.2 + 57.1) * 0.025 + 0.008;
    dust_dist = length(orig_uv - dust_pos);
    dust_alpha = smoothstep(dust_size, dust_size * 0.2, dust_dist);
    dust_alpha = dust_alpha * (hash11(11.0 * 337.9 + 11.3) * 0.5 + 0.3);
    dust_mask = max(dust_mask, dust_alpha);

    illuminated = mix(illuminated, illuminated * 0.5, dust_mask * uniforms.dust);

    // --- 11. Final amount blend ---
    // MicroscopeEffect.shader lines 322-324
    let original = textureSample(main_tex, tex_sampler, orig_uv).rgb;
    let result = mix(original, illuminated, uniforms.amount);

    return vec4<f32>(result, 1.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if uniforms.mode == 0u {
        return frag_hblur(in.uv);
    } else if uniforms.mode == 1u {
        return frag_vblur(in.uv);
    } else if uniforms.mode == 2u {
        return frag_edge(in.uv);
    } else {
        return frag_composite(in.uv);
    }
}
