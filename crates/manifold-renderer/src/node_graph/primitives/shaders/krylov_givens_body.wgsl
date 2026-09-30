// node.krylov_givens — fusable BUFFER body, GATHER. One thread per entry of
// the small GMRES state (m = passes, j = column, the pass index):
//   H[i][l] at l·(m + 1) + i, cs at m(m + 1), sn after it, g after sn.
// Every thread rebuilds column j: h = first + second for rows ≤ j, norm[0]
// in row j + 1, rotated by the stored rotations 0 .. j − 1. The new rotation
// (c, s) zeroes row j + 1; g[j], g[j + 1] become c·g[j], −s·g[j]. A column
// shorter than 1e-30 gets the identity rotation. Entries outside column j,
// cs[j], sn[j], g[j] and g[j + 1] pass through. m is clamped to the 32 the
// local column holds, and arrays shorter than m needs leave the state as it
// is, so no uniform value can index out of bounds.

fn body(idx: u32, count: u32, passes: i32, column: i32) -> f32 {
    let m = min(u32(max(passes, 1)), 32u);
    let len = m * m + 4u * m + 1u;
    let state_len = arrayLength(&buf_state);
    if idx >= state_len {
        return 0.0;
    }
    let old = buf_state[idx];
    let j = u32(max(column, 0));
    if j >= m || len > state_len || j + 1u > min(arrayLength(&buf_first), arrayLength(&buf_second))
        || arrayLength(&buf_norm) == 0u {
        return old;
    }
    let off_c = m * (m + 1u);
    let off_s = off_c + m;
    let off_g = off_s + m;
    var col: array<f32, 34>;
    for (var i = 0u; i <= j; i = i + 1u) {
        col[i] = buf_first[i] + buf_second[i];
    }
    col[j + 1u] = buf_norm[0];
    for (var i = 0u; i < j; i = i + 1u) {
        let c = buf_state[off_c + i];
        let s = buf_state[off_s + i];
        let t = c * col[i] + s * col[i + 1u];
        col[i + 1u] = -s * col[i] + c * col[i + 1u];
        col[i] = t;
    }
    let r = sqrt(col[j] * col[j] + col[j + 1u] * col[j + 1u]);
    let live = r > 1e-30;
    let c = select(1.0, col[j] / r, live);
    let s = select(0.0, col[j + 1u] / r, live);
    let start = j * (m + 1u);
    if idx >= start && idx < start + m + 1u {
        let i = idx - start;
        if i < j {
            return col[i];
        }
        if i == j {
            return r;
        }
        return 0.0;
    }
    let g = buf_state[off_g + j];
    if idx == off_c + j {
        return c;
    }
    if idx == off_s + j {
        return s;
    }
    if idx == off_g + j {
        return c * g;
    }
    if idx == off_g + j + 1u {
        return -s * g;
    }
    return old;
}
