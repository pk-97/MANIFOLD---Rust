// node.linear_gradient — directional 0→1 ramp in UV space.
//
// Conceptually a smoothstep across a line at (cx, cy) perpendicular to
// `rotation`. Output: 0 on the "negative" side of that line, 1 on the
// "positive" side, smoothstepped across a band of width `softness`
// centred at (cx, cy). With softness near 0 this is a hard step; with
// softness near 1.5+ it spans the canvas diagonal.
//
// Pure UV-space — no canvas-aspect correction (same convention as
// ellipse_mask / box_mask).
//
// Bindings:
//   @binding(0) uniforms
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    cx: f32,
    cy: f32,
    rotation: f32,    // radians; 0 = +X gradient direction
    softness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
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
    let dir = vec2<f32>(cos(u.rotation), sin(u.rotation));
    let t = dot(p, dir);

    let half_soft = max(u.softness * 0.5, 1e-6);
    let mask = smoothstep(-half_soft, half_soft, t);

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(mask, mask, mask, 1.0));
}
