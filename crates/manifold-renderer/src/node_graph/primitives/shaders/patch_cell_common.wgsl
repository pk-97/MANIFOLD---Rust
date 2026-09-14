// Shared reference-centroid spatial-cell quantization.
fn patch_cell_center(centroid_normalized: vec3<f32>, cell_size: f32) -> vec3<f32> {
    let safe_cell = max(abs(cell_size), 1e-6);
    return floor(centroid_normalized / safe_cell + vec3<f32>(0.5)) * safe_cell;
}
