// node.chromatic_displace — 3-tap RGB sample of an input texture
// displaced by a 2D vector field. R and B sample at opposite signed
// offsets along the velocity direction; G samples at the centre.
//
// Per-pixel:
//   v = velocity.rg
//   off = v * amount / dims
//   out.r = sample(in, uv - off).r
//   out.g = sample(in, uv      ).g
//   out.b = sample(in, uv + off).b
//   out.a = sample(in, uv      ).a
//
// Distinct from `node.chromatic_aberration` (radial UV split around a
// centre point — image-domain look). This is FLOW-driven: the offset
// direction comes from a per-pixel velocity field. Used for normal-map
// chromatic split in oily-fluid Oil Slick, signed-field chromatic
// trails, anywhere the displacement direction is data not symmetry.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in
//   @binding(2) tex_velocity
//   @binding(3) tex_sampler
//   @binding(4) output_tex (rgba16float storage)

struct Uniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var tex_velocity: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let v = textureSampleLevel(tex_velocity, tex_sampler, uv, 0.0).rg;
    let off = v * uniforms.amount * inv;

    let s_r = textureSampleLevel(tex_in, tex_sampler, uv - off, 0.0).r;
    let s_c = textureSampleLevel(tex_in, tex_sampler, uv,       0.0);
    let s_b = textureSampleLevel(tex_in, tex_sampler, uv + off, 0.0).b;

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(s_r, s_c.g, s_b, s_c.a));
}
