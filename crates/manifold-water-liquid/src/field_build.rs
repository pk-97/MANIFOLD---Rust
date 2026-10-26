//! The water field build's constants: how deep the level set is re-distanced,
//! how it is smoothed, how surface weights are counted, and how far the field
//! is carried into drawn solids. One home, so the build, the march and the
//! card read the same numbers. Settled by the level-set prototype on the Dam
//! Break at 64 and 128 (GPU_FLIP_END_GAME_DESIGN.md D2, "The field build,
//! settled by prototype").

/// [1 2 1] smoothing steps per axis (D2 step 3).
pub const FIELD_SMOOTH_STEPS: u32 = 3;

/// How far smoothing may move the field from its re-distanced value, in
/// cells: half a cell, the finest change a node-sampled grid represents
/// (D2 step 4).
pub const FIELD_LEASH_CELLS: f32 = 0.5;

/// The Water card's Smoothness at most: the leash it scales reaches twice
/// `FIELD_LEASH_CELLS`.
pub const FIELD_SMOOTHNESS_MAX: f32 = 2.0;

/// Re-distance depth in cells: the smoothing's reach (one cell per [1 2 1]
/// step) plus the leash, rounded up, so every node smoothing can touch holds
/// a true distance. The leash term is the largest leash the Smoothness slider
/// reaches.
pub const FIELD_REDISTANCE_CELLS: f32 =
    redistance_cells(FIELD_SMOOTH_STEPS, FIELD_LEASH_CELLS * FIELD_SMOOTHNESS_MAX);

const fn redistance_cells(steps: u32, leash_cells: f32) -> f32 {
    let reach = steps as f32 + leash_cells;
    let whole = reach as u32 as f32;
    if whole < reach { whole + 1.0 } else { whole }
}

/// Neighbour-count radius in field cells: FLIP Fluids' 1.5 radii, measured
/// in the prototype (`neighbour_counts`).
pub const NEIGHBOUR_COUNT_RADIUS_CELLS: f32 = 1.5;

/// Radius over which particle counts and velocities reach the lattice nodes,
/// in field cells: the prototype's `node_counts` interpolation radius.
pub const WEIGHT_CARRY_RADIUS_CELLS: f32 = 2.0;

/// Smoothing weight fades in from this fraction of the expected bulk
/// neighbour count (no smoothing below it)...
pub const WEIGHT_FADE_START: f32 = 0.18;

/// ...to full smoothing at this fraction: the prototype's 20 and 40 of a
/// bulk count of about 110.
pub const WEIGHT_FADE_END: f32 = 0.36;

/// How far the field is carried into a drawn solid, in cells (D2 step 5),
/// so water meets a wall flush instead of stopping short of it.
pub const SOLID_CARRY_CELLS: f32 = 2.0;

/// Neighbours a particle in bulk water has within
/// [`NEIGHBOUR_COUNT_RADIUS_CELLS`]: the particle density per field cell
/// times the volume of that sphere, in cells.
pub fn expected_bulk_neighbours(particles_per_field_cell: f32) -> f32 {
    let r = NEIGHBOUR_COUNT_RADIUS_CELLS;
    particles_per_field_cell * (4.0 / 3.0) * std::f32::consts::PI * r * r * r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redistance_depth_covers_the_smoothing_reach_and_the_leash() {
        assert_eq!(FIELD_REDISTANCE_CELLS, 4.0);
        assert_eq!(
            redistance_cells(FIELD_SMOOTH_STEPS, FIELD_LEASH_CELLS),
            FIELD_REDISTANCE_CELLS
        );
        assert_eq!(redistance_cells(3, 0.0), 3.0);
        assert_eq!(redistance_cells(2, 0.25), 3.0);
        assert_eq!(redistance_cells(5, 0.5), 6.0);
    }

    #[test]
    fn bulk_neighbours_scale_with_density() {
        // Eight particles per cell, the FLIP default: about 113 neighbours,
        // the prototype's bulk count.
        let n = expected_bulk_neighbours(8.0);
        assert!((n - 113.097).abs() < 0.01, "{n}");
        assert!((expected_bulk_neighbours(1.0) * 8.0 - n).abs() < 1e-3);
    }
}
