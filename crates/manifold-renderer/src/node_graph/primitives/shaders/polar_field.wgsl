// node.polar_field — per-pixel polar coordinates around a
// configurable center.
//
// R = angle in radians, normalized into [0, 1] via (atan2 + π) / (2π).
// G = radius (Euclidean distance from center, in UV units).
// B = 0
// A = 1

struct Uniforms {
    cx:   f32,   // center x in UV
    cy:   f32,   // center y in UV
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

const TAU: f32 = 6.28318530717958647692;
const PI:  f32 = 3.14159265358979323846;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let d = uv - vec2<f32>(u.cx, u.cy);
    let angle = (atan2(d.y, d.x) + PI) / TAU;   // normalized 0..1
    let radius = length(d);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(angle, radius, 0.0, 1.0));
}
