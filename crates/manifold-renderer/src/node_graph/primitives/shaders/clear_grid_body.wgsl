// node.clear_grid — fusable BUFFER body, SOURCE. Zeroes the i32
// mass/momentum accumulation wire (4 * nx * ny * nz items:
// 4*g+0..2 momentum xyz, 4*g+3 mass) at the start of every substep. Sticky
// fault status is never touched here — only reset clears it.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the i32 element written to
// buf_out[idx]. `max_capacity` is the allocation-only convention param —
// accepted and ignored (DCE drops it). The wrapper still binds its
// dispatch_count uniform; the dispatch grid is sized by run() to the
// configured item count.
fn body(idx: u32, count: u32, max_capacity: i32) -> i32 {
    return 0;
}
