//! The gesture preview.
//!
//! While a clip move or trim, a region move or an automation drag is in
//! flight, the editing host records the dragged state here as plain data and
//! never writes the project: the UI's project is the content thread's. Every
//! read that draws or hit-tests lays this record over the project
//! ([`GesturePreview::clip`], [`GesturePreview::clips_drawn`],
//! [`GesturePreview::lane_points`]), so the host and the projections see one
//! answer. An empty record costs a length check and allocates nothing.
//!
//! A commit leaves the record in place ("landing") until a project snapshot at
//! or past the commit's data version arrives ([`GesturePreview::snapshot_arrived`]),
//! so the clip never blinks back to its old place between release and the
//! content thread's answer. A gesture that sends nothing clears at once.

use manifold_core::clip::TimelineClip;
use manifold_core::effects::{AutomationPoint, ParamId};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::timeline::Timeline;
use manifold_core::{Beats, ClipId, GraphTarget, LayerId, Seconds};
use manifold_editing::command::Command;
use manifold_editing::commands::clip::SplitClipCommand;

use crate::frame_plan::UiStamps;

/// The two ledgers every UI-side writer reaches: the UI's part stamps and the
/// in-flight gesture preview. Owned by the App and by the script runner.
#[derive(Default)]
pub(crate) struct UiEdits {
    pub stamps: UiStamps,
    pub preview: GesturePreview,
}

/// Where one dragged clip is drawn: its layer, timing and source in-point.
pub(crate) struct ClipPreview {
    pub id: ClipId,
    pub layer: LayerId,
    pub start: Beats,
    pub duration: Beats,
    pub in_point: Seconds,
}

/// One dragged lane: the whole point list as drawn.
pub(crate) struct LanePreview {
    pub target: GraphTarget,
    pub param_id: ParamId,
    points: Vec<AutomationPoint>,
}

impl LanePreview {
    pub(crate) fn points(&self) -> &[AutomationPoint] {
        &self.points
    }
}

/// A clip as the timeline draws it: the clip's own data, placed where the
/// record says. `clip.start_beat`, `duration_beats` and `in_point` are the
/// project's; read `start`, `duration` and `in_point` here.
#[derive(Clone, Copy)]
pub(crate) struct DrawnClip<'a> {
    pub clip: &'a TimelineClip,
    pub layer_index: usize,
    pub start: Beats,
    pub duration: Beats,
    pub in_point: Seconds,
}

impl DrawnClip<'_> {
    pub(crate) fn end(&self) -> Beats {
        self.start + self.duration
    }
}

/// Separate clip, lane and marker versions, so a clip drag never re-projects
/// lanes and a point drag never re-projects clips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PreviewVersions {
    pub clips: u64,
    pub lanes: u64,
    pub markers: u64,
}

#[derive(Default)]
pub(crate) struct GesturePreview {
    version: PreviewVersions,
    /// A region move's boundary splits. Their results are recorded in
    /// `clips` and `added`; the commands lead the commit so its undo reverses
    /// them last.
    splits: Vec<Box<dyn Command>>,
    clips: Vec<ClipPreview>,
    /// Clips the gesture creates before commit (the tails of `splits`).
    added: Vec<TimelineClip>,
    lanes: Vec<LanePreview>,
    /// The data version whose snapshot carries the sent clip commit.
    clips_landing: Option<u64>,
    /// The data version whose snapshot carries the sent lane commit.
    lanes_landing: Option<u64>,
}

impl GesturePreview {
    /// `FrameInputs.preview`.
    pub(crate) fn version(&self) -> PreviewVersions {
        self.version
    }

    /// The clips a clip gesture is dragging, as last recorded.
    pub(crate) fn clips(&self) -> &[ClipPreview] {
        &self.clips
    }

    /// The lanes a point drag is moving, as last recorded.
    pub(crate) fn lanes(&self) -> &[LanePreview] {
        &self.lanes
    }

    /// A clip move or trim, a region move's splits, or a landing clip commit
    /// is being drawn from the record.
    pub(crate) fn clip_gesture_live(&self) -> bool {
        !self.clips.is_empty() || !self.added.is_empty() || !self.splits.is_empty()
    }

    /// How many clips the gesture has created so far.
    pub(crate) fn added_count(&self) -> usize {
        self.added.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        !self.clip_gesture_live() && self.lanes.is_empty()
    }

    // ── Reads: the record laid over the project ─────────────────────

    /// Clip `id` as drawn, in the project or among the added clips.
    pub(crate) fn clip<'a>(&'a self, project: &'a Project, id: &str) -> Option<DrawnClip<'a>> {
        let timeline = &project.timeline;
        let (layer_index, clip) = timeline
            .layers
            .iter()
            .enumerate()
            .find_map(|(li, l)| l.clips.iter().find(|c| c.id == id).map(|c| (li, c)))
            .or_else(|| {
                let clip = self.added.iter().find(|c| c.id == id)?;
                Some((timeline.layer_index_for_id(&clip.layer_id)?, clip))
            })?;
        Some(self.place(timeline, layer_index, clip))
    }

    /// Every clip as drawn: the project's clips layer by layer, then the added
    /// ones. A dragged clip appears on the layer the record gives it.
    pub(crate) fn clips_drawn<'a>(
        &'a self,
        project: &'a Project,
    ) -> impl Iterator<Item = DrawnClip<'a>> {
        self.drawn_over(project, &project.timeline.layers)
    }

    /// The clips drawn on layer `layer_index`. With no record that is the
    /// layer's own clips; otherwise a clip dragged in or out is accounted for.
    pub(crate) fn clips_on_layer<'a>(
        &'a self,
        project: &'a Project,
        layer_index: usize,
    ) -> impl Iterator<Item = DrawnClip<'a>> {
        let timeline = &project.timeline;
        let live = self.clip_gesture_live();
        let own = timeline
            .layers
            .get(layer_index)
            .filter(|_| !live)
            .into_iter()
            .flat_map(|l| l.clips.iter())
            .map(move |c| self.place(timeline, layer_index, c));
        let layers = if live { &timeline.layers[..] } else { &[] };
        own.chain(
            self.drawn_over(project, layers)
                .filter(move |d| d.layer_index == layer_index),
        )
    }

    /// The clips of `layers` and the added ones, placed by the record. A
    /// layer slice must be the timeline's own or empty.
    fn drawn_over<'a>(
        &'a self,
        project: &'a Project,
        layers: &'a [Layer],
    ) -> impl Iterator<Item = DrawnClip<'a>> {
        let timeline = &project.timeline;
        let stored = layers
            .iter()
            .enumerate()
            .flat_map(|(li, l)| l.clips.iter().map(move |c| (li, c)));
        let added = self
            .added
            .iter()
            .filter(|_| !layers.is_empty())
            .filter_map(|c| Some((timeline.layer_index_for_id(&c.layer_id)?, c)));
        stored
            .chain(added)
            .map(move |(li, clip)| self.place(timeline, li, clip))
    }

    /// `clip`, held on layer `layer_index`, placed by its record. A record
    /// whose layer is gone places nothing: the clip stays where the project
    /// has it.
    fn place<'a>(
        &self,
        timeline: &Timeline,
        layer_index: usize,
        clip: &'a TimelineClip,
    ) -> DrawnClip<'a> {
        let own = DrawnClip {
            clip,
            layer_index,
            start: clip.start_beat,
            duration: clip.duration_beats,
            in_point: clip.in_point,
        };
        let Some(rec) = self.clips.iter().find(|r| r.id == clip.id) else {
            return own;
        };
        match timeline.layer_index_for_id(&rec.layer) {
            Some(layer_index) => DrawnClip {
                clip,
                layer_index,
                start: rec.start,
                duration: rec.duration,
                in_point: rec.in_point,
            },
            None => own,
        }
    }

    /// The dragged points of a lane, when the record holds it.
    pub(crate) fn lane_points(
        &self,
        target: &GraphTarget,
        param_id: &str,
    ) -> Option<&[AutomationPoint]> {
        self.lanes
            .iter()
            .find(|l| l.target == *target && l.param_id.as_ref() == param_id)
            .map(|l| l.points.as_slice())
    }

    // ── Writes: the record only ─────────────────────────────────────

    /// Draw clip `id` on `layer` at this place.
    pub(crate) fn place_clip(
        &mut self,
        id: &ClipId,
        layer: &LayerId,
        start: Beats,
        duration: Beats,
        in_point: Seconds,
    ) {
        match self.clips.iter_mut().find(|c| c.id == *id) {
            Some(slot) => {
                slot.layer.clone_from(layer);
                slot.start = start;
                slot.duration = duration;
                slot.in_point = in_point;
            }
            None => self.clips.push(ClipPreview {
                id: id.clone(),
                layer: layer.clone(),
                start,
                duration,
                in_point,
            }),
        }
        self.clips_landing = None;
        self.version.clips += 1;
    }

    /// Record a boundary split's result: the clip it shortens keeps its
    /// place with the new duration, and the tail joins the added clips. The
    /// command is kept to lead the commit.
    pub(crate) fn record_split(&mut self, project: &Project, split: SplitClipCommand) {
        let Some(shortened) = self.clip(project, split.clip_id().as_str()) else {
            return;
        };
        let (start, in_point) = (shortened.start, shortened.in_point);
        self.place_clip(
            split.clip_id(),
            split.layer_id(),
            start,
            split.new_duration_beats(),
            in_point,
        );
        let mut tail = split.tail_clip().clone();
        tail.layer_id = split.layer_id().clone();
        self.added.push(tail);
        self.splits.push(Box::new(split));
    }

    /// The splits that lead the commit; the record keeps drawing their result.
    pub(crate) fn take_splits(&mut self) -> Vec<Box<dyn Command>> {
        std::mem::take(&mut self.splits)
    }

    /// The commit was sent: keep drawing the clip record until a snapshot at
    /// or past `data_version` arrives.
    pub(crate) fn land_clips(&mut self, data_version: u64) {
        self.clips_landing = Some(data_version);
    }

    /// The clip gesture ended with nothing sent, or its result arrived.
    pub(crate) fn clear_clips(&mut self) {
        if self.clip_gesture_live() {
            self.version.clips += 1;
        }
        self.clips.clear();
        self.added.clear();
        self.splits.clear();
        self.clips_landing = None;
    }

    /// Lane `param_id` on `target`, for editing: the record's points, seeded
    /// from the project's lane when the record has none. `None` when neither
    /// has the lane.
    pub(crate) fn lane_points_mut(
        &mut self,
        project: &Project,
        target: &GraphTarget,
        param_id: &str,
    ) -> Option<&mut Vec<AutomationPoint>> {
        let at = match self
            .lanes
            .iter()
            .position(|l| l.target == *target && l.param_id.as_ref() == param_id)
        {
            Some(at) => at,
            None => {
                let lane = project
                    .preset_instance(target)?
                    .automation_lanes
                    .as_ref()?
                    .iter()
                    .find(|l| l.param_id.as_ref() == param_id)?;
                self.lanes.push(LanePreview {
                    target: target.clone(),
                    param_id: lane.param_id.clone(),
                    points: lane.points.clone(),
                });
                self.lanes.len() - 1
            }
        };
        self.lanes_landing = None;
        self.version.lanes += 1;
        Some(&mut self.lanes[at].points)
    }

    /// Draw lane `param_id` on `target` with exactly `points`, whether or not
    /// the project holds the lane.
    pub(crate) fn set_lane(
        &mut self,
        target: &GraphTarget,
        param_id: &ParamId,
        points: impl Iterator<Item = AutomationPoint>,
    ) {
        match self
            .lanes
            .iter_mut()
            .find(|l| l.target == *target && l.param_id == *param_id)
        {
            Some(slot) => {
                slot.points.clear();
                slot.points.extend(points);
            }
            None => self.lanes.push(LanePreview {
                target: target.clone(),
                param_id: param_id.clone(),
                points: points.collect(),
            }),
        }
        self.lanes_landing = None;
        self.version.lanes += 1;
    }

    /// Stop drawing lane `param_id` on `target`. True when the record held it.
    pub(crate) fn drop_lane(&mut self, target: &GraphTarget, param_id: &str) -> bool {
        let before = self.lanes.len();
        self.lanes
            .retain(|l| !(l.target == *target && l.param_id.as_ref() == param_id));
        let dropped = self.lanes.len() != before;
        if dropped {
            self.version.lanes += 1;
        }
        dropped
    }

    /// The lane gesture ended. Lanes that match the project's already have
    /// nothing to show (a cancel, a click without a move) and drop at once;
    /// the rest keep drawing until a snapshot at or past `data_version`
    /// arrives.
    pub(crate) fn land_lanes(&mut self, project: &Project, data_version: u64) {
        let before = self.lanes.len();
        self.lanes.retain(|rec| {
            !project
                .preset_instance(&rec.target)
                .and_then(|inst| inst.automation_lanes.as_ref())
                .and_then(|lanes| lanes.iter().find(|l| l.param_id == rec.param_id))
                .is_some_and(|lane| lane.points == rec.points)
        });
        if self.lanes.len() != before {
            self.version.lanes += 1;
        }
        self.lanes_landing = (!self.lanes.is_empty()).then_some(data_version);
    }

    /// A marker drag wrote the dragged beat into the local project. The
    /// snapshot accept re-covers it through the scrub restore, so only the
    /// version is kept here.
    pub(crate) fn record_marker(&mut self) {
        self.version.markers += 1;
    }

    // ── Snapshot intake ─────────────────────────────────────────────

    /// A project snapshot with `data_version` replaced the UI's project. A
    /// landed record whose commit that snapshot carries has nothing left to
    /// show and clears; versions bump so projections re-run.
    pub(crate) fn snapshot_arrived(&mut self, data_version: u64) {
        if self
            .clips_landing
            .is_some_and(|landing| data_version >= landing)
        {
            self.clear_clips();
        }
        if self
            .lanes_landing
            .is_some_and(|landing| data_version >= landing)
        {
            self.clear_lanes();
        }
    }

    /// Drop what no live gesture owns and no commit is landing: a record a
    /// gesture left behind.
    pub(crate) fn clear_stale(&mut self) {
        if self.clips_landing.is_none() {
            self.clear_clips();
        }
        if self.lanes_landing.is_none() {
            self.clear_lanes();
        }
    }

    fn clear_lanes(&mut self) {
        if !self.lanes.is_empty() {
            self.lanes.clear();
            self.version.lanes += 1;
        }
        self.lanes_landing = None;
    }
}

#[cfg(test)]
mod tests {
    //! INV-12: a preview never moves a part stamp. Drives a clip move and an
    //! automation point drag through the real `InteractionOverlay` and the
    //! real `AppEditingHost`.
    use super::*;
    use manifold_core::change::Part;
    use manifold_core::effects::PresetInstance;
    use manifold_core::effects::{AutomationPoint, SegmentShape};
    use manifold_editing::service::EditingService;
    use manifold_ui::interaction_overlay::{DragMode, InteractionOverlay};
    use manifold_ui::layout::ScreenLayout;
    use manifold_ui::node::Vec2;
    use manifold_ui::panels::Panel;
    use manifold_ui::panels::viewport::{
        TimelineViewportPanel, TrackInfo, ViewportAutomationLane, ViewportClip,
    };
    use manifold_ui::tree::UITree;
    use manifold_ui::view::{
        UiAutomationLane, UiAutomationPoint, UiGraphTarget, UiLayer, UiSegmentShape,
    };

    struct Rig {
        project: Project,
        tx: crossbeam_channel::Sender<crate::content_command::ContentCommand>,
        rx: crossbeam_channel::Receiver<crate::content_command::ContentCommand>,
        content_state: crate::content_state::ContentState,
        cursor: manifold_ui::cursors::CursorManager,
        active_layer: Option<LayerId>,
        redraw: bool,
        edits: UiEdits,
    }

    impl Rig {
        fn new(project: Project) -> Self {
            let (tx, rx) = crossbeam_channel::unbounded();
            Self {
                project,
                tx,
                rx,
                content_state: Default::default(),
                cursor: Default::default(),
                active_layer: None,
                redraw: false,
                edits: UiEdits::default(),
            }
        }

        /// The commands the host has sent since the last call.
        fn sent(&self) -> Vec<crate::content_command::ContentCommand> {
            self.rx.try_iter().collect()
        }

        fn host(&mut self) -> crate::editing_host::AppEditingHost<'_> {
            crate::editing_host::AppEditingHost::new(
                crate::ui_bridge::ProjectRef::new(&self.project, &mut self.edits, &self.tx),
                &self.tx,
                &self.content_state,
                &mut self.cursor,
                &mut self.active_layer,
                &mut self.redraw,
            )
        }
    }

    fn viewport(
        layer_ids: &[LayerId],
        clips: Vec<ViewportClip>,
        lanes: Vec<ViewportAutomationLane>,
    ) -> TimelineViewportPanel {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(vec![TrackInfo::default(); layer_ids.len()]);
        panel.set_clips(clips);
        let ui_layers: Vec<UiLayer> = layer_ids
            .iter()
            .map(|layer_id| UiLayer {
                layer_id: layer_id.clone(),
                parent_layer_id: None,
                layer_type: manifold_ui::types::LayerType::Video,
                is_collapsed: false,
                automation_lane_count: lanes.len(),
            })
            .collect();
        panel.rebuild_mapper_layout(&ui_layers);
        let has_lanes = !lanes.is_empty();
        panel.set_automation_lanes(lanes);
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        // Lanes need room, and so do several tracks.
        if has_lanes || layer_ids.len() > 1 {
            layout.timeline_split_ratio = 0.70;
        }
        let region = tree.begin_region(
            layout.timeline_body(),
            manifold_ui::tree::ZTier::Base,
            "viewport",
            manifold_ui::node::UIFlags::empty(),
        );
        let start = tree.count();
        panel.build(&mut tree, &layout);
        tree.end_region(region, start);
        panel
    }

    #[test]
    fn drag_preview_moves_no_stamp() {
        // ── Clip move ──
        let mut project = Project::default();
        project.timeline.add_layer(
            "V",
            manifold_core::types::LayerType::Video,
            manifold_core::PresetTypeId::from_string("Video".to_string()),
        );
        let (mut create, clip_id) =
            EditingService::create_clip_at_position(&project, Beats(0.0), 0, Beats(8.0)).unwrap();
        create.execute(&mut project);
        let layer_id = project.timeline.layers[0].layer_id.clone();
        let mut panel = viewport(
            std::slice::from_ref(&layer_id),
            vec![ViewportClip {
                clip_id: clip_id.clone(),
                layer_index: 0,
                start_beat: Beats(0.0),
                duration_beats: Beats(8.0),
                name: "".into(),
                color: manifold_ui::color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
                is_audio: false,
                waveform: None,
                in_point_seconds: 0.0,
                waveform_breakpoints: Vec::new(),
            }],
            Vec::new(),
        );
        let body = |panel: &TimelineViewportPanel, beat: f32| {
            Vec2::new(
                panel.beat_to_pixel(Beats::from_f32(beat)),
                panel.tracks_rect().y + 70.0,
            )
        };
        let mut rig = Rig::new(project);
        let mut selection = manifold_ui::UIState::new();
        selection.select_clips(vec![clip_id.clone()]);
        let mut overlay = InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD);
        let before = rig.edits.stamps.parts();

        overlay.on_begin_drag(body(&panel, 2.0), &mut rig.host(), &mut selection, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::Move);
        for beat in [6.0, 10.0] {
            overlay.on_drag(
                body(&panel, beat),
                &mut rig.host(),
                &mut selection,
                &mut panel,
            );
            assert_eq!(
                rig.edits.stamps.parts(),
                before,
                "a clip move preview moved a stamp"
            );
        }
        assert!(
            rig.edits.preview.version().clips > 0,
            "the move wrote the preview"
        );
        assert!(!rig.edits.preview.is_empty());
        overlay.on_end_drag(&mut rig.host());
        assert_eq!(
            rig.edits.stamps.parts(),
            before,
            "the commit stamps nothing: its snapshot brings the stamps"
        );
        let sent = rig.sent();
        let [crate::content_command::ContentCommand::Execute(command)] = sent.as_slice() else {
            panic!("the commit sends one edit");
        };
        assert!(command.touches().contains(Part::Clips));
        assert!(
            !rig.edits.preview.is_empty(),
            "the commit keeps drawing until its snapshot arrives"
        );
        rig.edits.preview.snapshot_arrived(u64::MAX);
        assert!(rig.edits.preview.is_empty(), "the snapshot clears it");

        // ── Automation point drag ──
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
        fx.init_defaults();
        let fx_id = fx.id.clone();
        fx.automation_lanes = Some(vec![manifold_core::effects::AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![
                AutomationPoint {
                    beat: Beats(4.0),
                    value: 0.5,
                    shape: SegmentShape::Linear,
                },
                AutomationPoint {
                    beat: Beats(8.0),
                    value: 0.8,
                    shape: SegmentShape::Linear,
                },
            ],
        }]);
        let mut project = Project::default();
        project.timeline.add_layer(
            "V",
            manifold_core::types::LayerType::Video,
            manifold_core::PresetTypeId::from_string("Video".to_string()),
        );
        project.settings.master_effects.push(fx);
        let layer_id = project.timeline.layers[0].layer_id.clone();
        let target = UiGraphTarget::Effect(fx_id.clone());
        let mut panel = viewport(
            std::slice::from_ref(&layer_id),
            Vec::new(),
            vec![ViewportAutomationLane {
                layer_index: 0,
                lane: UiAutomationLane {
                    effect_id: fx_id,
                    param_id: "amount".into(),
                    target,
                    label: "Mirror: amount".into(),
                    points: vec![
                        UiAutomationPoint {
                            beat: Beats(4.0),
                            value_norm: 0.5,
                            shape: UiSegmentShape::Linear,
                            past_end: false,
                        },
                        UiAutomationPoint {
                            beat: Beats(8.0),
                            value_norm: 0.8,
                            shape: UiSegmentShape::Linear,
                            past_end: false,
                        },
                    ],
                    param_min: 0.0,
                    param_max: 1.0,
                    whole_numbers: false,
                    placeholder: false,
                    display: manifold_ui::view::UiLaneDisplay::default(),
                },
            }],
        );
        let lane = panel.automation_lane_screens()[0].clone();
        let press = Vec2::new(
            panel.beat_to_pixel(lane.dots[0].beat),
            lane.y_at_norm(lane.dots[0].value_norm),
        );
        let mut rig = Rig::new(project);
        let mut selection = manifold_ui::UIState::new();
        let mut overlay = InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD);
        let before = rig.edits.stamps.parts();

        overlay.on_begin_drag(press, &mut rig.host(), &mut selection, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint);
        for step in [Vec2::new(10.0, -15.0), Vec2::new(20.0, -30.0)] {
            overlay.on_drag(press + step, &mut rig.host(), &mut selection, &mut panel);
            assert_eq!(
                rig.edits.stamps.parts(),
                before,
                "a point drag preview moved a stamp"
            );
        }
        assert!(
            !rig.edits.preview.is_empty(),
            "the point drag wrote the preview"
        );
        overlay.on_end_drag(&mut rig.host());
        assert_eq!(
            rig.edits.stamps.parts(),
            before,
            "the commit stamps nothing: its snapshot brings the stamps"
        );
        let sent = rig.sent();
        assert!(
            sent.iter().any(|c| matches!(
                c,
                crate::content_command::ContentCommand::Execute(command)
                    if command.touches().contains(Part::Automation)
            )),
            "the commit sends the automation edit"
        );
        assert!(
            !rig.edits.preview.is_empty(),
            "the commit keeps drawing until its snapshot arrives"
        );
        rig.edits.preview.snapshot_arrived(u64::MAX);
        assert!(rig.edits.preview.is_empty(), "the snapshot clears it");
    }

    // ── Overlay tests ───────────────────────────────────────────────

    use manifold_ui::timeline_editing_host::TimelineEditingHost;

    fn video_layer(project: &mut Project, name: &str) {
        project.timeline.add_layer(
            name,
            manifold_core::types::LayerType::Video,
            manifold_core::PresetTypeId::from_string("Video".to_string()),
        );
    }

    fn add_clip(project: &mut Project, layer: usize, start: f64, duration: f64) -> ClipId {
        let (mut create, id) =
            EditingService::create_clip_at_position(project, Beats(start), layer, Beats(duration))
                .unwrap();
        create.execute(project);
        id
    }

    type Row = (usize, ClipId, f64, f64);

    fn sorted(mut rows: Vec<Row>) -> Vec<Row> {
        rows.sort_by(|a, b| (a.0, a.2).partial_cmp(&(b.0, b.2)).unwrap());
        rows
    }

    /// Every clip in the project as (layer, id, start, duration).
    fn table(project: &Project) -> Vec<Row> {
        sorted(
            project
                .timeline
                .layers
                .iter()
                .enumerate()
                .flat_map(|(li, l)| {
                    l.clips
                        .iter()
                        .map(move |c| (li, c.id.clone(), c.start_beat.0, c.duration_beats.0))
                })
                .collect(),
        )
    }

    /// Every clip as the preview draws it, in the same shape.
    fn drawn_table(preview: &GesturePreview, project: &Project) -> Vec<Row> {
        sorted(
            preview
                .clips_drawn(project)
                .map(|d| (d.layer_index, d.clip.id.clone(), d.start.0, d.duration.0))
                .collect(),
        )
    }

    fn row(table: &[Row], id: &ClipId) -> (usize, f64, f64) {
        let r = table.iter().find(|r| r.1 == *id).expect("clip in table");
        (r.0, r.2, r.3)
    }

    fn shown_on(ui: &crate::ui_root::UIRoot, layer: usize) -> Vec<(f64, f64)> {
        let mut rows: Vec<(f64, f64)> = ui
            .viewport
            .clips_for_layer(layer)
            .iter()
            .map(|c| (c.start_beat.0, c.duration_beats.0))
            .collect();
        rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
        rows
    }

    fn projected(project: &Project, preview: &GesturePreview) -> crate::ui_root::UIRoot {
        let mut ui = crate::ui_root::UIRoot::new();
        ui.viewport
            .set_tracks(vec![TrackInfo::default(); project.timeline.layers.len()]);
        crate::ui_bridge::sync_clip_positions(&mut ui, project, preview);
        ui
    }

    fn only_commit(sent: Vec<crate::content_command::ContentCommand>) -> Box<dyn Command> {
        let mut commands: Vec<_> = sent
            .into_iter()
            .filter_map(|c| match c {
                crate::content_command::ContentCommand::Execute(cmd) => Some(cmd),
                _ => None,
            })
            .collect();
        assert_eq!(commands.len(), 1, "one commit is sent");
        commands.remove(0)
    }

    /// A clip move through the real overlay and a trim through the host
    /// leave the project untouched while the host's reads and the viewport
    /// projection show the dragged place.
    #[test]
    fn a_clip_move_and_trim_draw_from_the_record_and_leave_the_project_alone() {
        let mut project = Project::default();
        for name in ["V0", "V1", "V2"] {
            video_layer(&mut project, name);
        }
        let a = add_clip(&mut project, 0, 1.5, 2.75);
        let b = add_clip(&mut project, 0, 6.0, 2.0);
        let c = add_clip(&mut project, 1, 0.5, 3.0);
        let _d = add_clip(&mut project, 2, 4.25, 1.25);
        let layer_ids: Vec<LayerId> = project
            .timeline
            .layers
            .iter()
            .map(|l| l.layer_id.clone())
            .collect();
        let before = table(&project);
        let viewport_clips = project
            .timeline
            .layers
            .iter()
            .enumerate()
            .flat_map(|(li, l)| l.clips.iter().map(move |c| (li, c)))
            .map(|(li, c)| ViewportClip {
                clip_id: c.id.clone(),
                layer_index: li,
                start_beat: c.start_beat,
                duration_beats: c.duration_beats,
                name: "".into(),
                color: manifold_ui::color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
                is_audio: false,
                waveform: None,
                in_point_seconds: 0.0,
                waveform_breakpoints: Vec::new(),
            })
            .collect();
        let mut panel = viewport(&layer_ids, viewport_clips, Vec::new());
        let at = |panel: &TimelineViewportPanel, beat: f32, layer: usize| {
            Vec2::new(
                panel.beat_to_pixel(Beats::from_f32(beat)),
                panel.track_y(layer) + 70.0,
            )
        };

        let mut rig = Rig::new(project);
        let mut selection = manifold_ui::UIState::new();
        selection.select_clips(vec![a.clone()]);
        let mut overlay = InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD);
        overlay.on_begin_drag(at(&panel, 2.0, 0), &mut rig.host(), &mut selection, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::Move);
        // Right 4 beats and down one layer.
        for beat in [4.0, 6.0] {
            overlay.on_drag(
                at(&panel, beat, 1),
                &mut rig.host(),
                &mut selection,
                &mut panel,
            );
        }
        assert_eq!(table(&rig.project), before, "a move writes no project clip");
        let (start, duration) = {
            let host = rig.host();
            let moved = host.find_clip_by_id(a.as_str()).unwrap();
            assert_eq!(moved.layer_index, 1);
            assert_eq!(moved.layer_id, layer_ids[1]);
            assert_ne!(moved.start_beat, Beats(1.5), "the clip moved in the record");
            assert_eq!(moved.duration_beats, Beats(2.75));
            let on_one: Vec<ClipId> = host
                .clips_on_layer(1)
                .into_iter()
                .map(|r| r.clip_id)
                .collect();
            assert!(on_one.contains(&a) && on_one.contains(&c));
            assert!(host.clips_on_layer(0).iter().all(|r| r.clip_id != a));
            (moved.start_beat.0, moved.duration_beats.0)
        };
        let mut ui = projected(&rig.project, &rig.edits.preview);
        assert!(shown_on(&ui, 1).contains(&(start, duration)));
        assert_eq!(shown_on(&ui, 0), vec![(6.0, 2.0)]);

        // A trim, with numbers that are not whole beats.
        {
            let mut host = rig.host();
            host.set_clip_trim(b.as_str(), Beats(6.375), Beats(1.5), Seconds(0.25));
            let trimmed = host.find_clip_by_id(b.as_str()).unwrap();
            assert_eq!(
                (trimmed.start_beat, trimmed.duration_beats, trimmed.end_beat),
                (Beats(6.375), Beats(1.5), Beats(7.875))
            );
            assert_eq!(trimmed.in_point, Seconds(0.25));
        }
        assert_eq!(table(&rig.project), before, "a trim writes no project clip");
        let project_b = rig.project.timeline.layers[0]
            .clips
            .iter()
            .find(|clip| clip.id == b)
            .unwrap();
        assert_eq!(
            (
                project_b.start_beat,
                project_b.duration_beats,
                project_b.in_point
            ),
            (Beats(6.0), Beats(2.0), Seconds(0.0))
        );
        crate::ui_bridge::sync_clip_positions(&mut ui, &rig.project, &rig.edits.preview);
        assert_eq!(shown_on(&ui, 0), vec![(6.375, 1.5)]);

        // Cancel puts the record back where the project has everything.
        overlay.cancel_drag(&mut rig.host());
        assert!(
            rig.sent().is_empty(),
            "a cancel without splits sends nothing"
        );
        assert!(
            rig.edits.preview.is_empty(),
            "a cancel without splits clears at once"
        );
        assert_eq!(table(&rig.project), before);

        // A press and release without a move sends nothing and leaves no record.
        overlay.on_begin_drag(at(&panel, 6.5, 0), &mut rig.host(), &mut selection, &panel);
        overlay.on_end_drag(&mut rig.host());
        assert!(
            rig.sent().is_empty(),
            "a click without a move sends nothing"
        );
        assert!(rig.edits.preview.is_empty());
    }

    /// A region move across two layers that splits clips: the pieces show
    /// mid-drag with the project untouched, and the sent commit, run on a
    /// copy of the project, gives the clips worked out by hand.
    #[test]
    fn a_region_move_across_two_layers_shows_its_pieces_and_commits_them() {
        let mut project = Project::default();
        for name in ["V0", "V1", "V2"] {
            video_layer(&mut project, name);
        }
        let a = add_clip(&mut project, 0, 0.0, 5.0);
        let b = add_clip(&mut project, 0, 7.25, 2.5);
        let c = add_clip(&mut project, 1, 3.0, 2.5);
        let d = add_clip(&mut project, 1, 9.0, 3.0);
        let original = project.clone();
        let before = table(&project);
        let layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        let mut selection = manifold_ui::UIState::new();
        selection.set_region(Beats(2.0), Beats(8.0), 0, 1, &layers);
        let region = selection.current_region().cloned().unwrap();
        let mut rig = Rig::new(project);

        // Region [2, 8) on layers 0..1 splits A at 2 and B at 8.
        let split = rig.host().split_clips_for_region_move(&region);
        assert_eq!(split.split_count, 2);
        let a2 = split.interior_clip_ids[0].clone();
        assert_eq!(
            split.interior_clip_ids,
            vec![a2.clone(), b.clone(), c.clone()]
        );
        assert_ne!(a2, a, "the start split's tail is the interior piece");
        assert_eq!(
            table(&rig.project),
            before,
            "the splits write no project clip"
        );
        let drawn = drawn_table(&rig.edits.preview, &rig.project);
        assert_eq!(row(&drawn, &a), (0, 0.0, 2.0));
        assert_eq!(row(&drawn, &a2), (0, 2.0, 3.0));
        assert_eq!(row(&drawn, &b), (0, 7.25, 0.75));
        assert_eq!(drawn.len(), 6, "two pieces joined the four clips");
        let b2 = drawn
            .iter()
            .find(|r| r.0 == 0 && r.2 == 8.0)
            .map(|r| r.1.clone())
            .unwrap();
        assert_eq!(row(&drawn, &b2), (0, 8.0, 1.75));

        // The overlay's moves: everything right 1.5 beats, down one layer.
        {
            let mut host = rig.host();
            for (id, layer, start) in [(&a2, 1, 3.5), (&b, 1, 8.75), (&c, 2, 4.5)] {
                host.move_clip_to_layer(id.as_str(), layer);
                host.set_clip_start_beat(id.as_str(), Beats(start));
            }
            let on = |host: &dyn TimelineEditingHost, layer| -> Vec<(f64, f64)> {
                let mut rows: Vec<_> = host
                    .clips_on_layer(layer)
                    .iter()
                    .map(|r| (r.start_beat.0, r.duration_beats.0))
                    .collect();
                rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
                rows
            };
            assert_eq!(on(&host, 0), vec![(0.0, 2.0), (8.0, 1.75)]);
            assert_eq!(on(&host, 1), vec![(3.5, 3.0), (8.75, 0.75), (9.0, 3.0)]);
            assert_eq!(on(&host, 2), vec![(4.5, 2.5)]);
        }
        assert_eq!(
            table(&rig.project),
            before,
            "the moves write no project clip"
        );
        let ui = projected(&rig.project, &rig.edits.preview);
        assert_eq!(shown_on(&ui, 1), vec![(3.5, 3.0), (8.75, 0.75), (9.0, 3.0)]);
        assert_eq!(shown_on(&ui, 2), vec![(4.5, 2.5)]);
        assert!(rig.sent().is_empty(), "nothing is sent mid-drag");

        // The drop.
        {
            let mut host = rig.host();
            host.begin_command_batch();
            host.record_move(a2.as_str(), Beats(2.0), Beats(3.5), 0, 1);
            host.record_move(b.as_str(), Beats(7.25), Beats(8.75), 0, 1);
            host.record_move(c.as_str(), Beats(3.0), Beats(4.5), 1, 2);
            host.resolve_overlaps(vec![a2.clone(), b.clone(), c.clone()]);
            host.commit_command_batch("Move clips");
        }
        let commit = only_commit(rig.sent());
        let mut copy = original.clone();
        let mut commit = commit;
        commit.execute(&mut copy);
        let after = table(&copy);
        // Layer 0: A shortened to [0, 2), B's tail [8, 9.75).
        assert_eq!(row(&after, &a), (0, 0.0, 2.0));
        assert_eq!(row(&after, &b2), (0, 8.0, 1.75));
        // Layer 1: A's tail and B land, and B trims D's front to [9.5, 12).
        assert_eq!(row(&after, &a2), (1, 3.5, 3.0));
        assert_eq!(row(&after, &b), (1, 8.75, 0.75));
        assert_eq!(row(&after, &d), (1, 9.5, 2.5));
        // Layer 2: C.
        assert_eq!(row(&after, &c), (2, 4.5, 2.5));
        assert_eq!(after.len(), 6);
        // The record still draws the same clips (ids, layers, starts, durations)
        // until the snapshot arrives; only D's trim waits for it.
        let landing = drawn_table(&rig.edits.preview, &rig.project);
        for id in [&a, &a2, &b, &b2, &c] {
            assert_eq!(row(&landing, id), row(&after, id));
        }
        assert_eq!(row(&landing, &d), (1, 9.0, 3.0));
        // Undo reverses the moves, then the splits.
        commit.undo(&mut copy);
        assert_eq!(table(&copy), before);
    }

    /// An alt-drag duplicate onto a neighbour: the commit leaves the copy at
    /// the old place and the trimmed neighbour, and undo restores everything.
    #[test]
    fn an_alt_drag_duplicate_onto_a_neighbour_commits_the_copy_and_the_trim() {
        let mut project = Project::default();
        video_layer(&mut project, "V0");
        video_layer(&mut project, "V1");
        let a = add_clip(&mut project, 0, 1.5, 2.5);
        let n = add_clip(&mut project, 0, 6.0, 4.0);
        let other = add_clip(&mut project, 1, 0.25, 1.0);
        let original = project.clone();
        let before = table(&project);
        let mut rig = Rig::new(project);
        {
            let mut host = rig.host();
            host.set_clip_start_beat(a.as_str(), Beats(7.5));
        }
        assert_eq!(table(&rig.project), before);
        {
            let mut host = rig.host();
            host.begin_command_batch();
            host.record_move(a.as_str(), Beats(1.5), Beats(7.5), 0, 0);
            host.duplicate_clip_to(a.as_str(), Beats(1.5), 0);
            host.resolve_overlaps(vec![a.clone()]);
            host.commit_command_batch("Duplicate clips");
        }
        let mut commit = only_commit(rig.sent());
        let mut copy = original.clone();
        commit.execute(&mut copy);
        let after = table(&copy);
        assert_eq!(after.len(), 4);
        assert_eq!(row(&after, &a), (0, 7.5, 2.5));
        // The neighbour [6, 10) keeps [6, 7.5) under the dropped clip.
        assert_eq!(row(&after, &n), (0, 6.0, 1.5));
        let dup = after
            .iter()
            .find(|r| r.0 == 0 && r.2 == 1.5)
            .expect("the copy sits at the old place");
        assert_eq!(dup.3, 2.5);
        assert_ne!(dup.1, a);
        assert_eq!(row(&after, &other), (1, 0.25, 1.0));
        commit.undo(&mut copy);
        assert_eq!(table(&copy), before, "undo restores everything");
    }

    /// After a commit the record keeps drawing; a snapshot below the
    /// commit's data version keeps it, one at or past it clears it. A click
    /// without a move clears at once and sends nothing.
    #[test]
    fn a_landed_record_clears_when_its_snapshot_arrives() {
        let mut project = Project::default();
        video_layer(&mut project, "V0");
        let a = add_clip(&mut project, 0, 1.5, 2.5);
        let mut rig = Rig::new(project);
        rig.content_state.data_version = 7;

        // A click without a move.
        {
            let mut host = rig.host();
            host.set_clip_start_beat(a.as_str(), Beats(1.5));
            host.begin_command_batch();
            host.commit_command_batch("Move clips");
        }
        assert!(rig.sent().is_empty());
        assert!(rig.edits.preview.is_empty());

        // A move that lands at data version 8.
        {
            let mut host = rig.host();
            host.set_clip_start_beat(a.as_str(), Beats(5.75));
            host.begin_command_batch();
            host.record_move(a.as_str(), Beats(1.5), Beats(5.75), 0, 0);
            host.commit_command_batch("Move clips");
            assert_eq!(
                host.find_clip_by_id(a.as_str()).unwrap().start_beat,
                Beats(5.75),
                "the dropped clip does not blink back"
            );
        }
        assert_eq!(rig.sent().len(), 1);
        rig.edits.preview.snapshot_arrived(7);
        assert_eq!(
            rig.host().find_clip_by_id(a.as_str()).unwrap().start_beat,
            Beats(5.75),
            "a snapshot below the landing version keeps the record"
        );
        let version = rig.edits.preview.version().clips;
        rig.edits.preview.snapshot_arrived(8);
        assert!(rig.edits.preview.is_empty());
        assert!(
            rig.edits.preview.version().clips > version,
            "projections re-run"
        );
        assert_eq!(
            rig.host().find_clip_by_id(a.as_str()).unwrap().start_beat,
            Beats(1.5)
        );
    }

    /// A lane drag draws from its record; a cancel drops it at once, and a
    /// commit lands until its snapshot arrives.
    #[test]
    fn a_lane_drag_lands_and_a_cancel_drops_at_once() {
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
        fx.init_defaults();
        let fx_id = fx.id.clone();
        let pts = |a: f32, b: f32| {
            vec![
                AutomationPoint {
                    beat: Beats(4.5),
                    value: a,
                    shape: SegmentShape::Linear,
                },
                AutomationPoint {
                    beat: Beats(8.25),
                    value: b,
                    shape: SegmentShape::Linear,
                },
            ]
        };
        fx.automation_lanes = Some(vec![manifold_core::effects::AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: pts(0.5, 0.75),
        }]);
        let mut project = Project::default();
        video_layer(&mut project, "V0");
        project.settings.master_effects.push(fx);
        let target = UiGraphTarget::Effect(fx_id);
        let param: manifold_core::effects::ParamId = "amount".into();
        let mut rig = Rig::new(project);
        rig.content_state.data_version = 3;

        let read = |rig: &mut Rig| {
            rig.host()
                .automation_lane_points(&target, &param)
                .unwrap()
                .iter()
                .map(|p| (p.0, p.1))
                .collect::<Vec<_>>()
        };
        {
            let mut host = rig.host();
            host.set_automation_point_preview(&target, &param, Beats(8.25), 0.75, Beats(9.5), 0.25);
        }
        assert_eq!(read(&mut rig), vec![(Beats(4.5), 0.5), (Beats(9.5), 0.25)]);
        let project_points = rig.project.settings.master_effects[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0]
            .points
            .clone();
        assert_eq!(
            project_points,
            pts(0.5, 0.75),
            "the project lane is untouched"
        );

        // Cancel: restore the original points, then clear.
        {
            let mut host = rig.host();
            host.restore_automation_lane_preview(
                &target,
                &param,
                Some(&[
                    (Beats(4.5), 0.5, UiSegmentShape::Linear),
                    (Beats(8.25), 0.75, UiSegmentShape::Linear),
                ]),
            );
            host.clear_automation_previews();
        }
        assert!(
            rig.edits.preview.is_empty(),
            "a cancel drops the record at once"
        );

        // Commit: the record lands at data version 4.
        {
            let mut host = rig.host();
            host.set_automation_point_preview(&target, &param, Beats(8.25), 0.75, Beats(9.5), 0.25);
            host.clear_automation_previews();
            host.commit_automation_point_move(
                &target,
                &param,
                (Beats(8.25), 0.75, UiSegmentShape::Linear),
                (Beats(9.5), 0.25, UiSegmentShape::Linear),
            );
        }
        assert_eq!(read(&mut rig), vec![(Beats(4.5), 0.5), (Beats(9.5), 0.25)]);
        let sent = rig.sent();
        assert!(
            sent.iter()
                .any(|c| matches!(c, crate::content_command::ContentCommand::Execute(_)))
        );
        rig.edits.preview.snapshot_arrived(3);
        assert_eq!(read(&mut rig).len(), 2);
        assert!(
            !rig.edits.preview.is_empty(),
            "version 3 does not carry the commit"
        );
        rig.edits.preview.snapshot_arrived(4);
        assert!(rig.edits.preview.is_empty());
        assert_eq!(read(&mut rig), vec![(Beats(4.5), 0.5), (Beats(8.25), 0.75)]);
    }

    /// With nothing recorded the reads are the project's own, and the
    /// record allocates nothing.
    #[test]
    fn an_empty_record_reads_the_project_as_it_is() {
        let mut project = Project::default();
        video_layer(&mut project, "V0");
        video_layer(&mut project, "V1");
        add_clip(&mut project, 0, 0.5, 1.25);
        add_clip(&mut project, 1, 2.0, 0.75);
        let preview = GesturePreview::default();
        assert!(preview.is_empty());
        assert_eq!(
            preview.clips.capacity() + preview.added.capacity() + preview.lanes.capacity(),
            0
        );
        assert_eq!(drawn_table(&preview, &project), table(&project));
        for layer in 0..2 {
            let ids: Vec<ClipId> = preview
                .clips_on_layer(&project, layer)
                .map(|d| d.clip.id.clone())
                .collect();
            let own: Vec<ClipId> = project.timeline.layers[layer]
                .clips
                .iter()
                .map(|c| c.id.clone())
                .collect();
            assert_eq!(ids, own);
        }
    }
}
