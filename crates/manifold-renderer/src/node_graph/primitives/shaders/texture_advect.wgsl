// node.texture_advect — backward (semi-Lagrangian) advection of a
// texture by a 2D velocity field.
//
// For each output pixel: read the velocity at this UV, sample the
// source at uv - velocity * dt / dims, write that sample. The minus
// sign is the standard fluid advection convention — at each pixel
// "look back" along the velocity to find where this pixel came from.
//
// `dt` has units of pixels-per-frame: dt = 1.0 means displace by
// `velocity` pixels per frame. Resolution-independent because the
// shader divides by texture dims internally.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in        (the field to advect; sampled at displaced UV)
//   @binding(2) tex_velocity  (RG = vec2 velocity)
//   @binding(3) tex_sampler   (Repeat or Clamp depending on host-bound sampler)
//   @binding(4) output_tex    (rgba16float storage)

struct Uniforms {
    dt: f32,
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
    let adv_uv = uv - v * uniforms.dt * inv;
    let sampled = textureSampleLevel(tex_in, tex_sampler, adv_uv, 0.0);

    textureStore(output_tex, vec2<i32>(id.xy), sampled);
}
