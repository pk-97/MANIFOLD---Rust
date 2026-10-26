//! Drag-preview projections. A preview move
//! updates exactly the clips and lanes the `GesturePreview` recorded, in
//! place; the full projections (`sync_clip_positions`,
//! `sync_automation_lane_geometry`) run on commit, cancel and snapshot
//! acceptance. Both return false when the viewport cannot show the gesture in
//! place (a clip or lane it does not hold), and the caller re-projects whole.

use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::work_count::{WorkKind, record_work};

use super::timeline::{audio_waveform_breakpoints_into, clip_base_color};
use crate::gesture_preview::GesturePreview;
use crate::ui_root::UIRoot;

/// Write each recorded clip's start, duration, in-point and layer into its
/// viewport entry. A layer change moves the entry to the new layer's bucket
/// and recomputes its colour; an audio clip refills its waveform breakpoints.
/// The name is never recomputed.
pub(crate) fn project_clip_preview(
    ui: &mut UIRoot,
    project: &Project,
    preview: &GesturePreview,
) -> bool {
    // A clip the viewport does not hold (a region-move split) is a structural
    // change this path cannot show.
    let total: usize = project
        .timeline
        .layers
        .iter()
        .map(|l| l.clips.len())
        .sum::<usize>()
        + preview.added_count();
    if total != ui.viewport.clip_total() {
        return false;
    }
    for rec in preview.clips() {
        let Some(drawn) = preview.clip(project, rec.id.as_str()) else {
            return false;
        };
        let layer_index = drawn.layer_index;
        let layer = &project.timeline.layers[layer_index];
        let in_point = drawn.in_point.0 as f32;
        let Some(shown) = ui.viewport.preview_clip(&rec.id) else {
            return false;
        };
        let layer_moved = shown.layer_index != layer_index;
        if !layer_moved
            && shown.start_beat == drawn.start
            && shown.duration_beats == drawn.duration
            && shown.in_point_seconds == in_point
        {
            continue;
        }
        if layer_moved && !ui.viewport.move_clip_to_layer(&rec.id, layer_index) {
            return false;
        }
        let Some(entry) = ui.viewport.preview_clip_mut(&rec.id) else {
            return false;
        };
        if layer_moved {
            entry.color = clip_base_color(layer, drawn.clip, 0.86);
            entry.is_generator = layer.layer_type == LayerType::Generator;
            entry.is_audio = layer.is_audio();
        }
        entry.start_beat = drawn.start;
        entry.duration_beats = drawn.duration;
        entry.in_point_seconds = in_point;
        if entry.is_audio {
            audio_waveform_breakpoints_into(&drawn, project, &mut entry.waveform_breakpoints);
        }
        record_work(WorkKind::ClipPreviewUpdate);
    }
    true
}

/// Refill each recorded lane's entry and rebuild that lane's screen alone.
pub(crate) fn project_lane_preview(
    ui: &mut UIRoot,
    project: &Project,
    preview: &GesturePreview,
    automation_visible: bool,
) -> bool {
    // Lanes are only held while automation mode shows them.
    if !automation_visible {
        return true;
    }
    for rec in preview.lanes() {
        let Some(instance) = project.preset_instance(&rec.target) else {
            return false;
        };
        let target = crate::editing_host::to_ui_graph_target(&rec.target);
        let mut points_ok = true;
        let held = ui
            .viewport
            .update_automation_lane(&target, &rec.param_id, |entry| {
                points_ok = crate::ui_translate::preview_lane_points_into(
                    instance,
                    rec.param_id.as_ref(),
                    rec.points(),
                    &mut entry.points,
                );
            });
        if !held || !points_ok {
            return false;
        }
        record_work(WorkKind::LanePreviewUpdate);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effects::{AutomationLane, AutomationPoint, PresetInstance, SegmentShape};
    use manifold_core::work_count::thread_work_snapshot;
    use manifold_core::{Beats, ClipId, GraphTarget};
    use manifold_editing::service::EditingService;
    use manifold_ui::panels::viewport::TrackInfo;

    fn video_project(layers: usize, clips: usize) -> Project {
        let mut project = Project::default();
        for i in 0..layers {
            project.timeline.add_layer(
                "V",
                LayerType::Video,
                manifold_core::PresetTypeId::from_string("Video".to_string()),
            );
            project.timeline.layers[i].layer_color = manifold_core::color::Color {
                r: 0.1 + 0.4 * i as f32,
                g: 0.3,
                b: 0.6,
                a: 1.0,
            };
        }
        for i in 0..clips {
            let (mut create, _) = EditingService::create_clip_at_position(
                &project,
                Beats(i as f64 * 2.0),
                0,
                Beats(1.0),
            )
            .unwrap();
            create.execute(&mut project);
        }
        project
    }

    fn shown(ui: &UIRoot) -> Vec<String> {
        (0..ui.viewport.layer_count())
            .flat_map(|l| {
                ui.viewport
                    .clips_for_layer(l)
                    .iter()
                    .map(|c| format!("{c:?}"))
            })
            .collect()
    }

    fn projected(project: &Project, preview: &GesturePreview) -> UIRoot {
        let mut ui = UIRoot::new();
        ui.viewport
            .set_tracks(vec![TrackInfo::default(); project.timeline.layers.len()]);
        super::super::timeline::sync_clip_positions(&mut ui, project, preview);
        ui
    }

    /// Dragging one clip in a 500-clip project updates that clip's entry and
    /// nothing else: no whole-project clip projection, one preview update.
    #[test]
    fn clip_drag_updates_one_entry_of_five_hundred() {
        let project = video_project(2, 500);
        let mut ui = projected(&project, &GesturePreview::default());
        let id: ClipId = project.timeline.layers[0].clips[250].id.clone();
        let before = shown(&ui);
        assert_eq!(before.len(), 500);

        let mut preview = GesturePreview::default();
        let layer_id = project.timeline.layers[0].layer_id.clone();
        let in_point = project.timeline.layers[0].clips[250].in_point;
        preview.place_clip(&id, &layer_id, Beats(701.25), Beats(3.5), in_point);

        let work = thread_work_snapshot();
        assert!(project_clip_preview(&mut ui, &project, &preview));
        let work = thread_work_snapshot().since(&work);
        assert_eq!(work.get(WorkKind::ClipPreviewUpdate), 1);
        assert_eq!(work.get(WorkKind::ClipProjection), 0);

        let after = shown(&ui);
        assert_eq!(after.len(), 500);
        let changed: Vec<usize> = (0..500).filter(|&i| before[i] != after[i]).collect();
        assert_eq!(changed, vec![250], "exactly the dragged clip changed");
        let entry = ui.viewport.preview_clip(&id).unwrap();
        assert_eq!(
            (entry.start_beat, entry.duration_beats),
            (Beats(701.25), Beats(3.5))
        );

        // A second step of the same drag writes the entry in place without
        // allocating (counted when the `work-trace` allocator is on).
        preview.place_clip(&id, &layer_id, Beats(702.5), Beats(3.5), in_point);
        let work = thread_work_snapshot();
        assert!(project_clip_preview(&mut ui, &project, &preview));
        let steady = thread_work_snapshot().since(&work);
        assert_eq!(steady.get(WorkKind::ClipPreviewUpdate), 1);
        assert_eq!(
            steady.get(WorkKind::FrameAlloc),
            0,
            "a steady clip drag allocates nothing"
        );
        let after = shown(&ui);

        // A second call with nothing new to write does no work.
        let work = thread_work_snapshot();
        assert!(project_clip_preview(&mut ui, &project, &preview));
        assert_eq!(
            thread_work_snapshot()
                .since(&work)
                .get(WorkKind::ClipPreviewUpdate),
            0
        );

        // The in-place result is what a whole projection shows.
        let whole = projected(&project, &preview);
        assert_eq!(shown(&whole), after);
        assert_eq!(
            project.timeline.layers[0].clips[250].start_beat,
            Beats(500.0),
            "the project is untouched"
        );
    }

    /// A clip dragged to another layer lands in that layer's bucket with that
    /// layer's colour; the name stays and the other clips are untouched.
    #[test]
    fn cross_layer_clip_drag_lands_in_the_right_bucket_with_the_right_colour() {
        let project = video_project(2, 40);
        let mut ui = projected(&project, &GesturePreview::default());
        let id: ClipId = project.timeline.layers[0].clips[7].id.clone();
        let name_before = ui.viewport.preview_clip(&id).unwrap().name.clone();
        let others_before: Vec<String> = shown(&ui)
            .into_iter()
            .filter(|s| !s.contains(id.as_str()))
            .collect();

        let (start, duration, in_point) = {
            let clip = &project.timeline.layers[0].clips[7];
            (clip.start_beat, clip.duration_beats, clip.in_point)
        };
        let other_layer = project.timeline.layers[1].layer_id.clone();
        let mut preview = GesturePreview::default();
        preview.place_clip(&id, &other_layer, start, duration, in_point);
        assert!(project_clip_preview(&mut ui, &project, &preview));

        assert_eq!(ui.viewport.clips_for_layer(0).len(), 39);
        assert_eq!(ui.viewport.clips_for_layer(1).len(), 1);
        let entry = ui.viewport.preview_clip(&id).unwrap();
        assert_eq!(entry.layer_index, 1);
        assert_eq!(entry.name, name_before, "the name is never recomputed");
        let layer = &project.timeline.layers[1];
        let clip = &project.timeline.layers[0].clips[7];
        assert_eq!(
            entry.color,
            super::super::timeline::clip_base_color(layer, clip, 0.86)
        );
        let others_after: Vec<String> = shown(&ui)
            .into_iter()
            .filter(|s| !s.contains(id.as_str()))
            .collect();
        let (mut a, mut b) = (others_before, others_after);
        a.sort();
        b.sort();
        assert_eq!(a, b, "no other clip changed (bucket order is free)");
        assert_eq!(ui.viewport.clip_total(), 40);

        // Every later preview write still finds every clip where the index says.
        let other: ClipId = project.timeline.layers[0].clips[3].id.clone();
        let layer_0 = project.timeline.layers[0].layer_id.clone();
        let other_clip = &project.timeline.layers[0].clips[3];
        preview.place_clip(
            &other,
            &layer_0,
            Beats(900.5),
            other_clip.duration_beats,
            other_clip.in_point,
        );
        assert!(project_clip_preview(&mut ui, &project, &preview));
        assert_eq!(
            ui.viewport.preview_clip(&other).unwrap().start_beat,
            Beats(900.5)
        );
        // Names number a layer's clips as drawn, so compare placement only.
        let placed = |ui: &UIRoot| -> Vec<(String, usize, Beats, Beats)> {
            let mut rows: Vec<_> = (0..ui.viewport.layer_count())
                .flat_map(|l| {
                    ui.viewport
                        .clips_for_layer(l)
                        .iter()
                        .map(|c| {
                            (
                                c.clip_id.to_string(),
                                c.layer_index,
                                c.start_beat,
                                c.duration_beats,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            rows
        };
        assert_eq!(
            placed(&projected(&project, &preview)),
            placed(&ui),
            "a whole projection shows the same clips"
        );
    }

    /// A clip the viewport does not hold (a region-move split added one) is
    /// not an in-place case.
    #[test]
    fn an_added_clip_falls_back_to_the_whole_projection() {
        let project = video_project(1, 3);
        let mut ui = projected(&project, &GesturePreview::default());
        let id = project.timeline.layers[0].clips[1].id.clone();
        let split = EditingService::split_clip_at_beat(&project, id.as_str(), Beats(2.5)).unwrap();
        let mut preview = GesturePreview::default();
        preview.record_split(&project, split);
        assert!(!project_clip_preview(&mut ui, &project, &preview));
        // The whole projection shows the pieces; the next preview step is in place.
        super::super::timeline::sync_clip_positions(&mut ui, &project, &preview);
        assert_eq!(ui.viewport.clip_total(), 4);
        assert!(project_clip_preview(&mut ui, &project, &preview));
        assert_eq!(
            project.timeline.layers[0].clips.len(),
            3,
            "the project is untouched"
        );
    }

    fn three_lane_project() -> (Project, Vec<GraphTarget>) {
        let mut project = Project::default();
        project.timeline.add_layer(
            "V",
            LayerType::Video,
            manifold_core::PresetTypeId::from_string("Video".to_string()),
        );
        let mut targets = Vec::new();
        for _ in 0..3 {
            let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
            fx.init_defaults();
            fx.automation_lanes = Some(vec![AutomationLane {
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
            targets.push(GraphTarget::Effect(fx.id.clone()));
            project.settings.master_effects.push(fx);
        }
        (project, targets)
    }

    fn lane_ui(project: &Project, targets: &[GraphTarget]) -> UIRoot {
        use manifold_ui::panels::viewport::ViewportAutomationLane;
        let mut ui = UIRoot::new();
        ui.viewport.set_tracks(vec![TrackInfo::default()]);
        ui.viewport.set_automation_lane_layout(&[vec![112.0; 3]]);
        ui.viewport
            .rebuild_mapper_layout(&[manifold_ui::view::UiLayer {
                layer_id: project.timeline.layers[0].layer_id.clone(),
                parent_layer_id: None,
                layer_type: manifold_ui::types::LayerType::Video,
                is_collapsed: false,
                automation_lane_count: 3,
            }]);
        ui.viewport
            .prepare_layout(&manifold_ui::layout::ScreenLayout::new(1920.0, 1080.0));
        let lanes = targets
            .iter()
            .map(|t| {
                let instance = project.preset_instance(t).unwrap();
                let lane = &instance.automation_lanes.as_ref().unwrap()[0];
                let mut points = Vec::new();
                assert!(crate::ui_translate::preview_lane_points_into(
                    instance,
                    lane.param_id.as_ref(),
                    &lane.points,
                    &mut points
                ));
                ViewportAutomationLane {
                    layer_index: 0,
                    lane: manifold_ui::view::UiAutomationLane {
                        effect_id: instance.id.clone(),
                        param_id: lane.param_id.clone(),
                        target: crate::editing_host::to_ui_graph_target(t),
                        label: "Mirror: amount".into(),
                        points,
                        param_min: 0.0,
                        param_max: 1.0,
                        whole_numbers: false,
                        placeholder: false,
                        display: manifold_ui::view::UiLaneDisplay::default(),
                    },
                }
            })
            .collect();
        ui.viewport.set_automation_lanes(lanes);
        ui
    }

    /// A point drag on one of three lanes refills that lane, leaves the
    /// other two screens as they were, and matches a whole rebuild.
    #[test]
    fn lane_drag_with_three_lanes_updates_one() {
        let (project, targets) = three_lane_project();
        let mut ui = lane_ui(&project, &targets);
        let screens = |ui: &UIRoot| -> Vec<String> {
            ui.viewport
                .automation_lane_screens()
                .iter()
                .map(|s| format!("{s:?}"))
                .collect()
        };
        let before = screens(&ui);
        assert_eq!(before.len(), 3, "three lane strips are on screen");

        let mut preview = GesturePreview::default();
        preview
            .lane_points_mut(&project, &targets[1], "amount")
            .unwrap()[1]
            .value = 0.1;

        let work = thread_work_snapshot();
        assert!(project_lane_preview(&mut ui, &project, &preview, true));
        let work = thread_work_snapshot().since(&work);
        assert_eq!(work.get(WorkKind::LanePreviewUpdate), 1);
        assert_eq!(work.get(WorkKind::LaneProjection), 0);

        let after = screens(&ui);
        assert_eq!(before[0], after[0]);
        assert_eq!(before[2], after[2]);
        assert_ne!(before[1], after[1]);
        let dragged = &ui.viewport.automation_lane_screens()[1];
        assert!((dragged.dots[1].value_norm - 0.1).abs() < 1e-6);
        // A second step refills the same buffers: no allocation.
        preview
            .lane_points_mut(&project, &targets[1], "amount")
            .unwrap()[1]
            .value = 0.2;
        let work = thread_work_snapshot();
        assert!(project_lane_preview(&mut ui, &project, &preview, true));
        let steady = thread_work_snapshot().since(&work);
        assert_eq!(steady.get(WorkKind::LanePreviewUpdate), 1);
        assert_eq!(
            steady.get(WorkKind::FrameAlloc),
            0,
            "a steady lane drag allocates nothing"
        );
        let mut dragged = project.clone();
        dragged
            .preset_instance_mut(&targets[1])
            .unwrap()
            .automation_lanes
            .as_mut()
            .unwrap()[0]
            .points[1]
            .value = 0.2;
        let rebuilt = lane_ui(&dragged, &targets);
        assert_eq!(
            screens(&ui),
            screens(&rebuilt),
            "the in-place result is what a whole rebuild makes"
        );
    }
}
