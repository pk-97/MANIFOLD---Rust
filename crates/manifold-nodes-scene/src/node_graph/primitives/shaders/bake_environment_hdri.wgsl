// node.bake_environment, mode = HDRI: the wired `hdri` texture scaled by
// `intensity` and turned about the up axis by `rotation`. The output is
// allocated at the HDRI's own size, so this is a 1:1 copy shifted along the
// longitude; the scaling only covers a size mismatch during a resize.
//
// The renderer reads the map at u = atan2(z, x) / 2pi + 0.5 (`pbr_equirect_uv`),
// so a counter-clockwise turn by `rotation` seen from above shows at column
// u what the file holds at u + rotation / 2pi.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) dst_tex (rgba16float storage)
//   @binding(2) src_tex (the HDRI)

const TAU: f32 = 6.283185307179586;

struct Uniforms {
    intensity: f32,
    rotation: f32,
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
    let u = fract((f32(gid.x) + 0.5) / f32(dst_dims.x) + uniforms.rotation / TAU);
    let x = min(u32(u * f32(src_dims.x)), src_dims.x - 1u);
    let y = min(gid.y * src_dims.y / dst_dims.y, src_dims.y - 1u);
    let radiance = textureLoad(src_tex, vec2<i32>(i32(x), i32(y)), 0).rgb;
    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(radiance * uniforms.intensity, 1.0));
}
