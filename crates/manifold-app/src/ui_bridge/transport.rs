//! Transport-related dispatch: Play/Pause/Stop/Seek/BPM/Recording/Clock/Zoom.

use manifold_core::project::Project;
use manifold_editing::commands::settings::ChangeQuantizeModeCommand;
use manifold_ui::PanelAction;

use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use super::DispatchResult;

pub(super) fn dispatch_transport(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        PanelAction::PlayPause => {
            if content_state.is_playing {
                let _ = content_tx.try_send(ContentCommand::Pause);
            } else {
                if let Some(cursor_beat) = selection.insert_cursor_beat {
                    let _ = content_tx.try_send(ContentCommand::SeekToBeat(cursor_beat));
                }
                let _ = content_tx.try_send(ContentCommand::Play);
            }
            DispatchResult::handled()
        }
        PanelAction::Stop => {
            let _ = content_tx.try_send(ContentCommand::Stop);
            if let Some(cursor_beat) = selection.insert_cursor_beat {
                let _ = content_tx.try_send(ContentCommand::SeekToBeat(cursor_beat));
            }
            DispatchResult::handled()
        }
        PanelAction::Record => {
            let _ = content_tx.try_send(ContentCommand::SetRecording(!content_state.is_recording));
            DispatchResult::handled()
        }
        PanelAction::ResetBpm => {
            // Intercepted by Application before dispatch
            DispatchResult::handled()
        }
        PanelAction::ClearBpm => {
            {
                let old_points = project.tempo_map.clone_points();
                let bpm = project.settings.bpm;
                let cmd = manifold_editing::commands::settings::ClearTempoMapCommand::new(old_points, bpm);
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
            }
            DispatchResult::handled()
        }
        PanelAction::BpmFieldClicked => {
            log::debug!("BPM field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::Seek(beat) => {
            let _ = content_tx.try_send(ContentCommand::SeekToBeat(*beat));
            DispatchResult::handled()
        }
        PanelAction::OverviewScrub(norm) => {
            // Unity: ViewportManager.OnOverviewStripScrub — center viewport on click
            let ppb = ui.viewport.pixels_per_beat();
            let viewport_w = ui.viewport.get_tracks_rect().width;
            // Compute total content width from clips
            let max_beat = ui.viewport.max_content_beat().max(1.0);
            let content_w = max_beat * ppb;
            let target_scroll = (norm * content_w - viewport_w * 0.5) / ppb;
            let target_scroll = target_scroll.max(0.0);
            ui.viewport.set_scroll(target_scroll, ui.viewport.scroll_y_px());
            DispatchResult::structural()
        }
        PanelAction::SetInsertCursor(beat) => {
            // Legacy path — when no layer context available.
            // Uses set_insert_cursor_beat (non-clearing variant)
            // since we don't have a layer index here.
            selection.set_insert_cursor_beat(*beat);
            DispatchResult::structural()
        }

        // ── Clock/Sync (handled at Application level, these are fallbacks) ──
        PanelAction::CycleClockAuthority
        | PanelAction::ToggleLink
        | PanelAction::ToggleMidiClock
        | PanelAction::ToggleSyncOutput => {
            // Intercepted by Application before dispatch — should not reach here
            DispatchResult::handled()
        }
        PanelAction::SelectClkDevice => {
            log::info!("Select clock device (dropdown not yet implemented)");
            DispatchResult::handled()
        }

        PanelAction::CycleQuantize => {
            {
                let old = project.settings.quantize_mode;
                let new = old.next();
                let cmd = ChangeQuantizeModeCommand::new(old, new);
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
            }
            DispatchResult::handled()
        }
        PanelAction::ResolutionClicked => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::FpsFieldClicked => {
            log::debug!("FPS field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }

        // ── Zoom ───────────────────────────────────────────────────
        PanelAction::ZoomIn => {
            let ppb = ui.viewport.pixels_per_beat();
            let levels = &manifold_ui::color::ZOOM_LEVELS;
            let current_idx = levels.iter()
                .position(|&l| (l - ppb).abs() < 0.01)
                .unwrap_or(manifold_ui::color::DEFAULT_ZOOM_INDEX);
            let new_idx = (current_idx + 1).min(levels.len() - 1);
            if new_idx != current_idx {
                ui.viewport.set_zoom(levels[new_idx]);
            }
            DispatchResult::structural()
        }
        PanelAction::ZoomOut => {
            let ppb = ui.viewport.pixels_per_beat();
            let levels = &manifold_ui::color::ZOOM_LEVELS;
            let current_idx = levels.iter()
                .position(|&l| (l - ppb).abs() < 0.01)
                .unwrap_or(manifold_ui::color::DEFAULT_ZOOM_INDEX);
            let new_idx = current_idx.saturating_sub(1);
            if new_idx != current_idx {
                ui.viewport.set_zoom(levels[new_idx]);
            }
            DispatchResult::structural()
        }

        // ── Inspector navigation ───────────────────────────────────
        PanelAction::SelectInspectorTab(tab) => {
            log::debug!("Inspector tab: {:?}", tab);
            DispatchResult::handled()
        }
        PanelAction::InspectorScrolled(delta) => {
            ui.inspector.handle_scroll(*delta);
            DispatchResult::handled()
        }
        PanelAction::InspectorSectionClicked(idx) => {
            log::debug!("Inspector section clicked: {}", idx);
            DispatchResult::handled()
        }

        _ => DispatchResult::unhandled(),
    }
}
