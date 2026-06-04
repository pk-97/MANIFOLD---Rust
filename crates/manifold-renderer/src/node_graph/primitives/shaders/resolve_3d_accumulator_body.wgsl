// node.resolve_3d_accumulator — BUFFER→TEXTURE resolve body (freeze §12). Reads
// the u32 fixed-point 3D accumulator cell, divides by 4096 (FluidSim3D's fixed-
// point multiplier), writes the density into R (G/B = 0), and self-clears the
// cell. Matches fluid_scatter_3d.wgsl `resolve_3d`.
//
// ABI: body(idx, vol_res, vol_depth) -> vec4 density. The accumulator is the
// `buf_accum` global (atomic read_write); the resolve wrapper derives `idx` and
// the dispatch grid from the output Texture3D's dims, so vol_res / vol_depth are
// unused here — they stay as the user-facing params (the codegen passes them and
// DCE drops them). `atomicStore(0)` self-clears for the next frame.
fn body(idx: u32, vol_res: i32, vol_depth: i32) -> vec4<f32> {
    let raw = atomicLoad(&buf_accum[idx]);
    let density = f32(raw) / 4096.0;
    atomicStore(&buf_accum[idx], 0u);
    return vec4<f32>(density, 0.0, 0.0, 1.0);
}
