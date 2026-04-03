//! Marker-related dispatch: click, drag, delete, rename.
use manifold_core::Beats;
use manifold_core::MarkerId;
use manifold_core::project::Project;
use manifold_editing::commands::marker::{DeleteMarkerCommand, MoveMarkerCommand};
use manifold_ui::PanelAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::content_command::ContentCommand;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_marker(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    _ui: &mut UIRoot,
    selection: &mut SelectionState,
    drag_snapshot: &mut Option<f32>,
) -> DispatchResult {
    match action {
        // ── Click: select/multi-select marker ──────────────────
        PanelAction::MarkerClicked(marker_id_str, modifiers) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            if modifiers.shift {
                selection.toggle_marker_selection(marker_id);
            } else {
                selection.select_marker(marker_id);
            }
            DispatchResult::structural()
        }

        // ── DoubleClick: intercepted in app_render.rs for text input
        PanelAction::MarkerDoubleClicked(_) => DispatchResult::handled(),

        // ── Drag start: snapshot beat for undo ─────────────────
        PanelAction::MarkerDragStarted(marker_id_str) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            *drag_snapshot = project
                .timeline
                .find_marker(&marker_id)
                .map(|m| m.beat.as_f32());
            // Select the marker being dragged
            selection.select_marker(marker_id);
            DispatchResult::handled()
        }

        // ── Drag move: update marker position for live preview ─
        PanelAction::MarkerDragMoved(marker_id_str, new_beat) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            if let Some(marker) = project.timeline.find_marker_mut(&marker_id) {
                marker.beat = Beats::from_f32(*new_beat);
            }
            project.timeline.sort_markers();
            DispatchResult::structural()
        }

        // ── Drag end: commit MoveMarkerCommand ─────────────────
        PanelAction::MarkerDragEnded(marker_id_str, final_beat) => {
            let marker_id = MarkerId::new(marker_id_str.as_str());
            if let Some(old_beat) = drag_snapshot.take() {
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
        PanelAction::MarkerRightClicked(_marker_id_str) => {
            // Future: show context menu with Delete / Change Color
            DispatchResult::handled()
        }

        // ── Delete selected markers ────────────────────────────
        PanelAction::DeleteSelectedMarkers => {
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

        _ => DispatchResult::unhandled(),
    }
}
