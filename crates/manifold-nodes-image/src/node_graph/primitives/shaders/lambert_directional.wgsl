// node.lambert_directional — Lambert (diffuse) lighting from a
// tangent-space normal map and a directional light.
//
// per-pixel:
//   n = sample(normal, uv).rgb
//   lambert = max(dot(n, light_dir), 0.0)
//   lit = lambert * (1.0 - ambient) + ambient
//   out.rgb = vec3(lit) * light_color; out.a = 1.0
//
// Output is grayscale [0, 1] when light_color = (1, 1, 1) (the
// scalar-driven default), tinted by light_color otherwise (the
// `node.light`-wired path). Caller tints further with downstream
// `node.color_grade` / `node.color_ramp` if needed.
//
// Bindings:
//   @binding(0) uniforms (32 bytes)
//   @binding(1) tex_normal
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    light_dir: vec3<f32>,    // unit vector, world-ish; normalised in-shader
    _pad: f32,
    light_color: vec3<f32>,  // pre-multiplied with intensity by the producer
    ambient: f32,
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
    let l = normalize(uniforms.light_dir + vec3<f32>(1e-8));
    let lambert = max(dot(n, l), 0.0);
    let lit = lambert * (1.0 - uniforms.ambient) + uniforms.ambient;
    let rgb = vec3<f32>(lit) * uniforms.light_color;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rgb, 1.0));
}
