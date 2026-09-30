// node.cosine_surface_scale — fusable BUFFER body, per element. In 2D cosine
// space the walled surface Laplacian is diagonal, −Δ_s → 4 Σ sin²(π k / 2N) / h²
// over the plane's two axes; the six-view helper scales each coefficient by
// the square root of that plus q0², the lowest wave the box holds. Planes are
// nodes_x × nodes_y, stacked along z.

fn body(idx: u32, count: u32, e_values: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32, lowest_wave: f32) -> f32 {
    let n = vec2<u32>(vec2<f32>(nodes_x, nodes_y));
    let k = vec2<f32>(vec2<u32>(idx % n.x, (idx / n.x) % n.y));
    let s = sin(1.5707963267948966 * k / vec2<f32>(n));
    let bend = 4.0 * dot(s, s) / (cell_size * cell_size);
    return e_values * sqrt(bend + lowest_wave * lowest_wave);
}
