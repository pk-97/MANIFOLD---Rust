// node.combine_rows — fusable BUFFER body. Element e of
// base_scale · base + scale · Σ_{i < rows} coef[i] · matrix row i, rows of
// `row_length` entries laid end to end. `matrix` and `coef` are gathered through
// buf_matrix and buf_coef; rows past either array's end count as zero, so no
// uniform value can read out of bounds.

fn body(idx: u32, count: u32, e_base: f32, row_length: i32, rows: i32, scale: f32, base_scale: f32) -> f32 {
    let length = u32(max(row_length, 1));
    let matrix_len = arrayLength(&buf_matrix);
    let used = min(u32(max(rows, 0)), arrayLength(&buf_coef));
    var acc = 0.0;
    for (var i = 0u; i < used; i = i + 1u) {
        let at = i * length + idx;
        if at >= matrix_len {
            break;
        }
        acc = acc + buf_coef[i] * buf_matrix[at];
    }
    return base_scale * e_base + scale * acc;
}
