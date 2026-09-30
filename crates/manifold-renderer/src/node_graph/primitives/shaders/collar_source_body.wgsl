// node.collar_source — fusable BUFFER body, GATHER. One thread per cell: the
// collar entry's value where the running total steps up, else 0. The
// vector's last element is its constant, never a cell's. `total` and `value`
// are gathered through buf_total and buf_value.

fn body(idx: u32, count: u32) -> f32 {
    if idx >= arrayLength(&buf_total) {
        return 0.0;
    }
    let running = buf_total[idx];
    let previous = select(0u, buf_total[max(idx, 1u) - 1u], idx > 0u);
    let entries = max(arrayLength(&buf_value), 1u) - 1u;
    if running <= previous || running - 1u >= entries {
        return 0.0;
    }
    return buf_value[running - 1u];
}
