//! Pure math hit-tester: maps (beat, Y) coordinates to a TimelineClip + region.
//! Mechanical translation of Assets/Scripts/UI/Timeline/ClipHitTester.cs.
//!
//! No MonoBehaviour, no allocations on the hot path.
//! The struct is stateless — CoordinateMapper and clip data are passed as parameters.

use crate::coordinate_mapper::CoordinateMapper;
use crate::panels::viewport::ViewportClip;
use manifold_core::ClipId;

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

// ── Constants ───────────────────────────────────────────────────

/// Maximum trim handle width in pixels (Unity ClipHitTester.TRIM_HANDLE_WIDTH_PX = 8f, line 25).
const MAX_TRIM_HANDLE_PX: f32 = 8.0;

/// Each trim handle takes at most this fraction of the clip width,
/// guaranteeing ≥70% of any clip is grabbable body.
const TRIM_HANDLE_RATIO: f32 = 0.15;

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
        if y_in_track_content < clip_top || y_in_track_content > clip_bottom {
            return None;
        }

        // Unity line 68: pixels per beat for trim handle detection
        let ppb = mapper.pixels_per_beat();

        // Unity lines 72-97: iterate this layer's clips in reverse (topmost/last wins)
        for clip in clips_for_layer(layer_index).iter().rev() {
            let clip_start_f32 = clip.start_beat.as_f32();
            let clip_end = clip_start_f32 + clip.duration_beats.as_f32();
            // Unity line 76: beat range check
            if beat_at_pointer < clip_start_f32 || beat_at_pointer >= clip_end {
                continue;
            }

            // Unity lines 80-81: determine hit region
            let local_px = (beat_at_pointer - clip_start_f32) * ppb;
            let clip_width_px = clip.duration_beats.as_f32() * ppb;

            // Trim handle detection — proportional width so narrow clips
            // stay grabbable (≥70% body). Caps at 8px for wide clips.
            let trim_w = MAX_TRIM_HANDLE_PX.min(clip_width_px * TRIM_HANDLE_RATIO);
            let region = if trim_w < 2.0 {
                // Clip too narrow for usable trim handles
                HitRegion::Body
            } else if local_px < trim_w {
                HitRegion::TrimLeft
            } else if local_px > clip_width_px - trim_w {
                HitRegion::TrimRight
            } else {
                HitRegion::Body
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
                if clip_end > min_beat && clip_start_f32 < max_beat {
                    results.push(clip.clip_id.clone());
                }
            }
        }

        results
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate_mapper::CoordinateMapper;
    use crate::node::Color32;
    use manifold_core::Beats;

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
            name: String::new(),
            color: Color32::new(100, 100, 100, 255),
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
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

    #[test]
    fn no_trim_on_narrow_clip() {
        let mapper = make_mapper(100.0, &[60.0]);
        let clips = vec![make_clip("c1", 0, 0.0, 0.1)];
        let by_layer = bucket(&clips, 1);
        let result = ClipHitTester::hit_test(
            0.005,
            30.0,
            6.0,
            &mapper,
            |i| by_layer.get(i).map(|v| v.as_slice()).unwrap_or(&[]),
            no_groups,
        );
        assert_eq!(result.unwrap().region, HitRegion::Body);
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
}
