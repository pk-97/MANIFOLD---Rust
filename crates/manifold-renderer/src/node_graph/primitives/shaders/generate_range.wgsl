// node.generate_range — emit `count` samples linearly spaced over
// `[start, end]` into an Array<f32>. The Pattern-CHOP atom; pair with
// array_math + pack_curve_xy for parametric curve graphs.
//
// Sample i (for i in [0, count)):
//   out[i] = start + i * (end - start) / max(count - 1, 1)
//
// End-inclusive: out[0] = start, out[count - 1] = end. For count == 1
// the divisor floors to 1 and the single sample emitted is `start`.

struct RangeUniforms {
    count: u32,
    _pad0: u32,
    start: f32,
    end:   f32,
};

@group(0) @binding(0) var<uniform> params: RangeUniforms;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    // max(count - 1, 1) guards count == 1 (avoid div-by-zero and emit
    // `start` as the single sample). Also keeps the divisor as u32 →
    // f32 with no precision loss across the usable range.
    let denom = f32(max(params.count - 1u, 1u));
    let span = params.end - params.start;
    out[i] = params.start + f32(i) * span / denom;
}
