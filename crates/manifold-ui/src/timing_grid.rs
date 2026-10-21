//! The timeline grid policy and line walker. One walker feeds the ruler, the
//! automation lanes and the layer tracks, so every surface agrees on which
//! beats get a line and how heavy it is. The tracks draw it as GPU lines
//! (`manifold-ui-paint::timeline_grid_draw`), once per frame.

use crate::color;
use crate::node::{Color32, Rect};

/// What the track grid pass needs, read off the viewport once per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineGridView {
    pub tracks: Rect,
    pub min_beat: f32,
    pub logical_ppb: f32,
    pub beats_per_bar: f32,
    /// Physical pixels per logical pixel; the grid snaps its lines to them.
    pub render_scale: f32,
    /// Top of layer 0 when it is visible: the top separator goes there.
    pub first_layer_top: Option<f32>,
}


/// The visible timing grid policy shared by bitmap layers, automation lanes,
/// and the ruler. The policy is expressed in beats; callers choose their own
/// screen mapping so logical and physical render targets stay aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingGridLineKind {
    Bar,
    Beat,
    Eighth,
    Sixteenth,
}

impl TimingGridLineKind {
    pub const fn physical_width(self) -> f32 {
        match self {
            Self::Bar => 2.0,
            Self::Beat | Self::Eighth | Self::Sixteenth => 1.0,
        }
    }

    pub const fn color(self) -> Color32 {
        match self {
            Self::Bar => color::GRID_BAR_LINE,
            Self::Beat => color::GRID_BEAT_LINE,
            Self::Eighth => color::GRID_SUBDIVISION_LINE,
            Self::Sixteenth => color::GRID_SIXTEENTH_LINE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimingGridPolicy {
    pub subdivisions_per_beat: u32,
    pub show_beat_lines: bool,
    pub bar_skip: u32,
}

pub fn timing_grid_policy(logical_ppb: f32, beats_per_bar: f32) -> TimingGridPolicy {
    let ppb = if logical_ppb.is_finite() {
        logical_ppb.max(0.0)
    } else {
        0.0
    };
    let bpb = if beats_per_bar.is_finite() {
        beats_per_bar.max(1.0)
    } else {
        1.0
    };
    let subdivisions_per_beat = if ppb * 0.25 >= 4.0 {
        4
    } else if ppb * 0.5 >= 6.0 {
        2
    } else {
        1
    };
    let bar_px = ppb * bpb;
    // Show every 2nd, 4th, 8th... bar so drawn bar lines stay >= 8px apart at
    // any zoom-out.
    let mut bar_skip = 1u32;
    while bar_px * (bar_skip as f32) < 8.0 && bar_skip < 1 << 20 {
        bar_skip *= 2;
    }
    TimingGridPolicy {
        subdivisions_per_beat,
        show_beat_lines: ppb >= 6.0,
        bar_skip,
    }
}

/// Allocation-free iterator over visible timing grid lines. `max_beat` is
/// inclusive, matching the viewport/ruler convention.
pub struct TimingGridLines {
    beat: f32,
    max_beat: f32,
    step: f32,
    beats_per_bar: f32,
    policy: TimingGridPolicy,
    remaining: usize,
}

pub fn timing_grid_lines(
    min_beat: f32,
    max_beat: f32,
    logical_ppb: f32,
    beats_per_bar: f32,
) -> TimingGridLines {
    let bpb = if beats_per_bar.is_finite() {
        beats_per_bar.max(1.0)
    } else {
        1.0
    };
    let policy = timing_grid_policy(logical_ppb, bpb);
    let divisions = policy.subdivisions_per_beat as f32;
    let step = if !policy.show_beat_lines && policy.subdivisions_per_beat == 1 {
        bpb * policy.bar_skip as f32
    } else {
        1.0 / divisions
    };
    let first = if min_beat.is_finite() {
        (min_beat / step).floor() * step
    } else {
        f32::INFINITY
    };
    TimingGridLines {
        beat: first,
        max_beat,
        step,
        beats_per_bar: bpb,
        policy,
        remaining: 4096,
    }
}

impl Iterator for TimingGridLines {
    type Item = (f32, TimingGridLineKind);

    fn next(&mut self) -> Option<Self::Item> {
        while self.remaining > 0 && self.beat.is_finite() && self.beat <= self.max_beat + 0.0001 {
            self.remaining -= 1;
            let beat = self.beat;
            self.beat += self.step;
            if self.beat <= beat { self.beat = f32::INFINITY; }
            let beat_in_bar = beat - (beat / self.beats_per_bar).floor() * self.beats_per_bar;
            let is_bar = beat_in_bar.abs() < 0.001;
            let is_beat = (beat - beat.round()).abs() < 0.001;
            if is_bar {
                let bar = (beat / self.beats_per_bar).round() as i64;
                if bar.rem_euclid(self.policy.bar_skip as i64) != 0 {
                    continue;
                }
                return Some((beat, TimingGridLineKind::Bar));
            }
            if is_beat {
                if self.policy.show_beat_lines {
                    return Some((beat, TimingGridLineKind::Beat));
                }
                continue;
            }
            return Some((
                beat,
                if self.policy.subdivisions_per_beat == 2
                    || ((beat * 2.0) - (beat * 2.0).round()).abs() < 0.001
                {
                    TimingGridLineKind::Eighth
                } else {
                    TimingGridLineKind::Sixteenth
                },
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_grid_policy_matches_zoom_thresholds() {
        assert_eq!(timing_grid_policy(5.0, 4.0).subdivisions_per_beat, 1);
        assert_eq!(timing_grid_policy(12.0, 4.0).subdivisions_per_beat, 2);
        assert_eq!(timing_grid_policy(16.0, 4.0).subdivisions_per_beat, 4);
        assert_eq!(timing_grid_policy(1.0, 4.0).bar_skip, 2);
    }

    /// At the deepest zoom-out bar lines keep thinning so they never pack
    /// tighter than 8px, and the walker still yields them.
    #[test]
    fn far_zoom_out_keeps_bar_lines_spaced_and_walked() {
        let ppb = color::MIN_PIXELS_PER_BEAT;
        let policy = timing_grid_policy(ppb, 4.0);
        let spacing = ppb * 4.0 * policy.bar_skip as f32;
        assert!((8.0..16.0).contains(&spacing), "bar spacing {spacing}px");
        assert_eq!(
            timing_grid_lines(0.0, 0.0, ppb, 4.0).next(),
            Some((0.0, TimingGridLineKind::Bar)),
            "bar line at beat 0 must walk"
        );
    }

    #[test]
    fn timing_grid_marks_eighths_and_terminates_at_large_beats() {
        assert_eq!(timing_grid_lines(0.5, 0.5, 12.0, 4.0).next(), Some((0.5, TimingGridLineKind::Eighth)));
        assert!(timing_grid_lines(1.0e20, 1.0e21, 24.0, 4.0).count() <= 1);
        assert_eq!(timing_grid_lines(f32::NAN, 4.0, 24.0, 4.0).count(), 0);
    }

    #[test]
    fn timing_grid_lines_skips_intermediate_bars_consistently() {
        let lines = timing_grid_lines(0.0, 16.0, 1.0, 4.0).collect::<Vec<_>>();
        assert_eq!(
            lines,
            vec![
                (0.0, TimingGridLineKind::Bar),
                (8.0, TimingGridLineKind::Bar),
                (16.0, TimingGridLineKind::Bar),
            ]
        );
    }
}
