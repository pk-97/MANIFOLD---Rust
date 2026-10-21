// node.bake_environment, mode = HDRI: the wired `hdri` texture scaled by
// `intensity`. The output is allocated at the HDRI's own size, so this is a
// 1:1 copy; the clamped scaling only covers a size mismatch during a resize.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) dst_tex (rgba16float storage)
//   @binding(2) src_tex (the HDRI)

struct Uniforms {
    intensity: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var src_tex: texture_2d<f32>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_dims = textureDimensions(dst_tex);
    if gid.x >= dst_dims.x || gid.y >= dst_dims.y { return; }
    let src_dims = textureDimensions(src_tex);
    let scaled = vec2<u32>(vec2<f32>(gid.xy) * vec2<f32>(src_dims) / vec2<f32>(dst_dims));
    let texel = min(scaled, src_dims - vec2<u32>(1u, 1u));
    let radiance = textureLoad(src_tex, vec2<i32>(texel), 0).rgb;
    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(radiance * uniforms.intensity, 1.0));
}
