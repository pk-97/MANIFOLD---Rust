// node.collar_pressure — fusable BUFFER body. solved − correction + c in
// water, 0 in air; c is the collar vector's last element, gathered through
// buf_vector.

fn body(idx: u32, count: u32, e_water: f32, e_solved: f32, e_correction: f32) -> f32 {
    let n = arrayLength(&buf_vector);
    if e_water <= 0.5 || n == 0u {
        return 0.0;
    }
    return e_solved - e_correction + buf_vector[n - 1u];
}
