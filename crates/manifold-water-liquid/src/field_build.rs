//! The water field build's constants: how deep the field's band reaches,
//! how it is smoothed, how surface weights are counted, and how far the field
//! is carried into drawn solids. One home, so the build, the march and the
//! card read the same numbers. Settled by the level-set prototype on the Dam
//! Break at 64 and 128 (GPU_FLIP_END_GAME_DESIGN.md D2, "The field build,
//! settled by prototype").

/// [1 2 1] smoothing steps per axis (D2 step 3).
pub const FIELD_SMOOTH_STEPS: u32 = 3;

/// How far smoothing may move the field from the particles' splat, in
/// cells: half a cell, the finest change a node-sampled grid represents
/// (D2 step 4).
pub const FIELD_LEASH_CELLS: f32 = 0.5;

/// The Water card's Smoothness at most: the leash it scales reaches twice
/// `FIELD_LEASH_CELLS`.
pub const FIELD_SMOOTHNESS_MAX: f32 = 2.0;

/// The field's band in cells: the smoothing's reach (one cell per [1 2 1]
/// step) plus the leash, rounded up, so every node smoothing can move lies
/// inside it. The weights, the leash and the carry into solids work within
/// it. The leash term is the largest leash the Smoothness slider reaches.
pub const FIELD_BAND_CELLS: f32 =
    band_cells(FIELD_SMOOTH_STEPS, FIELD_LEASH_CELLS * FIELD_SMOOTHNESS_MAX);

const fn band_cells(steps: u32, leash_cells: f32) -> f32 {
    let reach = steps as f32 + leash_cells;
    let whole = reach as u32 as f32;
    if whole < reach { whole + 1.0 } else { whole }
}

/// The most display nodes per simulation cell along an axis (Surface Detail
/// 2). The smoothing and Close Gaps stage this many nodes and one more
/// round each tile.
pub const MAX_DISPLAY_STRIDE: u32 = 3;

/// Display nodes per simulation cell along an axis, the stride at which the
/// smoothing and Close Gaps reach a simulation cell: a whole number from 1
/// to [`MAX_DISPLAY_STRIDE`], or an error naming `node`.
pub fn display_stride(node: &str, sim_cell_size: f32, cell_size: f32) -> Result<u32, String> {
    let ratio = sim_cell_size / cell_size;
    let stride = ratio.round();
    if !(ratio.is_finite() && (ratio - stride).abs() <= 1e-3 * stride) {
        return Err(format!(
            "{node}: the simulation cell {sim_cell_size} m is not a whole number of {cell_size} m display cells"
        ));
    }
    if !(1.0..=MAX_DISPLAY_STRIDE as f32).contains(&stride) {
        return Err(format!(
            "{node}: {stride} display cells per simulation cell; at most {MAX_DISPLAY_STRIDE}"
        ));
    }
    Ok(stride as u32)
}

/// How far a particle's surface blob looks for the neighbours that shape it,
/// in simulation cells: about thirty particles in bulk water.
pub const BLOB_NEIGHBOUR_CELLS: f32 = 1.0;

/// Neighbours within [`BLOB_NEIGHBOUR_CELLS`] a blob needs for its full
/// stretch and centre smoothing; fewer fade both out. Three is the fewest
/// that span a sheet: a FLIP sheet thinned by spreading holds two to four
/// within a cell.
pub const BLOB_FULL_NEIGHBOURS: f32 = 3.0;

/// Particles within a blob's blend radius (1.5 round radii) at which it takes
/// its full size: none leaves a lone drop at its own radius, one shrinks it
/// halfway, and a sheet's particles, one or two cells apart, have more.
pub const BLOB_JOINED_NEIGHBOURS: f32 = 2.0;

/// A blob's longest semi-axis over its round radius at most.
pub const BLOB_MAX_STRETCH: f32 = 2.0;

/// A blob's shortest axis over its longest at least (Yu and Turk's k_r).
pub const BLOB_AXIS_RATIO: f32 = 4.0;

/// How far a blob's centre moves toward its neighbours' weighted mean (Yu
/// and Turk's λ): the particles' own jitter smoothed before any field is
/// built.
pub const BLOB_CENTRE_SMOOTHING: f32 = 0.9;

/// A blob's shortest semi-axis at least: this many display cells for a lone
/// drop (thinner falls between the display's nodes), and this many
/// simulation cells for a blob joined to others, so a sheet is as thick at
/// every Surface Detail.
pub const BLOB_MIN_AXIS_CELLS: f32 = 0.75;

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
    fn the_band_covers_the_smoothing_reach_and_the_leash() {
        assert_eq!(FIELD_BAND_CELLS, 4.0);
        assert_eq!(
            band_cells(FIELD_SMOOTH_STEPS, FIELD_LEASH_CELLS),
            FIELD_BAND_CELLS
        );
        assert_eq!(band_cells(3, 0.0), 3.0);
        assert_eq!(band_cells(2, 0.25), 3.0);
        assert_eq!(band_cells(5, 0.5), 6.0);
    }

    #[test]
    fn the_display_stride_is_whole_display_cells_per_sim_cell() {
        assert_eq!(display_stride("t", 0.15, 0.15), Ok(1));
        assert_eq!(display_stride("t", 0.11, 0.055), Ok(2));
        assert_eq!(display_stride("t", 0.15, 0.15 / 3.0), Ok(3));
        assert!(display_stride("t", 0.1, 0.025).is_err());
        assert!(display_stride("t", 0.1, 0.04).is_err());
        assert!(display_stride("t", 0.1, 0.2).is_err());
        assert!(display_stride("t", 0.1, 0.0).is_err());
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
