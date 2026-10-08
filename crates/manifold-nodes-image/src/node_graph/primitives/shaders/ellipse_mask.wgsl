// node.ellipse_mask — rotated elliptical SDF mask.
//
// Output: inside the ellipse the mask is 1.0; outside it's 0.0;
// the transition smoothsteps over a band of width `softness`
// (measured in normalized-radius units; softness=0 = hard edge,
// softness=1 = the falloff extends a full radius beyond the
// nominal edge).
//
// Pure UV-space — no canvas-aspect correction. Wire aspect via
// `node.texture_dimensions` + `node.math(Divide)` if you want a
// "true circle" on a non-square canvas (set radius_x = R / aspect,
// radius_y = R). Same convention as `distance_to_point` and
// `centered_uv`.
//
// Bindings:
//   @binding(0) uniforms
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    cx: f32,
    cy: f32,
    radius_x: f32,
    radius_y: f32,
    rotation: f32,    // radians
    softness: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    let p = uv - vec2<f32>(u.cx, u.cy);
    let c = cos(u.rotation);
    let s = sin(u.rotation);
    // Inverse rotation — sample the ellipse in its local axis frame.
    let p_rot = vec2<f32>(p.x * c + p.y * s, -p.x * s + p.y * c);
    let rx = max(u.radius_x, 1e-6);
    let ry = max(u.radius_y, 1e-6);
    let n = p_rot / vec2<f32>(rx, ry);
    let dist = length(n);

    let soft = max(u.softness, 0.0);
    let edge_lo = max(1.0 - soft, 0.0);
    let edge_hi = 1.0 + soft;
    let mask = 1.0 - smoothstep(edge_lo, edge_hi, dist);

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(mask, mask, mask, 1.0));
}
