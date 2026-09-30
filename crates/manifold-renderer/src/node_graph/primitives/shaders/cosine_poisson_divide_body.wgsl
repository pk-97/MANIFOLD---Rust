// node.cosine_poisson_divide — fusable BUFFER body, per element. In cosine
// space the cell-centred 7-point Laplacian with walls on every face is
// diagonal: coefficient k scales by Σ_a (2 cos(π k_a / N_a) − 2) / h², written
// as −4 Σ sin²(π k_a / 2N_a) / h² so the long waves keep their float32
// precision. Dividing by it solves the Poisson equation; the constant mode
// (k = 0) has no inverse and is set to zero, which removes the mean.

fn body(idx: u32, count: u32, e_values: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32) -> f32 {
    if idx == 0u {
        return 0.0;
    }
    let n = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let k = vec3<f32>(vec3<u32>(idx % n.x, (idx / n.x) % n.y, idx / (n.x * n.y)));
    let s = sin(1.5707963267948966 * k / vec3<f32>(n));
    let eigen = -4.0 * dot(s, s) / (cell_size * cell_size);
    return e_values / eigen;
}
