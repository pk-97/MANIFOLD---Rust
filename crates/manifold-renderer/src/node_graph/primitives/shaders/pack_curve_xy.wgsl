// node.pack_curve_xy — zip two Array<f32> (x, y) into one
// Array<CurvePoint>. The curve-pipeline counterpart to
// node.array_unpack_vec2.
//
// Per element:
//   out[i].xy = vec2(x[i] * scale * PROJ_SCALE, y[i] * scale * PROJ_SCALE)
//
// PROJ_SCALE = 0.25 is the screen-fit factor baked into the curve-space
// contract render_lines expects — matches the legacy LissajousGenerator's
// use of generator_math::PROJ_SCALE so existing presets stay visually
// identical when their curve source switches to a decomposed graph.

struct PackUniforms {
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    scale: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
};

struct CurvePoint {
    xy: vec2<f32>,
};

const PROJ_SCALE: f32 = 0.25;

@group(0) @binding(0) var<uniform> params: PackUniforms;
@group(0) @binding(1) var<storage, read>       x:   array<f32>;
@group(0) @binding(2) var<storage, read>       y:   array<f32>;
@group(0) @binding(3) var<storage, read_write> out: array<CurvePoint>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    let k = params.scale * PROJ_SCALE;
    out[i].xy = vec2<f32>(x[i] * k, y[i] * k);
}
