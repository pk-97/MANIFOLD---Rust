//! Marker-related dispatch: click, drag, delete, rename.
use manifold_core::Beats;
use manifold_core::MarkerId;
use manifold_editing::commands::marker::{DeleteMarkerCommand, MoveMarkerCommand};
use manifold_ui::MarkerAction;

use super::DispatchResult;
use super::scrub::{ResolvedScrub, ScrubState, send_commit};
use crate::app::SelectionState;
use crate::content_command::ContentCommand;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_marker(
    action: &MarkerAction,
    project: &mut crate::ui_bridge::ProjectRef<'_>,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    _ui: &mut UIRoot,
    selection: &mut SelectionState,
    scrub: &mut ScrubState,
    data_version: u64,
) -> DispatchResult {
    match action {
        // ── Click: select/multi-select marker ──────────────────
        MarkerAction::MarkerClicked(marker_id_str, modifiers) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            if modifiers.shift {
                selection.toggle_marker_selection(marker_id);
            } else {
                selection.select_marker(marker_id);
            }
            DispatchResult::handled()
        }

        // ── DoubleClick: intercepted in app_render.rs for text input
        MarkerAction::MarkerDoubleClicked(_) => DispatchResult::handled(),

        // ── Drag start: snapshot beat for undo ─────────────────
        MarkerAction::MarkerDragStarted(marker_id_str) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            // The pre-drag beat is the undo baseline; the guard (baseline == live
            // at Begin) unifies into `ScrubState.active` — this gesture is
            // viewport-driven, not a `PanelAction::Scrub` family, but shares the
            // one snapshot-stomp restore slot (P-I).
            let baseline = project
                .timeline
                .find_marker(&marker_id)
                .map(|m| m.beat.as_f32());
            if let Some(beat) = baseline {
                scrub.active = Some(ResolvedScrub::Marker {
                    marker_id: marker_id.clone(),
                    baseline: beat,
                    live: beat,
                });
            }
            // Select the marker being dragged
            selection.select_marker(marker_id);
            DispatchResult::handled()
        }

        // ── Drag move: the dragged marker draws at the live beat ─
        MarkerAction::MarkerDragMoved(_, new_beat) => {
            if let Some(ResolvedScrub::Marker { live, .. }) = &mut scrub.active {
                *live = *new_beat;
            }
            let (_, preview) = project.preview_mut();
            preview.record_marker();
            DispatchResult::handled()
        }

        // ── Drag end: commit MoveMarkerCommand from the record ─
        MarkerAction::MarkerDragEnded(marker_id_str, final_beat) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            let (_, preview) = project.preview_mut();
            preview.record_marker();
            if let Some(record) = scrub.active.take()
                && let ResolvedScrub::Marker { baseline, .. } = &record
                && (*baseline - *final_beat).abs() > 0.001
            {
                let cmd = MoveMarkerCommand::new(
                    marker_id,
                    Beats::from_f32(*baseline),
                    Beats::from_f32(*final_beat),
                );
                send_commit(content_tx, scrub, data_version, record, Box::new(cmd));
            }
            DispatchResult::handled()
        }

        // ── Right-click: context menu (placeholder) ────────────
        MarkerAction::MarkerRightClicked(_marker_id_str) => {
            // Future: show context menu with Delete / Change Color
            DispatchResult::handled()
        }

        // ── Delete selected markers ────────────────────────────
        MarkerAction::DeleteSelectedMarkers => {
            let ids: Vec<MarkerId> = selection.selected_marker_ids().iter().cloned().collect();
            if ids.is_empty() {
                return DispatchResult::handled();
            }

            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for id in &ids {
                let cmd: Box<dyn manifold_editing::command::Command + Send> =
                    Box::new(DeleteMarkerCommand::new(id.clone()));
                commands.push(cmd);
            }
            project.execute_batch(commands, "Delete Markers".into());
            selection.clear_marker_selection();
            DispatchResult::handled()
        }
    }
}

#[cfg(test)]
mod drag_preview_tests {
    use crate::frame_plan::{FrameInputs, MainRegion, Projection, plan_frame};
    use crate::gesture_preview::UiEdits;
    use manifold_core::marker::TimelineMarker;
    use manifold_core::{Beats, project::Project};
    use manifold_ui::MarkerAction;

    fn inputs(edits: &UiEdits) -> FrameInputs {
        FrameInputs {
            parts: edits.stamps.parts(),
            preview: edits.preview.version(),
            ..Default::default()
        }
    }

    /// A marker drag is a preview (D12): each move moves the marker preview
    /// version and no stamp, so the plan re-projects the markers and moves the
    /// viewport flags in place, nothing else; the drop commits and stamps Markers.
    #[test]
    fn marker_drag_moves_the_preview_and_the_drop_stamps_markers() {
        let mut project = Project::default();
        let marker = TimelineMarker::new(Beats::from_f32(0.0));
        let id = marker.id.as_str().to_string();
        project.timeline.markers.push(marker);
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let mut edits = UiEdits::default();
        let mut run = |action: MarkerAction, edits: &mut UiEdits, project: &mut Project| {
            let mut project_ref = crate::ui_bridge::ProjectRef::new(project, edits, &tx);
            super::dispatch_marker(
                &action,
                &mut project_ref,
                &tx,
                &mut ui,
                &mut selection,
                &mut scrub,
                0,
            );
        };

        run(
            MarkerAction::MarkerDragStarted(id.clone()),
            &mut edits,
            &mut project,
        );
        let mut last = inputs(&edits);
        for beat in [2.0, 4.0] {
            run(
                MarkerAction::MarkerDragMoved(id.clone(), beat),
                &mut edits,
                &mut project,
            );
            let next = inputs(&edits);
            assert_eq!(next.parts, last.parts, "a preview moves no stamp");
            let plan = plan_frame(&last, &next);
            assert!(plan.projections.contains(Projection::MarkerPreview));
            assert!(!plan.projections.contains(Projection::ProjectSync));
            assert!(!plan.projections.contains(Projection::InspectorSync));
            assert!(plan.viewport.markers && plan.in_place.contains(MainRegion::Viewport));
            assert!(
                !plan.rebuild.contains(MainRegion::Viewport),
                "a marker drag moves flags in place"
            );
            last = next;
        }
        assert!(
            rx.try_recv().is_err(),
            "the content thread never sees a preview"
        );

        run(
            MarkerAction::MarkerDragEnded(id.clone(), 4.0),
            &mut edits,
            &mut project,
        );
        assert_ne!(
            inputs(&edits).preview,
            last.preview,
            "the drop re-projects the markers"
        );
        assert!(rx.try_recv().is_ok(), "the drop sends its command");
    }
}
