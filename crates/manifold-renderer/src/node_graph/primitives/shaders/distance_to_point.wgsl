// node.distance_to_point — per-pixel Euclidean distance from a
// configurable center point, written into all 4 output channels
// (RGB = distance, A = 1) so downstream per-pixel-math primitives
// can read the scalar from any channel.
//
// Distance is in normalized UV units — so the maximum value is
// roughly sqrt(2) ≈ 1.414 when center is at a corner.

struct Uniforms {
    cx:    f32,   // center x in UV (0..1)
    cy:    f32,   // center y in UV (0..1)
    scale: f32,   // multiplier on the distance (default 1.0)
    _pad:  f32,
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
    let d = length(uv - vec2<f32>(u.cx, u.cy)) * u.scale;
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(d, d, d, 1.0));
}
