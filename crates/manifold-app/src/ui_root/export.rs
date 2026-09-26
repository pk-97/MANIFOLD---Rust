//! Edge-triggered export notifications share the normal content channel, but
//! must not replace the full playback snapshot with their default fields.

use super::UIRoot;
use crate::content_state::ContentState;
use manifold_ui::color;

impl UIRoot {
    /// Consume each export notification exactly once while draining the channel.
    /// Returns false for normal playback snapshots. Per-file results may arrive
    /// between sections; only the run terminal closes the modal.
    pub(crate) fn consume_export_notification(&mut self, state: &ContentState) -> bool {
        let notification =
            state.is_exporting || state.export_finished.is_some() || state.export_run_finished;
        if !notification {
            return false;
        }
        if state.is_exporting {
            self.overlay_dirty |= self
                .export_progress
                .update(&state.export_status, state.export_progress);
        }
        if let Some(event) = &state.export_finished {
            if event.success {
                let filename = std::path::Path::new(&event.output_path)
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_else(|| event.output_path.as_str().into());
                self.toast
                    .show_with_accent(format!("{} — {filename}", event.message), color::GREEN_BASE);
            } else if event.message == "Export cancelled" {
                self.toast.show(event.message.clone());
            } else {
                self.toast
                    .show_with_accent(event.message.clone(), color::RED_BASE);
            }
            self.overlay_dirty = true;
        }
        if state.export_run_finished {
            self.overlay_dirty |= self.export_progress.finish();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_state::ExportFinishedEvent;
    use manifold_ui::input::{Key, Modifiers, UIEvent};
    use manifold_ui::node::{NodeId, Vec2};
    use manifold_ui::panels::{PanelAction, ProjectAction};

    fn progress(status: &str) -> ContentState {
        ContentState {
            is_exporting: true,
            export_status: status.into(),
            export_progress: 0.5,
            ..ContentState::default()
        }
    }

    #[test]
    fn export_modal_survives_section_results_and_closes_only_on_run_terminal() {
        let mut ui = UIRoot::new();
        ui.export_progress.begin("show.mp4");
        assert!(!ui.consume_export_notification(&ContentState::default()));
        assert!(
            ui.export_progress.is_open(),
            "queued idle snapshots cannot close preparation"
        );
        assert!(ui.consume_export_notification(&progress("section 1 of 2")));
        let section = ContentState {
            export_finished: Some(ExportFinishedEvent {
                success: true,
                message: "Export complete: 12 frames".into(),
                output_path: "show--one.mp4".into(),
            }),
            ..ContentState::default()
        };
        assert!(ui.consume_export_notification(&section));
        assert!(ui.export_progress.is_open());
        assert!(ui.consume_export_notification(&progress("section 2 of 2")));
        assert!(ui.consume_export_notification(&ContentState {
            export_run_finished: true,
            ..ContentState::default()
        }));
        assert!(!ui.export_progress.is_open());
    }

    #[test]
    fn export_modal_captures_background_input_and_escape_cancels_once() {
        let mut ui = UIRoot::new();
        ui.export_progress.begin("show.mp4");
        ui.build();
        assert!(ui.background_input_blocked());
        let mut actions = Vec::new();
        assert!(ui.route_overlay_event(
            &UIEvent::Scroll {
                pos: Vec2::new(2.0, 2.0),
                delta: Vec2::new(0.0, 20.0),
                modifiers: Modifiers::default(),
            },
            &mut actions
        ));
        assert!(actions.is_empty());
        let escape = UIEvent::KeyDown {
            node_id: NodeId::PLACEHOLDER,
            key: Key::Escape,
            modifiers: Modifiers::default(),
        };
        assert!(ui.route_overlay_event(&escape, &mut actions));
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::CancelExport)]
        ));
        actions.clear();
        ui.consume_export_notification(&progress("Exporting 12/24 (50%)"));
        assert!(ui.route_overlay_event(&escape, &mut actions));
        assert!(actions.is_empty(), "cancellation cannot be sent twice");
        assert!(ui.export_progress.is_open());
    }

    #[test]
    fn export_cancel_button_routes_a_real_click_and_hides_duplicate_warmup() {
        let mut ui = UIRoot::new();
        ui.warmup = Some(manifold_core::WarmupProgress {
            done: 1,
            total: 2,
            label: "Loading scene".into(),
        });
        ui.export_progress.begin("show.mp4");
        ui.resize(1280.0, 720.0);
        assert!(!ui.tree.nodes().iter().any(|node| {
            node.text
                .as_deref()
                .is_some_and(|text| text.contains("Warming project"))
        }));
        let button = ui
            .tree
            .nodes()
            .iter()
            .find(|node| node.text.as_deref() == Some("Cancel export"))
            .expect("modal cancel button")
            .bounds;
        let center = Vec2::new(
            button.x + button.width * 0.5,
            button.y + button.height * 0.5,
        );
        ui.pointer_event(center, manifold_ui::input::PointerAction::Down, 1.0);
        ui.pointer_event(center, manifold_ui::input::PointerAction::Up, 1.1);
        let actions = ui.process_events();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::CancelExport)]
        ));
        assert!(ui.export_progress.cancel_requested());
        assert!(ui.export_progress.is_open());
    }

    #[test]
    fn still_export_result_does_not_open_a_video_modal() {
        let mut ui = UIRoot::new();
        assert!(ui.consume_export_notification(&ContentState {
            export_finished: Some(ExportFinishedEvent {
                success: true,
                message: "Frame exported".into(),
                output_path: "frame.png".into(),
            }),
            ..ContentState::default()
        }));
        assert!(!ui.export_progress.is_open());
    }
}
