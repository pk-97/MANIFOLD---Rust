// node.centered_uv — per-pixel centered & per-axis scaled UV.
//
//   R = (u - 0.5) * scale_x
//   G = (v - 0.5) * scale_y
//   B = 0
//   A = 1
//
// The canonical "centered around 0, aspect-corrected" UV space for any
// procedural pattern that wants to compose around screen center. The
// X-axis multiplier typically composes (aspect_ratio * inverse_scale)
// in one wire; Y composes inverse_scale alone — see Plasma Classic.

struct Uniforms {
    scale_x: f32,
    scale_y: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let x = (uv.x - 0.5) * u.scale_x;
    let y = (uv.y - 0.5) * u.scale_y;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(x, y, 0.0, 1.0));
}
