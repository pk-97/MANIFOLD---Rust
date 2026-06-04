// node.resolve_accumulator — BUFFER→TEXTURE resolve body (freeze §12). Reads the
// u32 fixed-point accumulator cell, divides by `fixed_point_scale`, writes a
// grayscale density, and self-clears the cell to zero. Matches
// resolve_accumulator.wgsl.
//
// ABI: body(idx, fixed_point_scale) -> vec4 density. The accumulator is the
// `buf_accum` global (atomic read_write); the resolve wrapper computes `idx`
// from the dispatch id + output-texture dims and stores the returned vec4. The
// `atomicStore(0)` self-clear means the next frame's scatter starts fresh (the
// same "resolve owns the zeroing" contract the hand kernel has). inv_scale is
// computed in-body (bit-identical f32 reciprocal to the hand's CPU-side
// 1/fixed_point_scale); the `> 0` guard mirrors the hand run()'s fallback.
fn body(idx: u32, fixed_point_scale: f32) -> vec4<f32> {
    let raw = atomicLoad(&buf_accum[idx]);
    let inv_scale = select(1.0 / 4096.0, 1.0 / fixed_point_scale, fixed_point_scale > 0.0);
    let density = f32(raw) * inv_scale;
    atomicStore(&buf_accum[idx], 0u);
    return vec4<f32>(density, density, density, 1.0);
}
