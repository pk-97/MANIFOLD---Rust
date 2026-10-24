//! The one drag resolver for pointer-driven automation moves
//! (`docs/AUTOMATION_LANES_DESIGN.md` section 10 (Phasing), P6, D-P6.2): single
//! point, tail (beat locked), segment and group drags all resolve through
//! [`DragResolver::resolve`]; the overlay keeps only the plumbing.
//!
//! Rules, in one place:
//! - Nothing moves until the pointer is `DRAG_THRESHOLD_PX` from the press.
//!   Travel then counts from the press, so the dot stays under the pointer and
//!   a fast flick loses no distance.
//! - Snap is relative: the beat moves by whole grid steps of the pointer's
//!   travel since the press, so an off-grid beat keeps its offset. Cmd moves it
//!   freely. There is no axis lock.
//! - Shift scales each value increment by [`FINE_SCALE`] from the moment it is
//!   held; releasing it never rescales travel already made. The beat axis
//!   ignores Shift.
//! - The caller clamps what the resolver returns: [`clamp_single_beat`] for one
//!   point, [`clamp_group_beat_delta`] for a group, [`shared_norm_delta`] then
//!   [`apply_value`] for values of a segment or group.
//! - Whole-number lanes quantise through [`quantise_whole_number`] only.
//! - Preview and commit are the same resolve: callers store the last resolved
//!   (beat, value) and commit that, never a re-derivation.

use crate::color::DRAG_THRESHOLD_PX;
use crate::node::Vec2;
use manifold_foundation::Beats;

/// Value increments scale by this while Shift is held.
pub const FINE_SCALE: f32 = 0.25;

/// A drag's anchor and accumulated travel. Created at press.
#[derive(Debug, Clone)]
pub struct DragResolver {
    press: Vec2,
    beat_locked: bool,
    started: bool,
    /// Pointer beat at the press.
    press_beat: Beats,
    /// Pointer height at the previous resolve (the press until started).
    last_y: f32,
    /// Accumulated value travel as a fraction of the lane range, Shift-scaled
    /// per increment. Positive is up.
    norm_delta: f32,
}

/// What a resolve is derived from besides the resolver's own state.
#[derive(Debug, Clone, Copy)]
pub struct ResolveInput {
    pub pos: Vec2,
    /// Beat under the pointer now (scroll included).
    pub pointer_beat: Beats,
    /// Grid step at resolve time, in beats.
    pub grid_step: f32,
    pub command: bool,
    pub shift: bool,
    /// Height of the lane's curve area, in pixels.
    pub curve_height: f32,
}

/// Unclamped travel since the anchor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub beat_delta: f64,
    pub norm_delta: f32,
}

impl Resolved {
    pub const ZERO: Self = Self {
        beat_delta: 0.0,
        norm_delta: 0.0,
    };
}

impl DragResolver {
    pub fn new(press: Vec2, press_beat: Beats, beat_locked: bool) -> Self {
        Self {
            press,
            beat_locked,
            started: false,
            press_beat,
            last_y: press.y,
            norm_delta: 0.0,
        }
    }

    fn past_threshold(&self, pos: Vec2) -> bool {
        (pos.x - self.press.x).hypot(pos.y - self.press.y) >= DRAG_THRESHOLD_PX
    }

    /// Whether this drag is moving along the beat axis at `pos`, which decides
    /// edge autoscroll: a locked drag, or one that has only moved vertically,
    /// never scrolls the view. Stateless: the pointer's horizontal distance
    /// from the press, no latch.
    pub fn moves_in_time(&self, pos: Vec2) -> bool {
        !self.beat_locked && (pos.x - self.press.x).abs() >= DRAG_THRESHOLD_PX
    }

    pub fn resolve(&mut self, input: &ResolveInput) -> Resolved {
        if !self.started {
            if !self.past_threshold(input.pos) {
                return Resolved::ZERO;
            }
            // The crossing resolve applies the full travel since the press.
            self.started = true;
        }
        let fine = if input.shift { FINE_SCALE } else { 1.0 };
        self.norm_delta +=
            (self.last_y - input.pos.y) / input.curve_height.max(f32::EPSILON) * fine;
        self.last_y = input.pos.y;
        // Horizontal travel under the threshold is wobble, not a time move,
        // however fine the grid cell is.
        let beat_delta = if !self.moves_in_time(input.pos) {
            0.0
        } else {
            let travel = input.pointer_beat.0 - self.press_beat.0;
            let step = f64::from(input.grid_step);
            if input.command || step <= f64::EPSILON {
                travel
            } else {
                (travel / step).round() * step
            }
        };
        Resolved {
            beat_delta,
            norm_delta: self.norm_delta,
        }
    }
}

/// The one whole-number rule: round, then clamp into the range.
pub fn quantise_whole_number(value: f32, min: f32, max: f32) -> f32 {
    value.round().clamp(min.min(max), min.max(max))
}

/// A point's value after `norm_delta` of range travel. No travel keeps the
/// captured value exactly (some parameters legitimately sit outside their
/// display range).
pub fn apply_value(original: f32, norm_delta: f32, min: f32, max: f32, whole_numbers: bool) -> f32 {
    if norm_delta == 0.0 {
        return original;
    }
    let moved = original + norm_delta * (max - min);
    if whole_numbers {
        quantise_whole_number(moved, min, max)
    } else {
        moved.clamp(min.min(max), min.max(max))
    }
}

/// One shared value delta (normalised) for points moving together: the
/// requested delta, bounded so no point's clamped position crosses a lane
/// bound. `originals` is `(value, min, max)` per point. Out-of-range originals
/// bound as if sitting on the nearest edge, so the bounds never invert; no
/// travel stays exactly zero.
pub fn shared_norm_delta(originals: impl Iterator<Item = (f32, f32, f32)>, requested: f32) -> f32 {
    if requested == 0.0 {
        return 0.0;
    }
    let mut lo = f32::NEG_INFINITY;
    let mut hi = f32::INFINITY;
    for (value, min, max) in originals {
        let range = (max - min).max(f32::EPSILON);
        let norm = ((value - min) / range).clamp(0.0, 1.0);
        lo = lo.max(-norm);
        hi = hi.min(1.0 - norm);
    }
    if lo > hi {
        return 0.0;
    }
    requested.clamp(lo, hi)
}

/// Single-point beat: not before zero, and not across the neighbours captured
/// at press. Equal is allowed, which makes a vertical step.
pub fn clamp_single_beat(beat: Beats, prev: Beats, next: Option<Beats>) -> Beats {
    let beat = beat.max(Beats::ZERO).max(prev);
    next.map_or(beat, |next| beat.min(next))
}

/// Beats to the nearest unselected point on each side of the point at `index`
/// (beat `beat`) on its lane, `(left, right)`, infinite when there is none.
/// `others` is every point of the lane as `(index, beat, selected)`. A point
/// sharing the beat keeps its place in the lane order: it is a neighbour on the
/// side it already sits on, with no room.
pub fn room_to_unselected(
    index: usize,
    beat: f64,
    others: impl Iterator<Item = (usize, f64, bool)>,
) -> (f64, f64) {
    let mut left = f64::INFINITY;
    let mut right = f64::INFINITY;
    for (other_index, other_beat, selected) in others {
        if selected {
            continue;
        }
        let gap = other_beat - beat;
        if gap > 0.0 || (gap == 0.0 && other_index > index) {
            right = right.min(gap);
        } else {
            left = left.min(-gap);
        }
    }
    (left, right)
}

/// One shared beat delta for a group: it stops at beat zero (`min_beat` is the
/// earliest selected beat) and at the nearest unselected point on any lane, so
/// a move never crosses a point it was not asked to move.
pub fn clamp_group_beat_delta(delta: f64, min_beat: f64, room_left: f64, room_right: f64) -> f64 {
    delta.clamp(-min_beat.min(room_left), room_right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(x: f32, y: f32, beat: f64) -> ResolveInput {
        ResolveInput {
            pos: Vec2::new(x, y),
            pointer_beat: Beats(beat),
            grid_step: 1.0,
            command: false,
            shift: false,
            curve_height: 100.0,
        }
    }

    /// A resolver pressed at (100, 100) with the pointer on beat 10.
    fn pressed(beat_locked: bool) -> DragResolver {
        DragResolver::new(Vec2::new(100.0, 100.0), Beats(10.0), beat_locked)
    }

    /// A resolver that has crossed its threshold straight up and has been
    /// resolved there once.
    fn started(beat_locked: bool) -> DragResolver {
        let mut r = pressed(beat_locked);
        r.resolve(&input(100.0, 94.0, 10.0));
        r
    }

    #[test]
    fn nothing_moves_under_the_threshold() {
        let mut r = pressed(false);
        assert_eq!(r.resolve(&input(102.0, 101.0, 12.0)), Resolved::ZERO);
    }

    #[test]
    fn the_crossing_resolve_applies_the_full_travel_since_the_press() {
        // One fast flick: a single first move 20 px up and two beats right.
        let mut r = pressed(false);
        let moved = r.resolve(&input(120.0, 80.0, 12.0));
        assert_eq!(
            moved.beat_delta, 2.0,
            "the beat travel counts from the press"
        );
        assert_eq!(moved.norm_delta, 0.2, "exactly 20 px of a 100 px lane");
        // A fine first move is scaled by the Shift state at that resolve.
        let mut fine = input(100.0, 80.0, 10.0);
        fine.shift = true;
        let moved = pressed(false).resolve(&fine);
        assert!((moved.norm_delta - 0.05).abs() < 1e-7);
        // Later moves stay incremental.
        let next = r.resolve(&input(120.0, 70.0, 12.0));
        assert!((next.norm_delta - 0.3).abs() < 1e-6);
    }

    #[test]
    fn relative_snap_keeps_an_off_grid_offset_over_a_one_step_move() {
        let mut r = pressed(false);
        // Pointer travels one grid step (1 beat) right of the press.
        let moved = r.resolve(&input(130.0, 94.0, 11.0));
        let beat = 2.3 + moved.beat_delta;
        assert!((beat - 3.3).abs() < 1e-12, "{beat}");
    }

    #[test]
    fn a_three_pixel_wobble_on_a_vertical_drag_leaves_the_beat_alone() {
        // Grid cell 10 px wide: 3 px is under half a cell.
        let mut r = started(false);
        let pixels_per_beat = 10.0;
        let wobble = r.resolve(&input(103.0, 60.0, 10.0 + 3.0 / pixels_per_beat));
        assert_eq!(wobble.beat_delta, 0.0);
        assert!(wobble.norm_delta > 0.3, "the value still follows");
    }

    #[test]
    fn cmd_moves_the_beat_freely() {
        let mut r = started(false);
        let mut free = input(120.0, 94.0, 10.37);
        free.command = true;
        assert!((r.resolve(&free).beat_delta - 0.37).abs() < 1e-12);
    }

    #[test]
    fn shift_pressed_late_changes_only_later_increments() {
        let mut r = started(false);
        // 6 px already travelled; 44 more at full speed: 0.5 of the range.
        let first = r.resolve(&input(100.0, 50.0, 10.0));
        assert!(
            (first.norm_delta - 0.5).abs() < 1e-6,
            "{}",
            first.norm_delta
        );
        // 20 more px with Shift held: 0.2 * 0.25.
        let mut fine = input(100.0, 30.0, 10.0);
        fine.shift = true;
        let second = r.resolve(&fine);
        assert!(
            (second.norm_delta - 0.55).abs() < 1e-6,
            "{}",
            second.norm_delta
        );
        // Releasing Shift never rescales what is already travelled.
        let third = r.resolve(&input(100.0, 30.0, 10.0));
        assert!((third.norm_delta - 0.55).abs() < 1e-6);
    }

    #[test]
    fn shift_does_not_touch_the_beat_axis() {
        let mut r = started(false);
        let mut fine = input(120.0, 94.0, 12.0);
        fine.shift = true;
        assert_eq!(r.resolve(&fine).beat_delta, 2.0);
    }

    #[test]
    fn a_locked_drag_never_moves_in_time() {
        let mut r = started(true);
        assert_eq!(r.resolve(&input(300.0, 94.0, 40.0)).beat_delta, 0.0);
        assert!(!r.moves_in_time(Vec2::new(300.0, 94.0)));
        assert!(started(false).moves_in_time(Vec2::new(110.0, 94.0)));
        assert!(
            !started(false).moves_in_time(Vec2::new(101.0, 20.0)),
            "a vertical drag never scrolls"
        );
    }

    #[test]
    fn the_single_beat_clamp_allows_the_equal_beat() {
        let (prev, next) = (Beats(4.0), Some(Beats(8.0)));
        assert_eq!(clamp_single_beat(Beats(8.0), prev, next), Beats(8.0));
        assert_eq!(clamp_single_beat(Beats(9.0), prev, next), Beats(8.0));
        assert_eq!(clamp_single_beat(Beats(4.0), prev, next), Beats(4.0));
        assert_eq!(clamp_single_beat(Beats(1.0), prev, next), Beats(4.0));
        assert_eq!(
            clamp_single_beat(Beats(-3.0), Beats::ZERO, None),
            Beats::ZERO
        );
    }

    #[test]
    fn a_group_delta_stops_at_the_nearest_unselected_point() {
        // Lane beats 0,1,2,5; points 0 and 1 selected, 2 and 5 are not.
        let beats = [0.0, 1.0, 2.0, 5.0];
        let selected = [true, true, false, false];
        let (mut left, mut right) = (f64::INFINITY, f64::INFINITY);
        for &i in &[0usize, 1] {
            let (l, r) = room_to_unselected(
                i,
                beats[i],
                beats.iter().enumerate().map(|(j, b)| (j, *b, selected[j])),
            );
            left = left.min(l);
            right = right.min(r);
        }
        assert_eq!(
            right, 1.0,
            "point 1 has one beat before the unselected point at 2"
        );
        assert_eq!(clamp_group_beat_delta(3.0, 0.0, left, right), 1.0);
        assert_eq!(
            clamp_group_beat_delta(-3.0, 0.0, left, right),
            0.0,
            "stops at beat zero"
        );
        assert_eq!(clamp_group_beat_delta(0.5, 0.0, left, right), 0.5);
    }

    #[test]
    fn whole_number_lanes_round_then_clamp() {
        assert_eq!(quantise_whole_number(2.4, 0.0, 5.0), 2.0);
        assert_eq!(quantise_whole_number(2.6, 0.0, 5.0), 3.0);
        assert_eq!(quantise_whole_number(9.2, 0.0, 5.0), 5.0);
        assert_eq!(quantise_whole_number(-3.0, 0.0, 5.0), 0.0);
        assert_eq!(apply_value(2.0, 0.0, 0.0, 5.0, true), 2.0);
        assert_eq!(apply_value(2.0, 0.5, 0.0, 5.0, true), 5.0);
        assert_eq!(apply_value(0.5, 0.25, 0.0, 1.0, false), 0.75);
    }

    #[test]
    fn shared_delta_survives_out_of_range_originals() {
        let pts = [(-0.2_f32, 0.0_f32, 1.0_f32), (1.3, 0.0, 1.0)];
        // Both endpoints sit past opposite edges: no room either way, no panic.
        assert_eq!(shared_norm_delta(pts.iter().copied(), 0.4), 0.0);
        assert_eq!(shared_norm_delta(pts.iter().copied(), -0.4), 0.0);
        // Zero travel stays exactly zero even with an endpoint out of range.
        let one_out = [(-0.2_f32, 0.0_f32, 1.0_f32), (0.5, 0.0, 1.0)];
        assert_eq!(shared_norm_delta(one_out.iter().copied(), 0.0), 0.0);
        assert_eq!(shared_norm_delta(one_out.iter().copied(), 0.3), 0.3);
        let in_range = [(0.2_f32, 0.0_f32, 1.0_f32), (0.9, 0.0, 1.0)];
        assert!((shared_norm_delta(in_range.iter().copied(), 0.5) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn a_wobble_under_the_threshold_leaves_the_beat_alone_on_a_five_pixel_cell() {
        let mut r = started(false);
        let mut wobble = input(103.0, 60.0, 10.0 + 3.0 / 80.0);
        wobble.grid_step = 1.0 / 16.0;
        assert_eq!(r.resolve(&wobble).beat_delta, 0.0);
    }
}
