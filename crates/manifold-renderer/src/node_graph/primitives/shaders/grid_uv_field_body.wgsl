// node.grid_uv_field — fusable BUFFER body (freeze §12, buffer domain), SOURCE
// form (0 array inputs). Write the grid-cell-centre UV for element idx on an
// N×N grid: col = idx % N, row = idx / N, uv = ((col+0.5)/N, (row+0.5)/N).
//
// ABI (buffer standalone codegen, source shape): no input arrays, so the body
// takes only (idx, count, params) and returns the output element, which the
// wrapper writes to buf_uv[idx]. `count` is unused (the body derives col/row
// from grid_size). The codegen synthesizes `struct Element { x: f32, y: f32 }`
// from the [f32; 2] Channels signature — std430 stride 8, byte-identical to the
// hand kernel's `array<vec2<f32>>`. `grid_size` arrives as i32 (Int param), cast
// to u32 to match the hand shader's u32 grid arithmetic. Matches grid_uv_field.wgsl.
fn body(idx: u32, count: u32, grid_size: i32) -> Element {
    let gs = u32(grid_size);
    let col = idx % gs;
    let row = idx / gs;
    let inv_n = 1.0 / f32(gs);
    return Element(
        (f32(col) + 0.5) * inv_n,
        (f32(row) + 0.5) * inv_n,
    );
}
