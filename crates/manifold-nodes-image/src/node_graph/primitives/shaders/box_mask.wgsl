// node.box_mask — rotated rectangular SDF mask (Chebyshev distance).
//
// Output: inside the box the mask is 1.0; outside it's 0.0; smoothstep
// falloff of width `softness` (in normalized half-extent units).
//
// `half_width` and `half_height` are the box's half-extents from the
// center — same convention as `ellipse_mask`'s `radius_x`/`radius_y`.
// To make a band (DoF tilt-shift, scanline, letterbox) set one half-
// extent past the canvas edge (e.g. half_width = 1.0 spans the canvas
// in X regardless of where the center is) and the other to the band's
// half-thickness.
//
// Pure UV-space — no canvas-aspect correction. Wire aspect from
// `node.texture_dimensions` if you want a true square on a non-square
// canvas. Same convention as `ellipse_mask`.
//
// Bindings:
//   @binding(0) uniforms
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    cx: f32,
    cy: f32,
    half_width: f32,
    half_height: f32,
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
    // Inverse rotation — sample the box in its local axis frame.
    let p_rot = vec2<f32>(p.x * c + p.y * s, -p.x * s + p.y * c);
    let hw = max(u.half_width, 1e-6);
    let hh = max(u.half_height, 1e-6);
    let n_abs = abs(p_rot) / vec2<f32>(hw, hh);
    // Chebyshev distance — box SDF in normalized space.
    let dist = max(n_abs.x, n_abs.y);

    let soft = max(u.softness, 0.0);
    let edge_lo = max(1.0 - soft, 0.0);
    let edge_hi = 1.0 + soft;
    let mask = 1.0 - smoothstep(edge_lo, edge_hi, dist);

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(mask, mask, mask, 1.0));
}
