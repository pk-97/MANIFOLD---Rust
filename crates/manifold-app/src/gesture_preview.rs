//! The gesture preview (UI_INVALIDATION_DESIGN.md D12).
//!
//! While a clip move or trim, a region move or an automation drag is in
//! flight, the editing host writes the dragged state into the local project
//! for display and records it here. A preview write never moves a part stamp;
//! the host's committing method stamps once and clears the preview. Because
//! the preview is recorded, an accepted snapshot mid-drag is re-covered by
//! [`GesturePreview::reapply`], which is what lets the mid-drag snapshot hold
//! go.
//!
//! P4b keeps the display writes in the local project and replays them over a
//! snapshot, rather than overlaying them at projection time; the projection
//! side reads the project as before.

use manifold_core::change::ChangeSet;
use manifold_core::effects::{AutomationLane, AutomationPoint, ParamId};
use manifold_core::project::Project;
use manifold_core::{Beats, ClipId, GraphTarget, LayerId, Seconds};
use manifold_editing::command::Command;

use crate::frame_plan::UiStamps;

/// The two ledgers every UI-side writer reaches: the UI's part stamps and the
/// in-flight gesture preview. Owned by the App and by the script runner.
#[derive(Default)]
pub(crate) struct UiEdits {
    pub stamps: UiStamps,
    pub preview: GesturePreview,
}

struct ClipPreview {
    id: ClipId,
    layer: LayerId,
    start: Beats,
    duration: Beats,
    in_point: Seconds,
}

struct LanePreview {
    target: GraphTarget,
    param_id: ParamId,
    points: Vec<AutomationPoint>,
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
    /// A region move's boundary splits. Their ids are fixed at construction,
    /// so replaying them over a snapshot yields the same pieces; on commit
    /// they lead the composite so its undo reverses them last.
    splits: Vec<Box<dyn Command>>,
    clips: Vec<ClipPreview>,
    lanes: Vec<LanePreview>,
}

impl GesturePreview {
    /// `FrameInputs.preview`.
    pub(crate) fn version(&self) -> PreviewVersions {
        self.version
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.splits.is_empty() && self.clips.is_empty() && self.lanes.is_empty()
    }

    pub(crate) fn set_splits(&mut self, splits: Vec<Box<dyn Command>>) {
        self.splits = splits;
        self.version.clips += 1;
    }

    /// Record a clip's dragged layer and timing, read back from the project
    /// right after the host's display write.
    pub(crate) fn record_clip(&mut self, project: &Project, clip_id: &str) {
        let Some((layer, clip)) = project
            .timeline
            .layers
            .iter()
            .find_map(|l| l.clips.iter().find(|c| c.id == clip_id).map(|c| (l, c)))
        else {
            return;
        };
        let entry = ClipPreview {
            id: clip.id.clone(),
            layer: layer.layer_id.clone(),
            start: clip.start_beat,
            duration: clip.duration_beats,
            in_point: clip.in_point,
        };
        match self.clips.iter_mut().find(|c| c.id == entry.id) {
            Some(slot) => *slot = entry,
            None => self.clips.push(entry),
        }
        self.version.clips += 1;
    }

    /// Record a lane's dragged points, read back after the host's write.
    pub(crate) fn record_lane(&mut self, project: &Project, target: &GraphTarget, param_id: &str) {
        let Some(lane) = project
            .preset_instance(target)
            .and_then(|inst| inst.automation_lanes.as_ref())
            .and_then(|lanes| lanes.iter().find(|l| l.param_id.as_ref() == param_id))
        else {
            return;
        };
        match self.lanes.iter_mut().find(|l| l.target == *target && l.param_id == lane.param_id) {
            Some(slot) => {
                slot.points.clear();
                slot.points.extend_from_slice(&lane.points);
            }
            None => self.lanes.push(LanePreview {
                target: target.clone(),
                param_id: lane.param_id.clone(),
                points: lane.points.clone(),
            }),
        }
        self.version.lanes += 1;
    }

    /// A marker drag wrote the dragged beat into the local project. The
    /// snapshot accept re-covers it through the scrub restore, so only the
    /// version is kept here.
    pub(crate) fn record_marker(&mut self) {
        self.version.markers += 1;
    }

    /// The clip side ends (commit or cancel): hand back the splits.
    pub(crate) fn take_clip_gesture(&mut self) -> Vec<Box<dyn Command>> {
        if !self.splits.is_empty() || !self.clips.is_empty() {
            self.version.clips += 1;
        }
        self.clips.clear();
        std::mem::take(&mut self.splits)
    }

    pub(crate) fn clear_lanes(&mut self) {
        if !self.lanes.is_empty() {
            self.lanes.clear();
            self.version.lanes += 1;
        }
    }

    /// Re-cover a freshly accepted snapshot with the in-flight gesture.
    pub(crate) fn reapply(&mut self, project: &mut Project) {
        if self.is_empty() {
            return;
        }
        for split in &mut self.splits {
            split.execute(project);
        }
        for clip in &self.clips {
            reapply_clip(project, clip);
        }
        if !self.clips.is_empty() || !self.splits.is_empty() {
            project.timeline.mark_clip_lookup_dirty();
        }
        for lane in &self.lanes {
            let Some(inst) = project.preset_instance_mut(&lane.target) else {
                continue;
            };
            let lanes = inst.automation_lanes.get_or_insert_with(Vec::new);
            match lanes.iter_mut().find(|l| l.param_id == lane.param_id) {
                Some(existing) => {
                    existing.points.clear();
                    existing.points.extend_from_slice(&lane.points);
                }
                None => lanes.push(AutomationLane {
                    param_id: lane.param_id.clone(),
                    enabled: true,
                    points: lane.points.clone(),
                }),
            }
        }
    }
}

fn reapply_clip(project: &mut Project, clip: &ClipPreview) {
    let timeline = &mut project.timeline;
    let Some(src) = timeline
        .layers
        .iter()
        .position(|l| l.clips.iter().any(|c| c.id == clip.id))
    else {
        return;
    };
    let Some(dst) = timeline.layer_index_for_id(&clip.layer) else {
        return;
    };
    if src != dst
        && let Some(moved) = timeline.layers[src].remove_clip(&clip.id)
    {
        timeline.layers[dst].restore_clip(moved);
    }
    let layer = &mut timeline.layers[dst];
    if let Some(c) = layer.clips.iter_mut().find(|c| c.id == clip.id) {
        let moved_start = c.start_beat != clip.start;
        c.start_beat = clip.start;
        c.duration_beats = clip.duration;
        c.in_point = clip.in_point;
        if moved_start {
            layer.mark_clips_unsorted();
        }
    }
}

/// The parts a committed batch wrote.
pub(crate) fn touches_of(commands: &[Box<dyn Command>]) -> ChangeSet {
    commands.iter().fold(ChangeSet::NONE, |acc, c| acc.union(c.touches()))
}

#[cfg(test)]
mod tests {
    //! INV-12: a preview never moves a part stamp. Drives a clip move and an
    //! automation point drag through the real `InteractionOverlay` and the
    //! real `AppEditingHost`.
    use super::*;
    use manifold_core::change::Part;
    use manifold_core::effects::{AutomationPoint, SegmentShape};
    use manifold_core::effects::PresetInstance;
    use manifold_editing::service::EditingService;
    use manifold_ui::interaction_overlay::{DragMode, InteractionOverlay};
    use manifold_ui::layout::ScreenLayout;
    use manifold_ui::node::Vec2;
    use manifold_ui::panels::Panel;
    use manifold_ui::panels::viewport::{TimelineViewportPanel, TrackInfo, ViewportAutomationLane, ViewportClip};
    use manifold_ui::tree::UITree;
    use manifold_ui::view::{UiAutomationLane, UiAutomationPoint, UiGraphTarget, UiLayer, UiSegmentShape};

    struct Rig {
        project: Project,
        tx: crossbeam_channel::Sender<crate::content_command::ContentCommand>,
        _rx: crossbeam_channel::Receiver<crate::content_command::ContentCommand>,
        content_state: crate::content_state::ContentState,
        cursor: manifold_ui::cursors::CursorManager,
        active_layer: Option<LayerId>,
        redraw: bool,
        edits: UiEdits,
    }

    impl Rig {
        fn new(project: Project) -> Self {
            let (tx, _rx) = crossbeam_channel::unbounded();
            Self {
                project,
                tx,
                _rx,
                content_state: Default::default(),
                cursor: Default::default(),
                active_layer: None,
                redraw: false,
                edits: UiEdits::default(),
            }
        }

        fn host(&mut self) -> crate::editing_host::AppEditingHost<'_> {
            crate::editing_host::AppEditingHost::new(
                crate::ui_bridge::ProjectRef::new(&mut self.project, &mut self.edits, &self.tx),
                &self.tx,
                &self.content_state,
                &mut self.cursor,
                &mut self.active_layer,
                &mut self.redraw,
            )
        }
    }

    fn viewport(layer_id: &LayerId, clips: Vec<ViewportClip>, lanes: Vec<ViewportAutomationLane>) -> TimelineViewportPanel {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(vec![TrackInfo::default()]);
        panel.set_clips(clips);
        panel.rebuild_mapper_layout(&[UiLayer {
            layer_id: layer_id.clone(),
            parent_layer_id: None,
            layer_type: manifold_ui::types::LayerType::Video,
            is_collapsed: false,
            automation_lane_count: lanes.len(),
        }]);
        let has_lanes = !lanes.is_empty();
        panel.set_automation_lanes(lanes);
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        if has_lanes {
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
            EditingService::create_clip_at_position(&mut project, Beats(0.0), 0, Beats(8.0)).unwrap();
        create.execute(&mut project);
        let layer_id = project.timeline.layers[0].layer_id.clone();
        let mut panel = viewport(
            &layer_id,
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
            Vec2::new(panel.beat_to_pixel(Beats::from_f32(beat)), panel.tracks_rect().y + 70.0)
        };
        let mut rig = Rig::new(project);
        let mut selection = manifold_ui::UIState::new();
        selection.select_clips(vec![clip_id.clone()]);
        let mut overlay = InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD);
        let before = rig.edits.stamps.parts();

        overlay.on_begin_drag(body(&panel, 2.0), &mut rig.host(), &mut selection, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::Move);
        for beat in [6.0, 10.0] {
            overlay.on_drag(body(&panel, beat), &mut rig.host(), &mut selection, &mut panel);
            assert_eq!(rig.edits.stamps.parts(), before, "a clip move preview moved a stamp");
        }
        assert!(rig.edits.preview.version().clips > 0, "the move wrote the preview");
        assert!(!rig.edits.preview.is_empty());
        overlay.on_end_drag(&mut rig.host());
        let moved = rig.edits.stamps.parts().moved_since(&before);
        assert!(moved.contains(Part::Clips), "the commit stamps Clips once: {moved:?}");
        assert!(rig.edits.preview.is_empty(), "the commit clears the preview");

        // ── Automation point drag ──
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
        fx.init_defaults();
        let fx_id = fx.id.clone();
        fx.automation_lanes = Some(vec![manifold_core::effects::AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![
                AutomationPoint { beat: Beats(4.0), value: 0.5, shape: SegmentShape::Linear },
                AutomationPoint { beat: Beats(8.0), value: 0.8, shape: SegmentShape::Linear },
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
            &layer_id,
            Vec::new(),
            vec![ViewportAutomationLane {
                layer_index: 0,
                lane: UiAutomationLane {
                    effect_id: fx_id,
                    param_id: "amount".into(),
                    target,
                    label: "Mirror: amount".into(),
                    points: vec![
                        UiAutomationPoint { beat: Beats(4.0), value_norm: 0.5, shape: UiSegmentShape::Linear, past_end: false },
                        UiAutomationPoint { beat: Beats(8.0), value_norm: 0.8, shape: UiSegmentShape::Linear, past_end: false },
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
        let press = Vec2::new(panel.beat_to_pixel(lane.dots[0].beat), lane.y_at_norm(lane.dots[0].value_norm));
        let mut rig = Rig::new(project);
        let mut selection = manifold_ui::UIState::new();
        let mut overlay = InteractionOverlay::new(manifold_ui::color::CLIP_VERTICAL_PAD);
        let before = rig.edits.stamps.parts();

        overlay.on_begin_drag(press, &mut rig.host(), &mut selection, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::AutomationPoint);
        for step in [Vec2::new(10.0, -15.0), Vec2::new(20.0, -30.0)] {
            overlay.on_drag(press + step, &mut rig.host(), &mut selection, &mut panel);
            assert_eq!(rig.edits.stamps.parts(), before, "a point drag preview moved a stamp");
        }
        assert!(!rig.edits.preview.is_empty(), "the point drag wrote the preview");
        overlay.on_end_drag(&mut rig.host());
        let moved = rig.edits.stamps.parts().moved_since(&before);
        assert!(moved.contains(Part::Automation), "the commit stamps Automation once: {moved:?}");
        assert!(rig.edits.preview.is_empty(), "the commit clears the preview");
    }

    #[test]
    fn reapply_recovers_a_snapshot_mid_drag() {
        let mut project = Project::default();
        project.timeline.add_layer(
            "A",
            manifold_core::types::LayerType::Video,
            manifold_core::PresetTypeId::from_string("Video".to_string()),
        );
        project.timeline.add_layer(
            "B",
            manifold_core::types::LayerType::Video,
            manifold_core::PresetTypeId::from_string("Video".to_string()),
        );
        let (mut create, clip_id) =
            EditingService::create_clip_at_position(&mut project, Beats(0.0), 0, Beats(8.0)).unwrap();
        create.execute(&mut project);
        let snapshot = project.clone();
        let mut rig = Rig::new(project);
        {
            use manifold_ui::timeline_editing_host::TimelineEditingHost;
            let mut host = rig.host();
            host.move_clip_to_layer(clip_id.as_str(), 1);
            host.set_clip_trim(clip_id.as_str(), Beats(4.0), Beats(2.0), Seconds(0.5));
        }
        let mut fresh = snapshot.clone();
        rig.edits.preview.reapply(&mut fresh);
        let layer = fresh.timeline.layers[1].clips.iter().find(|c| c.id == clip_id).expect("moved to B");
        assert_eq!((layer.start_beat, layer.duration_beats, layer.in_point), (Beats(4.0), Beats(2.0), Seconds(0.5)));
        assert!(fresh.timeline.layers[0].clips.is_empty());
    }
}
