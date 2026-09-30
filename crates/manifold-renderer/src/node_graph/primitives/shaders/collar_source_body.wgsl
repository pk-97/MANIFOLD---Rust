// node.collar_source — fusable BUFFER body, GATHER `total` and `value`,
// COINCIDENT optional `base`. One thread per cell: base (0 past base_len)
// plus scale times the collar entry's value where the running total steps up.

fn body(idx: u32, count: u32, e_base: f32, scale: f32, base_len: u32) -> f32 {
    let base = select(0.0, e_base, idx < base_len);
    if idx >= arrayLength(&buf_total) { return base; }
    let running = buf_total[idx];
    let previous = select(0u, buf_total[max(idx, 1u) - 1u], idx > 0u);
    let entries = max(arrayLength(&buf_value), 1u) - 1u;
    if running <= previous || running - 1u >= entries { return base; }
    return base + scale * buf_value[running - 1u];
}
