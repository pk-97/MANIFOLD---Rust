//! Transport-related dispatch: Play/Pause/Stop/Seek/BPM/Recording/Clock/Zoom.

use manifold_core::Beats;
use manifold_editing::commands::settings::ChangeQuantizeModeCommand;
use manifold_ui::TransportAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_transport(
    action: &TransportAction,
    project: &mut crate::ui_bridge::ProjectRef<'_>,
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
                if let Some(cursor_beat) = selection.insert_cursor_beat() {
                    ContentCommand::send(content_tx, ContentCommand::SeekToBeat(cursor_beat));
                }
                ContentCommand::send(content_tx, ContentCommand::Play);
            }
            DispatchResult::handled()
        }
        TransportAction::Stop => {
            ContentCommand::send(content_tx, ContentCommand::Stop);
            if let Some(cursor_beat) = selection.insert_cursor_beat() {
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
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
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
            DispatchResult::handled()
        }
        TransportAction::TimelineScrollbarH(scroll_x_beats) => {
            // Horizontal scrollbar drag/jump (section 24 5e) — absolute scroll-x in beats.
            ui.viewport
                .set_scroll(*scroll_x_beats, ui.viewport.scroll_y_px());
            DispatchResult::handled()
        }
        TransportAction::SetInsertCursor(beat) => {
            // Legacy path — when no layer context available.
            // Uses set_insert_cursor_beat (non-clearing variant)
            // since we don't have a layer index here.
            selection.set_insert_cursor_beat(manifold_core::Beats::from_f32(*beat));
            DispatchResult::handled()
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

        // ── Automation globals ──
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
        // Pure UI view-state. The lanes change the Y-layout; the
        // `automation_view` version runs the project sync that re-derives the
        // lane list, so no structural flag.
        TransportAction::ToggleAutomationMode => {
            selection.toggle_automation_mode_visible();
            selection.clear_automation_selection();
            DispatchResult::handled()
        }

        TransportAction::ToggleAutomationDrawMode => {
            // The chrome push repaints DRAW; showing the lanes moves
            // automation_view, which plans the lane rebuild.
            selection.automation_draw_mode = !selection.automation_draw_mode;
            selection.set_automation_mode_visible(true);
            DispatchResult::handled()
        }

        TransportAction::CycleQuantize => {
            {
                let old = project.settings.quantize_mode;
                let new = old.next();
                let cmd = ChangeQuantizeModeCommand::new(old, new);
                {
                    let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                    project.execute(boxed);
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
        // (section 24 5e); `zoom_to` is the one shared anchored-zoom path.
        // The planner sees the moved scroll and zoom, so these return handled().
        TransportAction::ZoomIn => {
            let playhead = content_state.current_beat.as_f32();
            let playhead_px = ui.viewport.beat_to_pixel(Beats::from_f32(playhead));
            let new_ppb = ui.viewport.zoom_level_stepped(1);
            ui.viewport.zoom_to(new_ppb, playhead, playhead_px);
            DispatchResult::handled()
        }
        TransportAction::ZoomOut => {
            let playhead = content_state.current_beat.as_f32();
            let playhead_px = ui.viewport.beat_to_pixel(Beats::from_f32(playhead));
            let new_ppb = ui.viewport.zoom_level_stepped(-1);
            ui.viewport.zoom_to(new_ppb, playhead, playhead_px);
            DispatchResult::handled()
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

#[cfg(test)]
mod view_move_dispatch_tests {
    use crate::frame_plan::{FramePlanner, Projection};
    use manifold_ui::{PanelAction, TransportAction};

    /// Scrollbar, overview scrub and zoom buttons move only the timeline view:
    /// the plan updates the scroll panels and runs no project or inspector sync.
    #[test]
    fn scroll_and_zoom_are_view_moves_not_structural_syncs() {
        let project = manifold_core::project::Project::default();
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = None;
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let mut edits = crate::gesture_preview::UiEdits::default();
        let mut planner = FramePlanner::default();
        let inputs = |planner: &mut FramePlanner,
                      ui: &crate::ui_root::UIRoot,
                      selection: &mut manifold_ui::UIState,
                      project: &manifold_core::project::Project,
                      edits: &crate::gesture_preview::UiEdits| {
            crate::ui_frame::gather_frame_inputs(
                planner,
                ui,
                selection,
                &None,
                edits,
                &content_state,
                crate::frame_plan::ChromeSource {
                    project,
                    is_dirty: false,
                    project_path: None,
                    automation_draw_mode: false,
                },
            )
        };
        let seed = inputs(&mut planner, &ui, &mut selection, &project, &edits);
        planner.plan(seed);

        // (action, scroll moves, zoom moves); an anchored zoom may also scroll.
        let cases = [
            (TransportAction::TimelineScrollbarH(4.3), true, false),
            (TransportAction::OverviewScrub(0.0), true, false),
            (TransportAction::ZoomIn, false, true),
            (TransportAction::ZoomOut, false, true),
        ];
        for (action, scroll, zoom) in cases {
            {
                let mut ctx = crate::ui_bridge::DispatchCtx {
                    project: crate::ui_bridge::ProjectRef::new(&project, &mut edits, &content_tx),
                    content_tx: &content_tx,
                    content_state: &content_state,
                    ui: &mut ui,
                    selection: &mut selection,
                    active_layer: &mut active_layer,
                    user_prefs: &mut user_prefs,
                    editor_target: None,
                    scrub: &mut scrub,
                    dialogs: Default::default(),
                };
                crate::ui_bridge::dispatch(&PanelAction::Transport(action.clone()), &mut ctx)
            };
            assert!(content_rx.try_recv().is_err(), "{action:?} sends nothing");
            let next = inputs(&mut planner, &ui, &mut selection, &project, &edits);
            let plan = planner.plan(next);
            if !zoom {
                assert_eq!(plan.viewport.scroll_x, scroll, "{action:?} scroll");
            }
            assert_eq!(plan.viewport.zoom, zoom, "{action:?} zoom");
            assert!(
                !plan.projections.contains(Projection::ProjectSync),
                "{action:?}"
            );
            assert!(
                !plan.projections.contains(Projection::InspectorSync),
                "{action:?}"
            );
        }
    }
}
