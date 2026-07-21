//! Overlay driver: the single build/draw/input enumeration of the top-level
//! overlays (`OverlayId` registry), plus open-change detection, escape/close
//! bookkeeping, and selection lowering. Moved verbatim from ui_root/mod.rs
//! (UI_FUNNEL_DECOMPOSITION P-F2a, pure move).

use manifold_ui::{MappingAction};
use super::*;

impl UIRoot {
    /// The overlay for an id. The exhaustive match forces every new overlay to
    /// be wired into the driver.
    pub(crate) fn overlay_mut(&mut self, id: OverlayId) -> &mut dyn Overlay {
        match id {
            OverlayId::PerfHud => &mut self.perf_hud,
            OverlayId::Dropdown => &mut self.dropdown,
            OverlayId::Settings => &mut self.settings_popup,
            OverlayId::BrowserPopup => &mut self.browser_popup,
            OverlayId::AbletonPicker => &mut self.ableton_picker,
            OverlayId::Toast => &mut self.toast,
        }
    }

    /// Whether the overlay for `id` is currently open. Immutable mirror of
    /// `overlay_mut(id).is_open()` — the exhaustive match keeps it in lockstep
    /// with the driver registry. `pub(crate)`: used by `note_overlay_closed_if`
    /// callers outside this module to read the post-close state.
    pub(crate) fn overlay_is_open(&self, id: OverlayId) -> bool {
        match id {
            OverlayId::PerfHud => self.perf_hud.is_open(),
            OverlayId::Dropdown => self.dropdown.is_open(),
            OverlayId::Settings => self.settings_popup.is_open(),
            OverlayId::BrowserPopup => self.browser_popup.is_open(),
            OverlayId::AbletonPicker => self.ableton_picker.is_open(),
            OverlayId::Toast => self.toast.is_open(),
        }
    }

    /// Live open-set as a bitmask, bit `i` = `OverlayId::Z_ORDER[i]` is open.
    /// Seven overlays today, so a `u8` has room to spare.
    fn current_overlay_open_mask(&self) -> u8 {
        let mut mask = 0u8;
        for (i, id) in OverlayId::Z_ORDER.iter().enumerate() {
            if self.overlay_is_open(*id) {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// True if the live overlay open-set differs from what `build_overlays` last
    /// recorded — i.e. an overlay opened or closed (event-driven OR programmatic)
    /// and the overlay region in the tree is now stale. The app calls this once
    /// per frame and, on `true`, schedules a visual rebuild so the overlay region
    /// is re-recorded into `overlay_draw` and the offscreen recomposites. Read
    /// only; the snapshot updates when `build_overlays` actually runs.
    pub fn detect_overlay_open_change(&self) -> bool {
        self.built && self.current_overlay_open_mask() != self.overlay_open_snapshot
    }

    /// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D6: the redraw keepalive as ONE
    /// aggregate predicate, OR-ed into each window's own `offscreen_dirty` by
    /// its caller (never a per-window keepalive list).
    ///
    /// Membership re-derived at P2 impl via `rg "is_animating|tick\(" crates/
    /// manifold-ui/src/panels/`, not assumed from the design doc's original
    /// "toast timers and any remaining overlay tween" guess: the popup
    /// professional pass already stubbed `browser_popup`/`ableton_picker`/
    /// `settings_popup`'s `is_animating()` to a hardcoded `false` (their own
    /// doc comments say so — "no tween to settle"/"no tween to advance"), and
    /// `dropdown` never had one. Calling those permanently-false stubs here
    /// would be dead weight this predicate can never observe going `true` —
    /// so they're deliberately NOT OR-ed in; reviving any of their tweens is
    /// a one-line addition to this function, not a design change. The one
    /// live member today is the toast: its `Transient` keeps progressing
    /// through enter/hold/fade after `show()` fires and needs a tick each
    /// frame to detect completion — exactly the "hand-wired keepalive" this
    /// predicate exists to centralize.
    pub fn overlay_redraw_needed(&self) -> bool {
        self.toast.is_animating()
    }

    /// Build every open overlay into the tree, bottom→top, recording each one's
    /// node range for the draw pass. A modal that requests a dim background gets
    /// a full-screen scrim node first (and a click on it dismisses the modal,
    /// since the scrim is not one of the modal's own nodes).
    ///
    /// D1/D2: each open overlay gets its OWN region — `Overlay` tier for
    /// popups/modals, `Ghost` for the toast (a status message must stay
    /// legible over an open modal/dropdown, the same reason `Z_ORDER`
    /// already placed it last/topmost — D2/D3's "drag ghost/toast paths").
    /// `Z_ORDER`'s bottom→top loop order becomes insertion order WITHIN
    /// each tier, so relative overlay stacking is unchanged. The region's
    /// own root index is deliberately kept OUT of the `(start, end)` range
    /// this records into `overlay_draw` — `app_render.rs`'s shadow-peek
    /// heuristic (skip a leading full-screen scrim) reads `tree.id_at(start)`
    /// expecting the first REAL overlay node, not a region wrapper — so
    /// `app_render.rs` renders these ranges via `render_sub_region` (ancestor-
    /// aware: it walks the parent chain from `start` and picks up the
    /// region's `CLIPS_CHILDREN` even though the region root itself sits
    /// one index before `start`), not `render_tree_range`.
    pub(crate) fn build_overlays(&mut self) {
        let screen = Vec2::new(self.screen_width, self.screen_height);
        // Take the tree out so `overlay_mut` (which borrows all of self) can run
        // alongside tree writes — standard disjoint-borrow split.
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let region_start = tree.count();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut rects: Vec<(OverlayId, Rect)> = Vec::new();
        let full_screen = Rect::new(0.0, 0.0, screen.x, screen.y);
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if !ov.is_open() {
                continue;
            }
            let tier = if id == OverlayId::Toast { ZTier::Ghost } else { ZTier::Overlay };
            let region = tree.begin_region(full_screen, tier, "overlay", UIFlags::empty());
            let start = tree.count();
            if let Modality::Modal {
                dim_background: true,
            } = ov.modality()
            {
                tree.add_panel(
                    None,
                    0.0,
                    0.0,
                    screen.x,
                    screen.y,
                    manifold_ui::node::UIStyle {
                        bg_color: manifold_ui::node::Color32::new(0, 0, 0, 120),
                        ..manifold_ui::node::UIStyle::default()
                    },
                );
            }
            let anchor = ov.anchor();
            // Resolve the overlay's size policy against the screen (content-sized
            // by default; viewport-relative overlays scale here) before centering.
            let size = ov.size_policy().resolve(screen, ov.desired_size());
            let node_rect = if let Anchor::ToNode(nid) = anchor {
                Some(tree.get_bounds(nid))
            } else {
                None
            };
            let rect = compute_overlay_rect(&anchor, size, screen, node_rect);
            ov.build_at(&mut tree, OverlayPlacement { rect, screen });
            tree.end_region(region, start);
            ranges.push((start, tree.count()));
            rects.push((id, rect));
        }
        self.tree = tree;
        self.overlay_region_start = region_start;
        self.overlay_draw = ranges;
        self.overlay_rects = rects;
        // The tree's overlay region now matches the live open-set — record it so
        // `detect_overlay_open_change` only fires on the next genuine open/close.
        self.overlay_open_snapshot = self.current_overlay_open_mask();
    }

    /// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P1 (D2 precondition, fix-shape
    /// spec 2026-07-14): explicit-size entry point onto `build_overlays` for
    /// windows that never call `build()`. The graph editor's `Workspace::
    /// ui_root` is built via plain `UIRoot::new()` (`workspace.rs`) and never
    /// `.build()` — that method clears the tree and lays out the WHOLE
    /// main-window panel set (transport/header/footer/inspector-at-main-
    /// layout/audio-setup dock/timeline viewport) via `self.layout`, which
    /// would stomp the editor's own per-frame tree build and inject
    /// main-window UI into the editor. `build_overlays` itself is safe
    /// standalone: it only reads `screen_width`/`screen_height` and appends
    /// to the tree tail based on which overlays are open on THIS instance —
    /// no main-window-only state. The explicit size is load-bearing: the
    /// editor's `UIRoot` never receives `resize()` either (only `self.ws`
    /// does, and `resize()` itself calls `build()` — never usable for the
    /// editor), so `screen_width`/`screen_height` would otherwise be stuck at
    /// their `UIRoot::new()` default and `build_overlays`' full-screen region
    /// (`CLIPS_CHILDREN`) would clip the popup to nothing.
    pub(crate) fn build_overlays_for_screen(&mut self, width: f32, height: f32) {
        self.screen_width = width;
        self.screen_height = height;
        self.build_overlays();
    }

    /// Route one event to the open overlays, top→bottom. Returns true if an
    /// overlay consumed it (or a modal captured it), so the caller skips the
    /// lower panels. Stashed selections are lowered by `drain_overlay_selections`.
    /// Also records into `closed_overlays` any overlay whose `on_event` flipped
    /// it shut (self-close on Escape / backdrop / cell pick) — §3, D2.
    pub(crate) fn route_overlay_event(&mut self, event: &UIEvent, actions: &mut Vec<PanelAction>) -> bool {
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let mut consumed = false;
        for id in OverlayId::Z_ORDER.iter().rev() {
            let ov = self.overlay_mut(*id);
            if !ov.is_open() {
                continue;
            }
            let response = ov.on_event(event, &mut tree);
            let still_open = ov.is_open();
            let is_modal = matches!(ov.modality(), Modality::Modal { .. });
            if !still_open {
                self.closed_overlays.push(*id);
            }
            match response {
                OverlayResponse::Consumed(acts) => {
                    actions.extend(acts);
                    consumed = true;
                    if manifold_ui::input::input_trace_enabled() && trace_worthy(event) {
                        eprintln!(
                            "[input-trace] ui_root: {} CONSUMED by overlay {id:?}",
                            trace_kind(event)
                        );
                    }
                    break;
                }
                OverlayResponse::Ignored => {
                    if is_modal {
                        // A modal captures everything — no fall-through below it.
                        consumed = true;
                        if manifold_ui::input::input_trace_enabled() && trace_worthy(event) {
                            eprintln!(
                                "[input-trace] ui_root: {} CAPTURED by modal {id:?} (ignored \
                                 but not passed through)",
                                trace_kind(event)
                            );
                        }
                        break;
                    }
                }
            }
        }
        self.tree = tree;
        if consumed {
            self.overlay_dirty = true;
        }
        consumed
    }

    /// D5 — does an OPEN overlay's on-screen rect contain `pos`? Walks
    /// `Z_ORDER` top-down, same open-check `route_overlay_event` uses, over
    /// the rects `build_overlays` last recorded (`overlay_rects`) — the same
    /// rect an overlay was actually placed at, so this agrees with what's on
    /// screen. Used by `window_input`'s split-handle / inspector-edge press
    /// checks so a seam visually UNDER a floating overlay (the Audio Setup
    /// panel docked over the timeline, BUG-059) doesn't steal the press.
    /// `overlay_rects`' doc comment names the one known gap (`SelfManaged`
    /// overlays).
    pub(crate) fn overlay_contains_point(&self, pos: Vec2) -> bool {
        for id in OverlayId::Z_ORDER.iter().rev() {
            if !self.overlay_is_open(*id) {
                continue;
            }
            if let Some((_, rect)) = self.overlay_rects.iter().find(|(oid, _)| oid == id)
                && rect.contains(pos)
            {
                return true;
            }
        }
        false
    }

    /// Record `id` as closed if it was open before some out-of-band close
    /// attempt and isn't now — for close paths that don't go through
    /// `route_overlay_event`. The graph-editor window's browser popup is the
    /// live example: while it's open, the editor routes clicks straight to
    /// `browser_popup.handle_click`/`handle_escape` (bypassing the overlay
    /// driver entirely — see `app_render.rs`'s `browser_popup.is_open()`
    /// branch), so no `route_overlay_event` call ever observes its
    /// open→closed transition. The caller snapshots `was_open` immediately
    /// before the bespoke call and passes it here immediately after.
    pub(crate) fn note_overlay_closed_if(&mut self, id: OverlayId, was_open: bool) {
        if was_open && !self.overlay_is_open(id) {
            self.closed_overlays.push(id);
        }
    }

    /// Overlays whose `is_open()` flipped false since the last drain (via
    /// `route_overlay_event` or `note_overlay_closed_if`). Drained once per
    /// frame per window by the app pump, which maps each id to a
    /// `TextSessionOwner` and calls `cancel_if_owned_by` — closing the
    /// orphaned-search-session bug for every current and future
    /// overlay-hosted text field, not just the browser search
    /// (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3).
    pub fn take_closed_overlays(&mut self) -> smallvec::SmallVec<[OverlayId; 2]> {
        std::mem::take(&mut self.closed_overlays)
    }

    /// Lower any selection an overlay stashed during `route_overlay_event` into
    /// a `PanelAction`. The dropdown and Ableton picker can't form their actions
    /// themselves — the resolving context lives on `UIRoot` (the dropdown also
    /// needs cached device / resolution lists).
    pub(crate) fn drain_overlay_selections(&mut self, actions: &mut Vec<PanelAction>) {
        if let Some(dd_action) = self.dropdown.take_pending_action() {
            match dd_action {
                // Typed item — carries its own action, no index→meaning map (2b.11).
                DropdownAction::SelectedAction(action) => {
                    self.dropdown_context = None;
                    actions.push(action);
                }
                DropdownAction::Selected(_) => {
                    // 2b.11: every selectable item is typed and fires SelectedAction
                    // above, so a positional Selected can only be a non-action item.
                    // Nothing to map — just drop any stale context once closed.
                    if !self.dropdown.is_open() {
                        self.dropdown_context = None;
                    }
                }
                DropdownAction::ColorSelected(color_idx) => {
                    if let Some(ctx) = self.dropdown_context.take()
                        && let Some(action) = self.dropdown_color_to_action(ctx, color_idx)
                    {
                        actions.push(action);
                    }
                }
                DropdownAction::Dismissed => {
                    // Disabled-item clicks send Dismissed but keep the dropdown
                    // open — only clear context once it actually closed.
                    if !self.dropdown.is_open() {
                        self.dropdown_context = None;
                    }
                }
            }
        }
        if let Some(addr) = self.ableton_picker.take_pending_selection()
            && let Some(ctx) = self.ableton_picker_context.take()
        {
            use manifold_ui::panels::ableton_picker::AbletonPickerContext;
            actions.push(match ctx {
                AbletonPickerContext::Param { gpt, param_id } => {
                    PanelAction::Mapping(MappingAction::MapParamToAbleton(gpt, param_id, addr))
                }
                AbletonPickerContext::MacroSlot { slot_idx } => {
                    PanelAction::Mapping(MappingAction::MapMacroToAbleton(slot_idx, addr))
                }
            });
        }
    }

    /// Route an Escape through the overlay driver. The keyboard path consumes
    /// Escape before it reaches `process_events`, so the input-handler escape
    /// chain calls this. Returns true if an open, dismissable overlay handled it
    /// — the perf HUD (modeless, never-consuming) does not, so Escape falls
    /// through to selection clearing when only the HUD is up.
    pub fn escape_overlays(&mut self) -> bool {
        let event = UIEvent::KeyDown {
            node_id: NodeId::PLACEHOLDER,
            key: Key::Escape,
            modifiers: Modifiers::default(),
        };
        let mut actions = Vec::new();
        let consumed = self.route_overlay_event(&event, &mut actions);
        if consumed {
            self.drain_overlay_selections(&mut actions);
        }
        consumed
    }

    /// Filter overlay-generated actions (TrackRightClicked, ClipRightClicked)
    /// through the dropdown system. Called by app.rs after the overlay processes
    /// viewport events — these actions are generated AFTER process_events()
    /// returns, so they need a second pass through try_open_dropdown.
    pub fn intercept_overlay_actions(&mut self, actions: &mut Vec<PanelAction>) {
        actions.retain(|action| !self.try_open_dropdown(action, None));
    }
}

/// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` P2: `UIRoot::overlay_redraw_needed()`
/// unit tests — proves the aggregate's wiring to its one live member (the
/// toast; see that function's doc comment for why the popup professional
/// pass's now-permanently-`false` `is_animating()` stubs aren't OR-ed in).
#[cfg(test)]
mod overlay_redraw_needed_tests {
    use super::*;

    #[test]
    fn false_on_a_fresh_root_with_nothing_open() {
        let ui = UIRoot::new();
        assert!(!ui.overlay_redraw_needed());
    }

    /// The named per-member proof from the phase brief: an animating overlay
    /// (here, the toast just fired) flips the aggregate `true`.
    #[test]
    fn toast_animating_flips_the_aggregate_true() {
        let mut ui = UIRoot::new();
        assert!(!ui.overlay_redraw_needed(), "idle toast contributes nothing");

        ui.toast.show("Undo");
        assert!(
            ui.overlay_redraw_needed(),
            "a freshly-fired toast is still ramping in — the aggregate must \
             see it as needing a keepalive redraw"
        );
    }

    /// Once the toast's whole enter/hold/fade timeline elapses, the
    /// aggregate drops back to `false` — the keepalive isn't permanent.
    #[test]
    fn settles_back_to_false_once_the_toast_finishes() {
        let mut ui = UIRoot::new();
        ui.toast.show("Redo");
        assert!(ui.overlay_redraw_needed());

        // Comfortably longer than the toast's total enter+hold+fade timeline.
        ui.toast.tick(10_000.0);
        assert!(!ui.overlay_redraw_needed(), "toast settled — no more keepalive needed");
    }
}
