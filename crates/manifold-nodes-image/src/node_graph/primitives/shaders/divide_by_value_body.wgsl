// node.divide_by_value — fusable BUFFER body. values[e] / divisor[0], or 0
// when the divisor is under 1e-30 in size (a finished Krylov solve) or the
// divisor array is empty. `divisor` is gathered through buf_divisor.

fn body(idx: u32, count: u32, e_values: f32) -> f32 {
    if arrayLength(&buf_divisor) == 0u {
        return 0.0;
    }
    let d = buf_divisor[0];
    if abs(d) < 1e-30 {
        return 0.0;
    }
    return e_values / d;
}
