//! The viewport's coordinate surface over `CoordinateMapper` — beat↔pixel, the
//! per-track Y, grid snapping, and marker-flag geometry. See
//! `docs/TIMELINE_API_DESIGN.md` §3.4.

use super::*;

impl TimelineViewportPanel {
    /// The screen-space rect of a marker's ruler flag at `beat`.
    ///
    /// The single definition of marker-flag geometry — both the flag node
    /// (`build_markers` / the scroll update-in-place) and the hit-test
    /// read it, so a marker's clickable area cannot drift from where it is
    /// drawn. See `docs/TIMELINE_API_DESIGN.md` §3.5.
    pub(super) fn marker_flag_rect(&self, beat: Beats) -> Rect {
        let flag_w = color::MARKER_FLAG_WIDTH;
        let px = self.beat_to_pixel(beat);
        Rect::new(
            px - flag_w * 0.5,
            self.ruler_rect.y,
            flag_w,
            color::MARKER_FLAG_HEIGHT,
        )
    }

    // ── Coordinate mapping ────────────────────────────────────────

    /// Convert beat position to pixel X in the tracks area (screen-space).
    pub fn beat_to_pixel(&self, beat: Beats) -> f32 {
        (beat.as_f32() - self.scroll_x_beats.as_f32()) * self.mapper.pixels_per_beat()
            + self.tracks_rect.x
    }

    /// Beats per bar (time-signature numerator). Exposed for filmstrip cell
    /// geometry (one cell per bar), keeping bar math derived from the mapper.
    pub fn beats_per_bar(&self) -> f32 {
        self.beats_per_bar as f32
    }

    /// Beat (as `f64`) → pixel X — for callers (the clip filmstrip draw) that work
    /// in raw beats and shouldn't have to construct a `Beats` newtype.
    pub fn beat_f64_to_pixel(&self, beat: f64) -> f32 {
        self.beat_to_pixel(Beats(beat))
    }

    /// Convert pixel X in the tracks area to beat position.
    pub fn pixel_to_beat(&self, px: f32) -> Beats {
        Beats(
            ((px - self.tracks_rect.x) / self.mapper.pixels_per_beat()) as f64
                + self.scroll_x_beats.0,
        )
    }

    /// Convert panel-local pixel X (0 = left edge of tracks area) to beat position.
    /// Used by waveform/stem scrub where events are already offset to local coords.
    pub fn local_pixel_to_beat(&self, local_px: f32) -> Beats {
        Beats((local_px / self.mapper.pixels_per_beat()) as f64 + self.scroll_x_beats.0)
    }

    /// Snap a beat to the grid for ruler scrubbing, unless free-scrub is active.
    ///
    /// Unity `RulerScrubHandler.ScrubToPosition()`:
    /// - Default: snap to nearest grid line via `SnapBeatToGrid(beat, beatsPerBar)`
    /// - Alt/Option held: free scrub (no snap) for sample-accurate positioning
    /// - At max zoom level: auto-disable snapping (can place between grid lines)
    pub(super) fn scrub_snap_beat(&self, beat: Beats, free: bool) -> Beats {
        if free {
            return beat.max(Beats::ZERO);
        }
        // At max zoom, disable snapping (Unity: ShouldUseFreeScrub, lines 64-66)
        let max_zoom = *color::ZOOM_LEVELS.last().unwrap();
        if self.mapper.pixels_per_beat() >= max_zoom - 0.001 {
            return beat.max(Beats::ZERO);
        }
        let grid =
            snap::grid_interval_for_zoom(self.mapper.pixels_per_beat(), self.beats_per_bar as f32);
        snap::snap_beat_to_grid(beat, Beats::from_f32(grid)).max(Beats::ZERO)
    }

    /// Convert beat duration to pixel width.
    pub fn beat_duration_to_width(&self, beats: f32) -> f32 {
        self.mapper.beat_duration_to_width(Beats::from_f32(beats))
    }

    /// Get Y position of a track (relative to tracks_rect top, before scroll).
    pub fn track_y(&self, layer_index: usize) -> f32 {
        self.mapper.get_layer_y_offset(layer_index) + self.tracks_rect.y - self.scroll_y_px
    }

    /// Get height of a track — from the mapper, the sole Y-layout authority.
    pub fn track_height(&self, layer_index: usize) -> f32 {
        self.mapper.get_layer_height(layer_index)
    }

    /// Visible beat range (with buffer).
    pub(super) fn visible_beat_range(&self) -> (f32, f32) {
        let min_beat = self.scroll_x_beats.as_f32();
        let max_beat = min_beat + self.tracks_rect.width / self.mapper.pixels_per_beat();
        (min_beat, max_beat)
    }

    /// Determine which layer a Y coordinate falls in.
    pub fn layer_at_y(&self, y: f32) -> Option<usize> {
        if y < self.tracks_rect.y || y > self.tracks_rect.y_max() {
            return None;
        }
        // Delegate to the mapper (the sole Y authority), then re-impose the
        // strict upper bound it omits: `get_layer_at_y` returns the topmost
        // layer whose top is at/above `y`, but does NOT reject the dead space
        // *below* the last track. Without this check, a click under the final
        // clip would wrongly resolve to that track.
        let y_in_tracks = y - self.tracks_rect.y + self.scroll_y_px;
        let i = self.mapper.get_layer_at_y(y_in_tracks)?;
        let top = self.mapper.get_layer_y_offset(i);
        let height = self.mapper.get_layer_height(i);
        (y_in_tracks < top + height).then_some(i)
    }

    /// Snap a beat position to the snap grid (matches prominently visible grid lines).
    pub fn snap_to_grid(&self, beat: Beats) -> Beats {
        let step = self.snap_grid_step() as f64;
        Beats((beat.0 / step).round() * step)
    }

    /// Magnetic snap: snap to grid lines AND neighboring clip edges within threshold.
    ///
    /// Grid snap uses a threshold of at least half the grid interval, ensuring clips
    /// always jump between grid positions (standard DAW behavior). Clip edge snap
    /// uses the pixel-based threshold for fine-grained magnetic pull.
    /// `ignore_ids` are clip IDs being dragged (don't snap to self).
    pub fn magnetic_snap(&self, beat: Beats, layer_index: usize, ignore_ids: &[ClipId]) -> Beats {
        use crate::snap::SNAP_THRESHOLD_PX;

        let ppb = self.mapper.pixels_per_beat() as f64;

        // Pixel-based threshold (for clip edge snapping)
        let pixel_threshold_beats = if ppb > 0.0 {
            SNAP_THRESHOLD_PX as f64 / ppb
        } else {
            0.0
        };

        // Grid threshold: half the grid interval so every position snaps to the
        // nearest visible grid line (full cell coverage, standard DAW behavior).
        let half_grid = self.snap_grid_step() as f64 / 2.0;
        let grid_threshold = pixel_threshold_beats.max(half_grid);

        // Start with raw beat — only snap if a candidate is within threshold.
        let mut best_beat = beat;
        let mut best_dist = f64::MAX;

        // Grid candidate (uses wider threshold for full-coverage grid snap)
        let grid_snapped = self.snap_to_grid(beat);
        let grid_dist = (grid_snapped.0 - beat.0).abs();
        if grid_dist <= grid_threshold && grid_dist < best_dist {
            best_dist = grid_dist;
            best_beat = grid_snapped;
        }

        // Neighboring clip edges on the same layer (uses pixel-based threshold).
        // Clip edges that are closer than the grid snap win — this lets you
        // align clip boundaries precisely even between grid lines.
        let layer_clips = self
            .clips_by_layer
            .get(layer_index)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for clip in layer_clips {
            if ignore_ids.contains(&clip.clip_id) {
                continue;
            }

            // Check start edge
            let dist_start = (clip.start_beat.0 - beat.0).abs();
            if dist_start < grid_threshold && dist_start < best_dist {
                best_dist = dist_start;
                best_beat = clip.start_beat;
            }

            // Check end edge
            let end_beat = Beats(clip.start_beat.0 + clip.duration_beats.0);
            let dist_end = (end_beat.0 - beat.0).abs();
            if dist_end < grid_threshold && dist_end < best_dist {
                best_dist = dist_end;
                best_beat = end_beat;
            }
        }

        best_beat
    }

    /// Floor-snap a beat to the snap grid subdivision.
    /// Unlike `snap_to_grid` (rounds to nearest), this floors to the grid line
    /// at or before the beat. Used for clip creation (Unity: FloorBeatToGrid).
    pub fn floor_to_grid(&self, beat: Beats) -> Beats {
        let step = self.snap_grid_step() as f64;
        Beats((beat.0 / step).floor() * step)
    }

    /// Current visual grid step size in beats (for rendering: ruler ticks, etc.).
    pub fn grid_step(&self) -> f32 {
        match self.grid_subdivision() {
            GridSubdivision::Bar => self.beats_per_bar as f32,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        }
    }

    /// Snap grid step — matches the visible grid lines so snapping targets
    /// exactly what the user sees. Delegates to `grid_step()`.
    pub fn snap_grid_step(&self) -> f32 {
        self.grid_step()
    }

    /// Grid-aligned step for clip creation, guaranteed to produce a clip
    /// at least `MIN_CREATION_PX` wide on screen. Walks up musical grid
    /// levels (16th → 8th → beat → bar) until the threshold is met.
    const MIN_CREATION_PX: f32 = 40.0;

    pub fn clip_creation_step(&self) -> Beats {
        let ppb = self.mapper.pixels_per_beat();
        let candidates: [f32; 4] = [0.25, 0.5, 1.0, self.beats_per_bar as f32];
        let grid = self.snap_grid_step();
        for &step in &candidates {
            if step >= grid && step * ppb >= Self::MIN_CREATION_PX {
                return Beats(step as f64);
            }
        }
        // Fallback: bar is always the coarsest grid level
        Beats(self.beats_per_bar as f64)
    }

    /// At extreme zoom-out, bar lines are too dense. Returns the number of
    /// bars to skip between visible bar lines (1 = show every bar).
    pub(super) fn bar_skip(&self) -> u32 {
        let bar_px = self.mapper.pixels_per_beat() * self.beats_per_bar as f32;
        if bar_px >= 8.0 {
            1
        } else if bar_px >= 4.0 {
            2
        } else if bar_px >= 2.0 {
            4
        } else {
            8
        }
    }

    // ── Grid subdivision ──────────────────────────────────────────

    /// Determine visual grid subdivision level based on zoom.
    /// Uses per-note pixel widths (matching Unity's GridOverlay thresholds):
    ///   - Show 16ths when a 16th-note ≥ 4px wide
    ///   - Show 8ths  when an 8th-note ≥ 6px wide
    ///   - Show beats when a beat ≥ 6px wide
    pub(super) fn grid_subdivision(&self) -> GridSubdivision {
        let sixteenth_px = self.mapper.pixels_per_beat() * 0.25;
        let eighth_px = self.mapper.pixels_per_beat() * 0.5;
        if sixteenth_px >= 4.0 {
            GridSubdivision::Sixteenth
        } else if eighth_px >= 6.0 {
            GridSubdivision::Eighth
        } else if self.mapper.pixels_per_beat() >= 6.0 {
            GridSubdivision::Beat
        } else {
            GridSubdivision::Bar
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GridSubdivision {
    Bar,
    Beat,
    Eighth,
    Sixteenth,
}
