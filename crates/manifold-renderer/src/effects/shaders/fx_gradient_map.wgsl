// GradientMap effect — maps luminance to a three-way color gradient
// (shadow → midtone → highlight) with contrast control.

struct Uniforms {
    amount: f32,
    shadow_hue: f32,
    shadow_sat: f32,
    highlight_hue: f32,
    highlight_sat: f32,
    mid_hue: f32,
    mid_sat: f32,
    contrast: f32,
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

fn hsv_to_rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x, c.x, c.x) + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);

    // Compute luminance
    var luma = dot(src.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));

    // Apply contrast (pivot at 0.5); clamp negative only
    luma = max(0.0, (luma - 0.5) * uniforms.contrast + 0.5);

    // Convert hue params to [0,1] range
    let hA = fract(uniforms.shadow_hue / 360.0);
    let hB = fract(uniforms.highlight_hue / 360.0);
    let hM = fract(uniforms.mid_hue / 360.0);

    let colorA = hsv_to_rgb(vec3<f32>(hA, uniforms.shadow_sat, 1.0));
    let colorB = hsv_to_rgb(vec3<f32>(hB, uniforms.highlight_sat, 1.0));
    let colorM = hsv_to_rgb(vec3<f32>(hM, max(uniforms.shadow_sat, uniforms.highlight_sat) * 0.7, 1.0));

    // Bell curve for midtone contribution
    let midWeight = exp(-8.0 * (luma - 0.5) * (luma - 0.5));

    // Base two-tone gradient
    let twoTone = mix(colorA, colorB, luma);

    // Blend midtone on top with bell curve
    var mapped = mix(twoTone, colorM, midWeight * 0.6);

    // Scale by original luminance to preserve brightness structure
    mapped *= luma;

    let result = mix(src.rgb, mapped, uniforms.amount);
    return vec4<f32>(result, src.a);
}
