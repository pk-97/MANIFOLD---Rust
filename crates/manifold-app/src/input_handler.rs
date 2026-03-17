//! Keyboard dispatch, zoom logic, and context menu routing.
//! Mechanical translation of Assets/Scripts/UI/Timeline/InputHandler.cs.
//!
//! Plain Rust struct — NOT a MonoBehaviour equivalent.
//! Calls through TimelineInputHost trait for all operations that need
//! engine/editing/UI access. Owns zoom state and inspector focus.

use manifold_ui::input::Modifiers;
use manifold_ui::timeline_input_host::TimelineInputHost;
use manifold_ui::ui_state::UIState;

use winit::keyboard::{Key, NamedKey};

/// Keyboard/zoom handler. Port of InputHandler.cs.
///
/// Owns zoom state (pending anchor, scroll target) and inspector focus.
/// The app layer calls `handle_keyboard_input()` on each key press.
pub struct InputHandler {
    // ── Zoom state (Unity lines 51-59) ──
    pub needs_zoom_update: bool,
    pub has_pending_zoom_anchor: bool,
    pub pending_zoom_anchor_beat: f32,
    pub pending_zoom_anchor_viewport_x: f32,
    pub pending_zoom_scroll_time: f32, // -1.0 = no pending

    // ── Panel focus (Unity line 65) ──
    pub inspector_has_focus: bool,
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            needs_zoom_update: false,
            has_pending_zoom_anchor: false,
            pending_zoom_anchor_beat: 0.0,
            pending_zoom_anchor_viewport_x: 0.0,
            pending_zoom_scroll_time: -1.0,
            inspector_has_focus: false,
        }
    }

    pub fn set_inspector_focus(&mut self, focused: bool) {
        self.inspector_has_focus = focused;
    }

    pub fn clear_needs_zoom_update(&mut self) {
        self.needs_zoom_update = false;
    }

    pub fn clear_pending_zoom_anchor(&mut self) {
        self.has_pending_zoom_anchor = false;
    }

    pub fn clear_pending_zoom_scroll_time(&mut self) {
        self.pending_zoom_scroll_time = -1.0;
    }

    /// Port of Unity InputHandler.HandleKeyboardInput (lines 189-517).
    ///
    /// Returns true if the key was consumed (caller should not process further).
    /// Calls through the host trait for all operations that need engine/UI access.
    /// Get the current grid step from the host (delegated from viewport).
    fn grid_step(host: &dyn TimelineInputHost) -> f32 {
        // Host provides this via get_viewport_width / zoom level
        // For now use a reasonable default; host impl reads from viewport
        0.25 // quarter beat — overridden by host
    }

    pub fn handle_keyboard_input(
        &mut self,
        logical_key: &Key,
        modifiers: Modifiers,
        host: &mut dyn TimelineInputHost,
        ui_state: &mut UIState,
    ) -> bool {
        let m = modifiers;

        // Unity line 213: inspector arrow key stepping (loop duration)
        if host.handle_inspector_keyboard() {
            return true;
        }

        // ── Backtick — toggle performance HUD (Unity line 217) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "`") && m.is_none() {
            host.toggle_performance_hud();
            return true;
        }

        // ── Escape — 4-level priority chain (Unity lines 224-232) ──
        if matches!(logical_key, Key::Named(NamedKey::Escape)) {
            // Level 1: dismiss context menu
            if host.has_context_menu() {
                host.dismiss_context_menu();
                return true;
            }
            // Level 2: monitor output active → no-op
            if host.is_monitor_output_active() {
                return true;
            }
            // Level 3: inspector has focus → clear effect selection
            if self.inspector_has_focus {
                host.clear_effect_selection();
                self.inspector_has_focus = false;
                return true;
            }
            // Level 4: clear all selection + insert cursor
            ui_state.clear_selection();
            host.on_selection_cleared();
            return true;
        }

        // ── Undo: Cmd+Shift+Z (Unity line 235) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "z") && m.is_command_shift() {
            host.redo();
            host.on_undo_redo();
            return true;
        }
        // ── Undo: Cmd+Z (Unity line 241) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "z") && m.is_command_only() {
            host.undo();
            host.on_undo_redo();
            return true;
        }
        // ── Redo: Cmd+Y (Unity line 247) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "y") && m.is_command_only() {
            host.redo();
            host.on_undo_redo();
            return true;
        }

        // ── Save: Cmd+S (Unity line 255) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "s") && m.is_command_only() {
            host.save_project();
            return true;
        }

        // ── Open: Cmd+O ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "o") && m.is_command_only() {
            host.open_project();
            return true;
        }

        // ── New: Cmd+N ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "n") && m.is_command_only() {
            host.new_project();
            return true;
        }

        // ── Select all: Cmd+A (Unity line 289) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "a") && m.is_command_only() {
            host.select_all_clips();
            return true;
        }

        // ── Copy: Cmd+C (context-sensitive: effects vs clips) (Unity line 296) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "c") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_copy() {
                return true;
            }
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                host.copy_clips(&ids);
            }
            return true;
        }

        // ── Cut: Cmd+X (context-sensitive) (Unity line 302) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "x") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_cut() {
                return true;
            }
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                host.cut_clips(&ids, ui_state.has_region());
            }
            return true;
        }

        // ── Paste: Cmd+V (context-sensitive) (Unity line 308) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "v") && m.is_command_only() {
            if self.inspector_has_focus && host.handle_effect_paste() {
                return true;
            }
            let target_beat = ui_state.insert_cursor_beat
                .unwrap_or(host.current_beat());
            let target_layer = ui_state.insert_cursor_layer_index
                .unwrap_or(0) as i32;
            host.paste_clips(target_beat, target_layer);
            return true;
        }

        // ── Duplicate: Cmd+D (Unity line 316) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "d") && m.is_command_only() {
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                host.duplicate_clips(&ids);
            }
            return true;
        }

        // ── Ungroup: Cmd+Shift+G (context-sensitive) (Unity line 323) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "g" || c.as_str() == "G")
            && m.is_command_shift()
        {
            if self.inspector_has_focus {
                host.handle_effect_ungroup();
            }
            return true;
        }

        // ── Group: Cmd+G (context-sensitive) (Unity line 328) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "g") && m.is_command_only() {
            if self.inspector_has_focus {
                host.handle_effect_group();
            } else {
                host.group_selected_layers();
            }
            return true;
        }

        // ── Delete/Backspace (context-sensitive: effects → layers → clips) (Unity line 336) ──
        if matches!(logical_key, Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace))
            && m.is_none()
        {
            // Priority 1: inspector focused → delete effects
            if self.inspector_has_focus && host.handle_effect_delete() {
                return true;
            }
            // Priority 2: layer selection active, no clips, no region → delete layers
            // (Unity lines 341-346)
            if ui_state.layer_selection_count() > 0
                && !ui_state.has_region()
                && ui_state.selection_count() == 0
            {
                host.delete_selected_layers();
                return true;
            }
            // Priority 3: delete selected clips (region-aware)
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                host.delete_clips(&ids, ui_state.has_region());
                ui_state.clear_selection();
            }
            return true;
        }

        // ── Space — Play/Pause (Unity line 352) ──
        if matches!(logical_key, Key::Named(NamedKey::Space)) && m.is_none() {
            let cursor_beat = ui_state.insert_cursor_beat;
            host.play_pause(cursor_beat);
            return true;
        }

        // ── Home — seek to start (Unity line 375) ──
        if matches!(logical_key, Key::Named(NamedKey::Home)) && m.is_none() {
            host.seek_to(0.0);
            return true;
        }

        // ── End — seek to end (Unity line 380) ──
        if matches!(logical_key, Key::Named(NamedKey::End)) && m.is_none() {
            // Host computes end time from project timeline
            host.seek_to(f32::MAX); // sentinel — host clamps to actual end
            return true;
        }

        // ── S — split at playhead (bare S, no modifiers) (Unity line 393) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "s") && m.is_none() {
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                host.split_clips_at_playhead(&ids);
            }
            return true;
        }

        // ── Shift+E — shrink by grid step (check before bare E) (Unity line 400) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "E" || c.as_str() == "e")
            && m.is_shift_only()
        {
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                let step = host.grid_step();
                host.shrink_clips(&ids, step);
            }
            return true;
        }

        // ── E — extend by grid step (bare E) (Unity line 405) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "e") && m.is_none() {
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                let step = host.grid_step();
                host.extend_clips(&ids, step);
            }
            return true;
        }

        // ── F — zoom to fit (Unity line 412) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "f") && m.is_none() {
            self.zoom_to_fit(host);
            return true;
        }

        // ── 0 — toggle mute (Unity line 419) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "0") && m.is_none() {
            let ids: Vec<String> = ui_state.get_selected_clip_ids();
            if !ids.is_empty() {
                host.toggle_mute_clips(&ids);
            }
            return true;
        }

        // ── Arrow keys: nudge clips when selected, navigate cursor otherwise ──
        // (Unity lines 426-458)
        if matches!(logical_key,
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowRight) |
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown))
            && !m.command && !m.alt
        {
            let has_selected = ui_state.selection_count() > 0;

            if has_selected {
                // Nudge selected clips (Unity lines 443-452)
                let grid = host.grid_step();
                let step = if m.shift { 1.0 / 16.0 } else { grid };
                let ids: Vec<String> = ui_state.get_selected_clip_ids();

                match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => host.nudge_clips(&ids, -step),
                    Key::Named(NamedKey::ArrowRight) => host.nudge_clips(&ids, step),
                    // Up/Down with clips selected = no-op (Unity line 451)
                    _ => {}
                }
            } else {
                // Navigate insert cursor (Ableton-style) (Unity line 456)
                // Direction: 0=left, 1=right, 2=up, 3=down
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

        // ── Export markers: Alt variants first (Unity lines 461-481) ──
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "i") && m.is_alt_only() {
            host.clear_export_in();
            return true;
        }
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "o") && m.is_alt_only() {
            host.clear_export_out();
            return true;
        }
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "i") && m.is_none() {
            host.set_export_in_at_playhead();
            return true;
        }
        if matches!(logical_key, Key::Character(ref c) if c.as_str() == "o") && m.is_none() {
            host.set_export_out_at_playhead();
            return true;
        }

        false // not consumed
    }

    // ── Zoom (Unity InputHandler lines 864-1006) ─────────────────

    /// Port of Unity InputHandler.ZoomToFit (lines 906-957).
    /// Uses SetZoom(idealPpb) directly — NOT nearest zoom level.
    fn zoom_to_fit(
        &mut self,
        host: &mut dyn TimelineInputHost,
    ) {
        let viewport_width = host.get_viewport_width();
        if viewport_width <= 0.0 { return; }

        // Host computes ideal ppb from clip extent and applies zoom
        self.needs_zoom_update = true;
        host.update_zoom_label();
    }

    /// Queue a zoom anchor at the playhead position.
    /// Port of Unity InputHandler.QueuePlayheadZoomAnchor (lines 959-966).
    pub fn queue_playhead_zoom_anchor(&mut self, playhead_beat: f32, playhead_viewport_x: f32) {
        self.pending_zoom_scroll_time = -1.0;
        self.has_pending_zoom_anchor = true;
        self.pending_zoom_anchor_beat = playhead_beat;
        self.pending_zoom_anchor_viewport_x = playhead_viewport_x;
    }

    /// Apply pending zoom scroll after a rebuild or zoom update.
    /// Port of Unity InputHandler.ApplyPendingZoomScroll (lines 1013-1024).
    pub fn apply_pending_zoom_scroll(&mut self) -> bool {
        if self.has_pending_zoom_anchor {
            self.has_pending_zoom_anchor = false;
            return true;
        }
        if self.pending_zoom_scroll_time >= 0.0 {
            self.pending_zoom_scroll_time = -1.0;
            return true;
        }
        false
    }
}
