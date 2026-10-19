// Pure functions for grid and magnetic snapping.
//
// These match the Unity InteractionOverlay.cs snapping behavior exactly:
// - Grid interval depends on zoom level (pixels-per-beat)
// - Magnetic snap finds nearest candidate within a pixel threshold
// - Candidates include grid lines AND neighboring clip edges

use manifold_foundation::Beats;

/// Threshold in pixels — snap engages within this distance.
pub const SNAP_THRESHOLD_PX: f32 = 12.0;

/// Compute the grid interval based on the current zoom level (pixels per beat).
///
/// From Unity CoordinateMapper.cs:
/// - ppb >= 16: 0.25 beats (16th notes)
/// - ppb >= 12: 0.5 beats (8th notes)
/// - ppb >= 6:  1.0 beat (quarter notes)
/// - ppb < 6:   full bars
pub fn grid_interval_for_zoom(ppb: f32, beats_per_bar: f32) -> f32 {
    if ppb >= 16.0 {
        0.25
    } else if ppb >= 12.0 {
        0.5
    } else if ppb >= 6.0 {
        1.0
    } else {
        beats_per_bar.max(1.0)
    }
}

/// Snap a beat to the nearest grid line.
pub fn snap_beat_to_grid(beat: Beats, grid_interval: Beats) -> Beats {
    if grid_interval.0 <= 0.0 {
        return beat;
    }
    Beats((beat.0 / grid_interval.0).round() * grid_interval.0)
}

/// Floor a beat to the left edge of the grid cell.
/// Used for placement operations (double-click clip creation) where the click
/// should land in the grid cell the cursor is inside, not snap to nearest line.
/// From Unity CoordinateMapper.FloorBeatToGrid (lines 262-266).
pub fn floor_beat_to_grid(beat: Beats, grid_interval: Beats) -> Beats {
    if grid_interval.0 <= 0.0 {
        return beat;
    }
    Beats(((beat.0 / grid_interval.0).floor() * grid_interval.0).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_interval_at_each_zoom_level() {
        // ppb >= 16: 16th notes (0.25 beats)
        assert_eq!(grid_interval_for_zoom(16.0, 4.0), 0.25);
        assert_eq!(grid_interval_for_zoom(120.0, 4.0), 0.25);
        assert_eq!(grid_interval_for_zoom(400.0, 4.0), 0.25);

        // ppb >= 12 but < 16: 8th notes (0.5 beats)
        assert_eq!(grid_interval_for_zoom(12.0, 4.0), 0.5);
        assert_eq!(grid_interval_for_zoom(15.9, 4.0), 0.5);

        // ppb >= 6 but < 12: quarter notes (1.0 beat)
        assert_eq!(grid_interval_for_zoom(6.0, 4.0), 1.0);
        assert_eq!(grid_interval_for_zoom(11.9, 4.0), 1.0);

        // ppb < 6: full bars
        assert_eq!(grid_interval_for_zoom(5.9, 4.0), 4.0);
        assert_eq!(grid_interval_for_zoom(1.0, 4.0), 4.0);

        // Different time signature
        assert_eq!(grid_interval_for_zoom(3.0, 3.0), 3.0);

        // Zero beats_per_bar clamps to 1.0
        assert_eq!(grid_interval_for_zoom(3.0, 0.0), 1.0);
    }

    #[test]
    fn snap_beat_to_grid_rounds_correctly() {
        // Quarter note grid
        assert_eq!(
            snap_beat_to_grid(Beats::from_f32(4.1), Beats::from_f32(1.0)),
            Beats::from_f32(4.0)
        );
        assert_eq!(
            snap_beat_to_grid(Beats::from_f32(4.6), Beats::from_f32(1.0)),
            Beats::from_f32(5.0)
        );
        assert_eq!(
            snap_beat_to_grid(Beats::from_f32(4.5), Beats::from_f32(1.0)),
            Beats::from_f32(5.0)
        ); // .5 rounds away from zero

        // 16th note grid
        assert!(
            (snap_beat_to_grid(Beats::from_f32(4.13), Beats::from_f32(0.25)).as_f32() - 4.25).abs()
                < 0.001
        );
        assert!(
            (snap_beat_to_grid(Beats::from_f32(4.01), Beats::from_f32(0.25)).as_f32() - 4.0).abs()
                < 0.001
        );

        // Zero/negative grid interval returns raw beat
        assert_eq!(
            snap_beat_to_grid(Beats::from_f32(4.3), Beats::from_f32(0.0)),
            Beats::from_f32(4.3)
        );
    }
}
