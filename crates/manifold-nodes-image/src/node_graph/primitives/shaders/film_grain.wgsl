// node.film_grain — multiplicative white-noise grain. Darkens each
// pixel by a per-pixel hash so bright areas pick up paper-like texture
// while black stays black. Verbatim from Watercolor's grain pass.
//   out.rgb = src.rgb * (1 - amount * (1 - white_noise(pixel)))

struct Uniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

fn white_noise(coord: vec2<f32>) -> f32 {
    return fract(sin(dot(coord, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let pixel = uv * vec2<f32>(dims);
    let noise = white_noise(pixel);
    let rgb = src.rgb * (1.0 - u.amount * (1.0 - noise));
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rgb, src.a));
}
