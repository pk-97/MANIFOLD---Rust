// node.krylov_solve — fusable BUFFER body, GATHER. R y = g by
// back-substitution, R[k][l] = state[l·(m + 1) + k], g at m(m + 1) + 2m.
// Each thread runs the whole solve and returns its own y; a diagonal under
// 1e-30 in size gives y = 0. m is clamped to the 32 the local array holds;
// a state shorter than m needs, or a thread past m, gives 0.

fn body(idx: u32, count: u32, passes: i32) -> f32 {
    let m = min(u32(max(passes, 1)), 32u);
    if idx >= m || m * m + 4u * m + 1u > arrayLength(&buf_state) {
        return 0.0;
    }
    let off_g = m * (m + 1u) + 2u * m;
    var y: array<f32, 32>;
    for (var step = 0u; step < m; step = step + 1u) {
        let k = m - 1u - step;
        var acc = buf_state[off_g + k];
        for (var l = k + 1u; l < m; l = l + 1u) {
            acc = acc - buf_state[l * (m + 1u) + k] * y[l];
        }
        let d = buf_state[k * (m + 1u) + k];
        y[k] = select(0.0, acc / d, abs(d) > 1e-30);
    }
    return y[idx];
}
