use super::*;
use crate::layout::ScreenLayout;
use crate::panels::Panel;
use crate::panels::viewport::{TimelineViewportPanel, TrackInfo, ViewportClip};
use crate::timeline_editing_host::RegionSplitResult;
use crate::tree::UITree;
use crate::types::LayerType;
use crate::view::{SelectionRegion, UiLayer};
use manifold_foundation::LayerId;

/// Minimal host — only the handful of methods `on_pointer_click`'s and
/// `on_begin_drag`'s Body-hit arms actually read (clip/layer lookup) are
/// meaningfully implemented; everything else is a harmless no-op. A full
/// mock of the ~40-method trait was judged too costly for the S2 repro
/// in P1.0's evidence deck; this one is scoped to exactly what these two
/// entry points touch for a plain (non-automation, non-region-partial)
/// clip press.
struct TestHost {
    layers: Vec<UiLayer>,
    // (id, layer_index, start_beat, duration_beats, is_locked)
    clips: Vec<(ClipId, usize, Beats, Beats, bool)>,
}

impl TestHost {
    fn new(layer_ids: &[&str]) -> Self {
        let layers = layer_ids
            .iter()
            .map(|id| UiLayer {
                layer_id: LayerId::new(id),
                parent_layer_id: None,
                layer_type: LayerType::Video,
                is_collapsed: false,
                automation_lane_count: 0,
            })
            .collect();
        Self {
            layers,
            clips: Vec::new(),
        }
    }

    fn with_clip(mut self, id: &str, layer_index: usize, start: f32, duration: f32) -> Self {
        self.clips.push((
            ClipId::new(id),
            layer_index,
            Beats::from_f32(start),
            Beats::from_f32(duration),
            false,
        ));
        self
    }

    fn to_ref(&self, entry: &(ClipId, usize, Beats, Beats, bool)) -> ClipRef {
        let (id, li, start, dur, locked) = entry;
        ClipRef {
            clip_id: id.clone(),
            start_beat: *start,
            duration_beats: *dur,
            end_beat: *start + *dur,
            layer_index: *li,
            layer_id: self.layers[*li].layer_id.clone(),
            in_point: Seconds::ZERO,
            is_generator: false,
            is_locked: *locked,
            is_looping: false,
        }
    }
}

impl TimelineEditingHost for TestHost {
    fn layer_count(&self) -> usize {
        self.layers.len()
    }
    fn layers(&self) -> &[UiLayer] {
        &self.layers
    }
    fn layer_id_at_index(&self, index: usize) -> Option<LayerId> {
        self.layers.get(index).map(|l| l.layer_id.clone())
    }
    fn layer_accepts_clips_from(&self, _to: usize, _from: usize) -> bool {
        true
    }
    fn is_layer_muted(&self, _index: usize) -> bool {
        false
    }
    fn project_beats_per_bar(&self) -> u32 {
        4
    }
    fn clip_source_seconds(&self, _clip_id: &str, from: Beats, to: Beats) -> Seconds {
        Seconds((to - from).0 * TEST_SECONDS_PER_BEAT)
    }
    fn clip_beats_for_source(&self, _clip_id: &str, _from: Beats, seconds: Seconds) -> Beats {
        Beats(seconds.0 / TEST_SECONDS_PER_BEAT)
    }
    fn is_playing(&self) -> bool {
        false
    }
    fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef> {
        self.clips
            .iter()
            .find(|c| c.0.as_str() == clip_id)
            .map(|c| self.to_ref(c))
    }
    fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef> {
        self.clips
            .iter()
            .filter(|c| c.1 == layer_index)
            .map(|c| self.to_ref(c))
            .collect()
    }
    fn screen_position_to_beat(&self, _pos: Vec2) -> Beats {
        Beats::ZERO
    }
    fn get_layer_index_at_position(&self, _pos: Vec2) -> Option<usize> {
        None
    }
    fn beat_to_time(&self, _beat: Beats) -> Seconds {
        Seconds::ZERO
    }
    fn create_clip_at_position(
        &mut self,
        _beat: Beats,
        _layer: usize,
        _grid_step: Beats,
    ) -> Option<ClipId> {
        None
    }
    fn move_clip_to_layer(&mut self, _clip_id: &str, _target_layer: usize) {}
    fn on_clip_selected(&mut self, _clip_id: &str) {}
    fn on_clip_right_click(&mut self, _clip_id: &str, _screen_pos: Vec2) {}
    fn on_track_right_click(&mut self, _beat: Beats, _layer_index: usize, _screen_pos: Vec2) {}
    fn on_automation_lane_right_click(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _screen_pos: Vec2,
    ) {
    }
    fn inspect_layer(&mut self, _layer_index: usize) {}
    fn redraw(&mut self) {}
    fn set_cursor(&mut self, _cursor: TimelineCursor) {}
    fn scrub_to_time(&mut self, _time: Seconds) {}
    fn resolve_overlaps(&mut self, _clip_ids: Vec<ClipId>) {}
    fn split_clips_for_region_move(&mut self, _region: &SelectionRegion) -> RegionSplitResult {
        RegionSplitResult {
            interior_clip_ids: Vec::new(),
            split_count: 0,
        }
    }
    fn begin_command_batch(&mut self) {}
    fn record_move(
        &mut self,
        _clip_id: &str,
        _old_start: Beats,
        _new_start: Beats,
        _old_layer: usize,
        _new_layer: usize,
    ) {
    }
    fn record_trim(
        &mut self,
        _clip_id: &str,
        _old_start: Beats,
        _new_start: Beats,
        _old_duration: Beats,
        _new_duration: Beats,
        _old_in_point: Seconds,
        _new_in_point: Seconds,
    ) {
    }
    fn duplicate_clip_to(&mut self, _src_clip_id: &str, _target_beat: Beats, _target_layer: usize) {
    }
    fn commit_command_batch(&mut self, _description: &str) {}
    fn set_clip_start_beat(&mut self, _clip_id: &str, _beat: Beats) {}
    fn set_clip_trim(
        &mut self,
        _clip_id: &str,
        _start_beat: Beats,
        _duration_beats: Beats,
        _in_point: Seconds,
    ) {
    }
    fn get_max_duration_beats(&self, _clip_id: &str) -> Beats {
        Beats::ZERO
    }
    fn add_automation_point(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _beat: Beats,
        _value: f32,
        _shape: UiSegmentShape,
    ) {
    }
    fn insert_automation_point_on_curve(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _beat: Beats,
        _outside_shape: UiSegmentShape,
    ) -> Option<f32> {
        None
    }
    fn set_automation_point_preview(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _from_beat: Beats,
        _from_value: f32,
        _to_beat: Beats,
        _to_value: f32,
    ) {
    }
    fn commit_automation_point_move(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _old: (Beats, f32, UiSegmentShape),
        _new: (Beats, f32, UiSegmentShape),
    ) {
    }
    fn remove_automation_point(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _beat: Beats,
        _value_norm: f32,
    ) {
    }
    fn set_automation_segment_bend_preview(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _left_beat: Beats,
        _left_value: f32,
        _bend: f32,
    ) {
    }
    fn set_automation_segment_drag_preview(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _left_beat: Beats,
        _left_from_value: f32,
        _left_value: f32,
        _right_beat: Beats,
        _right_from_value: f32,
        _right_value: f32,
    ) {
    }
    fn commit_automation_segment_drag(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _left: (Beats, f32, f32, UiSegmentShape),
        _right: (Beats, f32, f32, UiSegmentShape),
    ) {
    }
    fn commit_automation_group_move(&mut self, _moves: Vec<AutomationPointMove>) {}
    fn automation_lane_points(
        &self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
    ) -> Option<Vec<(Beats, f32, UiSegmentShape)>> {
        None
    }
    fn set_automation_lane_preview(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _points: &[(Beats, f32, UiSegmentShape)],
    ) {
    }
    fn clear_automation_previews(&mut self) {}
    fn restore_automation_lane_preview(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _points: Option<&[(Beats, f32, UiSegmentShape)]>,
    ) {
    }
    fn commit_automation_draw_stroke(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        _new_points: Vec<(Beats, f32, UiSegmentShape)>,
        _old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    ) {
    }
}

fn test_clip(id: &str, layer_index: usize, start: f32, duration: f32) -> ViewportClip {
    ViewportClip {
        clip_id: id.into(),
        layer_index,
        start_beat: Beats::from_f32(start),
        duration_beats: Beats::from_f32(duration),
        name: "".into(),
        color: crate::color::CLIP_NORMAL,
        is_muted: false,
        is_locked: false,
        is_generator: false,
        is_audio: false,
        waveform: None,
        in_point_seconds: 0.0,
        waveform_breakpoints: Vec::new(),
    }
}

/// Two abutting clips (`clip_a` [0,4), `clip_b` [4,8)) on one layer, built
/// through the REAL `Panel::build` so the geometry matches production.
fn build_two_clip_viewport() -> TimelineViewportPanel {
    let mut tree = UITree::new();
    let mut panel = TimelineViewportPanel::new();
    panel.set_tracks(vec![TrackInfo::default()]);
    panel.set_clips(vec![
        test_clip("clip_a", 0, 0.0, 4.0),
        test_clip("clip_b", 0, 4.0, 4.0),
    ]);
    // The mapper's Y-layout (what `get_layer_at_y` — and so hit-testing —
    // reads) is rebuilt from real layer data, separately from `build()`'s
    // own tracks_rect/bitmap geometry (app_render.rs does both on every
    // structural sync). Skipping this leaves `mapper.get_layer_at_y`
    // empty even though the painted rects look right.
    panel.rebuild_mapper_layout(&[UiLayer {
        layer_id: LayerId::new("layer-0"),
        parent_layer_id: None,
        layer_type: LayerType::Video,
        is_collapsed: false,
        automation_lane_count: 0,
    }]);
    let layout = ScreenLayout::new(1920.0, 1080.0);
    panel.build(&mut tree, &layout);
    panel
}

/// Screen position over the body of whichever clip covers `beat_center`,
/// vertically centered in layer 0's row.
fn body_pos_for(panel: &TimelineViewportPanel, beat_center: f32) -> Vec2 {
    Vec2::new(
        panel.beat_to_pixel(Beats::from_f32(beat_center)),
        panel.tracks_rect().y + 70.0,
    )
}

/// Census bug 19's class (a dead region becomes layer 0): once both edge
/// layers of the region are deleted it anchors nothing, so Shift+click
/// places an insert cursor instead of a region reaching layer 0.
#[test]
fn shift_click_after_region_edge_layers_are_deleted_does_not_anchor_on_layer_0() {
    let before = TestHost::new(&["a", "b", "c", "d"]);
    let mut ui_state = UIState::new();
    ui_state.set_region(Beats(2.0), Beats(6.0), 1, 2, before.layers());
    let after = TestHost::new(&["a", "d"]);

    select_region_to(Beats(8.0), 1, &mut ui_state, &after);

    assert!(
        ui_state.current_region().is_none(),
        "region {:?}",
        ui_state.current_region()
    );
    assert_eq!(ui_state.insert_cursor_layer_id(), Some(&LayerId::new("d")));
}

#[test]
fn press_and_release_without_drag_collapses_to_the_clicked_clip() {
    let panel = build_two_clip_viewport();
    let mut host = TestHost::new(&["layer-0"])
        .with_clip("clip_a", 0, 0.0, 4.0)
        .with_clip("clip_b", 0, 4.0, 4.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a"), ClipId::new("clip_b")]);

    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    // A plain click (mouse-up-without-drag) on clip_a, part of the
    // existing multi-selection — must collapse to just this clip.
    overlay.on_pointer_click(
        body_pos_for(&panel, 2.0),
        false,
        false,
        1,
        false,
        &mut host,
        &mut ui_state,
        &panel,
    );

    let ids: HashSet<ClipId> = ui_state.get_selected_clip_ids().into_iter().collect();
    assert_eq!(
        ids,
        HashSet::from([ClipId::new("clip_a")]),
        "a plain click with no drag collapses the group to the clicked clip"
    );
}

#[test]
fn drag_from_any_selected_member_moves_the_whole_group() {
    let panel = build_two_clip_viewport();
    let mut host = TestHost::new(&["layer-0"])
        .with_clip("clip_a", 0, 0.0, 4.0)
        .with_clip("clip_b", 0, 4.0, 4.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a"), ClipId::new("clip_b")]);

    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    // Press on clip_b — NOT the anchor — and begin a drag: grab-any-member.
    overlay.on_begin_drag(body_pos_for(&panel, 6.0), &mut host, &mut ui_state, &panel);

    assert_eq!(overlay.drag_mode(), DragMode::Move);
    let ids: HashSet<ClipId> = ui_state.get_selected_clip_ids().into_iter().collect();
    let expected = HashSet::from([ClipId::new("clip_a"), ClipId::new("clip_b")]);
    assert_eq!(
        ids, expected,
        "a drag begun on any already-selected member keeps the whole group selected"
    );
}
