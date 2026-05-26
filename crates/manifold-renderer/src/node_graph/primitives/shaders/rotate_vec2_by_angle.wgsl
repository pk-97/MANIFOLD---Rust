// node.rotate_vec2_by_angle — per-pixel rotation of the RG vec2 field
// by an arbitrary angle (radians).
//
// Out.r = v.x * cos_a - v.y * sin_a
// Out.g = v.x * sin_a + v.y * cos_a
// Out.b = 0
// Out.a = 1
//
// The general curl-from-gradient atom — defaults to angle = PI/2
// (+90° CCW, the divergence-free curl-flow case) but the angle is
// port-shadow-param so a control wire can sweep it continuously.
// Legacy type-ID `node.rotate_vec2_90` aliases to this primitive
// with the default PI/2 angle.
//
// Bindings:
//   @binding(0) uniforms (cos_a + sin_a + 8 bytes pad → 16 bytes)
//   @binding(1) tex_in
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    cos_a: f32,
    sin_a: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
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

    let v = textureSampleLevel(tex_in, tex_sampler, uv, 0.0).rg;
    let r = vec2<f32>(
        v.x * uniforms.cos_a - v.y * uniforms.sin_a,
        v.x * uniforms.sin_a + v.y * uniforms.cos_a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(r, 0.0, 1.0));
}
