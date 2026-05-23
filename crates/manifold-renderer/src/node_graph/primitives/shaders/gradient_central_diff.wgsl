// node.gradient_central_diff — per-pixel central-difference gradient
// of a single channel of an input texture. Output: (dx, dy, 0, 1) in
// RGBA, where dx and dy are the half-difference of neighbouring samples
// along x and y respectively. The classic vec2 gradient used by
// Sobel-light edge detectors, fluid-sim curl-force extraction,
// height-to-normal pipelines (for the tangent part), and any per-pixel
// finite-difference math.
//
// Formula: dx = (R - L) * 0.5; dy = (U - D) * 0.5, where L/R/U/D are
// the chosen channel sampled at uv ± (texel_x, 0) / ± (0, texel_y).
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    channel: u32,   // 0=R, 1=G, 2=B, 3=A
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

fn select_channel(c: vec4<f32>, idx: u32) -> f32 {
    switch idx {
        case 0u: { return c.r; }
        case 1u: { return c.g; }
        case 2u: { return c.b; }
        default: { return c.a; }
    }
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let cL = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>(-inv.x, 0.0), 0.0);
    let cR = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>( inv.x, 0.0), 0.0);
    let cD = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>(0.0, -inv.y), 0.0);
    let cU = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>(0.0,  inv.y), 0.0);

    let dx = (select_channel(cR, uniforms.channel) - select_channel(cL, uniforms.channel)) * 0.5;
    let dy = (select_channel(cU, uniforms.channel) - select_channel(cD, uniforms.channel)) * 0.5;

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(dx, dy, 0.0, 1.0));
}
