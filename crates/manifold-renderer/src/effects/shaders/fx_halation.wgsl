// Halation effect — bright-area extraction with tinted blur, additive composite.
// Single-pass approximation of the Unity 3-pass version.

struct Uniforms {
    amount: f32,
    threshold: f32,
    spread: f32,
    hue: f32,          // 0..1 hue selector for tint color
    resolution_x: f32,
    resolution_y: f32,
    _pad0: f32,
    _pad1: f32,
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

// Simple HSV->RGB for tint color from hue parameter
fn hue_to_rgb(h: f32) -> vec3<f32> {
    let r = abs(h * 6.0 - 3.0) - 1.0;
    let g = 2.0 - abs(h * 6.0 - 2.0);
    let b = 2.0 - abs(h * 6.0 - 4.0);
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn sample_thresh(uv: vec2<f32>, offset: vec2<f32>, texel: vec2<f32>, r: f32, threshold: f32, tint: vec3<f32>) -> vec3<f32> {
    let suv = uv + offset * texel * r;
    let col = textureSample(source_tex, tex_sampler, suv).rgb;
    let lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mk = smoothstep(threshold - 0.1, threshold + 0.1, lm);
    return col * mk * tint;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);
    let texel = vec2<f32>(1.0 / uniforms.resolution_x, 1.0 / uniforms.resolution_y);
    let r = uniforms.spread * 6.0 + 1.0;
    let tint = hue_to_rgb(uniforms.hue);

    // 13-tap weighted blur with threshold extraction
    var acc = vec3<f32>(0.0);
    acc += sample_thresh(in.uv, vec2<f32>( 0.0,  0.0), texel, r, uniforms.threshold, tint) * 0.16;
    acc += sample_thresh(in.uv, vec2<f32>( 1.0,  0.0), texel, r, uniforms.threshold, tint) * 0.10;
    acc += sample_thresh(in.uv, vec2<f32>(-1.0,  0.0), texel, r, uniforms.threshold, tint) * 0.10;
    acc += sample_thresh(in.uv, vec2<f32>( 0.0,  1.0), texel, r, uniforms.threshold, tint) * 0.10;
    acc += sample_thresh(in.uv, vec2<f32>( 0.0, -1.0), texel, r, uniforms.threshold, tint) * 0.10;
    acc += sample_thresh(in.uv, vec2<f32>( 1.0,  1.0), texel, r, uniforms.threshold, tint) * 0.06;
    acc += sample_thresh(in.uv, vec2<f32>(-1.0,  1.0), texel, r, uniforms.threshold, tint) * 0.06;
    acc += sample_thresh(in.uv, vec2<f32>( 1.0, -1.0), texel, r, uniforms.threshold, tint) * 0.06;
    acc += sample_thresh(in.uv, vec2<f32>(-1.0, -1.0), texel, r, uniforms.threshold, tint) * 0.06;
    acc += sample_thresh(in.uv, vec2<f32>( 2.0,  0.0), texel, r, uniforms.threshold, tint) * 0.03;
    acc += sample_thresh(in.uv, vec2<f32>(-2.0,  0.0), texel, r, uniforms.threshold, tint) * 0.03;
    acc += sample_thresh(in.uv, vec2<f32>( 0.0,  2.0), texel, r, uniforms.threshold, tint) * 0.03;
    acc += sample_thresh(in.uv, vec2<f32>( 0.0, -2.0), texel, r, uniforms.threshold, tint) * 0.03;

    // Second blur pass (wider spread) — sample the already-blurred halo
    let r2 = uniforms.spread * 8.0 + 2.0;
    var acc2 = vec3<f32>(0.0);
    acc2 += sample_thresh(in.uv, vec2<f32>( 0.0,  0.0), texel, r2, uniforms.threshold, tint) * 0.16;
    acc2 += sample_thresh(in.uv, vec2<f32>( 1.0,  0.0), texel, r2, uniforms.threshold, tint) * 0.10;
    acc2 += sample_thresh(in.uv, vec2<f32>(-1.0,  0.0), texel, r2, uniforms.threshold, tint) * 0.10;
    acc2 += sample_thresh(in.uv, vec2<f32>( 0.0,  1.0), texel, r2, uniforms.threshold, tint) * 0.10;
    acc2 += sample_thresh(in.uv, vec2<f32>( 0.0, -1.0), texel, r2, uniforms.threshold, tint) * 0.10;
    acc2 += sample_thresh(in.uv, vec2<f32>( 1.0,  1.0), texel, r2, uniforms.threshold, tint) * 0.06;
    acc2 += sample_thresh(in.uv, vec2<f32>(-1.0,  1.0), texel, r2, uniforms.threshold, tint) * 0.06;
    acc2 += sample_thresh(in.uv, vec2<f32>( 1.0, -1.0), texel, r2, uniforms.threshold, tint) * 0.06;
    acc2 += sample_thresh(in.uv, vec2<f32>(-1.0, -1.0), texel, r2, uniforms.threshold, tint) * 0.06;
    acc2 += sample_thresh(in.uv, vec2<f32>( 2.0,  0.0), texel, r2, uniforms.threshold, tint) * 0.03;
    acc2 += sample_thresh(in.uv, vec2<f32>(-2.0,  0.0), texel, r2, uniforms.threshold, tint) * 0.03;
    acc2 += sample_thresh(in.uv, vec2<f32>( 0.0,  2.0), texel, r2, uniforms.threshold, tint) * 0.03;
    acc2 += sample_thresh(in.uv, vec2<f32>( 0.0, -2.0), texel, r2, uniforms.threshold, tint) * 0.03;

    // Combine both blur passes
    let halo = (acc + acc2) * 0.5;

    // Additive blend — physically correct for scattered light
    let result = src.rgb + halo * uniforms.amount;
    return vec4<f32>(result, src.a);
}
