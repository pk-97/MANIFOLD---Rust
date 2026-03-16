/// Pure functions for grid and magnetic snapping.
///
/// These match the Unity InteractionOverlay.cs snapping behavior exactly:
/// - Grid interval depends on zoom level (pixels-per-beat)
/// - Magnetic snap finds nearest candidate within a pixel threshold
/// - Candidates include grid lines AND neighboring clip edges

/// Threshold in pixels — snap engages within this distance.
pub const SNAP_THRESHOLD_PX: f32 = 12.0;

/// Maximum snap range in beats — caps behavior at extreme zoom-out
/// so snapping doesn't jump across beats unexpectedly.
pub const MAX_SNAP_BEATS: f32 = 0.5;

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
        beats_per_bar
    }
}

/// Snap a beat to the nearest grid line.
pub fn snap_beat_to_grid(beat: f32, grid_interval: f32) -> f32 {
    if grid_interval <= 0.0 {
        return beat;
    }
    (beat / grid_interval).round() * grid_interval
}

/// Floor a beat to the left edge of the grid cell.
/// Used for placement operations (double-click clip creation) where the click
/// should land in the grid cell the cursor is inside, not snap to nearest line.
/// From Unity CoordinateMapper.FloorBeatToGrid (lines 262-266).
pub fn floor_beat_to_grid(beat: f32, grid_interval: f32) -> f32 {
    if grid_interval <= 0.0 {
        return beat;
    }
    ((beat / grid_interval).floor() * grid_interval).max(0.0)
}

/// Magnetic snap: finds nearest candidate (grid line + neighbor clip edges)
/// and snaps if within pixel threshold.
///
/// # Arguments
/// - `raw_beat`: the unsnapped beat position
/// - `grid_interval`: current grid interval (from `grid_interval_for_zoom`)
/// - `neighbor_edges`: start and end beats of clips on the same layer (excluding self)
/// - `ppb`: pixels per beat (current zoom level)
/// - `snap_threshold_px`: pixel distance threshold (default: 12.0)
/// - `max_snap_beats`: beat-space cap (default: 0.5)
///
/// Returns the snapped beat, or `raw_beat` if nothing is within threshold.
pub fn magnetic_snap_beat(
    raw_beat: f32,
    grid_interval: f32,
    neighbor_edges: &[f32],
    ppb: f32,
    snap_threshold_px: f32,
    max_snap_beats: f32,
) -> f32 {
    if ppb <= 0.0 {
        return raw_beat;
    }

    // Effective threshold: minimum of pixel threshold and beat cap (converted to pixels)
    let threshold_beats = snap_threshold_px / ppb;
    let effective_threshold = threshold_beats.min(max_snap_beats);

    let mut best_beat = raw_beat;
    let mut best_distance = f32::MAX;

    // Candidate 1: nearest grid line
    if grid_interval > 0.0 {
        let grid_beat = (raw_beat / grid_interval).round() * grid_interval;
        let distance = (grid_beat - raw_beat).abs();
        if distance < best_distance {
            best_distance = distance;
            best_beat = grid_beat;
        }
    }

    // Candidates 2+: neighbor clip edges (start and end beats)
    for &edge in neighbor_edges {
        let distance = (edge - raw_beat).abs();
        if distance < best_distance {
            best_distance = distance;
            best_beat = edge;
        }
    }

    // Only snap if the best candidate is within the effective threshold
    if best_distance <= effective_threshold {
        best_beat
    } else {
        raw_beat
    }
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
    }

    #[test]
    fn snap_beat_to_grid_rounds_correctly() {
        // Quarter note grid
        assert_eq!(snap_beat_to_grid(4.1, 1.0), 4.0);
        assert_eq!(snap_beat_to_grid(4.6, 1.0), 5.0);
        assert_eq!(snap_beat_to_grid(4.5, 1.0), 5.0); // .5 rounds away from zero

        // 16th note grid
        assert!((snap_beat_to_grid(4.13, 0.25) - 4.25).abs() < 0.001);
        assert!((snap_beat_to_grid(4.01, 0.25) - 4.0).abs() < 0.001);

        // Zero/negative grid interval returns raw beat
        assert_eq!(snap_beat_to_grid(4.3, 0.0), 4.3);
    }

    #[test]
    fn magnetic_snap_to_grid_line() {
        // At 120 ppb, threshold = 12/120 = 0.1 beats
        let snapped = magnetic_snap_beat(4.05, 1.0, &[], 120.0, 12.0, 0.5);
        assert_eq!(snapped, 4.0); // Grid line at 4.0 is within 0.1 beats
    }

    #[test]
    fn magnetic_snap_to_neighbor_edge() {
        // Neighbor clip ends at beat 8.0. Raw position is 7.95.
        // At 120 ppb, threshold = 0.1 beats. Distance = 0.05 < 0.1.
        let snapped = magnetic_snap_beat(7.95, 1.0, &[8.0], 120.0, 12.0, 0.5);
        assert_eq!(snapped, 8.0); // Snaps to neighbor edge
    }

    #[test]
    fn magnetic_snap_neighbor_wins_over_grid() {
        // Grid at 8.0, neighbor at 7.9. Raw = 7.92.
        // Distance to grid = 0.08, distance to neighbor = 0.02. Neighbor wins.
        let snapped = magnetic_snap_beat(7.92, 1.0, &[7.9], 120.0, 12.0, 0.5);
        assert!((snapped - 7.9).abs() < 0.001);
    }

    #[test]
    fn magnetic_snap_respects_pixel_threshold() {
        // At 120 ppb, threshold = 12/120 = 0.1 beats.
        // Raw = 4.2, grid at 4.0. Distance = 0.2 > 0.1 threshold.
        let snapped = magnetic_snap_beat(4.2, 1.0, &[], 120.0, 12.0, 0.5);
        assert_eq!(snapped, 4.2); // No snap — too far
    }

    #[test]
    fn magnetic_snap_max_beat_cap() {
        // At 2 ppb (zoomed way out), threshold = 12/2 = 6.0 beats.
        // But max_snap_beats = 0.5 caps it.
        // Raw = 4.4, grid at 4.0. Distance = 0.4 < 0.5.
        let snapped = magnetic_snap_beat(4.4, 1.0, &[], 2.0, 12.0, 0.5);
        assert_eq!(snapped, 4.0); // Snaps within 0.5 beat cap

        // Raw = 4.6, grid at 5.0. Distance = 0.4 < 0.5.
        let snapped2 = magnetic_snap_beat(4.6, 1.0, &[], 2.0, 12.0, 0.5);
        assert_eq!(snapped2, 5.0); // Snaps within cap

        // Raw = 3.0, grid at 4.0 or 3.0 — but grid at 3.0 is distance 0, so it snaps.
        let snapped3 = magnetic_snap_beat(3.0, 1.0, &[], 2.0, 12.0, 0.5);
        assert_eq!(snapped3, 3.0);
    }

    #[test]
    fn no_snap_when_no_candidates_near() {
        // Raw = 4.3, grid at 4.0 (dist=0.3) and 5.0 (dist=0.7).
        // At 120 ppb, threshold = 0.1. Both too far.
        let snapped = magnetic_snap_beat(4.3, 1.0, &[], 120.0, 12.0, 0.5);
        assert_eq!(snapped, 4.3); // No snap
    }
}
