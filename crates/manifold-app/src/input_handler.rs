//! Keyboard dispatch and context menu routing.
//! Mechanical translation of Assets/Scripts/UI/Timeline/InputHandler.cs.
//!
//! Plain Rust struct — NOT a MonoBehaviour equivalent.
//! Calls through TimelineInputHost trait for all operations that need
//! engine/editing/UI access. Owns inspector focus.
use manifold_core::{Beats, ClipId, Seconds};
use manifold_ui::cursor_nav::FINE_NUDGE_BEATS;
use manifold_ui::input::Modifiers;
use manifold_ui::timeline_input_host::TimelineInputHost;

use winit::keyboard::{Key, NamedKey};

/// Keyboard handler. Port of InputHandler.cs.
///
/// Owns inspector focus. The app layer calls `handle_keyboard_input()` on
/// each key press.
pub struct InputHandler {
    // ── Panel focus (Unity line 65) ──
    pub inspector_has_focus: bool,
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            inspector_has_focus: false,
        }
    }

    /// Port of Unity InputHandler.HandleKeyboardInput (lines 189-517).
    ///
    /// Returns true if the key was consumed (caller should not process further).
    /// All state access goes through the host trait — InputHandler owns no data references.
    pub fn handle_keyboard_input(
        &mut self,
        logical_key: &Key,
        modifiers: Modifiers,
        host: &mut dyn TimelineInputHost,
    ) -> bool {
        let m = modifiers;

        // Unity line 213: inspector arrow key stepping (loop duration)
        if host.handle_inspector_keyboard() {
            return true;
        }

        // ── Backtick — toggle performance HUD (Unity line 217) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "`") && m.is_none() {
            host.toggle_performance_hud();
            return true;
        }

        // ── B — toggle automation draw/pencil mode (P4 Unit B,
        // `docs/AUTOMATION_LANES_DESIGN.md` §7's "Draw mode", Live's `B`).
        // No pre-existing MANIFOLD binding on this key — bound directly,
        // Live-exact, no remap needed. Gated on automation mode being
        // visible: pencil mode is meaningless with no lanes shown. ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "b" || c.as_str() == "B")
            && m.is_none()
            && host.automation_mode_visible()
        {
            host.toggle_automation_draw_mode();
            return true;
        }

        // ── Escape — priority chain (Unity lines 224-232) ──
        if matches!(logical_key, Key::Named(NamedKey::Escape)) {
            // Level 0: dismiss the top-most open overlay (modal or dropdown).
            // One call covers every modal + context menu via the overlay driver;
            // the perf HUD is modeless and won't consume, so Escape falls
            // through to selection clearing when only the HUD is up.
            if host.dismiss_top_overlay() {
                return true;
            }
            // Level 2: inspector has focus → clear effect selection
            if self.inspector_has_focus {
                host.clear_effect_selection();
                self.inspector_has_focus = false;
                return true;
            }
            // Level 3: clear all selection + insert cursor
            host.clear_selection();
            host.on_selection_cleared();
            return true;
        }

        // ── Redo: Cmd+Shift+Z (with Shift, winit reports "Z") ──
        if matches!(logical_key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("z"))
            && m.is_command_shift()
        {
            host.redo();
            host.on_undo_redo();
            return true;
        }
        // ── Undo: Cmd+Z (Unity line 241) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "z") && m.is_command_only() {
            host.undo();
            host.on_undo_redo();
            return true;
        }
        // ── Redo: Cmd+Y (Unity line 247) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "y") && m.is_command_only() {
            host.redo();
            host.on_undo_redo();
            return true;
        }

        // ── Z — zoom to selection (B14 — `docs/TIMELINE_INTERACTION_P1_SPEC.md`
        // §5 P1.6). Bare/Shift only — Cmd+Z and Cmd+Shift+Z above are
        // undo/redo and take priority since they're checked first. ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "z") && m.is_none() {
            host.zoom_to_selection();
            return true;
        }
        // ── Shift+Z — zoom back (B14) ──
        if matches!(logical_key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("z"))
            && m.is_shift_only()
        {
            host.zoom_back();
            return true;
        }

        // ── Save/Open/New: Cmd+S, Cmd+O, Cmd+N ──
        // These require rfd dialogs and window handles that AppInputHost
        // cannot access. Return false to let the legacy block handle them.
        // TODO: Move to host when legacy block is deleted (use flag pattern).
        if matches!(logical_key, Key::Character(c) if c.as_str() == "s") && m.is_command_only() {
            return false; // handled by legacy block
        }
        if matches!(logical_key, Key::Character(c) if c.as_str() == "o") && m.is_command_only() {
            return false; // handled by legacy block
        }
        if matches!(logical_key, Key::Character(c) if c.as_str() == "n") && m.is_command_only() {
            return false; // handled by legacy block
        }

        // ── Select all: Cmd+A (context-sensitive: effects vs clips) (Unity line 289) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "a") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_select_all() {
                return true;
            }
            host.select_all_clips();
            return true;
        }

        // ── Copy: Cmd+C (context-sensitive: effects vs clips) (Unity line 296) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "c") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_copy() {
                return true;
            }
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.copy_clips(&ids);
            }
            return true;
        }

        // ── Cut: Cmd+X (context-sensitive) (Unity line 302) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "x") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_cut() {
                return true;
            }
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.cut_clips(&ids, host.has_region());
            }
            return true;
        }

        // ── Paste: Cmd+V (context-sensitive) (Unity line 308) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "v") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_paste() {
                return true;
            }
            let target_beat = host.insert_cursor_beat().unwrap_or(host.current_beat());
            let target_layer = host.insert_cursor_layer_index().unwrap_or(0) as i32;
            host.paste_clips(target_beat, target_layer);
            return true;
        }

        // ── Duplicate: Cmd+D (Unity line 316) ──
        // Context-sensitive: clips take priority; layers if no clips selected.
        if matches!(logical_key, Key::Character(c) if c.as_str() == "d") && m.is_command_only() {
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.duplicate_clips(&ids);
            } else if host.layer_selection_count() > 0 {
                host.duplicate_selected_layers();
            }
            return true;
        }

        // ── Ungroup: Cmd+Shift+G (context-sensitive) (Unity line 323) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "g" || c.as_str() == "G")
            && m.is_command_shift()
        {
            if self.inspector_has_focus {
                host.handle_effect_ungroup();
            } else {
                host.ungroup_selected_layers();
            }
            return true;
        }

        // ── Group: Cmd+G (context-sensitive) (Unity line 328) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "g") && m.is_command_only() {
            if self.inspector_has_focus {
                host.handle_effect_group();
            } else {
                host.group_selected_layers();
            }
            return true;
        }

        // ── Delete/Backspace (context-sensitive: effects → markers → layers → clips) (Unity line 336) ──
        if matches!(
            logical_key,
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace)
        ) && m.is_none()
        {
            // Priority 1: inspector focused → delete effects
            if self.inspector_has_focus && host.handle_effect_delete() {
                return true;
            }
            // Priority 1.4: marquee-selected automation breakpoints → delete
            // the whole group as one undo entry (P4 Unit B, §7's
            // "Marquee-select ... drag/delete them together"). Checked
            // BEFORE the single-point path below — a marquee selection
            // takes precedence over a stale single click-selection.
            if host.has_selected_automation_points() {
                host.delete_selected_automation_points();
                return true;
            }
            // Priority 1.5: selected automation breakpoint → delete it
            // (P4 Unit A, `docs/AUTOMATION_LANES_DESIGN.md` §7's "Delete
            // removes the selection").
            if host.has_selected_automation_point() {
                host.delete_selected_automation_point();
                return true;
            }
            // Priority 2: selected markers → delete markers
            if host.has_selected_markers() {
                host.delete_selected_markers();
                return true;
            }
            // Priority 3: layer selection active, no clips, no region → delete layers
            // (Unity lines 341-346)
            if host.layer_selection_count() > 0 && !host.has_region() && host.selection_count() == 0
            {
                host.delete_selected_layers();
                return true;
            }
            // Priority 4: delete selected clips (region-aware)
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.delete_clips(&ids, host.has_region());
                host.clear_selection();
            }
            return true;
        }

        // ── Space — Play/Pause (Unity line 352) ──
        if matches!(logical_key, Key::Named(NamedKey::Space)) && m.is_none() {
            let cursor_beat = host.insert_cursor_beat().map(Beats::from_f32);
            host.play_pause(cursor_beat);
            return true;
        }

        // ── Home — seek to start (Unity line 375) ──
        if matches!(logical_key, Key::Named(NamedKey::Home)) && m.is_none() {
            host.seek_to(Seconds::ZERO);
            return true;
        }

        // ── End — seek to end (Unity line 380) ──
        if matches!(logical_key, Key::Named(NamedKey::End)) && m.is_none() {
            // Host computes end time from project timeline
            host.seek_to(Seconds(f64::MAX)); // sentinel — host clamps to actual end
            return true;
        }

        // ── S — split at playhead (bare S, no modifiers) (Unity line 393) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "s") && m.is_none() {
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.split_clips_at_playhead(&ids);
            }
            return true;
        }

        // ── Cmd+E — split at playhead, Ableton-style binding (B14 —
        // `docs/TIMELINE_INTERACTION_P1_SPEC.md` §5 P1.6). Same action as
        // bare `S` above; both bindings ship side by side per the doc. ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "e") && m.is_command_only() {
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.split_clips_at_playhead(&ids);
            }
            return true;
        }

        // ── Shift+E — shrink by grid step (check before bare E) (Unity line 400) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "E" || c.as_str() == "e")
            && m.is_shift_only()
        {
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                let step = host.grid_step();
                host.shrink_clips(&ids, step);
            }
            return true;
        }

        // ── E — extend by grid step (bare E) (Unity line 405) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "e") && m.is_none() {
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                let step = host.grid_step();
                host.extend_clips(&ids, step);
            }
            return true;
        }

        // ── F — zoom to fit (Unity line 412) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "f") && m.is_none() {
            host.zoom_to_fit();
            return true;
        }

        // ── 0 / Numpad0 — toggle mute (Unity line 419-420) ──
        // winit: Numpad0 with numlock on produces Key::Character("0"), same as main row.
        if matches!(logical_key, Key::Character(c) if c.as_str() == "0") && m.is_none() {
            let ids: Vec<ClipId> = host.get_selected_clip_ids();
            if !ids.is_empty() {
                host.toggle_mute_clips(&ids);
            }
            return true;
        }

        // ── Arrow keys: nudge/cross-layer-move clips when selected, navigate
        // cursor otherwise (Unity lines 426-458, extended by B14 —
        // `docs/TIMELINE_INTERACTION_P1_SPEC.md` §5 P1.6: Shift+arrow = 1 beat,
        // Cmd+arrow = fine/1 tick, Up/Down cross layers instead of no-op). ──
        if matches!(
            logical_key,
            Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowRight)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::ArrowDown)
        ) && !m.alt
        {
            let has_selected = host.selection_count() > 0;

            if has_selected {
                match logical_key {
                    Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowRight) => {
                        // B14: plain = one grid step, Shift = 1 beat, Cmd = fine/1 tick.
                        let step = if m.is_command_only() {
                            FINE_NUDGE_BEATS
                        } else if m.is_shift_only() {
                            1.0
                        } else if m.is_none() {
                            host.grid_step()
                        } else {
                            return true; // unrecognized modifier combo — consume, no-op
                        };
                        let ids: Vec<ClipId> = host.get_selected_clip_ids();
                        let delta = if matches!(logical_key, Key::Named(NamedKey::ArrowLeft)) {
                            -step
                        } else {
                            step
                        };
                        host.nudge_clips(&ids, delta);
                    }
                    Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown)
                        if m.is_none() =>
                    {
                        // B14: Up/Down move the selection across layers, one
                        // undo entry per press.
                        let ids: Vec<ClipId> = host.get_selected_clip_ids();
                        let layer_delta =
                            if matches!(logical_key, Key::Named(NamedKey::ArrowUp)) {
                                -1
                            } else {
                                1
                            };
                        host.move_selection_across_layers(&ids, layer_delta);
                    }
                    _ => {}
                }
            } else {
                // Navigate insert cursor (Ableton-style) (Unity line 456)
                // Direction: 0=left, 1=right, 2=up, 3=down
                if m.command {
                    return true;
                }
                let direction = match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => 0u8,
                    Key::Named(NamedKey::ArrowRight) => 1u8,
                    Key::Named(NamedKey::ArrowUp) => 2u8,
                    Key::Named(NamedKey::ArrowDown) => 3u8,
                    _ => return true,
                };
                let grid_step = host.grid_step();
                host.navigate_cursor(direction, m.shift, grid_step);
            }
            return true;
        }

        // ── M — add marker at playhead (bare M, no modifiers) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "m") && m.is_none() {
            host.add_marker_at_playhead();
            return true;
        }

        // ── Export markers: Alt variants first (Unity lines 461-481) ──
        if matches!(logical_key, Key::Character(c) if c.as_str() == "i") && m.is_alt_only() {
            host.clear_export_in();
            return true;
        }
        if matches!(logical_key, Key::Character(c) if c.as_str() == "o") && m.is_alt_only() {
            host.clear_export_out();
            return true;
        }
        if matches!(logical_key, Key::Character(c) if c.as_str() == "i") && m.is_none() {
            host.set_export_in_at_playhead();
            return true;
        }
        if matches!(logical_key, Key::Character(c) if c.as_str() == "o") && m.is_none() {
            host.set_export_out_at_playhead();
            return true;
        }

        false // not consumed
    }
}

#[cfg(test)]
mod b14_keyboard_layer_tests {
    //! B14 dispatch tests (`docs/TIMELINE_INTERACTION_P1_SPEC.md` §5 P1.6) —
    //! exercise `InputHandler::handle_keyboard_input` end to end through a
    //! recording mock of `TimelineInputHost`, the same shape as
    //! `interaction_overlay.rs`'s `TimelineEditingHost` test mock: most
    //! methods are stubs, the ones this phase's bindings call are recorded.
    use super::*;
    use manifold_ui::input::Modifiers;

    #[derive(Default)]
    struct MockHost {
        selected_ids: Vec<ClipId>,
        selection_count_val: usize,
        grid_step_val: f32,
        nudge_calls: Vec<(Vec<ClipId>, f32)>,
        move_layer_calls: Vec<(Vec<ClipId>, i32)>,
        split_calls: Vec<Vec<ClipId>>,
        zoom_to_selection_calls: u32,
        zoom_back_calls: u32,
    }

    impl TimelineInputHost for MockHost {
        fn handle_inspector_keyboard(&mut self) -> bool {
            false
        }
        fn toggle_performance_hud(&mut self) {}
        fn is_monitor_output_active(&self) -> bool {
            false
        }
        fn close_output_window(&mut self) {}
        fn request_rebuild(&mut self) {}
        fn on_undo_redo(&mut self) {}
        fn on_selection_cleared(&mut self) {}
        fn mark_compositor_dirty(&mut self) {}
        fn invalidate_all_layer_bitmaps(&mut self) {}
        fn update_zoom_label(&mut self) {}
        fn get_playhead_viewport_x(&self) -> f32 {
            0.0
        }
        fn get_viewport_width(&self) -> f32 {
            800.0
        }
        fn get_seconds_per_beat(&self) -> f32 {
            0.5
        }
        fn on_clip_selected(&mut self, _clip_id: &str) {}
        fn handle_effect_copy(&mut self) -> bool {
            false
        }
        fn handle_effect_cut(&mut self) -> bool {
            false
        }
        fn handle_effect_paste(&mut self) -> bool {
            false
        }
        fn handle_effect_delete(&mut self) -> bool {
            false
        }
        fn handle_effect_group(&mut self) -> bool {
            false
        }
        fn handle_effect_ungroup(&mut self) -> bool {
            false
        }
        fn handle_effect_select_all(&mut self) -> bool {
            false
        }
        fn clear_effect_selection(&mut self) {}
        fn show_toast(&mut self, _message: &str) {}
        fn undo(&mut self) {}
        fn redo(&mut self) {}
        fn save_project(&mut self) {}
        fn open_project(&mut self) {}
        fn new_project(&mut self) {}
        fn play_pause(&mut self, _insert_cursor_beat: Option<Beats>) {}
        fn seek_to(&mut self, _time: Seconds) {}
        fn current_beat(&self) -> f32 {
            0.0
        }
        fn is_playing(&self) -> bool {
            false
        }
        fn select_all_clips(&mut self) {}
        fn copy_clips(&mut self, _clip_ids: &[ClipId]) {}
        fn cut_clips(&mut self, _clip_ids: &[ClipId], _has_region: bool) {}
        fn paste_clips(&mut self, _target_beat: f32, _target_layer: i32) {}
        fn duplicate_clips(&mut self, _clip_ids: &[ClipId]) {}
        fn delete_clips(&mut self, _clip_ids: &[ClipId], _has_region: bool) {}
        fn delete_layer(&mut self, _layer_index: usize) {}
        fn split_clips_at_playhead(&mut self, clip_ids: &[ClipId]) {
            self.split_calls.push(clip_ids.to_vec());
        }
        fn extend_clips(&mut self, _clip_ids: &[ClipId], _grid_step: f32) {}
        fn shrink_clips(&mut self, _clip_ids: &[ClipId], _grid_step: f32) {}
        fn nudge_clips(&mut self, clip_ids: &[ClipId], beat_delta: f32) {
            self.nudge_calls.push((clip_ids.to_vec(), beat_delta));
        }
        fn move_selection_across_layers(&mut self, clip_ids: &[ClipId], layer_delta: i32) {
            self.move_layer_calls.push((clip_ids.to_vec(), layer_delta));
        }
        fn toggle_mute_clips(&mut self, _clip_ids: &[ClipId]) {}
        fn group_selected_layers(&mut self) {}
        fn ungroup_selected_layers(&mut self) {}
        fn delete_selected_layers(&mut self) {}
        fn duplicate_selected_layers(&mut self) {}
        fn layer_count(&self) -> usize {
            2
        }
        fn project_beats_per_bar(&self) -> u32 {
            4
        }
        fn set_export_in_at_playhead(&mut self) {}
        fn set_export_out_at_playhead(&mut self) {}
        fn clear_export_in(&mut self) {}
        fn clear_export_out(&mut self) {}
        fn start_export(&mut self) {}
        fn dismiss_top_overlay(&mut self) -> bool {
            false
        }
        fn grid_step(&self) -> f32 {
            self.grid_step_val
        }
        fn navigate_cursor(&mut self, _direction: u8, _is_fine: bool, _grid_step: f32) {}
        fn get_selected_clip_ids(&self) -> Vec<ClipId> {
            self.selected_ids.clone()
        }
        fn selection_count(&self) -> usize {
            self.selection_count_val
        }
        fn layer_selection_count(&self) -> usize {
            0
        }
        fn has_region(&self) -> bool {
            false
        }
        fn insert_cursor_beat(&self) -> Option<f32> {
            None
        }
        fn insert_cursor_layer_index(&self) -> Option<usize> {
            None
        }
        fn clear_selection(&mut self) {}
        fn zoom_to_fit(&mut self) {}
        fn zoom_to_selection(&mut self) {
            self.zoom_to_selection_calls += 1;
        }
        fn zoom_back(&mut self) {
            self.zoom_back_calls += 1;
        }
        fn add_marker_at_playhead(&mut self) {}
        fn delete_selected_markers(&mut self) {}
        fn has_selected_markers(&self) -> bool {
            false
        }
        fn has_selected_automation_point(&self) -> bool {
            false
        }
        fn delete_selected_automation_point(&mut self) {}
        fn has_selected_automation_points(&self) -> bool {
            false
        }
        fn delete_selected_automation_points(&mut self) {}
        fn toggle_automation_draw_mode(&mut self) {}
        fn automation_mode_visible(&self) -> bool {
            false
        }
    }

    fn selected_host() -> (InputHandler, MockHost) {
        let handler = InputHandler::new();
        let host = MockHost {
            selected_ids: vec![ClipId::from("clip-1")],
            selection_count_val: 1,
            grid_step_val: 0.25,
            ..Default::default()
        };
        (handler, host)
    }

    #[test]
    fn plain_arrow_nudges_by_grid_step() {
        let (mut handler, mut host) = selected_host();
        let consumed = handler.handle_keyboard_input(
            &Key::Named(NamedKey::ArrowRight),
            Modifiers::NONE,
            &mut host,
        );
        assert!(consumed);
        assert_eq!(host.nudge_calls.len(), 1, "exactly one nudge per press");
        assert_eq!(host.nudge_calls[0].1, 0.25, "plain arrow = one grid step");
    }

    #[test]
    fn shift_arrow_nudges_by_one_beat() {
        let (mut handler, mut host) = selected_host();
        let m = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        handler.handle_keyboard_input(&Key::Named(NamedKey::ArrowLeft), m, &mut host);
        assert_eq!(host.nudge_calls.len(), 1);
        assert_eq!(host.nudge_calls[0].1, -1.0, "Shift+Left = -1 beat");
    }

    #[test]
    fn command_arrow_nudges_by_fine_tick() {
        let (mut handler, mut host) = selected_host();
        let m = Modifiers {
            command: true,
            ..Modifiers::NONE
        };
        handler.handle_keyboard_input(&Key::Named(NamedKey::ArrowRight), m, &mut host);
        assert_eq!(host.nudge_calls.len(), 1);
        assert!(
            (host.nudge_calls[0].1 - manifold_ui::cursor_nav::FINE_NUDGE_BEATS).abs() < 1e-6,
            "Cmd+Right = fine/1 tick step"
        );
    }

    #[test]
    fn up_down_move_selection_across_layers_not_nudge() {
        let (mut handler, mut host) = selected_host();
        handler.handle_keyboard_input(&Key::Named(NamedKey::ArrowUp), Modifiers::NONE, &mut host);
        handler.handle_keyboard_input(&Key::Named(NamedKey::ArrowDown), Modifiers::NONE, &mut host);
        assert!(host.nudge_calls.is_empty(), "Up/Down must not nudge");
        assert_eq!(host.move_layer_calls.len(), 2, "one call per press");
        assert_eq!(host.move_layer_calls[0].1, -1, "Up = layer_delta -1");
        assert_eq!(host.move_layer_calls[1].1, 1, "Down = layer_delta +1");
    }

    #[test]
    fn cmd_e_splits_at_playhead_same_as_bare_s() {
        let (mut handler, mut host) = selected_host();
        let m = Modifiers {
            command: true,
            ..Modifiers::NONE
        };
        let consumed = handler.handle_keyboard_input(
            &Key::Character(winit::keyboard::SmolStr::new("e")),
            m,
            &mut host,
        );
        assert!(consumed);
        assert_eq!(host.split_calls.len(), 1);
        assert_eq!(host.split_calls[0], host.selected_ids);
    }

    #[test]
    fn bare_z_zooms_to_selection_shift_z_zooms_back() {
        let (mut handler, mut host) = selected_host();
        handler.handle_keyboard_input(
            &Key::Character(winit::keyboard::SmolStr::new("z")),
            Modifiers::NONE,
            &mut host,
        );
        assert_eq!(host.zoom_to_selection_calls, 1);
        assert_eq!(host.zoom_back_calls, 0);

        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        handler.handle_keyboard_input(
            &Key::Character(winit::keyboard::SmolStr::new("z")),
            shift,
            &mut host,
        );
        assert_eq!(host.zoom_to_selection_calls, 1);
        assert_eq!(host.zoom_back_calls, 1);
    }

    #[test]
    fn cmd_z_still_routes_to_undo_not_zoom() {
        let (mut handler, mut host) = selected_host();
        let m = Modifiers {
            command: true,
            ..Modifiers::NONE
        };
        handler.handle_keyboard_input(
            &Key::Character(winit::keyboard::SmolStr::new("z")),
            m,
            &mut host,
        );
        assert_eq!(host.zoom_to_selection_calls, 0);
        assert_eq!(host.zoom_back_calls, 0);
    }
}
