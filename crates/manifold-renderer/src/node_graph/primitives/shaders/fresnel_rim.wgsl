// node.fresnel_rim — fresnel-based edge highlight from a tangent-space
// normal map. Per pixel: f = pow(1 - max(dot(n, view), 0), power),
// out.rgb = color.rgb * f, out.a = f.
//
// Output is an ADDITIVE rim term: black at face-on surfaces, the
// `color` tint scaled toward 1.0 at grazing angles. Sum with a base
// shading via `node.compose` mode=Add (or mode=Screen for HDR-safe).
//
// Bindings:
//   @binding(0) uniforms (32 bytes)
//   @binding(1) tex_normal
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    view: vec3<f32>,
    power: f32,
    color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_normal: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let n = textureSampleLevel(tex_normal, tex_sampler, uv, 0.0).rgb;
    let v = normalize(uniforms.view + vec3<f32>(1e-8));
    let face = max(dot(n, v), 0.0);
    let f = pow(1.0 - face, max(uniforms.power, 1e-4));
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(uniforms.color.rgb * f, f));
}
