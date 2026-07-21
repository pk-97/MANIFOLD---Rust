//! Marker-related dispatch: click, drag, delete, rename.
use manifold_core::Beats;
use manifold_core::MarkerId;
use manifold_core::project::Project;
use manifold_editing::commands::marker::{DeleteMarkerCommand, MoveMarkerCommand};
use manifold_ui::MarkerAction;

use super::DispatchResult;
use super::scrub::{ResolvedScrub, ScrubState};
use crate::app::SelectionState;
use crate::content_command::ContentCommand;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_marker(
    action: &MarkerAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    _ui: &mut UIRoot,
    selection: &mut SelectionState,
    scrub: &mut ScrubState,
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
            DispatchResult::structural()
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

        // ── Drag move: update marker position for live preview ─
        MarkerAction::MarkerDragMoved(marker_id_str, new_beat) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            if let Some(marker) = project.timeline.find_marker_mut(&marker_id) {
                marker.beat = Beats::from_f32(*new_beat);
            }
            project.timeline.sort_markers();
            if let Some(ResolvedScrub::Marker { live, .. }) = &mut scrub.active {
                *live = *new_beat;
            }
            DispatchResult::structural()
        }

        // ── Drag end: commit MoveMarkerCommand ─────────────────
        MarkerAction::MarkerDragEnded(marker_id_str, final_beat) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            let baseline = match &scrub.active {
                Some(ResolvedScrub::Marker { baseline, .. }) => Some(*baseline),
                _ => None,
            };
            scrub.active = None;
            if let Some(old_beat) = baseline {
                // Only commit if the marker actually moved
                if (old_beat - *final_beat).abs() > 0.001 {
                    // Undo the live preview mutation first — the command will redo it
                    if let Some(marker) = project.timeline.find_marker_mut(&marker_id) {
                        marker.beat = Beats::from_f32(old_beat);
                    }
                    project.timeline.sort_markers();

                    let mut cmd: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(MoveMarkerCommand::new(
                            marker_id,
                            Beats::from_f32(old_beat),
                            Beats::from_f32(*final_beat),
                        ));
                    cmd.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
                }
            }
            DispatchResult::structural()
        }

        // ── Right-click: context menu (placeholder) ────────────
        MarkerAction::MarkerRightClicked(_marker_id_str) => {
            // Future: show context menu with Delete / Change Color
            DispatchResult::handled()
        }

        // ── Delete selected markers ────────────────────────────
        MarkerAction::DeleteSelectedMarkers => {
            let ids: Vec<MarkerId> = selection.selected_marker_ids.iter().cloned().collect();
            if ids.is_empty() {
                return DispatchResult::handled();
            }

            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for id in &ids {
                let mut cmd: Box<dyn manifold_editing::command::Command + Send> =
                    Box::new(DeleteMarkerCommand::new(id.clone()));
                cmd.execute(project);
                commands.push(cmd);
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::ExecuteBatch(commands, "Delete Markers".into()),
            );
            selection.selected_marker_ids.clear();
            DispatchResult::structural()
        }

    }
}

#[cfg(test)]
mod tests {
    use manifold_core::marker::TimelineMarker;
    use manifold_core::{Beats, project::Project};

    /// BUG-280 regression. Marker drag is driven by `ViewportDrag::MarkerDrag`,
    /// outside `InteractionOverlay`'s `DragMode`, so a mid-gesture content-thread
    /// snapshot swap used to revert the in-flight `marker.beat`. This mirrors
    /// `bound_node_param_drag_survives_snapshot_stomp`: build the guard the live
    /// `MarkerDragMoved` arm installs (now a `ResolvedScrub::Marker` in
    /// `ScrubState.active`), run the real restore entry point on a stomped (stale)
    /// project, and confirm the dragged beat survives.
    #[test]
    fn marker_drag_survives_snapshot_stomp() {
        use crate::ui_bridge::scrub::{ResolvedScrub, ScrubState};

        let mut project = Project::default();
        let marker = TimelineMarker::new(Beats::from_f32(0.0));
        let marker_id = marker.id.clone();
        project.timeline.markers.push(marker);

        let scrub = ScrubState {
            active: Some(ResolvedScrub::Marker {
                marker_id: marker_id.clone(),
                baseline: 0.0,
                live: 5.0,
            }),
        };

        // A full snapshot lands mid-drag carrying the stale pre-drag project;
        // app_render restores the guarded drag onto it via the real restore path.
        let mut stomped = project.clone();
        scrub.restore_dragged(&mut stomped);

        let after = stomped
            .timeline
            .find_marker(&marker_id)
            .expect("marker still present")
            .beat
            .as_f32();
        assert_eq!(
            after, 5.0,
            "marker-drag stomp must be undone so the marker doesn't revert mid-gesture"
        );
    }
}
