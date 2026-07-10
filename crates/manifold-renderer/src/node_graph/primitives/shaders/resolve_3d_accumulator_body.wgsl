// node.resolve_3d_accumulator — BUFFER→TEXTURE resolve body (freeze §12). Reads
// the u32 fixed-point 3D accumulator cell, divides by fixed_point_scale
// (default 4096, FluidSim3D's legacy multiplier), writes the density into R
// (G/B = 0), and self-clears the cell. Mirrors the 2D resolve_accumulator's
// parameterized scale: FluidSim3D's density-normalized containers need
// fractional per-particle energies, which only stay representable when the
// splat energy and this divisor are raised together (the preset runs 16x:
// energy x16, scale 65536).
//
// ABI: body(idx, vol_res, vol_depth, fixed_point_scale) -> vec4 density. The
// accumulator is the `buf_accum` global (atomic read_write); the resolve
// wrapper derives `idx` and the dispatch grid from the output Texture3D's
// dims, so vol_res / vol_depth are unused here — they stay as the user-facing
// params (the codegen passes them and DCE drops them). The `> 0` guard mirrors
// the 2D resolve's fallback. `atomicStore(0)` self-clears for the next frame.
fn body(idx: u32, vol_res: i32, vol_depth: i32, fixed_point_scale: f32) -> vec4<f32> {
    let raw = atomicLoad(&buf_accum[idx]);
    let inv_scale = select(1.0 / 4096.0, 1.0 / fixed_point_scale, fixed_point_scale > 0.0);
    let density = f32(raw) * inv_scale;
    atomicStore(&buf_accum[idx], 0u);
    return vec4<f32>(density, 0.0, 0.0, 1.0);
}
