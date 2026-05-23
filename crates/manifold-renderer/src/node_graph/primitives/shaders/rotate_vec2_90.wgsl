// node.rotate_vec2_90 — rotate the RG vec2 field by ±90°.
// Out.r = -in.g (for +90° / "math convention" CCW)
// Out.g =  in.r
// (or negate both for -90° / CW; param toggles the sign).
//
// The curl-from-gradient pattern: rotating a gradient by 90° gives the
// perpendicular direction, which is the velocity component of a curl
// (divergence-free flow). Used by every fluid-sim / reaction-diffusion
// curl forcing step.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    direction: u32,   // 0 = +90° CCW, 1 = -90° CW
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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
    var r: vec2<f32>;
    if uniforms.direction == 0u {
        // +90° CCW: (x, y) -> (-y, x)
        r = vec2<f32>(-v.y, v.x);
    } else {
        // -90° CW: (x, y) -> (y, -x)
        r = vec2<f32>(v.y, -v.x);
    }
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(r, 0.0, 1.0));
}
