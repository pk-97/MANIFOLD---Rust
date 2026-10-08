// node.gradient_central_diff — per-pixel central-difference gradient
// of a single channel of an input texture. Output: (dx, dy, 0, 1) in
// RGBA, where dx and dy are the half-difference of neighbouring samples
// along x and y respectively. The classic vec2 gradient used by
// Sobel-light edge detectors, fluid-sim curl-force extraction,
// height-to-normal pipelines (for the tangent part), and any per-pixel
// finite-difference math.
//
// `scale_mode` selects the output scaling:
//   0 = Texel — dx = (R - L) * 0.5; dy = (U - D) * 0.5
//               (default; matches oily-fluid / heightmap-to-normal usage)
//   1 = UV    — dx = (R - L) * W * 0.5; dy = (U - D) * H * 0.5
//               (per-axis multiplied by half the dimension so output is
//                in per-UV-unit space; matches the legacy
//                fluid_gradient_rotate's `grad / (2 * texel)` math)
//
// Boundary policy is resolved MANUALLY (D6(a), no sampler at all): Clamp
// (default) clamps the neighbour index to the texture bounds; Repeat
// modulo-wraps it toroidally — for cyclic fluid-sim density fields.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in
//   @binding(2) output_tex (rgba16float storage)

struct Uniforms {
    channel: u32,    // 0=R, 1=G, 2=B, 3=A
    scale_mode: u32, // 0=Texel, 1=UV
    wrap_mode: u32,  // 0=Clamp, 1=Repeat
    _pad0: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

fn select_channel(c: vec4<f32>, idx: u32) -> f32 {
    switch idx {
        case 0u: { return c.r; }
        case 1u: { return c.g; }
        case 2u: { return c.b; }
        default: { return c.a; }
    }
}

// See gradient_central_diff_body.wgsl's gcd_wrap_coord for the exactness
// argument (offset is always precisely one texel from an exact texel-center
// fragment coordinate, so clamp/modulo agree bit-for-bit with the retired
// sampler-address-mode read).
fn gcd_wrap_coord(c: vec2<i32>, dims_i: vec2<i32>, wrap_mode: u32) -> vec2<i32> {
    if wrap_mode == 1u {
        return ((c % dims_i) + dims_i) % dims_i;
    }
    return clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(id.xy);

    let cL = textureLoad(tex_in, gcd_wrap_coord(c - vec2<i32>(1, 0), dims_i, uniforms.wrap_mode), 0);
    let cR = textureLoad(tex_in, gcd_wrap_coord(c + vec2<i32>(1, 0), dims_i, uniforms.wrap_mode), 0);
    let cD = textureLoad(tex_in, gcd_wrap_coord(c - vec2<i32>(0, 1), dims_i, uniforms.wrap_mode), 0);
    let cU = textureLoad(tex_in, gcd_wrap_coord(c + vec2<i32>(0, 1), dims_i, uniforms.wrap_mode), 0);

    let diff_x = select_channel(cR, uniforms.channel) - select_channel(cL, uniforms.channel);
    let diff_y = select_channel(cU, uniforms.channel) - select_channel(cD, uniforms.channel);

    // scale_mode = 0 (Texel): factor = 0.5 on both axes.
    // scale_mode = 1 (UV):    factor = W*0.5 on x, H*0.5 on y so the
    //                         output is per-UV-unit (matches legacy
    //                         fluid_gradient_rotate's `grad / (2 * texel)`).
    let scale_xy = select(
        vec2<f32>(0.5, 0.5),
        vec2<f32>(f32(dims.x) * 0.5, f32(dims.y) * 0.5),
        uniforms.scale_mode == 1u,
    );
    let dx = diff_x * scale_xy.x;
    let dy = diff_y * scale_xy.y;

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(dx, dy, 0.0, 1.0));
}
