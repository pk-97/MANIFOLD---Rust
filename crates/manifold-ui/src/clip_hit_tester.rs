//! Pure math hit-tester: maps (beat, Y) coordinates to a TimelineClip + region.
//! Mechanical translation of Assets/Scripts/UI/Timeline/ClipHitTester.cs.
//!
//! No MonoBehaviour, no allocations on the hot path.
//! The struct is stateless — CoordinateMapper and clip data are passed as parameters.

use crate::coordinate_mapper::CoordinateMapper;
use crate::hit::Span;
use crate::hit_targets::{HitTargetEntry, HitTargets};
use crate::panels::viewport::{ClipScreenRect, ViewportClip, zone_widths};
use manifold_foundation::ClipId;

// ── Data Types ──────────────────────────────────────────────────

/// Which part of a clip was hit.
/// Matches Unity HitRegion enum (ClipHitTester.cs line 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitRegion {
    Body,
    TrimLeft,
    TrimRight,
}

/// Result of a clip hit-test.
/// Matches Unity ClipHitResult struct (ClipHitTester.cs lines 9-17).
#[derive(Debug, Clone)]
pub struct ClipHitResult {
    pub clip_id: ClipId,
    pub layer_index: usize,
    pub region: HitRegion,
}

// ── ClipHitTester ───────────────────────────────────────────────

/// Pure math hit-tester: maps (beat, Y) coordinates to a clip + region.
/// Stateless — all data passed as parameters.
///
/// Mechanical translation of ClipHitTester.cs.
pub struct ClipHitTester;

impl ClipHitTester {
    /// Hit-test at a point in track-content-local space.
    ///
    /// Port of Unity ClipHitTester.HitTest (lines 46-100).
    ///
    /// `beat_at_pointer`: beat value at the pointer X position.
    /// `y_in_track_content`: Y offset from top of tracks area (positive downward).
    /// `clip_vertical_padding`: vertical inset clips have within their track.
    /// `mapper`: coordinate mapper for layer layout queries.
    /// `clips_for_layer`: returns the clips for a given layer index.
    /// `is_group_layer`: closure that returns true if the layer at a given index is a group.
    pub fn hit_test<'a>(
        beat_at_pointer: f32,
        y_in_track_content: f32,
        clip_vertical_padding: f32,
        mapper: &CoordinateMapper,
        clips_for_layer: impl Fn(usize) -> &'a [ViewportClip],
        is_group_layer: impl Fn(usize) -> bool,
    ) -> Option<ClipHitResult> {
        // Unity line 51: get layer from Y
        let layer_index = mapper.get_layer_at_y(y_in_track_content)?;

        // Unity line 56: skip group layers
        if is_group_layer(layer_index) {
            return None;
        }

        // Unity lines 60-65: check Y is within clip area (not in padding)
        let track_top = mapper.get_layer_y_offset(layer_index);
        let track_height = mapper.get_layer_height(layer_index);
        let clip_top = track_top + clip_vertical_padding;
        let clip_bottom = track_top + track_height - clip_vertical_padding;
        // Closed Y band — the clip area includes both padding edges.
        if !Span::new(clip_top, clip_bottom).contains_inclusive(y_in_track_content) {
            return None;
        }

        // Pixels per beat — the local coordinate frame trim geometry works in.
        let ppb = mapper.pixels_per_beat();
        let pointer_px = beat_at_pointer * ppb;
        let clips = clips_for_layer(layer_index);

        // Iterate this layer's clips in reverse (topmost/last wins), same as
        // before. Ownership of a beat position is no longer decided by
        // `Span::contains(beat_at_pointer)` alone — D4's outward-extended trim
        // zones can reach beyond a clip's own [start, end), into neighboring
        // empty lane space, which is exactly what makes a hairline-narrow clip
        // grabbable. So each candidate is tested against its own zone extent
        // (`zone_widths`, the same rule `clip_zones` uses for painting),
        // computed relative to the clip's own start — one geometry authority
        // for both.
        for (idx, clip) in clips.iter().enumerate().rev() {
            let clip_start_px = clip.start_beat.as_f32() * ppb;
            let clip_width_px = clip.duration_beats.as_f32() * ppb;
            let local_px = pointer_px - clip_start_px;

            let (gap_left, gap_right) = neighbor_gap_px(clips, idx, ppb);
            let zw = zone_widths(clip_width_px, (gap_left, gap_right));

            let left_lo = -zw.left_extend;
            let left_hi = zw.inner;
            let right_lo = clip_width_px - zw.inner;
            let right_hi = clip_width_px + zw.right_extend;

            // Half-open at both outer edges, same convention the old
            // `Span::contains` used — a point exactly at a shared (abutting)
            // boundary belongs to the clip on its right, never both.
            if local_px < left_lo || local_px >= right_hi {
                continue;
            }

            // On a clip narrow enough that `inner` exceeds half its own
            // width, the two trim zones overlap each other (both reach past
            // the midpoint) — split ownership at the midpoint rather than
            // letting evaluation order silently prefer one side, so a grab
            // right at the clip's own right edge still resolves to
            // TrimRight, not TrimLeft.
            let in_left_zone = local_px >= left_lo && local_px < left_hi;
            let in_right_zone = local_px >= right_lo && local_px < right_hi;
            let region = match (in_left_zone, in_right_zone) {
                (true, true) => {
                    if local_px < clip_width_px * 0.5 {
                        HitRegion::TrimLeft
                    } else {
                        HitRegion::TrimRight
                    }
                }
                (true, false) => HitRegion::TrimLeft,
                (false, true) => HitRegion::TrimRight,
                (false, false) => HitRegion::Body,
            };

            return Some(ClipHitResult {
                clip_id: clip.clip_id.clone(),
                layer_index,
                region,
            });
        }

        None
    }

    /// Collect all clip IDs that overlap the given beat/layer rectangle.
    /// Used for box/region selection.
    ///
    /// Port of Unity ClipHitTester.BoxSelect (lines 105-129).
    ///
    /// `min_beat`/`max_beat`: horizontal extent in beats.
    /// `min_layer`/`max_layer`: vertical extent in layer indices.
    /// `clips_for_layer`: returns the clips for a given layer index.
    /// `layer_count`: total number of layers.
    /// `is_group_layer`: closure returning true for group layers.
    pub fn box_select<'a>(
        min_beat: f32,
        max_beat: f32,
        min_layer: usize,
        max_layer: usize,
        clips_for_layer: impl Fn(usize) -> &'a [ViewportClip],
        layer_count: usize,
        is_group_layer: impl Fn(usize) -> bool,
    ) -> Vec<ClipId> {
        let mut results = Vec::new();

        // Unity lines 111-113: clamp layer bounds
        let lo = min_layer;
        let hi = if max_layer < layer_count {
            max_layer
        } else {
            layer_count - 1
        };

        // Unity lines 115-128: iterate per-layer clips within layer range
        for layer_idx in lo..=hi {
            if is_group_layer(layer_idx) {
                continue;
            }
            for clip in clips_for_layer(layer_idx) {
                let clip_start_f32 = clip.start_beat.as_f32();
                let clip_end = clip_start_f32 + clip.duration_beats.as_f32();
                // Interval overlap with the selection's beat span.
                if Span::new(clip_start_f32, clip_end).overlaps(Span::new(min_beat, max_beat)) {
                    results.push(clip.clip_id.clone());
                }
            }
        }

        results
    }
}

/// Pixel gap from `clips[self_idx]` to its nearest left/right neighbor in the
/// same layer, by beat position (not list order — `clips_for_layer` is
/// bucketed in arrival order, not sorted). `f32::MAX` when there is no
/// neighbor on that side, so `zone_widths`' `min(4.0, gap)` always yields the
/// full outward extension. O(n) per call, no allocation — matches the
/// existing O(n)-per-hit-test cost of the candidate loop itself.
fn neighbor_gap_px(clips: &[ViewportClip], self_idx: usize, ppb: f32) -> (f32, f32) {
    let me = &clips[self_idx];
    let me_start = me.start_beat.as_f32();
    let me_end = me_start + me.duration_beats.as_f32();
    let mut left_gap = f32::MAX;
    let mut right_gap = f32::MAX;
    for (i, other) in clips.iter().enumerate() {
        if i == self_idx {
            continue;
        }
        let o_start = other.start_beat.as_f32();
        let o_end = o_start + other.duration_beats.as_f32();
        if o_end <= me_start {
            left_gap = left_gap.min((me_start - o_end) * ppb);
        } else if o_start >= me_end {
            right_gap = right_gap.min((o_start - me_end) * ppb);
        }
    }
    (left_gap, right_gap)
}

// ── Automation surface (UI_AUTOMATION_DESIGN.md D5/§5) ───────────

/// [`HitTargets`] over the timeline's currently-visible clips — the same
/// [`ClipScreenRect`] list `ClipHitTester::hit_test` and the clip painter both
/// read (`TimelineViewportPanel::visible_clip_rects`), so an enumerated clip's
/// rect can never disagree with what's on screen or what's clickable.
pub struct ClipHitTargets<'a>(pub &'a [ClipScreenRect]);

impl HitTargets for ClipHitTargets<'_> {
    fn surface_id(&self) -> &'static str {
        "timeline_clips"
    }

    fn enumerate(&self, out: &mut Vec<HitTargetEntry>) {
        out.reserve(self.0.len());
        for cr in self.0 {
            out.push(HitTargetEntry {
                kind: "clip",
                label: cr.name.to_string(),
                rect: cr.rect,
                payload: cr.clip_id.as_str().to_string(),
            });
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate_mapper::CoordinateMapper;
    use crate::node::Color32;
    use manifold_foundation::Beats;

    fn make_mapper(ppb: f32, layer_heights: &[f32]) -> CoordinateMapper {
        let mut mapper = CoordinateMapper::new();
        mapper.set_zoom(ppb);
        mapper.set_layout(layer_heights);
        mapper
    }

    fn make_clip(id: &str, layer: usize, start: f32, duration: f32) -> ViewportClip {
        ViewportClip {
            clip_id: ClipId::new(id),
            layer_index: layer,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(duration),
            name: "".into(),
            color: Color32::new(100, 100, 100, 255),
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            waveform_breakpoints: Vec::new(),
        }
    }

    fn no_groups(_: usize) -> bool {
        false
    }

    /// Bucket flat clip list by layer_index for use with the per-layer API.
    fn bucket(clips: &[ViewportClip], layer_count: usize) -> Vec<Vec<ViewportClip>> {
        let mut buckets = vec![Vec::new(); layer_count];
        for c in clips {
            if c.layer_index < layer_count {
                buckets[c.layer_index].push(c.clone());
            }
        }
        buckets
    }

    #[test]
    fn hit_body_region() {
        // Clip at beat 0..4, layer 0 (height 60), ppb=100 → 400px wide
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 4.0)];
        let by_layer = bucket(&clips, 1);
        // Click at beat 2.0, Y=30 (center of 60px track), padding=6
        let result = ClipHitTester::hit_test(
            2.0,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        let hit = result.unwrap();
        assert_eq!(hit.clip_id, "c1");
        assert_eq!(hit.region, HitRegion::Body);
        assert_eq!(hit.layer_index, 0);
    }

    #[test]
    fn hit_trim_left() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 4.0)];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            0.05,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        assert_eq!(result.unwrap().region, HitRegion::TrimLeft);
    }

    #[test]
    fn hit_trim_right() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 4.0)];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            3.95,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        assert_eq!(result.unwrap().region, HitRegion::TrimRight);
    }

    /// D4/B1: a 1px-wide clip still has two grabbable handles, via outward
    /// extension into empty lane space — the narrow-clip disable
    /// (`trim_w < 2.0 → Body`) is gone.
    #[test]
    fn one_pixel_clip_has_two_grabbable_handles() {
        let ppb = 100.0;
        let mapper = make_mapper(ppb, &[60.0]);
        // duration 0.01 beat * ppb 100 = 1px wide. Isolated (no neighbor on
        // either side), so both handles get the full 4px outward extension.
        let clips = vec![make_clip("c1", 0, 0.0, 0.01)];
        let by_layer = bucket(&clips, 1);
        let hit_at = |beat: f32| {
            ClipHitTester::hit_test(
                beat,
                30.0,
                6.0,
                &mapper,
                |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
                no_groups,
            )
        };
        // 2px left of the clip's own start (beat 0.0) — only reachable via
        // the outward extension.
        let left = hit_at(-0.02).expect("left handle should be grabbable");
        assert_eq!(left.region, HitRegion::TrimLeft);
        // 3px right of the clip's own end (beat 0.01) — only reachable via
        // the outward extension.
        let right = hit_at(0.04).expect("right handle should be grabbable");
        assert_eq!(right.region, HitRegion::TrimRight);
    }

    /// D4: two abutting clips (gap 0) split the shared boundary 50/50 — each
    /// clip's outward extension on the touching side is 0, so the boundary
    /// point falls to exactly one clip's zone, with no gap and no overlap.
    #[test]
    fn abutting_clips_split_boundary_50_50() {
        let ppb = 100.0;
        let mapper = make_mapper(ppb, &[60.0]);
        // c1: beats [0,4). c2: beats [4,8). Fully abutting at beat 4.0.
        let clips = vec![make_clip("c1", 0, 0.0, 4.0), make_clip("c2", 0, 4.0, 4.0)];
        let by_layer = bucket(&clips, 1);
        let hit_at = |beat: f32| {
            ClipHitTester::hit_test(
                beat,
                30.0,
                6.0,
                &mapper,
                |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
                no_groups,
            )
        };
        // Just inside c1's own inner trim (width 4 beats = 400px, inner = 8px
        // -> beat 3.96 = 4px before the boundary).
        let just_before = hit_at(3.96).unwrap();
        assert_eq!(just_before.clip_id, "c1");
        assert_eq!(just_before.region, HitRegion::TrimRight);
        // Exactly at the shared boundary: belongs to c2 (half-open convention
        // — matches the pre-existing `Span::contains` rule), never to both.
        let boundary = hit_at(4.0).unwrap();
        assert_eq!(boundary.clip_id, "c2");
        assert_eq!(boundary.region, HitRegion::TrimLeft);
        // Just inside c2's own inner trim.
        let just_after = hit_at(4.04).unwrap();
        assert_eq!(just_after.clip_id, "c2");
        assert_eq!(just_after.region, HitRegion::TrimLeft);
    }

    /// D4: a 100px-wide clip gets exactly the 8px capped inner trim on each
    /// side (`min(8, 100/3=33.3).max(2) == 8`).
    #[test]
    fn wide_clip_gets_8px_inner_handles() {
        let ppb = 100.0;
        let mapper = make_mapper(ppb, &[60.0]);
        // duration 1.0 beat * ppb 100 = 100px wide, isolated.
        let clips = vec![make_clip("c1", 0, 0.0, 1.0)];
        let by_layer = bucket(&clips, 1);
        let hit_at = |beat: f32| {
            ClipHitTester::hit_test(
                beat,
                30.0,
                6.0,
                &mapper,
                |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
                no_groups,
            )
            .unwrap()
            .region
        };
        assert_eq!(hit_at(0.07), HitRegion::TrimLeft); // 7px in — inside the 8px inner trim
        assert_eq!(hit_at(0.09), HitRegion::Body); // 9px in — just past it
        assert_eq!(hit_at(0.50), HitRegion::Body); // dead centre
        assert_eq!(hit_at(0.91), HitRegion::Body); // 9px from the right edge — just outside
        assert_eq!(hit_at(0.93), HitRegion::TrimRight); // 7px from the right edge — inside
    }

    /// B2: the cursor-affordance path and the click/drag path both resolve
    /// through this same `ClipHitTester::hit_test` (verified by reading —
    /// `manifold-ui/src/panels/viewport/interaction.rs::hit_test_clip` and
    /// `manifold-ui/src/interaction_overlay.rs::hit_test_at` both call it
    /// directly, no private cursor-side geometry exists). So "agreement" is
    /// architectural, not coincidental: the same (clip, pointer) input must
    /// yield the same region at every zoom level, with no zoom-dependent
    /// special case creeping into the shared rule. Swept across a range of
    /// ppb (ruled out: the D4 zone rule silently behaving differently at
    /// some zoom level).
    #[test]
    fn cursor_and_hit_test_agree_across_zoom_sweep() {
        for &ppb in &[10.0_f32, 50.0, 100.0, 500.0, 2000.0] {
            let mapper = make_mapper(ppb, &[60.0]);
            // 4-beat clip, isolated, at every zoom level.
            let clips = vec![make_clip("c1", 0, 0.0, 4.0)];
            let by_layer = bucket(&clips, 1);
            let hit_at = |beat: f32| {
                ClipHitTester::hit_test(
                    beat,
                    30.0,
                    6.0,
                    &mapper,
                    |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
                    no_groups,
                )
            };
            // A fixed PIXEL offset (3px into the clip, well inside the 8px
            // inner trim at every ppb tested — width stays >= 24px
            // throughout this sweep, so `inner` is capped at 8 the whole
            // way) converted to the beat position that pixel offset lands
            // on at this zoom level. A fixed *beat* offset would drift in
            // and out of the zone as ppb changes — exactly the zoom-
            // dependent bug this test exists to catch.
            let offset_beat = 3.0 / ppb;
            // Same beat position queried twice — standing in for the
            // "cursor path" and the "click/drag path" — must agree, at
            // every zoom level, since both are the identical function.
            let cursor_call = hit_at(offset_beat).map(|h| h.region);
            let click_call = hit_at(offset_beat).map(|h| h.region);
            assert_eq!(
                cursor_call, click_call,
                "cursor and hit-test disagreed at ppb={ppb}"
            );
            assert_eq!(
                cursor_call,
                Some(HitRegion::TrimLeft),
                "left handle unreachable at ppb={ppb}"
            );
        }
    }

    #[test]
    fn miss_gap_between_clips() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 2.0), make_clip("c2", 0, 4.0, 2.0)];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            3.0,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        assert!(result.is_none());
    }

    #[test]
    fn miss_in_padding() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 4.0)];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            2.0,
            2.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        assert!(result.is_none());
    }

    #[test]
    fn miss_group_layer() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 4.0)];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            2.0,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            |_| true,
        );
        assert!(result.is_none());
    }

    #[test]
    fn reverse_iteration_last_wins() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![
            make_clip("first", 0, 0.0, 4.0),
            make_clip("last", 0, 0.0, 4.0),
        ];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            2.0,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        assert_eq!(result.unwrap().clip_id, "last");
    }

    #[test]
    fn box_select_collects_overlapping() {
        let clips = vec![
            make_clip("c1", 0, 0.0, 2.0), // beats 0-2
            make_clip("c2", 0, 3.0, 2.0), // beats 3-5
            make_clip("c3", 1, 1.0, 3.0), // beats 1-4, layer 1
            make_clip("c4", 2, 0.0, 1.0), // beats 0-1, layer 2 (outside)
        ];
        let by_layer = bucket(&clips, 3);
        // Region: beats 1-4, layers 0-1
        let result = ClipHitTester::box_select(
            1.0,
            4.0,
            0,
            1,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            3,
            no_groups,
        );
        assert!(result.contains(&ClipId::new("c1"))); // 0-2 overlaps 1-4
        assert!(result.contains(&ClipId::new("c2"))); // 3-5 overlaps 1-4
        assert!(result.contains(&ClipId::new("c3"))); // 1-4 overlaps 1-4
        assert!(!result.contains(&ClipId::new("c4"))); // layer 2 outside
    }

    #[test]
    fn box_select_skips_groups() {
        let clips = vec![make_clip("c1", 0, 0.0, 4.0), make_clip("c2", 1, 0.0, 4.0)];
        let by_layer = bucket(&clips, 2);
        // Layer 0 is a group
        let result = ClipHitTester::box_select(
            0.0,
            4.0,
            0,
            1,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            2,
            |i| i == 0,
        );
        assert!(!result.contains(&ClipId::new("c1"))); // group layer skipped
        assert!(result.contains(&ClipId::new("c2"))); // non-group collected
    }

    // ── HitTargets (UI_AUTOMATION_DESIGN.md P1) ──────────────────────

    fn make_screen_rect(id: &str, name: &str, rect: crate::node::Rect) -> ClipScreenRect {
        ClipScreenRect {
            clip_id: ClipId::new(id),
            layer_index: 0,
            rect,
            base_color: Color32::new(100, 100, 100, 255), // design-token-exempt: test fixture (HitTargets screen-rect helper)
            name: name.into(),
            start_beat: Beats::from_f32(0.0),
            end_beat: Beats::from_f32(4.0),
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            waveform_breakpoints: Vec::new(),
        }
    }

    #[test]
    fn clip_hit_targets_enumerates_every_visible_clip_with_a_payload_id() {
        let rects = vec![
            make_screen_rect("c1", "EXILE", crate::node::Rect::new(0.0, 0.0, 400.0, 60.0)),
            make_screen_rect("c2", "RETURN", crate::node::Rect::new(400.0, 0.0, 300.0, 60.0)),
        ];
        let targets = ClipHitTargets(&rects);
        assert_eq!(targets.surface_id(), "timeline_clips");
        let mut out = Vec::new();
        targets.enumerate(&mut out);
        assert_eq!(out.len(), 2, "one entry per visible clip");
        assert_eq!(out[0].kind, "clip");
        assert_eq!(out[0].label, "EXILE");
        assert_eq!(out[0].payload, "c1");
        assert_eq!(out[1].payload, "c2");
    }
}
