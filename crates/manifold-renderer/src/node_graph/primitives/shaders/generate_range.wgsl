// node.generate_range — emit `count` samples linearly spaced over
// `[start, end]` into an Array<f32>. The Pattern-CHOP atom; pair with
// array_math + pack_curve_xy for parametric curve graphs.
//
// End-inclusive (end_inclusive = 1): out[0] = start, out[count-1] = end.
//   denom = max(count - 1, 1) (guards count == 1, emits `start`).
//   The conventional shape for closed parametric curves (Lissajous).
// End-exclusive (end_inclusive = 0): out[0] = start, step = (end-start)/count.
//   The right shape for regular N-gons sampled around a circle, where
//   vertex 0 and vertex count-1 must be distinct points.

struct RangeUniforms {
    count:         u32,
    end_inclusive: u32,
    start:         f32,
    end:           f32,
};

@group(0) @binding(0) var<uniform> params: RangeUniforms;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.count {
        return;
    }
    // End-inclusive: denom = max(count - 1, 1). End-exclusive: denom = count.
    // The `max(., 1u)` floor guards count == 1 in the inclusive branch
    // (avoids div-by-zero, emits `start` as the single sample).
    var denom: u32;
    if params.end_inclusive == 1u {
        denom = max(params.count - 1u, 1u);
    } else {
        denom = max(params.count, 1u);
    }
    let span = params.end - params.start;
    out[i] = params.start + f32(i) * span / f32(denom);
}
