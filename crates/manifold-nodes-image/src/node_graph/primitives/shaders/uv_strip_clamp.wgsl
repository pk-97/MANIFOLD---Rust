// node.uv_strip_clamp — edge-stretch coordinate generator. Clamps the
// per-pixel UV to a center strip of width `width` on the selected axis
// (Horiz / Vert / Both); pixels outside the strip collapse to its edge, so
// resampling there stretches the edge row/column outward. Verbatim port of
// the legacy node.edge_stretch clamp (sample + crossfade split into
// node.remap + node.mix).
//
// R = clamped_u, G = clamped_v, B = 0, A = 1.

struct Uniforms {
    width: f32,  // full strip width; half_width = width * 0.5
    mode: u32,   // 0 = Horiz, 1 = Vert, 2 = Both
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let half_width = u.width * 0.5;
    let lo = 0.5 - half_width;
    let hi = 0.5 + half_width;

    var s = uv;
    if u.mode == 0u || u.mode == 2u {
        s.x = clamp(uv.x, lo, hi);
    }
    if u.mode == 1u || u.mode == 2u {
        s.y = clamp(uv.y, lo, hi);
    }
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(s.x, s.y, 0.0, 1.0));
}
