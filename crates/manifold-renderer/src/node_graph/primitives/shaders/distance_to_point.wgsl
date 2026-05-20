// node.distance_to_point — per-pixel Euclidean distance from a
// configurable center point, with optional anisotropic per-axis
// scaling. Written into all 4 output channels (RGB = distance, A = 1)
// so downstream per-pixel-math primitives can read the scalar from any
// channel.
//
//   d = length((uv - center) * (scale_x, scale_y)) * scale
//
// `scale_x` and `scale_y` default to 1.0; non-default values let the
// primitive compute aspect-corrected or stretched distance fields
// (e.g. plasma_classic's `length(uv * freq * 1.2)` with aspect on x).
// `scale` is a final uniform multiplier on top, kept for backward
// compatibility with the simpler isotropic-scale case.

struct Uniforms {
    cx:      f32,   // center x in UV (0..1)
    cy:      f32,   // center y in UV (0..1)
    scale:   f32,   // multiplier on the distance (default 1.0)
    scale_x: f32,   // per-axis scale on x (default 1.0)
    scale_y: f32,   // per-axis scale on y (default 1.0)
    _pad0:   f32,
    _pad1:   f32,
    _pad2:   f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let offset = (uv - vec2<f32>(u.cx, u.cy)) * vec2<f32>(u.scale_x, u.scale_y);
    let d = length(offset) * u.scale;
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(d, d, d, 1.0));
}
