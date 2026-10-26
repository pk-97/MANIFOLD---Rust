// Weighted, leashed blend of a smoothed level set toward its prior, held within
// a leash of the re-distanced reference. Every input is read at this node's own
// index; lattice sizes are in metres, band in cells. The leash is Smoothness
// times the field build's half-cell leash (Smoothness 1 is the tuned surface).
const FSB_LEASH_CELLS: f32 = 0.5; // field_build::FIELD_LEASH_CELLS
fn body(idx: u32, count: u32, e_smoothed: f32, e_prior: f32, e_weights: f32, e_reference: f32, nodes_x: f32, nodes_y: f32, nodes_z: f32, size_x: f32, size_y: f32, size_z: f32, smoothness: f32, band_cells: f32) -> f32 {
    if min(min(nodes_x, nodes_y), nodes_z) < 2.0 { return e_smoothed; }
    let cell = min(min(size_x / (nodes_x - 1.0), size_y / (nodes_y - 1.0)), size_z / (nodes_z - 1.0));
    if abs(e_reference) >= band_cells * cell { return e_prior; }
    let leash = smoothness * FSB_LEASH_CELLS * cell;
    return clamp(e_prior + e_weights * (e_smoothed - e_prior), e_reference - leash, e_reference + leash);
}
