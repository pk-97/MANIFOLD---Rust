// Edge Detect effect — pure edge detection without glow.
// Modes: 0=Sobel, 1=Laplacian, 2=Frei-Chen.
// Use Bloom or Halation after this effect if glow is desired.

@id(0) override MODE: u32 = 0u;

struct Uniforms {
    amount: f32,
    threshold: f32,
    texel_size_x: f32,
    texel_size_y: f32,
    _pad: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn sample_lum(uv: vec2<f32>, offset: vec2<f32>) -> f32 {
    let texel = vec2<f32>(uniforms.texel_size_x, uniforms.texel_size_y);
    return luminance(textureSample(source_tex, tex_sampler, uv + offset * texel).rgb);
}

fn edge_sobel(uv: vec2<f32>) -> f32 {
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

    return sqrt(gx * gx + gy * gy);
}

fn edge_laplacian(uv: vec2<f32>) -> f32 {
    let c = sample_lum(uv, vec2<f32>( 0.0,  0.0));
    let t = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let b = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let l = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let r = sample_lum(uv, vec2<f32>( 1.0,  0.0));

    return abs(t + b + l + r - 4.0 * c);
}

fn edge_frei_chen(uv: vec2<f32>) -> f32 {
    let tl = sample_lum(uv, vec2<f32>(-1.0, -1.0));
    let tc = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let tr = sample_lum(uv, vec2<f32>( 1.0, -1.0));
    let ml = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let mr = sample_lum(uv, vec2<f32>( 1.0,  0.0));
    let bl = sample_lum(uv, vec2<f32>(-1.0,  1.0));
    let bc = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let br = sample_lum(uv, vec2<f32>( 1.0,  1.0));

    let s = 1.41421356;
    let gx = (tr + s * mr + br) - (tl + s * ml + bl);
    let gy = (bl + s * bc + br) - (tl + s * tc + tr);
    let div = 2.0 + s;

    return sqrt(gx * gx + gy * gy) / div;
}

fn detect_edge(uv: vec2<f32>) -> f32 {
    // Normalize all modes to ~[0,1] so the threshold behaves consistently.
    // Sobel max ≈ 4.0, Laplacian max ≈ 4.0, Frei-Chen already ~1.0.
    if MODE == 0u {
        return edge_sobel(uv) * 0.25;
    } else if MODE == 1u {
        return edge_laplacian(uv) * 0.25;
    } else {
        return edge_frei_chen(uv);
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);

    var edge = detect_edge(in.uv);

    let thresh = uniforms.threshold;
    edge = smoothstep(thresh * 0.5, thresh * 1.5 + 0.01, edge);

    let result = mix(src.rgb, vec3<f32>(edge), uniforms.amount);
    return vec4<f32>(result, src.a);
}
