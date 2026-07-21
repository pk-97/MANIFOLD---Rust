//! Transport-related dispatch: Play/Pause/Stop/Seek/BPM/Recording/Clock/Zoom.

use manifold_core::{Beats, project::Project};
use manifold_editing::commands::settings::ChangeQuantizeModeCommand;
use manifold_ui::TransportAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_transport(
    action: &TransportAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        TransportAction::PlayPause => {
            if content_state.is_playing {
                ContentCommand::send(content_tx, ContentCommand::Pause);
            } else {
                if let Some(cursor_beat) = selection.insert_cursor_beat {
                    ContentCommand::send(content_tx, ContentCommand::SeekToBeat(cursor_beat));
                }
                ContentCommand::send(content_tx, ContentCommand::Play);
            }
            DispatchResult::handled()
        }
        TransportAction::Stop => {
            ContentCommand::send(content_tx, ContentCommand::Stop);
            if let Some(cursor_beat) = selection.insert_cursor_beat {
                ContentCommand::send(content_tx, ContentCommand::SeekToBeat(cursor_beat));
            }
            DispatchResult::handled()
        }
        TransportAction::Record => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SetRecording(!content_state.is_recording),
            );
            DispatchResult::handled()
        }
        TransportAction::ResetBpm => {
            // Intercepted by Application before dispatch
            DispatchResult::handled()
        }
        TransportAction::ClearBpm => {
            {
                let old_points = project.tempo_map.clone_points();
                let bpm = project.settings.bpm;
                let cmd = manifold_editing::commands::settings::ClearTempoMapCommand::new(
                    old_points, bpm,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }
        TransportAction::BpmFieldClicked => {
            log::debug!("BPM field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }
        TransportAction::Seek(beat) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SeekToBeat(Beats::from_f32(*beat)),
            );
            DispatchResult::handled()
        }
        TransportAction::OverviewScrub(norm) => {
            // Unity: ViewportManager.OnOverviewStripScrub — center viewport on click
            let ppb = ui.viewport.pixels_per_beat();
            let viewport_w = ui.viewport.get_tracks_rect().width;
            // Compute total content width from clips
            let max_beat = ui.viewport.max_content_beat().max(1.0);
            let content_w = max_beat * ppb;
            let target_scroll = (norm * content_w - viewport_w * 0.5) / ppb;
            let target_scroll = target_scroll.max(0.0);
            ui.viewport
                .set_scroll(target_scroll, ui.viewport.scroll_y_px());
            DispatchResult::structural()
        }
        TransportAction::TimelineScrollbarH(scroll_x_beats) => {
            // Horizontal scrollbar drag/jump (§24 5e) — absolute scroll-x in beats.
            ui.viewport
                .set_scroll(*scroll_x_beats, ui.viewport.scroll_y_px());
            DispatchResult::structural()
        }
        TransportAction::SetInsertCursor(beat) => {
            // Legacy path — when no layer context available.
            // Uses set_insert_cursor_beat (non-clearing variant)
            // since we don't have a layer index here.
            selection.set_insert_cursor_beat(manifold_core::Beats::from_f32(*beat));
            DispatchResult::structural()
        }

        // ── Clock/Sync (handled at Application level, these are fallbacks) ──
        TransportAction::CycleClockAuthority
        | TransportAction::ToggleLink
        | TransportAction::ToggleMidiClock
        | TransportAction::ToggleSyncOutput => {
            // Intercepted by Application before dispatch — should not reach here
            DispatchResult::handled()
        }
        TransportAction::SelectClkDevice => {
            // Handled by try_open_dropdown — should not reach here
            DispatchResult::handled()
        }
        TransportAction::SetMidiClockDevice(index) => {
            // Handled by app_render.rs intercept — should not reach here
            ContentCommand::send(content_tx, ContentCommand::SetMidiClockDevice(*index));
            DispatchResult::handled()
        }

        // ── Automation globals (P4, docs/AUTOMATION_LANES_DESIGN.md §4/§5) ──
        // Runtime-only latch/arm state, not a project mutation — no undo entry,
        // same shape as `SessionBackToArrangement`/session quantize.
        TransportAction::ToggleAutomationArm => {
            ContentCommand::send(
                content_tx,
                ContentCommand::AutomationSetArmed(!content_state.automation_armed),
            );
            DispatchResult::handled()
        }
        TransportAction::AutomationBackToArrangement => {
            ContentCommand::send(content_tx, ContentCommand::AutomationBackToArrangement);
            DispatchResult::handled()
        }
        // Pure UI view-state — no project mutation, no runtime playback
        // state, so no ContentCommand. It DOES change the Y-layout (a visible
        // lane grows its track), so this must return `structural()`, not
        // `handled()`, to force `sync_project_data` to re-derive
        // `automation_lane_count` and the lane list on the next frame.
        TransportAction::ToggleAutomationMode => {
            selection.automation_mode_visible = !selection.automation_mode_visible;
            DispatchResult::structural()
        }

        TransportAction::CycleQuantize => {
            {
                let old = project.settings.quantize_mode;
                let new = old.next();
                let cmd = ChangeQuantizeModeCommand::new(old, new);
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }
        TransportAction::ResolutionClicked => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        TransportAction::FpsFieldClicked => {
            log::debug!("FPS field clicked (text input not yet implemented)");
            DispatchResult::handled()
        }

        // ── Zoom ───────────────────────────────────────────────────
        // The +/- buttons step one discrete zoom level, anchored on the playhead
        // (no cursor to anchor to). `zoom_level_stepped` resolves the nearest
        // level first, so the buttons stay sane after a continuous scroll-zoom
        // (§24 5e); `zoom_to` is the one shared anchored-zoom path.
        TransportAction::ZoomIn => {
            let playhead = content_state.current_beat.as_f32();
            let playhead_px = ui.viewport.beat_to_pixel(Beats::from_f32(playhead));
            let new_ppb = ui.viewport.zoom_level_stepped(1);
            ui.viewport.zoom_to(new_ppb, playhead, playhead_px);
            DispatchResult::structural()
        }
        TransportAction::ZoomOut => {
            let playhead = content_state.current_beat.as_f32();
            let playhead_px = ui.viewport.beat_to_pixel(Beats::from_f32(playhead));
            let new_ppb = ui.viewport.zoom_level_stepped(-1);
            ui.viewport.zoom_to(new_ppb, playhead, playhead_px);
            DispatchResult::structural()
        }

        // ── Inspector navigation ───────────────────────────────────
        // SelectInspectorTab is handled in `dispatch` (it needs active_layer).
        TransportAction::InspectorScrolled(delta) => {
            ui.inspector.handle_scroll(*delta);
            DispatchResult::handled()
        }
        TransportAction::InspectorSectionClicked(idx) => {
            log::debug!("Inspector section clicked: {}", idx);
            DispatchResult::handled()
        }

    }
}
