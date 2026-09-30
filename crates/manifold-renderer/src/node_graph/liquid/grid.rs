//! The face grid a liquid domain publishes (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 3.2 (Grid outputs)): MAC faces in the FLIP engine's layout over
//! the domain's cells, one f32 array per axis in m/s, scene space. Every
//! solver resamples its own lattice into this layout; no consumer sees a
//! native one.

/// The frame ports that carry the grid: one array per axis, the cells per
/// axis, and how many face layers past the liquid carry velocity.
pub const FACE_GRID_PORTS: [&str; 7] =
    ["face_u", "face_v", "face_w", "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"];

/// FLIP's MAC trilinear on the face arrays, for bodies that sample velocity
/// at a point.
pub(crate) const LIQUID_FACES: &str = include_str!("../primitives/shaders/liquid_faces.wgsl");

/// Faces per axis of `axis`'s array: one more than the cells along `axis`,
/// the cells on the other two.
pub fn face_dims(cells: [u32; 3], axis: usize) -> [u32; 3] {
    let mut dims = cells;
    dims[axis] += 1;
    dims
}

/// Records in `axis`'s array, in u64 so no size wraps.
pub fn face_len(cells: [u32; 3], axis: usize) -> u64 {
    face_dims(cells, axis).iter().map(|&n| u64::from(n)).product()
}

/// Index of face `f` in `axis`'s array, x fastest.
pub fn face_index(cells: [u32; 3], axis: usize, f: [u32; 3]) -> usize {
    let d = face_dims(cells, axis).map(|n| n as usize);
    let f = f.map(|n| n as usize);
    f[0] + d[0] * (f[1] + d[1] * f[2])
}

/// Face `f` of `axis`'s array from its index.
pub fn face_coords(cells: [u32; 3], axis: usize, index: usize) -> [u32; 3] {
    let d = face_dims(cells, axis).map(|n| n as usize);
    [index % d[0], (index / d[0]) % d[1], index / (d[0] * d[1])].map(|n| n as u32)
}

/// Where face `f` of `axis` sits, in scene metres: on the cell boundary
/// along `axis`, at the cell centre on the other two.
pub fn face_position(min: [f32; 3], cell_size: f32, axis: usize, f: [u32; 3]) -> [f32; 3] {
    std::array::from_fn(|b| {
        let half = if b == axis { 0.0 } else { 0.5 };
        min[b] + (f[b] as f32 + half) * cell_size
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam table's lengths and index rules, on unequal sides so a
    /// swapped axis shows.
    #[test]
    fn face_grid_layout_matches_the_seam_table() {
        let n = [6, 5, 4];
        assert_eq!([0, 1, 2].map(|a| face_len(n, a)), [7 * 5 * 4, 6 * 6 * 4, 6 * 5 * 5]);
        assert_eq!(face_index(n, 0, [2, 3, 1]), 2 + 7 * (3 + 5));
        assert_eq!(face_index(n, 1, [2, 3, 1]), 2 + 6 * (3 + 6));
        assert_eq!(face_index(n, 2, [2, 3, 1]), 2 + 6 * (3 + 5));
        for a in 0..3 {
            for i in 0..face_len(n, a) as usize {
                assert_eq!(face_index(n, a, face_coords(n, a, i)), i);
            }
        }
        assert_eq!(face_position([-2.0, 0.0, 1.0], 0.5, 1, [1, 2, 3]), [-1.25, 1.0, 2.75]);
    }
}
