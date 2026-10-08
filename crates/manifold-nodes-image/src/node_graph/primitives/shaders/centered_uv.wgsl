// node.centered_uv — per-pixel centered & per-axis scaled UV.
//
//   R = (u - cx) * scale_x
//   G = (v - cy) * scale_y
//   B = 0
//   A = 1
//
// The canonical "centered around (cx, cy), aspect-corrected" UV space
// for any procedural pattern that wants to compose around a chosen
// origin. With the defaults (cx = cy = 0.5) this is screen-centered;
// override cx/cy to recenter on any UV point. The X-axis multiplier
// typically composes (aspect_ratio * inverse_scale) in one wire; Y
// composes inverse_scale alone — see Plasma Classic.

struct Uniforms {
    cx: f32,
    cy: f32,
    scale_x: f32,
    scale_y: f32,
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
    let x = (uv.x - u.cx) * u.scale_x;
    let y = (uv.y - u.cy) * u.scale_y;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(x, y, 0.0, 1.0));
}
