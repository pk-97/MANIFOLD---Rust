//! Event pump: node-intent registry (re)population and resolution, the main
//! `process_events` drain-and-route loop, and viewport-event stashing. Moved
//! verbatim from ui_root/mod.rs (UI_FUNNEL_DECOMPOSITION P-F2a, pure move).

use super::*;

impl UIRoot {
    /// Clear and repopulate node-intent dispatch from every panel's currently
    /// stored node ids. A full rebuild each call keeps the registry consistent
    /// with partial tree rebuilds (truncate_from) without per-range bookkeeping
    /// — panels register against whatever ids they hold now.
    pub(crate) fn repopulate_intents(&mut self) {
        use manifold_ui::panels::Panel;
        self.intents.clear();
        self.transport.register_intents(&mut self.intents);
        self.header.register_intents(&mut self.intents);
        self.footer.register_intents(&mut self.intents);
        self.layer_headers.register_intents(&mut self.intents);
        self.inspector.register_intents(&mut self.intents);
        self.viewport.register_intents(&mut self.intents);
        self.scene_setup_panel.register_intents(&mut self.intents);
    }

    /// Resolve a discrete-gesture event through node-intent dispatch. Returns
    /// the registered `PanelAction` for the nearest intent-bearing ancestor of
    /// the hit node, or None for non-gesture events / un-registered surfaces.
    pub(crate) fn resolve_intent(&self, event: &UIEvent) -> Option<PanelAction> {
        use manifold_ui::intent::Gesture;
        let (node_id, gesture) = match event {
            UIEvent::Click { node_id, .. } => (Some(*node_id), Gesture::Click),
            UIEvent::DoubleClick { node_id, .. } => (Some(*node_id), Gesture::DoubleClick),
            UIEvent::RightClick { node_id, .. } => (*node_id, Gesture::RightClick),
            _ => return None,
        };
        self.intents.resolve(&self.tree, node_id, gesture)
    }

    /// Drain events from the input system and route to panels.
    /// Returns all panel actions for the app layer to dispatch.
    pub fn process_events(&mut self) -> Vec<PanelAction> {
        if !self.built {
            return Vec::new();
        }

        // Refresh node-intent dispatch only when the tree structurally changed
        // (gated on the tree's structure_version) — never per-frame, so the
        // registry's per-entry boxing stays off the hot path. Set-only frames
        // (hover, value sync) leave node ids intact and skip this entirely.
        let sv = self.tree.structure_version();
        if sv != self.intents_structure_version {
            self.repopulate_intents();
            self.intents_structure_version = sv;
        }

        let events = self.input.drain_events();
        let mut actions = Vec::new();

        // Drain continuous hover actions accumulated from cursor movement.
        actions.append(&mut self.cursor_hover_actions);
        // Drain keyboard-driven picker actions (arrow/Enter nav) stashed by
        // `window_input.rs`'s text-input-active branch — see
        // `pending_keyboard_actions`'s doc comment.
        actions.append(&mut self.pending_keyboard_actions);

        let mut last_click_node: Option<NodeId> = None;
        for event in &events {
            // Track which node was clicked (for dropdown anchoring).
            if let UIEvent::Click { node_id, .. } = event {
                last_click_node = Some(*node_id);
            }
            if let UIEvent::RightClick { pos, .. } = event {
                self.last_right_click_pos = *pos;
            }

            // Self-heal: a stale owner can only mean
            // the previous gesture's terminal event never reached the
            // broadcast. The next PointerDown clears it, firing the same
            // unconditional broadcast a normal terminal event would.
            if matches!(event, UIEvent::PointerDown { .. }) && self.drag_owner.is_some() {
                self.broadcast_gesture_end();
            }

            // Global: ⌘⇧A toggles the Audio Setup panel. Emit the same action the
            // "audio" button does so the single app-side handler owns the toggle
            // plus its one-shot data sync — rather than toggling here and leaving
            // the panel's device/send list unpopulated. Handled before overlay
            // routing so an open modal can't capture the keystroke and block it
            // from toggling shut.
            if let UIEvent::KeyDown { key: Key::A, modifiers, .. } = event
                && modifiers.command
                && modifiers.shift
            {
                actions.push(PanelAction::OpenAudioSetup);
                continue;
            }

            // All open overlays (dropdown, modals, perf HUD) get first crack at
            // the event through the single driver. If one consumes it (or a modal
            // captures it), lower any stashed selection and skip the panels below.
            if self.route_overlay_event(event, &mut actions) {
                // D6/§3.4 (`docs/DRAG_CAPTURE_DESIGN.md`): the consuming overlay
                // may have just armed a precision-drag surface (audio panel's
                // band divider) on this exact PointerDown — request zero-
                // threshold drag for the current press so the very next Move
                // begins the drag instead of waiting for the 4px threshold.
                if matches!(event, UIEvent::PointerDown { .. })
                    && self.any_overlay_wants_immediate_drag()
                {
                    self.input.request_immediate_drag();
                }
                self.drain_overlay_selections(&mut actions);
                continue;
            }

            // Escape closes the Audio Setup dock (D1/§3.5) — the ONE key path,
            // handled AFTER overlays (a dropdown/settings opened over the app
            // gets Escape first) and routed through the same `OpenAudioSetup`
            // toggle the header button and the × use.
            if self.audio_setup_panel.is_open()
                && matches!(event, UIEvent::KeyDown { key: Key::Escape, .. })
            {
                actions.push(PanelAction::OpenAudioSetup);
                continue;
            }

            // Escape closes the Scene Setup dock — same mirrored path
            // (SCENE_SETUP_PANEL_DESIGN D2).
            if self.scene_setup_panel.is_open()
                && matches!(event, UIEvent::KeyDown { key: Key::Escape, .. })
            {
                actions.push(PanelAction::OpenSceneSetup);
                continue;
            }

            // Audio Setup dock (D1) — a docked panel routed here, not an
            // overlay. It handles its own clicks + band/calibration drags and
            // consumes them so they don't fall through to the panels beneath.
            // A `PointerDown` that armed a band-divider grab requests immediate
            // drag so a 1px move begins the drag (no 4px threshold wait).
            if self.audio_setup_panel.is_open() {
                let (consumed, mut acts) = self.audio_setup_panel.handle_event(event);
                actions.append(&mut acts);
                if consumed {
                    if matches!(event, UIEvent::PointerDown { .. })
                        && self.audio_setup_panel.wants_immediate_drag()
                    {
                        self.input.request_immediate_drag();
                    }
                    continue;
                }
            }

            // Scene Setup dock — mirror of the Audio Setup routing above.
            if self.scene_setup_panel.is_open() {
                let (consumed, mut acts) = self.scene_setup_panel.handle_event(event, &self.tree);
                actions.append(&mut acts);
                if consumed {
                    continue;
                }
            }

            // Node-intent dispatch: discrete gestures (click / double-click /
            // right-click) resolve by folding the hit node up its parent chain
            // to the nearest ancestor carrying intent. Migrated panels register
            // intent in `build`/`register_intents` and drop their `handle_event`
            // arms; for un-migrated surfaces `resolve` returns None and the
            // event flows to the per-panel handlers below unchanged. A resolved
            // gesture is consumed here — it would otherwise double-fire.
            if let Some(action) = self.resolve_intent(event) {
                actions.push(action);
                continue;
            }

            // Route to panels. Transport, header, and footer are fully
            // intent-dispatched (see `resolve_intent` above) — their clicks
            // resolve and `continue` before reaching here, so they have no
            // panel-side click handler to call.
            let mut panel_actions = self.layer_headers.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            panel_actions = self.inspector.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);

            // Viewport: ruler events handled by viewport panel (Seek/scrub).
            // Tracks-area events stashed for InteractionOverlay in app.rs.
            panel_actions = self.viewport.handle_event(event, &self.tree);
            actions.append(&mut panel_actions);
        }

        // Route Drag/DragEnd/PointerUp to inspector directly (needs &mut tree for
        // slider feedback). Separate from the panel event loop because
        // Panel::handle_event takes &UITree, but slider drag updates need &mut UITree.
        //
        // PointerUp handling: Unity's OnPointerUp ALWAYS fires on mouse release.
        // If the user clicked a slider without crossing the 4px DRAG_THRESHOLD,
        // no DragEnd fires — but PointerUp still does. We route PointerUp through
        // handle_drag_end so the sub-panel's dragging state is cleared and the
        // undo snapshot is committed. handle_drag_end is idempotent: if DragEnd
        // already cleared pressed_target, PointerUp is a no-op.
        //
        // D1/D2 (`docs/DRAG_CAPTURE_DESIGN.md` §3.2/§3.3): `DragBegin` still arms
        // the inspector/layer-header drag state unconditionally, exactly as
        // before — that arming is what `resolve_drag_owner` reads immediately
        // after, to fix `drag_owner` for the rest of the gesture. `Drag`
        // continuation is gated on the now-fixed owner instead of re-checking
        // the private flags directly. `DragEnd`/`PointerUp` keep the existing
        // unconditional, idempotent end-calls (the broadcast precedent this
        // design generalizes), then `fire_gesture_end_hooks` runs every other
        // overlay's `gesture_ended` hook. The `drag_owner` clear is deferred
        // to the END of the terminal iteration — past the stash read — so the
        // timeline's terminal `DragEnd` is still routed to it (BUG-075).
        for event in &events {
            match event {
                UIEvent::DragBegin { node_id, origin, .. } => {
                    // Effect card drag handle — try to start card reorder drag
                    self.inspector.try_begin_card_drag(*node_id, &mut self.tree);
                    // Layer header drag handle — needs &mut tree for dim/indicator
                    let mut lh_actions = self
                        .layer_headers
                        .handle_drag_begin(&mut self.tree, *node_id);
                    actions.append(&mut lh_actions);
                    self.drag_owner = self.resolve_drag_owner(*origin, *node_id);
                }
                UIEvent::Drag { pos, .. } => {
                    if self.drag_owner == Some(DragOwner::Inspector) {
                        if self.inspector.is_card_drag_active() {
                            self.inspector.update_card_drag(*pos, &mut self.tree);
                        } else if self.inspector.has_pressed_target() {
                            let mut drag_actions =
                                self.inspector.handle_drag(*pos, &mut self.tree);
                            actions.append(&mut drag_actions);
                        }
                    }
                    if self.drag_owner == Some(DragOwner::LayerHeaders) {
                        if self.layer_headers.is_dragging() {
                            let mut lh_actions = self.layer_headers.handle_drag(
                                &mut self.tree,
                                *pos,
                                self.viewport.mapper(),
                            );
                            actions.append(&mut lh_actions);
                        }
                        if self.layer_headers.is_gain_dragging() {
                            let mut g_actions =
                                self.layer_headers.handle_gain_drag(&mut self.tree, pos.x);
                            actions.append(&mut g_actions);
                        }
                    }
                }
                UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                    if self.inspector.is_card_drag_active() {
                        let mut reorder_actions = self.inspector.end_card_drag(&mut self.tree);
                        actions.append(&mut reorder_actions);
                    } else if self.inspector.has_pressed_target() {
                        let mut end_actions = self.inspector.handle_drag_end(&mut self.tree);
                        actions.append(&mut end_actions);
                    }
                    if self.layer_headers.is_dragging() {
                        let mut lh_actions = self.layer_headers.handle_drag_end(&mut self.tree);
                        actions.append(&mut lh_actions);
                    }
                    if self.layer_headers.is_gain_dragging() {
                        let mut g_actions = self.layer_headers.handle_gain_drag_end();
                        actions.append(&mut g_actions);
                    }
                    // Fire the overlay end hooks now, but leave `drag_owner`
                    // set — the stash classification below (`should_stash_for
                    // _tracks`) needs it to route this terminal `DragEnd` to
                    // the timeline. The owner is cleared as the last step of
                    // this iteration, past the stash read (BUG-075).
                    self.fire_gesture_end_hooks();
                }
                _ => {}
            }

            // Stash for `InteractionOverlay` (tracks-area events).
            let stash = self.should_stash_for_tracks(event);
            if manifold_ui::input::input_trace_enabled()
                && matches!(event, UIEvent::DragBegin { .. } | UIEvent::DragEnd { .. })
            {
                eprintln!(
                    "[input-trace] ui_root: {} {} for timeline overlay (drag_owner={:?})",
                    trace_kind(event),
                    if stash { "STASHED" } else { "NOT stashed" },
                    self.drag_owner
                );
            }
            if stash {
                self.viewport_events.push(event.clone());
            }

            // Owner lifetime: the stash read just above
            // still needs `drag_owner`, so the terminal clear happens HERE —
            // after both the fire-hooks (in the match arm) and the stash
            // classification — never earlier. This is the fix's whole point:
            // fire hooks, stash by owner, then clear. Re-folding the clear
            // into `fire_gesture_end_hooks` would reintroduce BUG-075.
            if matches!(event, UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. }) {
                self.drag_owner = None;
            }
        }

        // Intercept dropdown-triggering actions and open dropdowns here
        // (where we have access to the tree for node bounds).
        let popup_open_before = self.browser_popup.is_open();
        let mut filtered = Vec::with_capacity(actions.len());
        for action in actions {
            if self.try_open_dropdown(&action, last_click_node) {
                // Consumed — don't forward to dispatch.
                continue;
            }
            filtered.push(action);
        }

        // If a popup was just opened, flag for rebuild so nodes appear this frame.
        if !popup_open_before && (self.browser_popup.is_open() || self.ableton_picker.is_open()) {
            self.overlay_dirty = true;
        }

        filtered
    }

    /// Drain viewport-area events stashed during process_events().
    /// App.rs routes these through the InteractionOverlay with a host trait.
    pub fn drain_viewport_events(&mut self) -> Vec<manifold_ui::input::UIEvent> {
        std::mem::take(&mut self.viewport_events)
    }
}
