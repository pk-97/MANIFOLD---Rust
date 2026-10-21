use super::*;
use crate::input::{PointerAction as InputPointerAction, UIEvent, UIInputSystem};
use crate::layout::ScreenLayout;
use crate::panels::Panel;
use crate::panels::viewport::{TimelineViewportPanel, TrackInfo, ViewportClip};
use crate::timeline_editing_host::RegionSplitResult;
use crate::node::UIStyle;
use crate::tree::UITree;
use crate::types::LayerType;
use crate::view::{SelectionRegion, UiLayer};
use manifold_foundation::LayerId;

/// A move/trim-capable, mutation-tracking host. Unlike
/// `b4_group_move_tests::TestHost` (whose `set_clip_start_beat`/
/// `move_clip_to_layer` are no-ops — sufficient for that module's
/// selection-only assertions), P1.4's tests need to observe the actual
/// live model mutation a drag performs, and what `record_move`/
/// `record_trim`/`commit_command_batch` do with it — so every mutating
/// method here is real, and `commit_command_batch` mirrors
/// `AppEditingHost`'s own `if commands.is_empty() { return; }`
/// short-circuit (an empty batch pushes no undo entry).
#[derive(Clone, Debug, PartialEq)]
struct ClipEntry {
    id: ClipId,
    layer_index: usize,
    start_beat: Beats,
    duration_beats: Beats,
    in_point: Seconds,
    is_locked: bool,
    is_generator: bool,
}

type AutomationPointMoveRecord = (
    UiGraphTarget,
    ParamId,
    (Beats, f32, UiSegmentShape),
    (Beats, f32, UiSegmentShape),
);
type AutomationSegmentDragCommit = (
    UiGraphTarget,
    ParamId,
    (Beats, f32, f32, UiSegmentShape),
    (Beats, f32, f32, UiSegmentShape),
);
type AutomationGroupMoveCommit = Vec<AutomationPointMove>;
type AutomationDrawCommit = (
    UiGraphTarget,
    ParamId,
    Vec<(Beats, f32, UiSegmentShape)>,
    Option<Vec<(Beats, f32, UiSegmentShape)>>,
);

struct GestureTestHost {
    layers: Vec<UiLayer>,
    clips: Vec<ClipEntry>,
    move_records: Vec<(ClipId, Beats, Beats, usize, usize)>,
    trim_records: Vec<(ClipId, Beats, Beats, Beats, Beats)>,
    batch_ops: usize,
    committed_batches: Vec<usize>,
    // P7.3 automation-fold pinning (recorded, not no-op'd, so the tests
    // below can assert exactly what a gesture committed).
    automation_added: Vec<(Beats, f32)>,
    automation_removed: Vec<(Beats, f32)>,
    automation_point_moves: Vec<AutomationPointMoveRecord>,
    automation_lane_preview: Vec<(Beats, f32, UiSegmentShape)>,
    automation_lane_exists: bool,
    automation_original_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    automation_preview_clears: usize,
    automation_lane_restores: Vec<AutomationLaneGestureSnapshot>,
    automation_segment_drag_commits: Vec<AutomationSegmentDragCommit>,
    automation_group_move_commits: Vec<AutomationGroupMoveCommit>,
    automation_draw_commits: Vec<AutomationDrawCommit>,
    // records every `on_automation_lane_right_click` call.
    automation_lane_right_clicks: Vec<(UiGraphTarget, ParamId)>,
    // Invalidation signals, counted so tests can pin rebuild-vs-redraw.
    redraw_requests: usize,
}

impl GestureTestHost {
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
            move_records: Vec::new(),
            trim_records: Vec::new(),
            batch_ops: 0,
            committed_batches: Vec::new(),
            automation_added: Vec::new(),
            automation_removed: Vec::new(),
        automation_point_moves: Vec::new(),
            automation_lane_preview: Vec::new(),
            automation_lane_exists: true,
            automation_original_points: None,
            automation_preview_clears: 0,
            automation_lane_restores: Vec::new(),
            automation_segment_drag_commits: Vec::new(),
            automation_group_move_commits: Vec::new(),
            automation_draw_commits: Vec::new(),
            automation_lane_right_clicks: Vec::new(),
            redraw_requests: 0,
        }
    }

    fn with_clip(mut self, id: &str, layer_index: usize, start: f32, duration: f32) -> Self {
        self.clips.push(ClipEntry {
            id: ClipId::new(id),
            layer_index,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(duration),
            in_point: Seconds::ZERO,
            is_locked: false,
            is_generator: false,
        });
        self
    }

    fn with_generator_clip(mut self, id: &str, layer_index: usize, start: f32, duration: f32) -> Self {
        self.clips.push(ClipEntry {
            id: ClipId::new(id),
            layer_index,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(duration),
            in_point: Seconds::ZERO,
            is_locked: false,
            is_generator: true,
        });
        self
    }

    fn with_locked_clip(mut self, id: &str, layer_index: usize, start: f32, duration: f32) -> Self {
        self.clips.push(ClipEntry {
            id: ClipId::new(id),
            layer_index,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(duration),
            in_point: Seconds::ZERO,
            is_locked: true,
            is_generator: false,
        });
        self
    }

    fn to_ref(&self, e: &ClipEntry) -> ClipRef {
        ClipRef {
            clip_id: e.id.clone(),
            start_beat: e.start_beat,
            duration_beats: e.duration_beats,
            end_beat: e.start_beat + e.duration_beats,
            layer_index: e.layer_index,
            layer_id: self.layers[e.layer_index].layer_id.clone(),
            in_point: e.in_point,
            is_generator: e.is_generator,
            is_locked: e.is_locked,
            is_looping: false,
        }
    }
}

impl TimelineEditingHost for GestureTestHost {
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
            .find(|c| c.id.as_str() == clip_id)
            .map(|c| self.to_ref(c))
    }
    fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef> {
        self.clips
            .iter()
            .filter(|c| c.layer_index == layer_index)
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
    fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize) {
        if let Some(c) = self.clips.iter_mut().find(|c| c.id.as_str() == clip_id) {
            c.layer_index = target_layer;
        }
    }
    fn on_clip_selected(&mut self, _clip_id: &str) {}
    fn on_clip_right_click(&mut self, _clip_id: &str, _screen_pos: Vec2) {}
    fn on_track_right_click(&mut self, _beat: Beats, _layer_index: usize, _screen_pos: Vec2) {}
    fn on_automation_lane_right_click(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        _screen_pos: Vec2,
    ) {
        self.automation_lane_right_clicks.push((target.clone(), param_id.clone()));
    }
    fn inspect_layer(&mut self, _layer_index: usize) {}
    fn redraw(&mut self) {
        self.redraw_requests += 1;
    }
    fn set_cursor(&mut self, _cursor: TimelineCursor) {}
    fn scrub_to_time(&mut self, _time: Seconds) {}
    fn enforce_non_overlap(&mut self, _clip_id: &str, _ignore_ids: &HashSet<ClipId>) {}
    fn split_clips_for_region_move(&mut self, _region: &SelectionRegion) -> RegionSplitResult {
        RegionSplitResult {
            interior_clip_ids: Vec::new(),
            split_count: 0,
        }
    }
    fn begin_command_batch(&mut self) {
        self.batch_ops = 0;
    }
    fn record_move(
        &mut self,
        clip_id: &str,
        old_start: Beats,
        new_start: Beats,
        old_layer: usize,
        new_layer: usize,
    ) {
        self.move_records
            .push((ClipId::new(clip_id), old_start, new_start, old_layer, new_layer));
        self.batch_ops += 1;
    }
    fn record_trim(
        &mut self,
        clip_id: &str,
        old_start: Beats,
        new_start: Beats,
        old_duration: Beats,
        new_duration: Beats,
        _old_in_point: Seconds,
        _new_in_point: Seconds,
    ) {
        self.trim_records
            .push((ClipId::new(clip_id), old_start, new_start, old_duration, new_duration));
        self.batch_ops += 1;
    }
    fn duplicate_clip_to(&mut self, _src_clip_id: &str, _target_beat: Beats, _target_layer: usize) {
        self.batch_ops += 1;
    }
    fn commit_command_batch(&mut self, _description: &str) {
        // Mirrors `AppEditingHost::commit_command_batch`'s
        // `if commands.is_empty() { return; }` — a batch with nothing
        // recorded produces no undo entry and reaches nothing downstream.
        if self.batch_ops > 0 {
            self.committed_batches.push(self.batch_ops);
        }
        self.batch_ops = 0;
    }
    fn set_clip_start_beat(&mut self, clip_id: &str, beat: Beats) {
        if let Some(c) = self.clips.iter_mut().find(|c| c.id.as_str() == clip_id) {
            c.start_beat = beat;
        }
    }
    fn set_clip_trim(
        &mut self,
        clip_id: &str,
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
    ) {
        if let Some(c) = self.clips.iter_mut().find(|c| c.id.as_str() == clip_id) {
            c.start_beat = start_beat;
            c.duration_beats = duration_beats;
            c.in_point = in_point;
        }
    }
    fn get_max_duration_beats(&self, _clip_id: &str) -> Beats {
        Beats::ZERO
    }
    fn add_automation_point(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        beat: Beats,
        value: f32,
        _shape: UiSegmentShape,
    ) {
        self.automation_added.push((beat, value));
    }
    fn insert_automation_point_on_curve(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        beat: Beats,
        _outside_shape: UiSegmentShape,
    ) -> Option<f32> {
        let points = self.automation_original_points.clone().unwrap_or_else(|| {
            vec![(Beats(4.0), 0.5, UiSegmentShape::Linear), (Beats(8.0), 0.8, UiSegmentShape::Linear)]
        });
        // Past the end points the line holds the end value, as the command does.
        let value = match (
            points.iter().rev().find(|point| point.0 <= beat),
            points.iter().find(|point| point.0 > beat),
        ) {
            (Some(left), Some(right)) => {
                let t = ((beat.0 - left.0.0) / (right.0.0 - left.0.0)).clamp(0.0, 1.0) as f32;
                left.1 + (right.1 - left.1) * left.2.sample(t)
            }
            (Some(end), None) | (None, Some(end)) => end.1,
            (None, None) => return None,
        };
        self.automation_added.push((beat, value));
        Some(value)
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
        target: &UiGraphTarget,
        param_id: &ParamId,
        old: (Beats, f32, UiSegmentShape),
        new: (Beats, f32, UiSegmentShape),
    ) {
        self.automation_point_moves
            .push((target.clone(), param_id.clone(), old, new));
    }
    fn remove_automation_point(&mut self, _target: &UiGraphTarget, _param_id: &ParamId, beat: Beats, value_norm: f32) {
        self.automation_removed.push((beat, value_norm));
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
        target: &UiGraphTarget,
        param_id: &ParamId,
        left: (Beats, f32, f32, UiSegmentShape),
        right: (Beats, f32, f32, UiSegmentShape),
    ) {
        self.automation_segment_drag_commits
            .push((target.clone(), param_id.clone(), left, right));
    }
    fn commit_automation_group_move(
        &mut self,
        moves: Vec<AutomationPointMove>,
    ) {
        self.automation_group_move_commits.push(moves);
    }
    fn automation_lane_points(
        &self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
    ) -> Option<Vec<(Beats, f32, UiSegmentShape)>> {
        self.automation_lane_exists.then(|| self.automation_original_points.clone().unwrap_or_else(|| vec![(Beats(4.0), 0.5, UiSegmentShape::Linear), (Beats(8.0), 0.8, UiSegmentShape::Linear)]))
    }
    fn set_automation_lane_preview(
        &mut self,
        _target: &UiGraphTarget,
        _param_id: &ParamId,
        points: &[(Beats, f32, UiSegmentShape)],
    ) {
        self.automation_lane_preview = points.to_vec();
    }
    fn clear_automation_previews(&mut self) {
        self.automation_preview_clears += 1;
        self.automation_lane_preview.clear();
    }
    fn restore_automation_lane_preview(&mut self, target: &UiGraphTarget, param_id: &ParamId, points: Option<&[(Beats, f32, UiSegmentShape)]>) {
        self.automation_lane_restores.push(AutomationLaneGestureSnapshot { target: target.clone(), param_id: param_id.clone(), points: points.map(|p| p.to_vec()) });
    }
    fn commit_automation_draw_stroke(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        new_points: Vec<(Beats, f32, UiSegmentShape)>,
        old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    ) {
        self.automation_draw_commits
            .push((target.clone(), param_id.clone(), new_points, old_points));
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

/// One clip `[0,8)` and a second `[16,20)` on one layer — the second
/// gives magnetic snap a neighbor edge to pull toward. Built through the
/// REAL `Panel::build` so beat<->pixel geometry matches production.
fn build_viewport() -> TimelineViewportPanel {
    let mut tree = UITree::new();
    let mut panel = TimelineViewportPanel::new();
    panel.set_tracks(vec![TrackInfo::default()]);
    panel.set_clips(vec![
        test_clip("clip_a", 0, 0.0, 8.0),
        test_clip("clip_b", 0, 16.0, 4.0),
    ]);
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

// ── P7.3 automation-fold pinning ────────────────────────────────────
// `docs/UI_WIDGET_UNIFICATION_DESIGN.md` P7.3: prove the six automation
// gestures' begin→track→commit behavior is byte-identical now that their
// state lives on `TimelineDrag`'s payload instead of six parallel
// `Option<...State>` fields. One real lane ("Mirror: amount") with two
// points `[(4.0, 0.5, Linear), (8.0, 0.8, Linear)]`, param range 0..1 —
// enough geometry for point/segment/marquee/group/draw gestures to all
// have real targets. Built through the REAL `Panel::build` +
// `set_automation_lanes` so strip/dot screen geometry matches production.
#[test]
fn automation_resize_changes_only_view_height_and_clamps() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let rect = lane.resize_rect();
    let target = lane.target.clone();
    let param = lane.param_id.clone();
    let press = Vec2::new(rect.x + 20.0, rect.y + 2.5);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationLaneResize);
    overlay.on_drag(press + Vec2::new(0.0, 72.0), &mut host, &mut state, &mut panel);
    assert_eq!(state.automation_lane_height(&target, &param), color::AUTOMATION_LANE_STRIP_HEIGHT + 72.0);
    overlay.on_drag(press + Vec2::new(0.0, 1000.0), &mut host, &mut state, &mut panel);
    assert_eq!(state.automation_lane_height(&target, &param), 240.0);
    overlay.on_drag(press - Vec2::new(0.0, 1000.0), &mut host, &mut state, &mut panel);
    assert_eq!(state.automation_lane_height(&target, &param), color::AUTOMATION_LANE_MIN_HEIGHT);
    overlay.on_end_drag(&mut host);
    assert!(host.automation_point_moves.is_empty());
    assert!(host.automation_lane_preview.is_empty());
    assert!(host.automation_draw_commits.is_empty());
}

fn build_viewport_with_automation() -> TimelineViewportPanel {
    use crate::view::UiAutomationPoint;
    build_viewport_with_lane(
        vec![
            UiAutomationPoint { beat: Beats::from_f32(4.0), value_norm: 0.5, shape: UiSegmentShape::Linear, past_end: false },
            UiAutomationPoint { beat: Beats::from_f32(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false },
        ],
        false,
    )
}

fn build_viewport_with_lane(
    points: Vec<crate::view::UiAutomationPoint>,
    placeholder: bool,
) -> TimelineViewportPanel {
    build_viewport_with_lane_range(points, placeholder, 1.0, false)
}

fn build_viewport_with_lane_range(
    points: Vec<crate::view::UiAutomationPoint>,
    placeholder: bool,
    param_max: f32,
    whole_numbers: bool,
) -> TimelineViewportPanel {
    use crate::panels::viewport::ViewportAutomationLane;
    use crate::view::UiAutomationLane;
    use manifold_foundation::EffectId;

    let mut tree = UITree::new();
    let mut panel = TimelineViewportPanel::new();
    panel.set_tracks(vec![TrackInfo::default()]);
    panel.set_clips(vec![]);
    panel.rebuild_mapper_layout(&[UiLayer {
        layer_id: LayerId::new("layer-0"),
        parent_layer_id: None,
        layer_type: LayerType::Video,
        is_collapsed: false,
        automation_lane_count: 1,
    }]);
    panel.set_automation_lanes(vec![ViewportAutomationLane {
        layer_index: 0,
        lane: UiAutomationLane {
            effect_id: EffectId::new("fx-mirror"),
            param_id: ParamId::Borrowed("amount"),
            target: UiGraphTarget::Effect(EffectId::new("fx-mirror")),
            label: "Mirror: amount".into(),
            points,
            param_min: 0.0,
            param_max,
            whole_numbers,
            placeholder,
            display: crate::view::UiLaneDisplay::default(),
        },
    }]);
    let mut layout = ScreenLayout::new(1920.0, 1080.0);
    layout.timeline_split_ratio = 0.70;
    panel.build(&mut tree, &layout);
    panel
}

fn build_viewport_with_coincident_automation() -> TimelineViewportPanel {
    use crate::panels::viewport::ViewportAutomationLane;
    use crate::view::{UiAutomationLane, UiAutomationPoint};
    use manifold_foundation::EffectId;

    let param_min = 10.5;
    let param_max = 137.25;
    let first_value = 37.25;
    let second_value = 113.5;
    let mut tree = UITree::new();
    let mut panel = TimelineViewportPanel::new();
    panel.set_tracks(vec![TrackInfo::default()]);
    panel.set_clips(vec![]);
    panel.rebuild_mapper_layout(&[UiLayer {
        layer_id: LayerId::new("layer-0"),
        parent_layer_id: None,
        layer_type: LayerType::Video,
        is_collapsed: false,
        automation_lane_count: 1,
    }]);
    panel.set_automation_lanes(vec![ViewportAutomationLane {
        layer_index: 0,
        lane: UiAutomationLane {
            effect_id: EffectId::new("fx-coincident"),
            param_id: ParamId::Borrowed("amount"),
            target: UiGraphTarget::Effect(EffectId::new("fx-coincident")),
            label: "Coincident: amount".into(),
            points: vec![
                UiAutomationPoint {
                    beat: Beats::from_f32(4.0),
                    value_norm: BitmapSlider::value_to_normalized(first_value, param_min, param_max),
                    shape: UiSegmentShape::Linear,
                    past_end: false,
                },
                UiAutomationPoint {
                    beat: Beats::from_f32(4.0),
                    value_norm: BitmapSlider::value_to_normalized(second_value, param_min, param_max),
                    shape: UiSegmentShape::Linear,
                    past_end: false,
                },
            ],
            param_min,
            param_max,
            whole_numbers: false,
            placeholder: false,
            display: crate::view::UiLaneDisplay::default(),
        },
    }]);
    let mut layout = ScreenLayout::new(1920.0, 1080.0);
    layout.timeline_split_ratio = 0.70;
    panel.build(&mut tree, &layout);
    panel
}

/// Screen position of the lane strip's grabbed dot — reads back through
/// the real `automation_lane_screens` geometry rather than recomputing
/// pixel math independently, so the test can't silently drift from
/// production's own conversion.
fn dot_pos(panel: &TimelineViewportPanel, dot_index: usize) -> Vec2 {
    let lanes = panel.automation_lane_screens();
    let dot = lanes[0].dots[dot_index];
    Vec2::new(
        panel.beat_to_pixel(dot.beat),
        lanes[0].y_at_norm(dot.value_norm),
    )
}

#[test]
fn automation_clicks_select_and_shift_toggles_without_inserting() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.set_modifiers(Modifiers { command: true, ..Modifiers::NONE });
    overlay.handle_automation_click(dot_pos(&panel, 0), 2, &mut host, &mut state, &panel);
    assert_eq!(state.selected_automation_point.as_ref().unwrap().beat, Beats(4.0));
    overlay.set_modifiers(Modifiers { shift: true, ..Modifiers::NONE });
    overlay.handle_automation_click(dot_pos(&panel, 1), 1, &mut host, &mut state, &panel);
    assert_eq!(state.selected_automation_points.len(), 2);
    overlay.handle_automation_click(dot_pos(&panel, 0), 1, &mut host, &mut state, &panel);
    assert_eq!(state.selected_automation_points.len(), 1);
    assert_eq!(state.selected_automation_points[0].beat, Beats(8.0));
    let lanes = panel.automation_lane_screens();
    let pos = Vec2::new(lanes[0].curve_rect().x + 33.0, lanes[0].y_at_norm(0.1));
    overlay.handle_automation_click(pos, 1, &mut host, &mut state, &panel);
    assert!(state.selected_automation_points.is_empty());
    assert!(state.selected_automation_point.is_none());
    assert!(host.automation_added.is_empty());
    assert!(host.automation_removed.is_empty());
}

#[test]
fn plain_click_on_dot_deletes_it_and_double_click_does_not_readd() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let (target, param_id) = (lane.target.clone(), lane.param_id.clone());
    let dot0 = lane.dots[0];
    state.selected_automation_points = vec![UiAutomationPointRef {
        target, param_id, beat: dot0.beat, value_norm: dot0.value_norm,
    }];

    let click_pos = dot_pos(&panel, 0);
    let click = |overlay: &mut InteractionOverlay,
                 host: &mut GestureTestHost,
                 state: &mut UIState,
                 panel: &TimelineViewportPanel,
                 count: u32| {
        overlay.on_pointer_click(click_pos, false, false, count, false, host, state, panel);
    };

    // First click: the pointer entry points, as the app routes them.
    overlay.on_pointer_down(click_pos, &mut host, &mut state, &panel);
    assert!(!overlay.has_pending_automation_press(), "a press on a dot arms no draft");
    overlay.on_pointer_up(&mut host, &mut state);
    click(&mut overlay, &mut host, &mut state, &panel, 1);
    assert_eq!(host.automation_removed, vec![(Beats(4.0), 0.5)]);
    assert!(state.selected_automation_points.is_empty(), "the deleted point leaves the selection");

    // The lane now has only the beat-8 point: the second press of the
    // double-click lands on the bare strip the dot left behind. The app
    // sends Click (count 1) then DoubleClick (count 2) for it.
    let panel = build_viewport_with_lane(
        vec![crate::view::UiAutomationPoint {
            beat: Beats::from_f32(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false,
        }],
        false,
    );
    overlay.on_pointer_down(click_pos, &mut host, &mut state, &panel);
    assert!(!overlay.has_pending_automation_press(), "the second press must not arm an insert draft");
    assert!(host.automation_lane_preview.is_empty());
    overlay.on_pointer_up(&mut host, &mut state);
    click(&mut overlay, &mut host, &mut state, &panel, 1);
    click(&mut overlay, &mut host, &mut state, &panel, 2);
    assert!(host.automation_added.is_empty(), "no point is re-added");
    assert_eq!(host.automation_removed.len(), 1, "one delete, one undo entry");

    // A later, separate press and click on the bare strip adds a point.
    std::thread::sleep(std::time::Duration::from_millis(350));
    overlay.on_pointer_down(click_pos, &mut host, &mut state, &panel);
    assert!(overlay.has_pending_automation_press());
    overlay.on_pointer_up(&mut host, &mut state);
    assert_eq!(host.automation_added.len(), 1);
}

#[test]
fn modified_click_on_dot_selects_instead_of_deleting() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.set_modifiers(Modifiers { command: true, ..Modifiers::NONE });
    overlay.handle_automation_click(dot_pos(&panel, 0), 1, &mut host, &mut state, &panel);
    assert_eq!(state.selected_automation_point.as_ref().unwrap().beat, Beats(4.0));
    assert!(host.automation_removed.is_empty());
}

#[test]
fn shift_drag_marquee_adds_to_the_selection_and_plain_drag_replaces_it() {
    let lane_ref = |panel: &TimelineViewportPanel, dot: usize| {
        let lane = &panel.automation_lane_screens()[0];
        UiAutomationPointRef {
            target: lane.target.clone(), param_id: lane.param_id.clone(),
            beat: lane.dots[dot].beat, value_norm: lane.dots[dot].value_norm,
        }
    };
    for (shift, expected) in [(true, 2), (false, 1)] {
        let mut panel = build_viewport_with_automation();
        let mut host = GestureTestHost::new(&["layer-0"]);
        let mut state = UIState::new();
        state.selected_automation_points = vec![lane_ref(&panel, 0)];
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay.set_modifiers(Modifiers { shift, ..Modifiers::NONE });
        let strip = panel.automation_lane_screens()[0].curve_rect();
        // Rectangle around the beat-8 dot only.
        let press = Vec2::new(panel.beat_to_pixel(Beats(6.0)), strip.y_max() - 1.0);
        overlay.on_begin_drag(press, &mut host, &mut state, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::AutomationMarquee);
        let end = Vec2::new(panel.beat_to_pixel(Beats(10.0)), strip.y + 1.0);
        overlay.on_drag(end, &mut host, &mut state, &mut panel);
        assert_eq!(state.selected_automation_points.len(), expected, "shift={shift}");
        assert!(state.selected_automation_points.contains(&lane_ref(&panel, 1)));
        assert_eq!(state.selected_automation_points.contains(&lane_ref(&panel, 0)), shift);
    }
}

/// A value-only drag of a point parked in the edge zone must not scroll
/// the view; scrolling starts once the drag has moved in time.
#[test]
fn vertical_point_drag_near_the_edge_does_not_autoscroll() {
    let mut panel = build_viewport_with_automation();
    let rect = panel.tracks_rect();
    // Put the beat-8 point 5 px inside the right edge.
    let dx = panel.beat_to_pixel(Beats(8.0)) - rect.x;
    panel.set_zoom(panel.pixels_per_beat() * (rect.width - 5.0) / dx);
    let press = dot_pos(&panel, 1);
    assert!(press.x > rect.x_max() - 32.0 && press.x < rect.x_max(), "setup: dot in the edge zone");
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint);

    let scroll = panel.scroll_x_beats();
    let up = Vec2::new(press.x + 1.0, press.y - 20.0);
    overlay.on_drag(up, &mut host, &mut state, &mut panel);
    for _ in 0..8 {
        overlay.poll_drag(up, &mut host, &mut state, &mut panel);
    }
    assert_eq!(panel.scroll_x_beats(), scroll, "a vertical drag must not scroll");
    assert_ne!(host.automation_lane_preview[1].1, 0.8, "the value still follows the pointer");

    let right = Vec2::new(press.x + 10.0, press.y - 20.0);
    for _ in 0..8 {
        overlay.poll_drag(right, &mut host, &mut state, &mut panel);
    }
    assert!(panel.scroll_x_beats() > scroll, "a drag that moves in time scrolls at the edge");
}

/// Edge autoscroll must advance a parked automation point drag through
/// `poll_drag`, with the dragged point following the scrolled view.
#[test]
fn automation_point_drag_autoscrolls_from_poll_drag() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    // The last point has no right neighbour to clamp it, so it follows the view.
    let press = dot_pos(&panel, 1);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint);
    assert!(InteractionOverlay::autoscrolls_at_edge(overlay.drag_mode()));

    let edge = Vec2::new(panel.tracks_rect().x_max() - 5.0, press.y);
    overlay.poll_drag(edge, &mut host, &mut state, &mut panel);
    let first_scroll = panel.scroll_x_beats();
    let moved_beat = |host: &GestureTestHost| host.automation_lane_preview.iter().map(|p| p.0).fold(Beats::ZERO, |a, b| a.max(b));
    let first_beat = moved_beat(&host);
    for _ in 0..8 {
        overlay.poll_drag(edge, &mut host, &mut state, &mut panel);
    }
    assert!(panel.scroll_x_beats() > first_scroll, "view must keep scrolling while the pointer is parked");
    assert!(moved_beat(&host) > first_beat, "the dragged point must follow the scrolling view");
}

/// The marquee corner stays on the beat it was pressed on while the view
/// scrolls under a parked pointer, so the range grows rather than slides.
#[test]
fn automation_marquee_autoscroll_keeps_its_press_corner() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let strip = panel.automation_lane_screens()[0].curve_rect();
    let press = Vec2::new(panel.beat_to_pixel(Beats(2.0)), strip.y_max() - 1.0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationMarquee);

    let edge = Vec2::new(panel.tracks_rect().x_max() - 5.0, strip.y_max() - 1.0);
    overlay.poll_drag(edge, &mut host, &mut state, &mut panel);
    let first_end = state.automation_time_selection.as_ref().unwrap().end;
    let scroll = panel.scroll_x_beats();
    for _ in 0..8 {
        overlay.poll_drag(edge, &mut host, &mut state, &mut panel);
    }
    let range = state.automation_time_selection.as_ref().unwrap();
    assert!(panel.scroll_x_beats() > scroll, "view must keep scrolling");
    assert_eq!(range.start, Beats(2.0), "press corner stays on its beat");
    assert!(range.end > first_end, "range must grow as the view scrolls");
}

/// Hovering across a lane changes the feedback readout on every move; that
/// must only ask for a repaint, never a full rebuild.
#[test]
fn pointer_move_across_lane_never_forces_rebuild() {
    let panel = build_viewport_with_automation();
    let lane = panel.automation_lane_screens()[0].clone();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let curve = lane.curve_rect();
    for step in 0..40 {
        let x = curve.x + 4.0 + step as f32 * 9.0;
        let pos = Vec2::new(x, curve.y + curve.height * (step as f32 / 40.0));
        overlay.on_pointer_move(pos, &mut host, &mut state, &panel);
        let lanes = panel.automation_lane_screens();
        overlay.refresh_automation_feedback(pos, lanes, &mut state, &panel);
    }
    overlay.on_pointer_move(Vec2::new(curve.x + 20.0, panel.tracks_rect().y_max() + 50.0), &mut host, &mut state, &panel);
    assert!(host.redraw_requests >= 40);
}

/// A curve click that snaps onto an existing breakpoint's beat (but lands
/// outside its dot) selects that point; it never drafts or inserts one.
#[test]
fn segment_click_snapping_onto_point_selects_it() {
    let panel = build_viewport_with_automation();
    let lane = panel.automation_lane_screens()[0].clone();
    let dot = lane.dots[0];
    let step_px = panel.grid_step() * panel.pixels_per_beat();
    let offset = automation_hit_tester::DOT_HIT_RADIUS_PX + 3.0;
    assert!(offset < step_px * 0.5, "grid step {step_px}px too fine for this fixture");
    let x = dot.x + offset;
    let beat = panel.pixel_to_beat(x).as_f32();
    let norm = 0.5 + 0.3 * (beat - 4.0) / 4.0;
    let pos = Vec2::new(x, lane.y_at_norm(norm));
    assert_eq!(panel.snap_to_grid(panel.pixel_to_beat(x)), dot.beat);
    assert!(matches!(
        automation_hit_tester::hit_test_automation(pos, panel.automation_lane_screens()),
        Some(AutomationHit::Segment { .. })
    ));

    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_lane_exists = true;
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    assert!(!overlay.has_pending_automation_press());
    assert!(host.automation_lane_preview.is_empty());
    let selected = state.selected_automation_point.clone().expect("point selected on press");
    assert_eq!((selected.beat, selected.value_norm), (dot.beat, dot.value_norm));

    let mut state = UIState::new();
    assert!(overlay.handle_automation_click(pos, 1, &mut host, &mut state, &panel));
    assert!(host.automation_added.is_empty());
    let selected = state.selected_automation_point.expect("point selected on click");
    assert_eq!((selected.beat, selected.value_norm), (dot.beat, dot.value_norm));
}

#[test]
fn automation_feedback_matches_snapped_insertion_and_protects_chrome() {
    let panel = build_viewport_with_automation();
    let lanes = panel.automation_lane_screens();
    let lane = &lanes[0];
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let pos = Vec2::new(lane.curve_rect().x + 33.7, lane.y_at_norm(0.1));
    assert!(panel.tracks_rect().contains(pos));
    overlay.refresh_automation_feedback(pos, lanes, &mut state, &panel);
    let feedback = state.automation_feedback.clone().unwrap();
    assert_eq!(feedback.operation, AutomationOperation::Insert);
    assert!(feedback.hint.starts_with("Click: add point"));
    assert_eq!(feedback.beat, panel.snap_to_grid(panel.pixel_to_beat(pos.x)));
    overlay.handle_automation_click(pos, 1, &mut host, &mut state, &panel);
    assert_eq!(host.automation_added, vec![(feedback.beat, feedback.value)]);
    // Title text over the line is the line's (it stays editable); chrome
    // is the bare space of the title rect, clear of the line.
    let t = lane.title_rect;
    let chrome = [t.y + 1.0, t.y_max() - 1.0]
        .into_iter()
        .map(|y| Vec2::new(t.x + t.width * 0.5, y))
        .find(|p| !automation_hit_tester::hit_test_automation(*p, lanes).is_some_and(|h| h.is_line_or_point()))
        .expect("a title point clear of the line");
    overlay.refresh_automation_feedback(chrome, lanes, &mut state, &panel);
    assert_eq!(state.automation_feedback.as_ref().unwrap().operation, AutomationOperation::Title);
    overlay.handle_automation_click(chrome, 2, &mut host, &mut state, &panel);
    overlay.on_begin_drag(chrome, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::None);
    assert_eq!(host.automation_added.len(), 1);
}

#[test]
fn automation_feedback_locks_drag_and_reports_committed_point() {
    let mut panel = build_viewport_with_automation();
    let lanes = panel.automation_lane_screens().to_vec();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0) + Vec2::new(2.0, 2.0);
    overlay.refresh_automation_feedback(press, &lanes, &mut state, &panel);
    assert_eq!(state.automation_feedback.as_ref().unwrap().position, dot_pos(&panel, 0));
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.set_modifiers(Modifiers { alt: true, shift: true, ..Modifiers::NONE });
    let pos = press + Vec2::new(15.0, -70.0);
    overlay.on_drag(pos, &mut host, &mut state, &mut panel);
    overlay.refresh_automation_feedback(pos, &lanes, &mut state, &panel);
    let feedback = state.automation_feedback.clone().unwrap();
    assert!(feedback.active);
    assert_eq!(feedback.operation, AutomationOperation::Point);
    assert_ne!(feedback.position, pos);
    overlay.on_end_drag(&mut host);
    let new = host.automation_point_moves[0].3;
    assert_eq!((feedback.beat, feedback.value), (new.0, new.1));
    overlay.refresh_automation_feedback(Vec2::new(-100.0, -100.0), &lanes, &mut state, &panel);
    assert!(state.automation_feedback.is_none());
}

#[test]
fn automation_point_drag_begin_track_commit() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint, "grabbing an existing dot must begin a point drag");

    // Drag it up (higher value) and a bit right (later beat).
    overlay.on_drag(press + Vec2::new(20.0, -30.0), &mut host, &mut ui_state, &mut panel);
    overlay.on_end_drag(&mut host);

    assert_eq!(overlay.drag_mode(), DragMode::None, "drag must end");
    assert_eq!(host.automation_point_moves.len(), 1, "exactly one commit for a moved point");
    let (_, _, old, new) = &host.automation_point_moves[0];
    assert_eq!(old.0, Beats::from_f32(4.0), "old beat must be the grabbed point's original beat");
    assert!(new.1 > old.1, "dragging up must raise the value");
    assert_eq!(ui_state.selected_automation_point.as_ref().unwrap().beat, new.0);
    assert!(ui_state.automation_point_selected(&host.automation_point_moves[0].0, &host.automation_point_moves[0].1, new.0, new.1));
}

#[test]
fn automation_drag_commits_exactly_the_last_previewed_point() {
    let (mut panel, mut host) = three_point_lane([0.5, 0.8, 0.3]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.on_drag(press + Vec2::new(37.0, -41.3), &mut host, &mut state, &mut panel);
    overlay.modifiers.shift = true;
    overlay.on_drag(press + Vec2::new(61.0, -77.7), &mut host, &mut state, &mut panel);
    let previewed = host
        .automation_lane_preview
        .iter()
        .copied()
        .find(|p| (p.0, p.1) != (Beats(8.0), 0.8) && (p.0, p.1) != (Beats(12.0), 0.3))
        .expect("the dragged point is in the preview");
    overlay.on_end_drag(&mut host);
    let new = host.automation_point_moves[0].3;
    assert_eq!((new.0.0.to_bits(), new.1.to_bits()), (previewed.0.0.to_bits(), previewed.1.to_bits()));
}

#[test]
fn automation_point_cancel_restores_full_lane_and_clears_once() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    let collision = dot_pos(&panel, 1);
    overlay.on_drag(collision, &mut host, &mut state, &mut panel);
    overlay.cancel_drag(&mut host);
    assert_eq!(host.automation_preview_clears, 1);
    assert_eq!(host.automation_lane_restores.len(), 1);
    assert_eq!(host.automation_lane_restores[0].points.as_ref().unwrap(),
        &vec![(Beats(4.0), 0.5, UiSegmentShape::Linear), (Beats(8.0), 0.8, UiSegmentShape::Linear)]);
    assert!(host.automation_point_moves.is_empty());
}

#[test]
fn automation_draw_cancel_restores_absent_lane() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_lane_exists = false;
    let mut state = UIState::new();
    state.automation_draw_mode = true;
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let strip = panel.automation_lane_screens()[0].curve_rect();
    let press = Vec2::new(strip.x + 10.0, strip.y + strip.height * 0.5);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.cancel_drag(&mut host);
    assert_eq!(host.automation_lane_restores.len(), 1);
    assert!(host.automation_lane_restores[0].points.is_none());
    assert_eq!(host.automation_preview_clears, 1);
    assert!(host.automation_draw_commits.is_empty());
}

#[test]
fn automation_point_noop_release_clears_once_without_commit() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.on_end_drag(&mut host);
    assert_eq!(host.automation_preview_clears, 1);
    assert!(host.automation_point_moves.is_empty());
}

#[test]
fn automation_group_cancel_restores_each_lane_once() {
    let panel = build_viewport_with_automation();
    let mut lanes = panel.automation_lane_screens().to_vec();
    let mut second = lanes[0].clone();
    second.param_id = "second_param".into();
    lanes.push(second);
    let mut state = UIState::new();
    state.selected_automation_points = lanes.iter().flat_map(|lane| {
        lane.dots.iter().map(|dot| UiAutomationPointRef {
            target: lane.target.clone(), param_id: lane.param_id.clone(), beat: dot.beat, value_norm: dot.value_norm,
        })
    }).collect();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.begin_automation_group_drag(dot_pos(&panel, 0), &lanes, &state, &mut host, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationGroupMove);
    overlay.cancel_drag(&mut host);
    assert_eq!(host.automation_lane_restores.len(), 2);
    assert_eq!(host.automation_lane_restores[0].param_id, lanes[0].param_id);
    assert_eq!(host.automation_lane_restores[1].param_id, lanes[1].param_id);
    for restored in &host.automation_lane_restores {
        assert_eq!(restored.points.as_ref().unwrap(),
            &vec![(Beats(4.0), 0.5, UiSegmentShape::Linear), (Beats(8.0), 0.8, UiSegmentShape::Linear)]);
    }
    assert_eq!(host.automation_preview_clears, 1);
    assert!(host.automation_group_move_commits.is_empty());
    assert!(overlay.automation_lane_snapshots.is_empty());
}

#[test]
fn automation_visible_segment_keeps_both_offscreen_endpoints() {
    let mut panel = build_viewport_with_automation();
    panel.set_zoom(1200.0);
    panel.set_scroll(5.0, 0.0);
    let lanes = panel.automation_lane_screens();
    let lane = &lanes[0];
    assert_eq!(lane.dots.len(), 2);
    assert!(lane.dots[0].x < lane.strip_rect.x);
    assert!(lane.dots[1].x > lane.strip_rect.x_max());
    let x = lane.strip_rect.x + lane.strip_rect.width * 0.5;
    let t = (x - lane.dots[0].x) / (lane.dots[1].x - lane.dots[0].x);
    let y = lane.y_at_norm(0.5 + 0.3 * t);
    assert_eq!(automation_hit_tester::hit_test_automation(Vec2::new(x, y), lanes),
        Some(AutomationHit::Segment { lane_index: 0, left_dot_index: 0 }));
}

#[test]
fn automation_point_preview_restores_crossed_points_and_preserves_grab_offset() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0) + Vec2::new(2.0, 2.0);
    overlay.modifiers.command = true;
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.on_drag(press, &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview[0], (Beats(4.0), 0.5, UiSegmentShape::Linear));
    overlay.modifiers.command = false;
    let collision = Vec2::new(panel.beat_to_pixel(Beats(8.0)) + 2.0, press.y);
    overlay.on_drag(collision, &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview, vec![
        (Beats(8.0), 0.5, UiSegmentShape::Linear),
        (Beats(8.0), 0.8, UiSegmentShape::Linear),
    ]);
    // Past the next neighbour the point stops at it instead of crossing.
    let past = Vec2::new(panel.beat_to_pixel(Beats(12.0)) + 2.0, press.y);
    overlay.on_drag(past, &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview, vec![
        (Beats(8.0), 0.5, UiSegmentShape::Linear),
        (Beats(8.0), 0.8, UiSegmentShape::Linear),
    ]);
    overlay.on_end_drag(&mut host);
    assert_eq!(state.selected_automation_point.unwrap().beat, Beats(8.0));
}

fn three_point_lane(values: [f32; 3]) -> (TimelineViewportPanel, GestureTestHost) {
    use crate::view::UiAutomationPoint;
    let beats = [4.0, 8.0, 12.0];
    let panel = build_viewport_with_lane(
        beats.iter().zip(values).map(|(&b, v)| UiAutomationPoint {
            beat: Beats(b), value_norm: v, shape: UiSegmentShape::Linear, past_end: false,
        }).collect(),
        false,
    );
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_original_points = Some(
        beats.iter().zip(values).map(|(&b, v)| (Beats(b), v, UiSegmentShape::Linear)).collect(),
    );
    (panel, host)
}

#[test]
fn automation_point_drag_clamps_to_neighbours_both_ways_and_keeps_all_points() {
    let (mut panel, mut host) = three_point_lane([0.5, 0.8, 0.3]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 1);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.on_drag(Vec2::new(panel.beat_to_pixel(Beats(20.0)), press.y), &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview.iter().map(|p| p.0).collect::<Vec<_>>(),
        vec![Beats(4.0), Beats(12.0), Beats(12.0)]);
    assert_eq!(host.automation_lane_preview[1].1, 0.8, "the dragged point stays before its neighbour");
    overlay.on_drag(Vec2::new(panel.beat_to_pixel(Beats(0.0)), press.y), &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview.iter().map(|p| p.0).collect::<Vec<_>>(),
        vec![Beats(4.0), Beats(4.0), Beats(12.0)]);
    assert_eq!(host.automation_lane_preview[1].1, 0.8);
    overlay.on_end_drag(&mut host);
    assert_eq!(host.automation_point_moves[0].3.0, Beats(4.0));
}

#[test]
fn automation_point_dropped_on_exact_twin_deletes_nothing() {
    let (mut panel, mut host) = three_point_lane([0.5, 0.5, 0.3]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.on_drag(Vec2::new(panel.beat_to_pixel(Beats(8.0)), press.y), &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview, vec![
        (Beats(8.0), 0.5, UiSegmentShape::Linear),
        (Beats(8.0), 0.5, UiSegmentShape::Linear),
        (Beats(12.0), 0.3, UiSegmentShape::Linear),
    ]);
}

#[test]
fn automation_point_vertical_drag_keeps_off_grid_beat_until_it_moves_a_whole_step() {
    use crate::view::UiAutomationPoint;
    let off_grid = Beats(4.3);
    let build = || {
        let panel = build_viewport_with_lane(vec![
            UiAutomationPoint { beat: off_grid, value_norm: 0.5, shape: UiSegmentShape::Linear, past_end: false },
            UiAutomationPoint { beat: Beats(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false },
        ], false);
        let mut host = GestureTestHost::new(&["layer-0"]);
        host.automation_original_points = Some(vec![
            (off_grid, 0.5, UiSegmentShape::Linear), (Beats(8.0), 0.8, UiSegmentShape::Linear),
        ]);
        (panel, host)
    };

    // A wobble under half a grid cell: only the value moves, the beat is exact.
    let (mut panel, mut host) = build();
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    let cell_px = panel.grid_step() * panel.pixels_per_beat();
    assert!(cell_px > 6.0, "setup: a 3 px wobble is under half a cell");
    overlay.on_drag(press + Vec2::new(3.0, -30.0), &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    let (_, _, old, new) = &host.automation_point_moves[0];
    assert_eq!(new.0, off_grid);
    assert!(new.1 > old.1);

    // One whole grid step right: the point moves one step and keeps its offset.
    let (mut panel, mut host) = build();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    overlay.on_drag(press + Vec2::new(cell_px, -30.0), &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    let new = host.automation_point_moves[0].3;
    assert!((new.0.0 - (off_grid.0 + f64::from(panel.grid_step()))).abs() < 1e-5, "{:?}", new.0);
    assert_ne!(new.0, panel.snap_to_grid(new.0), "the off-grid offset is kept");
}

#[test]
fn automation_group_vertical_drag_keeps_off_grid_beats_until_horizontal_travel() {
    use crate::view::UiAutomationPoint;
    let off_grid = Beats(4.3);
    let mut panel = build_viewport_with_lane(vec![
        UiAutomationPoint { beat: off_grid, value_norm: 0.5, shape: UiSegmentShape::Linear, past_end: false },
        UiAutomationPoint { beat: Beats(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false },
    ], false);
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_original_points = Some(vec![
        (off_grid, 0.5, UiSegmentShape::Linear), (Beats(8.0), 0.8, UiSegmentShape::Linear),
    ]);
    let lanes = panel.automation_lane_screens();
    let mut state = UIState::new();
    state.selected_automation_points = lanes[0].dots.iter().map(|dot| UiAutomationPointRef {
        target: lanes[0].target.clone(), param_id: lanes[0].param_id.clone(),
        beat: dot.beat, value_norm: dot.value_norm,
    }).collect();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationGroupMove);
    overlay.on_drag(press + Vec2::new(2.0, -20.0), &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    let moves = &host.automation_group_move_commits[0];
    assert_eq!(moves[0].new.0, off_grid);
    assert_eq!(moves[1].new.0, Beats(8.0));
    assert!(moves[0].new.1 > moves[0].old.1);
}

#[test]
fn automation_segment_vertical_drag_rounds_on_stepped_lanes() {
    use crate::view::UiAutomationPoint;
    let mut panel = build_viewport_with_lane_range(vec![
        UiAutomationPoint { beat: Beats(4.0), value_norm: 0.2, shape: UiSegmentShape::Hold, past_end: false },
        UiAutomationPoint { beat: Beats(8.0), value_norm: 0.5, shape: UiSegmentShape::Hold, past_end: false },
    ], false, 10.0, true);
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_original_points = Some(vec![
        (Beats(4.0), 2.0, UiSegmentShape::Hold), (Beats(8.0), 5.0, UiSegmentShape::Hold),
    ]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lanes = panel.automation_lane_screens();
    let (d0, d1) = (lanes[0].dots[0], lanes[0].dots[1]);
    let press = Vec2::new((d0.x + d1.x) / 2.0, d0.y);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationSegmentDrag);
    // A drag worth 0.37 of the range: unrounded it would land between steps.
    let height = lanes[0].curve_rect().height;
    overlay.on_drag(press + Vec2::new(0.0, -height * 0.37), &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    let (_, _, left, right) = &host.automation_segment_drag_commits[0];
    assert_ne!(left.2, left.1, "the segment moved");
    assert_eq!(left.2.fract(), 0.0);
    assert_eq!(right.2.fract(), 0.0);
}

#[test]
fn automation_point_drag_moves_only_the_selected_raw_value_at_a_coincident_beat() {
    let mut panel = build_viewport_with_coincident_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_original_points = Some(vec![
        (Beats(4.0), 37.25, UiSegmentShape::Linear),
        (Beats(4.0), 113.5, UiSegmentShape::Linear),
    ]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint);

    // The first point has no earlier neighbour, so it can move left of
    // its coincident twin (rightwards it would be stopped by it).
    let destination = Vec2::new(panel.beat_to_pixel(Beats(2.0)), press.y);
    overlay.on_drag(destination, &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview.len(), 2);
    assert_eq!(host.automation_lane_preview[0].0, Beats(2.0));
    assert!((host.automation_lane_preview[0].1 - 37.25).abs() < 1e-3);
    assert_eq!(host.automation_lane_preview[1].0, Beats(4.0));
    assert_eq!(host.automation_lane_preview[1].1, 113.5);

    overlay.on_end_drag(&mut host);
    assert_eq!(host.automation_point_moves.len(), 1);
    let (_, _, old, new) = &host.automation_point_moves[0];
    assert_eq!(old.0, Beats(4.0));
    assert_eq!(old.1, 37.25);
    assert_eq!(new.0, Beats(2.0));
    assert_eq!(new.1, 37.25);
}

#[test]
fn automation_group_drag_preserves_order_for_coincident_raw_values() {
    let mut panel = build_viewport_with_coincident_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_original_points = Some(vec![
        (Beats(4.0), 37.25, UiSegmentShape::Linear),
        (Beats(4.0), 113.5, UiSegmentShape::Linear),
    ]);
    let mut state = UIState::new();
    let lanes = panel.automation_lane_screens();
    state.selected_automation_points = lanes[0]
        .dots
        .iter()
        .map(|dot| UiAutomationPointRef {
            target: lanes[0].target.clone(),
            param_id: lanes[0].param_id.clone(),
            beat: dot.beat,
            value_norm: dot.value_norm,
        })
        .collect();

    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationGroupMove);
    let destination = Vec2::new(panel.beat_to_pixel(Beats(8.0)), press.y);
    overlay.on_drag(destination, &mut host, &mut state, &mut panel);
    assert_eq!(host.automation_lane_preview.len(), 2);
    assert_eq!(host.automation_lane_preview[0].0, Beats(8.0));
    assert!((host.automation_lane_preview[0].1 - 37.25).abs() < 1e-3);
    assert_eq!(host.automation_lane_preview[1].0, Beats(8.0));
    assert_eq!(host.automation_lane_preview[1].1, 113.5);

    overlay.on_end_drag(&mut host);
    assert_eq!(host.automation_group_move_commits.len(), 1);
    assert_eq!(host.automation_group_move_commits[0].len(), 2);
    assert_eq!(host.automation_group_move_commits[0][0].old.1, 37.25);
    assert_eq!(host.automation_group_move_commits[0][1].old.1, 113.5);
}

#[test]
fn moved_point_preview_preserves_equal_beat_order_in_both_directions() {
    let mut right = vec![
        (Beats(0.0), 0.1, UiSegmentShape::Linear),
        (Beats(4.0), 0.4, UiSegmentShape::Linear),
        (Beats(4.0), 0.5, UiSegmentShape::Linear),
    ];
    insert_moved_point(&mut right, Beats(0.0), 0.1, Beats(4.0), 0.9, UiSegmentShape::Linear);
    assert_eq!(right.iter().map(|p| (p.0, p.1)).collect::<Vec<_>>(), vec![
        (Beats(4.0), 0.9), (Beats(4.0), 0.4), (Beats(4.0), 0.5),
    ]);

    let mut left = vec![
        (Beats(4.0), 0.4, UiSegmentShape::Linear),
        (Beats(4.0), 0.5, UiSegmentShape::Linear),
        (Beats(8.0), 0.9, UiSegmentShape::Linear),
    ];
    insert_moved_point(&mut left, Beats(8.0), 0.9, Beats(4.0), 0.8, UiSegmentShape::Linear);
    assert_eq!(left.iter().map(|p| (p.0, p.1)).collect::<Vec<_>>(), vec![
        (Beats(4.0), 0.4), (Beats(4.0), 0.5), (Beats(4.0), 0.8),
    ]);
}

#[test]
fn automation_point_shift_drag_is_fine_and_empty_click_only_deselects() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.modifiers.shift = true;
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    let quarter_height = panel.automation_lane_screens()[0].curve_rect().height * 0.25;
    overlay.on_drag(press + Vec2::new(0.0, -quarter_height), &mut host, &mut state, &mut panel);
    assert!((host.automation_lane_preview[0].1 - 0.5625).abs() < 0.00001);
    overlay.on_end_drag(&mut host);
    let lane = &panel.automation_lane_screens()[0];
    let empty = Vec2::new(panel.beat_to_pixel(Beats(12.0)), lane.curve_rect().y + 2.0);
    overlay.on_pointer_click(empty, false, false, 1, false, &mut host, &mut state, &panel);
    assert!(state.selected_automation_point.is_none());
}

/// a right-click on an automation lane's dot opens the lane's
/// context menu (via `on_automation_lane_right_click`) instead of falling
/// through to `handle_automation_click`'s point-delete/select logic —
/// the panel/dispatch layer (BUG_BACKLOG.md's fix note) is only reachable
/// if this routing decision is made correctly first.
#[test]
fn right_click_on_automation_dot_opens_lane_context_menu_not_point_logic() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let pos = dot_pos(&panel, 0);
    overlay.on_pointer_click(pos, false, false, 1, true, &mut host, &mut ui_state, &panel);

    assert_eq!(host.automation_lane_right_clicks.len(), 1, "exactly one lane right-click recorded");
    assert_eq!(ui_state.selected_automation_point.as_ref().unwrap().beat, Beats(4.0),
        "the context menu must address the point that was right-clicked");
    assert!(host.automation_added.is_empty());
}

#[test]
fn automation_context_menu_preserves_selection_across_lanes() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let point = UiAutomationPointRef {
        target: lane.target.clone(), param_id: lane.param_id.clone(),
        beat: lane.dots[0].beat, value_norm: lane.dots[0].value_norm,
    };
    let other = UiAutomationPointRef { param_id: "other-param".into(), ..point.clone() };
    state.selected_automation_points = vec![point.clone(), other.clone()];
    state.automation_time_selection = Some(crate::ui_state::AutomationTimeSelection {
        start: Beats(3.0), end: Beats(7.0),
        lanes: vec![(point.target.clone(), point.param_id.clone()), (other.target.clone(), other.param_id.clone())],
    });
    overlay.on_pointer_click(dot_pos(&panel, 0), false, false, 1, true, &mut host, &mut state, &panel);
    assert_eq!(state.selected_automation_points.len(), 2);
    assert_eq!(state.automation_time_selection.as_ref().unwrap().lanes.len(), 2);

    overlay.on_pointer_click(dot_pos(&panel, 1), false, false, 1, true, &mut host, &mut state, &panel);
    assert!(state.automation_time_selection.is_none());
    assert_eq!(state.selected_automation_point.as_ref().unwrap().beat, lane.dots[1].beat);
}

#[test]
fn automation_marquee_drag_selects_points_and_clears_on_end() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    // Press on bare strip (not on a dot) to begin a marquee — top-left
    // corner, so the rect built by dragging to bottom-right spans both
    // dots' differing Y (value_norm 0.5 vs 0.8), not just their X.
    let lanes = panel.automation_lane_screens();
    let strip = lanes[0].curve_rect();
    let press = Vec2::new(strip.x + 2.0, strip.y + 1.0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationMarquee);

    // Drag across both dots, full strip height.
    let far = Vec2::new(panel.tracks_rect().x_max() - 2.0, strip.y + strip.height - 1.0);
    overlay.on_drag(far, &mut host, &mut ui_state, &mut panel);
    assert_eq!(
        ui_state.selected_automation_points.len(),
        2,
        "marquee spanning both dots must select both"
    );

    overlay.on_end_drag(&mut host);
    assert_eq!(overlay.drag_mode(), DragMode::None);
    // Selection itself isn't cleared by end-drag (it's UIState's, written
    // live) — only the drag lifecycle ends.
    assert_eq!(ui_state.selected_automation_points.len(), 2);
}

#[test]
fn automation_time_selection_persists_without_interior_points_and_reverses() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let y = panel.automation_lane_screens()[0].curve_rect().y_max() - 1.0;
    let start = Vec2::new(panel.beat_to_pixel(Beats(5.0)), y);
    let end = Vec2::new(panel.beat_to_pixel(Beats(7.0)), y);
    overlay.on_pointer_down(start, &mut host, &mut state, &panel);
    assert!(overlay.has_pending_automation_press());
    overlay.on_begin_drag(start, &mut host, &mut state, &panel);
    overlay.on_drag(end, &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    let range = state.automation_time_selection.as_ref().unwrap();
    assert_eq!((range.start, range.end), (Beats(5.0), Beats(7.0)));
    assert_eq!(range.lanes.len(), 1);
    assert!(state.selected_automation_points.is_empty());
    assert!(host.automation_added.is_empty());

    let start = Vec2::new(panel.beat_to_pixel(Beats(9.0)), y);
    let end = Vec2::new(panel.beat_to_pixel(Beats(3.0)), y);
    overlay.on_begin_drag(start, &mut host, &mut state, &panel);
    overlay.on_drag(end, &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    assert_eq!(state.selected_automation_points.len(), 2, "selection is independent of point height");
    let range = state.automation_time_selection.as_ref().unwrap();
    assert_eq!((range.start, range.end), (Beats(3.0), Beats(9.0)));
}

#[test]
fn automation_drag_from_any_height_in_the_lane_selects_time() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = panel.automation_lane_screens()[0].clone();
    let strip = lane.strip_rect;
    // Clear of the resize grip at the left and the title at the right.
    let x0 = lane.resize_rect().x_max() + 20.0;
    let x1 = x0 + 120.0;
    assert!(x1 < lane.title_rect.x);
    let mut off_line = 0;
    for y in [strip.y + 0.5, strip.y + strip.height * 0.5, strip.y_max() - 0.5] {
        let mut state = UIState::new();
        let (start, end) = (Vec2::new(x0, y), Vec2::new(x1, y));
        // On the line the press grabs the line (a tail drag), not time.
        if automation_hit_tester::hit_test_automation(start, panel.automation_lane_screens())
            .is_some_and(|h| h.is_line_or_point())
        {
            continue;
        }
        off_line += 1;
        overlay.on_pointer_down(start, &mut host, &mut state, &panel);
        overlay.on_begin_drag(start, &mut host, &mut state, &panel);
        overlay.on_drag(end, &mut host, &mut state, &mut panel);
        overlay.on_end_drag(&mut host);
        let range = state.automation_time_selection.as_ref()
            .unwrap_or_else(|| panic!("a drag at y {y} made no time selection"));
        assert!(range.start < range.end, "y {y}");
    }
    assert!(off_line >= 2, "the strip has bare height above and below the line");
    assert!(host.automation_added.is_empty());
}

/// The automation fixture zoomed out so beats 0..~25 are on screen.
fn zoomed_out_automation_panel() -> TimelineViewportPanel {
    let mut panel = build_viewport_with_automation();
    panel.set_zoom(40.0);
    panel
}

/// The x of a beat inside the lane's strip, snapped exactly.
fn beat_x(panel: &TimelineViewportPanel, beat: f32) -> f32 {
    panel.beat_to_pixel(Beats(beat as f64))
}

#[test]
fn click_past_the_last_point_inserts_the_lines_value_not_the_pointers_height() {
    let panel = zoomed_out_automation_panel();
    let lane = &panel.automation_lane_screens()[0];
    // 3 beats past the last point (beat 8, value 0.8), pointer at a
    // height far from the line.
    let pos = Vec2::new(beat_x(&panel, 11.0), lane.y_at_norm(0.8) - SEGMENT_PX_OFF);
    assert!(matches!(
        automation_hit_tester::hit_test_automation(pos, panel.automation_lane_screens()),
        Some(AutomationHit::Tail { dot_index: Some(1), .. })
    ), "the pointer is on the tail");
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_lane_exists = true;
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    assert!(overlay.has_pending_automation_press());
    overlay.on_pointer_up(&mut host, &mut state);
    assert_eq!(host.automation_added, vec![(Beats(11.0), 0.8)], "exactly the line's value");
    let selected = state.selected_automation_point.expect("the made point is selected");
    assert_eq!((selected.beat, selected.value_norm), (Beats(11.0), 0.8));
}

/// Pixels off the line that still count as on it.
const SEGMENT_PX_OFF: f32 = 3.0;

#[test]
fn click_before_the_first_point_inserts_the_first_value() {
    let panel = zoomed_out_automation_panel();
    let lane = &panel.automation_lane_screens()[0];
    let pos = Vec2::new(beat_x(&panel, 2.0), lane.y_at_norm(0.5));
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    assert!(overlay.handle_automation_click(pos, 1, &mut host, &mut state, &panel));
    assert_eq!(host.automation_added, vec![(Beats(2.0), 0.5)]);
}

#[test]
fn hover_ghost_shows_the_value_the_click_will_insert() {
    let panel = zoomed_out_automation_panel();
    let lanes = panel.automation_lane_screens();
    let pos = Vec2::new(beat_x(&panel, 11.0), lanes[0].y_at_norm(0.8) - SEGMENT_PX_OFF);
    let mut state = UIState::new();
    let overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.refresh_automation_feedback(pos, lanes, &mut state, &panel);
    let feedback = state.automation_feedback.expect("feedback over the tail");
    assert_eq!(feedback.operation, AutomationOperation::Segment);
    assert_eq!((feedback.beat, feedback.value), (Beats(11.0), 0.8));
    assert_eq!(feedback.segment_end_beat, None, "a tail names no right endpoint");
}

#[test]
fn point_hover_hint_names_the_gestures() {
    let panel = build_viewport_with_automation();
    let mut state = UIState::new();
    let overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.refresh_automation_feedback(dot_pos(&panel, 0), panel.automation_lane_screens(), &mut state, &panel);
    assert_eq!(state.automation_feedback.unwrap().hint, "drag: move · click: delete · shift-click: select");
}

#[test]
fn placeholder_click_keeps_the_lanes_current_value() {
    let panel = build_viewport_with_lane(
        vec![crate::view::UiAutomationPoint { beat: Beats::ZERO, value_norm: 0.25, shape: UiSegmentShape::Linear, past_end: false }],
        true,
    );
    let lane = &panel.automation_lane_screens()[0];
    assert_eq!(lane.placeholder_value, Some(0.25));
    let pos = Vec2::new(beat_x(&panel, 5.0), lane.y_at_norm(0.25));
    assert!(matches!(
        automation_hit_tester::hit_test_automation(pos, panel.automation_lane_screens()),
        Some(AutomationHit::Tail { dot_index: None, .. })
    ));
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    overlay.on_pointer_up(&mut host, &mut state);
    assert_eq!(host.automation_added, vec![(Beats(5.0), 0.25)]);
}

#[test]
fn double_click_on_empty_space_ends_with_one_point() {
    let before = zoomed_out_automation_panel();
    let lane = &before.automation_lane_screens()[0];
    let pos = Vec2::new(beat_x(&before, 11.0), lane.y_at_norm(0.8));
    let mut host = GestureTestHost::new(&["layer-0"]);
    host.automation_lane_exists = true;
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    // First press, release and click make the point.
    overlay.on_pointer_down(pos, &mut host, &mut state, &before);
    overlay.on_pointer_up(&mut host, &mut state);
    overlay.on_pointer_click(pos, false, false, 1, false, &mut host, &mut state, &before);
    assert_eq!(host.automation_added.len(), 1);
    // The lane now has that point; the second press and clicks land on it.
    use crate::view::UiAutomationPoint;
    let mut after = build_viewport_with_lane(
        vec![
            UiAutomationPoint { beat: Beats::from_f32(4.0), value_norm: 0.5, shape: UiSegmentShape::Linear, past_end: false },
            UiAutomationPoint { beat: Beats::from_f32(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false },
            UiAutomationPoint { beat: Beats::from_f32(11.0), value_norm: 0.8, shape: UiSegmentShape::Hold, past_end: false },
        ],
        false,
    );
    after.set_zoom(40.0);
    let dot = after.automation_lane_screens()[0].dots[2];
    let on_dot = Vec2::new(dot.x, after.automation_lane_screens()[0].y_at_norm(dot.value_norm));
    overlay.on_pointer_down(on_dot, &mut host, &mut state, &after);
    overlay.on_pointer_up(&mut host, &mut state);
    overlay.on_pointer_click(on_dot, false, false, 1, false, &mut host, &mut state, &after);
    overlay.on_pointer_click(on_dot, false, false, 2, false, &mut host, &mut state, &after);
    assert_eq!(host.automation_added.len(), 1, "the double-click adds no second point");
    assert!(host.automation_removed.is_empty(), "and does not delete the one it made");
    assert_eq!(state.selected_automation_point.as_ref().map(|p| p.beat), Some(Beats(11.0)));
}

#[test]
fn click_on_an_old_point_still_deletes_it() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.handle_automation_click(dot_pos(&panel, 0), 1, &mut host, &mut state, &panel);
    assert_eq!(host.automation_removed.len(), 1);
}

#[test]
fn tail_drag_moves_the_end_point_up_and_down_with_its_beat_locked() {
    let mut panel = zoomed_out_automation_panel();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let press = Vec2::new(beat_x(&panel, 11.0), lane.y_at_norm(0.8));
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint, "a tail press drags the end point");
    // Well past the old dead zone sideways, and up.
    overlay.on_drag(press + Vec2::new(60.0, -30.0), &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);
    let (_, _, old, new) = &host.automation_point_moves[0];
    assert_eq!(old.0, Beats(8.0), "it grabbed the last point");
    assert_eq!(new.0, Beats(8.0), "the beat did not change");
    assert!(new.1 > old.1, "dragging up raised the value");
}

#[test]
fn placeholder_line_does_not_drag_the_point() {
    let mut panel = build_viewport_with_lane(
        vec![crate::view::UiAutomationPoint { beat: Beats::ZERO, value_norm: 0.25, shape: UiSegmentShape::Linear, past_end: false }],
        true,
    );
    let lane = &panel.automation_lane_screens()[0];
    let press = Vec2::new(beat_x(&panel, 5.0), lane.y_at_norm(0.25));
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationMarquee);
    overlay.on_end_drag(&mut host);
    assert!(host.automation_point_moves.is_empty());
    let _ = &mut panel;
}

#[test]
fn automation_cancelled_press_does_not_reinsert_on_release() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let pos = Vec2::new(panel.beat_to_pixel(Beats(6.0)), lane.curve_rect().y_max() - 1.0);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    overlay.cancel_drag(&mut host);
    overlay.on_pointer_click(pos, false, false, 1, false, &mut host, &mut state, &panel);
    overlay.on_pointer_click(pos, false, false, 2, false, &mut host, &mut state, &panel);
    overlay.on_pointer_up(&mut host, &mut state);
    assert!(!overlay.has_pending_automation_press());
    assert!(host.automation_added.is_empty());
    assert_eq!(host.automation_lane_restores.len(), 1);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    overlay.on_pointer_click(pos, false, false, 1, false, &mut host, &mut state, &panel);
    overlay.on_pointer_up(&mut host, &mut state);
    assert_eq!(host.automation_added.len(), 1, "next press must be independent of the cancelled gesture");
}

#[test]
fn automation_press_commits_on_release_without_click() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let pos = Vec2::new(panel.beat_to_pixel(Beats(6.0)), lane.curve_rect().y_max() - 1.0);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    overlay.on_pointer_up(&mut host, &mut state);
    assert_eq!(host.automation_added.len(), 1, "release alone commits the press");
    assert!(state.selected_automation_point.is_some());
    overlay.on_pointer_click(pos, false, false, 1, false, &mut host, &mut state, &panel);
    assert_eq!(host.automation_added.len(), 1, "a late click must not insert again");
}

#[test]
fn automation_first_click_on_empty_lane_uses_current_value() {
    use crate::view::UiAutomationPoint;
    let panel = build_viewport_with_lane(
        vec![UiAutomationPoint { beat: Beats::ZERO, value_norm: 0.25, shape: UiSegmentShape::Linear, past_end: false }],
        true,
    );
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    assert_eq!(lane.placeholder_value, Some(0.25));
    let pos = Vec2::new(panel.beat_to_pixel(Beats(6.0)), lane.curve_rect().y + 1.0);
    overlay.on_pointer_down(pos, &mut host, &mut state, &panel);
    overlay.on_pointer_click(pos, false, false, 1, false, &mut host, &mut state, &panel);
    overlay.on_pointer_up(&mut host, &mut state);
    assert_eq!(host.automation_added, vec![(Beats(6.0), 0.25)]);

    // The no-press fallback path lands on the same value.
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_pointer_click(pos, false, false, 1, false, &mut host, &mut state, &panel);
    assert_eq!(host.automation_added, vec![(Beats(6.0), 0.25)]);
}

#[test]
fn automation_marquee_cancel_ends_the_gesture() {
    let panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let lane = &panel.automation_lane_screens()[0];
    let pos = Vec2::new(panel.beat_to_pixel(Beats(6.0)), lane.curve_rect().y_max() - 1.0);
    overlay.on_begin_drag(pos, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationMarquee);
    overlay.cancel_drag(&mut host);
    assert_eq!(overlay.drag_mode(), DragMode::None);
    overlay.on_end_drag(&mut host);
    assert!(host.automation_added.is_empty());
}

#[test]
fn automation_group_move_drag_moves_every_selected_point() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();

    // Pre-select both points (mirrors a completed marquee) so the next
    // grab on a selected dot begins a GROUP move, not a single-point drag.
    let lanes = panel.automation_lane_screens();
    ui_state.selected_automation_points = lanes[0]
        .dots
        .iter()
        .map(|d| UiAutomationPointRef {
            target: lanes[0].target.clone(),
            param_id: lanes[0].param_id.clone(),
            beat: d.beat,
            value_norm: d.value_norm,
        })
        .collect();

    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationGroupMove, "grabbing a multi-selected dot must begin a group move");

    overlay.on_drag(press + Vec2::new(0.0, -20.0), &mut host, &mut ui_state, &mut panel);
    overlay.on_end_drag(&mut host);

    assert_eq!(overlay.drag_mode(), DragMode::None);
    assert_eq!(host.automation_group_move_commits.len(), 1, "one batched commit for the group move");
    assert_eq!(
        host.automation_group_move_commits[0].len(),
        2,
        "both selected points must move together"
    );
}

#[test]
fn automation_group_move_includes_points_on_scrolled_off_lanes() {
    use crate::panels::viewport::ViewportAutomationLane;
    use crate::view::{UiAutomationLane, UiAutomationPoint};
    use manifold_foundation::EffectId;

    // Four tracks (~360px each with one lane strip) in a ~756px viewport,
    // so scrolling to the bottom culls track 0 completely. Lanes live on
    // track 0 ("amount") and track 3 ("blend").
    let mut tree = UITree::new();
    let mut panel = TimelineViewportPanel::new();
    panel.set_tracks(vec![TrackInfo::default(); 4]);
    panel.set_clips(vec![]);
    let ui_layer = |name: &str, lanes: usize| UiLayer {
        layer_id: LayerId::new(name),
        parent_layer_id: None,
        layer_type: LayerType::Video,
        is_collapsed: false,
        automation_lane_count: lanes,
    };
    panel.rebuild_mapper_layout(&[
        ui_layer("layer-0", 1),
        ui_layer("layer-1", 0),
        ui_layer("layer-2", 0),
        ui_layer("layer-3", 1),
    ]);
    let lane = |effect: &str, param: &'static str, layer_index: usize| ViewportAutomationLane {
        layer_index,
        lane: UiAutomationLane {
            effect_id: EffectId::new(effect),
            param_id: ParamId::Borrowed(param),
            target: UiGraphTarget::Effect(EffectId::new(effect)),
            label: format!("{effect}: {param}"),
            points: vec![
                UiAutomationPoint { beat: Beats::from_f32(4.0), value_norm: 0.5, shape: UiSegmentShape::Linear, past_end: false },
                UiAutomationPoint { beat: Beats::from_f32(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false },
            ],
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
            placeholder: false,
            display: crate::view::UiLaneDisplay::default(),
        },
    };
    panel.set_automation_lanes(vec![
        lane("fx-top", "amount", 0),
        lane("fx-bottom", "blend", 3),
    ]);
    let mut layout = ScreenLayout::new(1920.0, 1080.0);
    layout.timeline_split_ratio = 0.70;
    panel.build(&mut tree, &layout);

    let mut host = GestureTestHost::new(&["layer-0", "layer-1", "layer-2", "layer-3"]);
    let mut state = UIState::new();

    // Marquee-select the beat-4 point on each lane (selection refs are
    // data — they don't need the lane on screen).
    state.selected_automation_points = ["fx-top", "fx-bottom"]
        .into_iter()
        .map(|effect| UiAutomationPointRef {
            target: UiGraphTarget::Effect(EffectId::new(effect)),
            param_id: ParamId::Borrowed(if effect == "fx-top" { "amount" } else { "blend" }),
            beat: Beats(4.0),
            value_norm: 0.5,
        })
        .collect();

    // Scroll to the bottom: track 0 (the "amount" lane) culls off the top.
    assert!(panel.set_scroll(0.0, 10_000.0), "setup: scroll must take");
    let visible = panel.automation_lane_screens();
    assert!(
        visible.iter().all(|lane| lane.param_id != ParamId::Borrowed("amount")),
        "setup: top lane must be culled"
    );
    let blend_lane = visible
        .iter()
        .find(|lane| lane.param_id == ParamId::Borrowed("blend"))
        .expect("setup: bottom lane visible")
        .clone();

    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.set_modifiers(Modifiers { command: true, ..Modifiers::NONE });
    let press = Vec2::new(
        panel.beat_to_pixel(blend_lane.dots[0].beat),
        blend_lane.y_at_norm(blend_lane.dots[0].value_norm),
    );
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationGroupMove);
    let destination = Vec2::new(panel.beat_to_pixel(Beats(8.0)), press.y);
    overlay.on_drag(destination, &mut host, &mut state, &mut panel);
    overlay.on_end_drag(&mut host);

    assert_eq!(host.automation_group_move_commits.len(), 1, "one batched commit");
    let moves = &host.automation_group_move_commits[0];
    assert_eq!(moves.len(), 2, "the scrolled-off lane's point must move too");
    assert!(moves.iter().any(|m| m.param_id == ParamId::Borrowed("amount") && m.old.0 == Beats(4.0) && m.new.0 == Beats(8.0)));
    assert!(moves.iter().any(|m| m.param_id == ParamId::Borrowed("blend") && m.old.0 == Beats(4.0) && m.new.0 == Beats(8.0)));
    assert_eq!(
        state.selected_automation_points.len(),
        2,
        "selection must keep the scrolled-off lane's point after the move"
    );
}

#[test]
fn automation_group_move_rebuilds_lane_for_horizontal_collision_and_updates_selection() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();
    let lanes = panel.automation_lane_screens();
    ui_state.selected_automation_points = lanes[0]
        .dots
        .iter()
        .map(|dot| UiAutomationPointRef {
            target: lanes[0].target.clone(),
            param_id: lanes[0].param_id.clone(),
            beat: dot.beat,
            value_norm: dot.value_norm,
        })
        .collect();

    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.set_modifiers(Modifiers { command: true, ..Modifiers::NONE });
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    let destination = Vec2::new(panel.beat_to_pixel(Beats(8.0)), press.y);
    overlay.on_drag(destination, &mut host, &mut ui_state, &mut panel);

    assert_eq!(
        host.automation_lane_preview,
        vec![(Beats(8.0), 0.5, UiSegmentShape::Linear), (Beats(12.0), 0.8, UiSegmentShape::Linear)],
        "horizontal group preview must rebuild from the full original lane and replace the collision"
    );
    assert!(ui_state.selected_automation_points.iter().any(|point| point.beat == Beats(8.0)));
    assert!(ui_state.selected_automation_points.iter().any(|point| point.beat == Beats(12.0)));

    overlay.on_end_drag(&mut host);
    assert_eq!(host.automation_group_move_commits[0].len(), 2);
    assert!(host.automation_group_move_commits[0].iter().any(|move_| move_.old.0 == Beats(4.0) && move_.new.0 == Beats(8.0)));
    assert!(host.automation_group_move_commits[0].iter().any(|move_| move_.old.0 == Beats(8.0) && move_.new.0 == Beats(12.0)));
}

#[test]
fn automation_phrase_snap_bypass_and_shared_zero_boundary() {
    for bypass in [false, true] {
        let mut panel = build_viewport_with_automation();
        let mut host = GestureTestHost::new(&["layer-0"]);
        let lanes = panel.automation_lane_screens();
        let mut state = UIState::new();
        state.selected_automation_points = lanes[0].dots.iter().map(|dot| UiAutomationPointRef {
            target: lanes[0].target.clone(), param_id: lanes[0].param_id.clone(), beat: dot.beat, value_norm: dot.value_norm,
        }).collect();
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay.set_modifiers(Modifiers { command: bypass, ..Modifiers::NONE });
        let press = dot_pos(&panel, 1);
        overlay.on_begin_drag(press, &mut host, &mut state, &panel);
        let requested = f64::from(panel.snap_grid_step()) * 0.3;
        let destination = Vec2::new(panel.beat_to_pixel(Beats(8.0 + requested)), press.y);
        overlay.on_drag(destination, &mut host, &mut state, &mut panel);
        let expected_delta = if bypass { requested } else { 0.0 };
        assert!((host.automation_lane_preview[0].0.0 - 4.0 - expected_delta).abs() < 1e-5);
        assert!((host.automation_lane_preview[1].0.0 - 8.0 - expected_delta).abs() < 1e-5);
        let destination = Vec2::new(panel.beat_to_pixel(Beats(-10.0)), press.y);
        overlay.on_drag(destination, &mut host, &mut state, &mut panel);
        assert_eq!(host.automation_lane_preview[0].0, Beats(0.0));
        assert_eq!(host.automation_lane_preview[1].0, Beats(4.0));
        assert_eq!(host.automation_lane_preview[0].1, 0.5);
        assert_eq!(host.automation_lane_preview[1].1, 0.8);
    }
}

#[test]
fn automation_phrase_moves_offscreen_selection_and_stops_at_the_unselected_point() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let original = vec![
        (Beats(4.0), 0.5, UiSegmentShape::Linear),
        (Beats(8.0), 0.8, UiSegmentShape::Linear),
        (Beats(12.0), 0.9, UiSegmentShape::Hold),
        (Beats(240.0), 1.2, UiSegmentShape::Curved(0.2)),
    ];
    host.automation_original_points = Some(original.clone());
    let lanes = panel.automation_lane_screens();
    let mut state = UIState::new();
    state.selected_automation_points = [(4.0, 0.5), (8.0, 0.8), (240.0, 1.2)].into_iter().map(|(beat, value)| UiAutomationPointRef {
        target: lanes[0].target.clone(),
        param_id: lanes[0].param_id.clone(),
        beat: Beats(beat),
        value_norm: BitmapSlider::value_to_normalized(value, 0.0, 1.0),
    }).collect();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.set_modifiers(Modifiers { command: false, ..Modifiers::NONE });
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut state, &panel);
    // The second destination is past the unselected point at 12: the group
    // stops against it (equal beat) instead of crossing it.
    for (destination, expected) in [(8.0, vec![8.0, 12.0, 12.0, 244.0]), (9.0, vec![8.0, 12.0, 12.0, 244.0])] {
        let pos = Vec2::new(panel.beat_to_pixel(Beats(destination)), press.y);
        overlay.on_drag(pos, &mut host, &mut state, &mut panel);
        assert_eq!(host.automation_lane_preview.iter().map(|p| p.0.0).collect::<Vec<_>>(), expected);
        assert_eq!(host.automation_lane_preview.last().unwrap().1, 1.2);
        assert_eq!(state.selected_automation_points.len(), 3);
    }
    assert_eq!(host.automation_lane_preview[2], original[2], "the unselected point is untouched");
    overlay.cancel_drag(&mut host);
    assert_eq!(host.automation_lane_restores[0].points.as_ref().unwrap(), &original);
    assert!(host.automation_group_move_commits.is_empty());
}

#[test]
fn automation_group_move_noop_preserves_snapshot_and_does_not_commit() {
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();
    let lanes = panel.automation_lane_screens();
    ui_state.selected_automation_points = lanes[0]
        .dots
        .iter()
        .map(|dot| UiAutomationPointRef {
            target: lanes[0].target.clone(),
            param_id: lanes[0].param_id.clone(),
            beat: dot.beat,
            value_norm: dot.value_norm,
        })
        .collect();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    let press = dot_pos(&panel, 0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    overlay.on_drag(press, &mut host, &mut ui_state, &mut panel);
    assert_eq!(host.automation_lane_preview, vec![
        (Beats(4.0), 0.5, UiSegmentShape::Linear),
        (Beats(8.0), 0.8, UiSegmentShape::Linear),
    ]);
    overlay.on_end_drag(&mut host);
    assert!(host.automation_group_move_commits.is_empty());
}

#[test]
fn automation_segment_bend_and_vertical_drag_commit() {
    // Alt-held: bend the segment between the two points.
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut ui_state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.set_modifiers(Modifiers { alt: true, ..Modifiers::NONE });

    let lanes = panel.automation_lane_screens();
    let (d0, d1) = (lanes[0].dots[0], lanes[0].dots[1]);
    // Midpoint of the ACTUAL segment line (linear interp of screen y, not
    // a naive average of param values — the two differ whenever the two
    // dots don't straddle the strip's vertical center).
    let press = Vec2::new((d0.x + d1.x) / 2.0, (d0.y + d1.y) / 2.0);

    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    overlay.on_drag(press, &mut host, &mut ui_state, &mut panel);
    overlay.on_end_drag(&mut host);
    assert!(host.automation_point_moves.is_empty(), "unchanged bend must not add an undo entry");

    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationSegmentBend, "Alt-press on a segment must begin a bend");
    overlay.on_drag(press + Vec2::new(0.0, -20.0), &mut host, &mut ui_state, &mut panel);
    overlay.on_end_drag(&mut host);
    assert_eq!(overlay.drag_mode(), DragMode::None);
    assert_eq!(host.automation_point_moves.len(), 1, "bend commits through the point-move path (shape only)");

    // Plain (no Alt) press on the same segment: vertical value drag.
    let mut panel2 = build_viewport_with_automation();
    let mut host2 = GestureTestHost::new(&["layer-0"]);
    let mut ui_state2 = UIState::new();
    let mut overlay2 = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay2.on_begin_drag(press, &mut host2, &mut ui_state2, &panel2);
    assert_eq!(overlay2.drag_mode(), DragMode::AutomationSegmentDrag, "plain press on a segment must begin a vertical drag");
    overlay2.on_drag(press + Vec2::new(0.0, -20.0), &mut host2, &mut ui_state2, &mut panel2);
    overlay2.on_end_drag(&mut host2);
    assert_eq!(overlay2.drag_mode(), DragMode::None);
    assert_eq!(host2.automation_segment_drag_commits.len(), 1, "one batched commit covering both endpoints");
}

#[test]
fn automation_draw_stroke_writes_points_and_commits_on_release() {
    let mut ui_state = UIState::new();
    ui_state.automation_draw_mode = true;
    let mut panel = build_viewport_with_automation();
    let mut host = GestureTestHost::new(&["layer-0"]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let lanes = panel.automation_lane_screens();
    let strip = lanes[0].curve_rect();
    let press = Vec2::new(strip.x + 10.0, strip.y + strip.height * 0.5);

    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::AutomationDraw, "pencil mode overrides all other drag routing on a lane strip");
    overlay.on_drag(press + Vec2::new(40.0, -10.0), &mut host, &mut ui_state, &mut panel);
    overlay.on_end_drag(&mut host);

    assert_eq!(overlay.drag_mode(), DragMode::None);
    assert_eq!(host.automation_draw_commits.len(), 1, "one commit per finished stroke");
}

mod trim_and_move_tests;
