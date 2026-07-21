//! Drag-capture ownership: who owns the in-flight pointer gesture
//! (`DragOwner`), resolved once at DragBegin and cleared by the terminal
//! broadcast, plus the tracks-stash classification. Moved verbatim from
//! ui_root/mod.rs (UI_FUNNEL_DECOMPOSITION P-F2a, pure move).

use super::*;

impl UIRoot {
    /// D1 — resolve who owns an in-flight drag gesture, once, at the
    /// gesture's first `DragBegin`. Fixed order, first claim wins (§3.2):
    /// open overlays z-top-down → inspector → layer headers → ruler →
    /// timeline tracks → nobody. `node_id` is accepted for signature parity
    /// with the design's committed call (`docs/DRAG_CAPTURE_DESIGN.md` §3.2)
    /// — no resolution step needs it today; every claim is origin/state based.
    pub(crate) fn resolve_drag_owner(&mut self, origin: Vec2, _node_id: Option<NodeId>) -> Option<DragOwner> {
        // 1. Open overlays, z-top-down — same walk as `route_overlay_event`,
        // but this pass only reads `is_open`/`modality`/`claims_drag`; it
        // never delivers the event (that still happens through the normal
        // gauntlet). A modal claims unconditionally (D4). A modeless overlay
        // claims iff `claims_drag(origin)` says so (P1: no overlay overrides
        // the default `false` yet — the audio panel's override lands P2).
        // The dropdown specifically: an open dropdown that does NOT claim is
        // dismissed here as a side effect, WITHOUT consuming (D3) — same UX
        // as today's outside-click dismiss, minus the BUG-058 eat-arm.
        let mut tree = std::mem::replace(&mut self.tree, UITree::new());
        let mut owner = None;
        for id in OverlayId::Z_ORDER.iter().rev() {
            let ov = self.overlay_mut(*id);
            if !ov.is_open() {
                continue;
            }
            if matches!(ov.modality(), Modality::Modal { .. }) {
                owner = Some(DragOwner::Overlay(*id));
                break;
            }
            if ov.claims_drag(origin) {
                owner = Some(DragOwner::Overlay(*id));
                break;
            }
            if *id == OverlayId::Dropdown {
                self.dropdown.close(&mut tree);
                self.closed_overlays.push(OverlayId::Dropdown);
                self.overlay_dirty = true;
            }
        }
        self.tree = tree;
        if owner.is_some() {
            return owner;
        }

        // 2. Inspector — slider drag (`pressed_target`, armed on PointerDown)
        // or effect-card reorder (`card_drag_active`, armed on DragBegin by
        // the caller just before this resolution runs).
        if self.inspector.has_pressed_target() || self.inspector.is_card_drag_active() {
            return Some(DragOwner::Inspector);
        }
        // 3. Layer headers — reorder or gain drag (same arm-then-resolve
        // ordering as the inspector).
        if self.layer_headers.is_dragging() || self.layer_headers.is_gain_dragging() {
            return Some(DragOwner::LayerHeaders);
        }
        // 4. Ruler (D7 — confirmed kept, `viewport/interaction.rs` scrub is
        // Drag-based).
        if self.viewport.ruler_rect().contains(origin) {
            return Some(DragOwner::Ruler);
        }
        // 5. TimelineTracks — the fallback today's stash gate approximated.
        if self.viewport.tracks_rect().contains(origin) {
            return Some(DragOwner::TimelineTracks);
        }
        // 6. Nobody.
        None
    }

    /// Fire the end-of-gesture hook on every OPEN overlay (idempotent
    /// `gesture_ended` clears; default no-op, the audio panel overrides).
    /// Does NOT touch `drag_owner` — split out of `broadcast_gesture_end`
    /// so the terminal-event path can fire the hooks while `drag_owner` is
    /// still live for the stash classification, then clear the owner as the
    /// last step of the terminal iteration (see `process_events`). Clearing
    /// the owner one line too early was BUG-075: it nulled the owner before
    /// `should_stash_for_tracks` read it, so a timeline gesture's terminal
    /// `DragEnd` was never stashed and `on_end_drag` never ran (trim /
    /// marquee never finalized).
    pub(crate) fn fire_gesture_end_hooks(&mut self) {
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if ov.is_open() {
                ov.gesture_ended();
            }
        }
        // The Audio Setup dock is no longer an overlay (D1) but keeps the same
        // idempotent end-of-gesture clear for its band/calibration drags.
        if self.audio_setup_panel.is_open() {
            self.audio_setup_panel.gesture_ended();
        }
    }

    /// D2/§3.3 — the terminal broadcast every gesture that began gets,
    /// exactly once, no matter who owned it or what ate the routed event.
    /// The fused form (hooks + `drag_owner` clear) is the self-heal on the
    /// next `PointerDown` when a stale owner survived (a lost OS release —
    /// `docs/DRAG_CAPTURE_DESIGN.md` §3.3 failure story). The normal terminal
    /// path does NOT use this — it calls `fire_gesture_end_hooks` and defers
    /// the clear past the stash read (see `process_events`); the two must not
    /// be re-fused (BUG-075).
    pub(crate) fn broadcast_gesture_end(&mut self) {
        self.fire_gesture_end_hooks();
        self.drag_owner = None;
    }

    /// D6/§3.4 (`docs/DRAG_CAPTURE_DESIGN.md`) — after a `PointerDown` is
    /// consumed by `route_overlay_event`, does any OPEN overlay want
    /// immediate-drag armed for this press? `route_overlay_event` returns
    /// only whether an overlay consumed the event, not which one — but only
    /// the overlay that actually consumed THIS `PointerDown` could have just
    /// armed anything (every other open overlay never saw the event, and its
    /// per-press state is cleared every gesture end by `gesture_ended`), so
    /// polling the whole open set (same `Z_ORDER` walk `broadcast_gesture_end`
    /// uses) identifies the same overlay without threading its identity back
    /// out of `route_overlay_event`.
    pub(crate) fn any_overlay_wants_immediate_drag(&mut self) -> bool {
        for id in OverlayId::Z_ORDER {
            let ov = self.overlay_mut(id);
            if ov.is_open() && ov.wants_immediate_drag() {
                return true;
            }
        }
        false
    }

    /// Whether `event` should stash into `viewport_events` for
    /// `InteractionOverlay` (`docs/DRAG_CAPTURE_DESIGN.md` §3.2). The drag
    /// family (`DragBegin`/`Drag`/`DragEnd`) stashes by OWNERSHIP,
    /// unconditionally, no position check — `resolve_drag_owner` fixes
    /// `drag_owner` on `DragBegin` (see the `process_events` drag loop), and
    /// it persists across frames for `Drag`/`DragEnd`. This is what makes a
    /// timeline drag released outside `tracks_rect` (e.g. over the inspector)
    /// still reach `InteractionOverlay::on_end_drag` — today's positional
    /// gate would have dropped it. Every other event kind keeps the
    /// positional classification (`is_event_in_tracks_area`).
    pub(crate) fn should_stash_for_tracks(&self, event: &manifold_ui::input::UIEvent) -> bool {
        use manifold_ui::input::UIEvent;
        match event {
            UIEvent::DragBegin { .. } | UIEvent::Drag { .. } | UIEvent::DragEnd { .. } => {
                self.drag_owner == Some(DragOwner::TimelineTracks)
            }
            _ => self.is_event_in_tracks_area(event),
        }
    }

    /// Check if a UI event's position falls within the tracks area. The drag
    /// family (`DragBegin`/`Drag`/`DragEnd`) no longer classifies here — it
    /// stashes by ownership instead (`should_stash_for_tracks`). This keeps
    /// only the positional classification for discrete/non-drag events.
    fn is_event_in_tracks_area(&self, event: &manifold_ui::input::UIEvent) -> bool {
        use manifold_ui::input::UIEvent;
        let pos = match event {
            UIEvent::Click { pos, .. } => *pos,
            UIEvent::DoubleClick { pos, .. } => *pos,
            UIEvent::RightClick { pos, .. } => *pos,
            UIEvent::HoverEnter { pos, .. } => *pos,
            UIEvent::PointerDown { pos, .. } => *pos,
            _ => return false,
        };
        self.viewport.tracks_rect().contains(pos)
    }
}

#[cfg(test)]
mod drag_capture_tests {
    //! `docs/DRAG_CAPTURE_DESIGN.md` P1 unit tests. `UIRoot::new()` +
    //! `resize()` is enough scaffolding for these — `resize` runs `build()`
    //! (sets `built = true`, computes real `tracks_rect`/`ruler_rect` from
    //! the layout) without needing a live `Project` (the same sequencing
    //! `ui_snapshot/script.rs` uses before `sync_build`).
    use super::*;

    const W: f32 = 1536.0;
    const H: f32 = 1216.0;

    fn new_root() -> UIRoot {
        let mut ui = UIRoot::new();
        ui.resize(W, H);
        ui
    }

    fn center(r: Rect) -> Vec2 {
        Vec2::new(r.x + r.width * 0.5, r.y + r.height * 0.5)
    }

    /// D4: a modal claims a drag unconditionally, regardless of where the
    /// drag originated — even far outside the modal's own rect.
    #[test]
    fn modal_overlay_claims_drag_unconditionally() {
        let mut ui = new_root();
        ui.settings_popup.open();
        assert!(matches!(ui.settings_popup.modality(), Modality::Modal { .. }));

        let far_away = Vec2::new(W - 1.0, H - 1.0);
        let owner = ui.resolve_drag_owner(far_away, None);
        assert_eq!(owner, Some(DragOwner::Overlay(OverlayId::Settings)));
    }

    /// D3: an open dropdown that does NOT claim a foreign drag is dismissed
    /// as a side effect of resolution — WITHOUT consuming — and ownership
    /// passes to the real owner underneath (here, the tracks area). This is
    /// the BUG-058 wedge fixed by construction: nothing ever routes the
    /// drag's terminal event to the dropdown for it to eat.
    #[test]
    fn dropdown_open_at_drag_start_dismisses_without_consuming_and_falls_through() {
        let mut ui = new_root();
        let mut scratch_tree = UITree::new();
        ui.dropdown.open(
            vec![],
            Rect::new(10.0, 10.0, 50.0, 20.0),
            50.0,
            &mut scratch_tree,
        );
        assert!(ui.dropdown.is_open());

        let tracks_origin = center(ui.viewport.tracks_rect());
        let owner = ui.resolve_drag_owner(tracks_origin, None);

        assert!(!ui.dropdown.is_open(), "foreign drag dismisses the dropdown");
        assert_eq!(
            owner,
            Some(DragOwner::TimelineTracks),
            "ownership passes to the real owner instead of being eaten"
        );
    }

    /// §3.2 order: Ruler wins over TimelineTracks when the origin is in the
    /// ruler rect; TimelineTracks wins when it's only in the tracks rect;
    /// neither wins when the origin is in open space (e.g. above the ruler).
    #[test]
    fn owner_resolution_order_ruler_before_tracks_before_none() {
        let mut ui = new_root();

        let ruler_origin = center(ui.viewport.ruler_rect());
        assert_eq!(ui.resolve_drag_owner(ruler_origin, None), Some(DragOwner::Ruler));

        let tracks_origin = center(ui.viewport.tracks_rect());
        assert_eq!(
            ui.resolve_drag_owner(tracks_origin, None),
            Some(DragOwner::TimelineTracks)
        );

        let dead_space = Vec2::new(-100.0, -100.0);
        assert_eq!(ui.resolve_drag_owner(dead_space, None), None);
    }

    /// §3.3 failure story (a): a `DragEnd` released outside `tracks_rect`
    /// (e.g. the cursor drifted over the inspector) must still stash for
    /// `InteractionOverlay::on_end_drag` — ownership decides, not position.
    /// The old `is_event_in_tracks_area` positional gate would have dropped
    /// this exact case (BUG-058's leak-adjacent failure mode).
    #[test]
    fn drag_end_stashes_by_ownership_regardless_of_release_position() {
        let mut ui = new_root();
        let far_outside = Vec2::new(ui.viewport.tracks_rect().x_max() + 500.0, -200.0);
        let drag_end = UIEvent::DragEnd { node_id: None, pos: far_outside };

        ui.drag_owner = Some(DragOwner::TimelineTracks);
        assert!(
            ui.should_stash_for_tracks(&drag_end),
            "TimelineTracks owns the gesture, so the release position is irrelevant"
        );

        ui.drag_owner = Some(DragOwner::Inspector);
        assert!(
            !ui.should_stash_for_tracks(&drag_end),
            "a DragEnd owned by someone else must not stash for the timeline"
        );

        ui.drag_owner = None;
        assert!(!ui.should_stash_for_tracks(&drag_end));
    }

    /// §3.3: the terminal broadcast always clears the owner, so the next
    /// gesture starts from a clean slate — this is what makes a drag
    /// released over the inspector followed immediately by a new drag on a
    /// second clip behave (the no-wedge proof `drag-clip-release-over-
    /// inspector.json` exercises end-to-end).
    #[test]
    fn broadcast_gesture_end_clears_owner() {
        let mut ui = new_root();
        ui.drag_owner = Some(DragOwner::TimelineTracks);
        ui.broadcast_gesture_end();
        assert_eq!(ui.drag_owner, None);
    }

    /// BUG-075 regression: a timeline drag driven through the REAL
    /// `process_events` path must stash its terminal `DragEnd` for
    /// `InteractionOverlay::on_end_drag` — which is the only thing that
    /// finalizes trim / marquee / move (commits undo, resets `drag_mode`).
    ///
    /// This drives the same seam the shipped bug lived in: the terminal
    /// broadcast used to null `drag_owner` BEFORE `should_stash_for_tracks`
    /// read it, so the `DragEnd` never reached `viewport_events` and the
    /// gesture never finalized. The pre-existing ownership tests set
    /// `drag_owner` by hand and called `should_stash_for_tracks` directly, so
    /// they never exercised the broadcast-before-stash ordering — which is
    /// exactly why the bug shipped. This test refuses to do that: it goes
    /// Down → Move-past-threshold → Up through `pointer_event`/`process_events`
    /// and asserts the drained events, so the ordering is under test.
    #[test]
    fn timeline_drag_end_reaches_viewport_events_through_process_events() {
        let mut ui = new_root();
        let origin = center(ui.viewport.tracks_rect());

        // Press inside the tracks area.
        ui.pointer_event(origin, PointerAction::Down, 0.0);
        let _ = ui.process_events();
        let _ = ui.drain_viewport_events(); // clear the PointerDown stash

        // Move well past DRAG_THRESHOLD_PX (4px) → DragBegin + Drag. The
        // DragBegin is where `resolve_drag_owner` fixes the owner.
        let moved = Vec2::new(origin.x + 40.0, origin.y + 6.0);
        ui.pointer_event(moved, PointerAction::Move, 0.02);
        let _ = ui.process_events();
        assert_eq!(
            ui.drag_owner,
            Some(DragOwner::TimelineTracks),
            "the tracks-area DragBegin must resolve ownership to TimelineTracks \
             (if this fails the input never emitted DragBegin, not the bug)"
        );
        let mid = ui.drain_viewport_events();
        assert!(
            mid.iter().any(|e| matches!(e, UIEvent::DragBegin { .. })),
            "the DragBegin must stash while the gesture is owned: {mid:?}"
        );

        // Release. The terminal DragEnd must stash for on_end_drag, and the
        // owner must be cleared afterward (self-heal invariant preserved).
        ui.pointer_event(moved, PointerAction::Up, 0.04);
        let _ = ui.process_events();
        let end = ui.drain_viewport_events();
        assert!(
            end.iter().any(|e| matches!(e, UIEvent::DragEnd { .. })),
            "BUG-075: the terminal DragEnd must reach viewport_events so \
             on_end_drag finalizes the gesture — pre-fix this vec has no \
             DragEnd because the broadcast nulled the owner first: {end:?}"
        );
        assert_eq!(
            ui.drag_owner, None,
            "the owner must be cleared once the terminal event is fully routed"
        );
    }

    /// D1/§3.5: full-stack proof that a PointerDown landing on the docked Audio
    /// Setup panel's Low/Mid crossover divider requests immediate drag for that
    /// press, so a 1px Move begins the drag immediately (not after the usual
    /// `DRAG_THRESHOLD_PX = 4.0`) and the following Drag reaches the panel as an
    /// `AudioCrossoverChanged` action. Drives the real entry points
    /// (`pointer_event` → `process_events`) so it exercises the exact
    /// docked-panel routing `process_events` does — the seam this phase built.
    #[test]
    fn divider_grab_requests_immediate_drag_and_one_pixel_move_yields_crossover_changed() {
        let mut ui = new_root();
        ui.audio_setup_panel.open();
        // Open the dock column so `ui.build()` builds it into `audio_setup()`.
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.audio_setup_panel.configure(
            None,
            vec![manifold_ui::panels::audio_setup_panel::AudioSendRow {
                id: manifold_core::AudioSendId::new("s1"),
                label: "Audio 1".into(),
                channels: vec![0],
                channel_label: "Channel 1".into(),
                gain_db: 0.0,
                floor_db: manifold_ui::types::FLOOR_DB_OFF,
                driven_count: 0,
                routings: vec!["Capture: Channel 1".into()],
                has_clip_triggers: false,
                feeding_layers: Vec::new(),
                consumers: Vec::new(),
            }],
            None,
        );
        let (low_hz, mid_hz, fmin, fmax) = (200.0_f32, 2000.0_f32, 20.0_f32, 20_000.0_f32);
        ui.audio_setup_panel.set_scope_bands(low_hz, mid_hz, fmin, fmax);
        ui.build();

        let scope =
            ui.audio_setup_panel.scope_rect().expect("scope present once open, sent, and built");
        // Same log-scale mapping `AudioSetupPanel::scope_line_y` documents on
        // `set_scope_bands` — reproduced here from the public scope_rect() +
        // the bands just set, rather than reaching into the panel's private
        // hit-test math, so the test only depends on the panel's public
        // contract.
        let yn = (low_hz / fmin).log2() / (fmax / fmin).log2();
        let divider_y = scope.y + scope.height * (1.0 - yn);
        let origin = Vec2::new(scope.x + scope.width * 0.5, divider_y);

        ui.pointer_event(origin, PointerAction::Down, 0.0);
        let down_actions = ui.process_events();
        assert!(
            down_actions
                .iter()
                .any(|a| matches!(a, PanelAction::AudioCrossoverDragBegin)),
            "PointerDown on the divider should arm the band grab: {down_actions:?}"
        );
        assert!(ui.audio_setup_panel.is_dragging_band(), "divider grab should be armed");

        // First Move: only 1px past the origin. With the global threshold
        // (4px) this would NOT begin a drag — proving this requires the
        // wiring having actually called `request_immediate_drag` off the
        // PointerDown above.
        let move1 = Vec2::new(origin.x, origin.y + 1.0);
        ui.pointer_event(move1, PointerAction::Move, 0.01);
        let move1_actions = ui.process_events();
        assert!(
            ui.input.is_dragging(),
            "a 1px move on an immediate-drag press must begin the drag \
             immediately, not wait for DRAG_THRESHOLD_PX; actions: {move1_actions:?}"
        );

        // Second Move: now a Drag (not DragBegin) event fires and reaches the
        // panel as the crossover-changed action.
        let move2 = Vec2::new(origin.x, origin.y + 2.0);
        ui.pointer_event(move2, PointerAction::Move, 0.02);
        let move2_actions = ui.process_events();
        assert!(
            move2_actions
                .iter()
                .any(|a| matches!(a, PanelAction::AudioCrossoverChanged(BandDivider::Low, _))),
            "the Drag following the immediate DragBegin should yield an \
             AudioCrossoverChanged(Low, _) action: {move2_actions:?}"
        );
    }

    // P3 regression guard (buttons still need the ordinary 4px threshold) is
    // proved directly in `manifold-ui`'s `input.rs` test module — that's
    // where `DRAG_THRESHOLD`/`immediate_drag_armed` actually live, and it
    // already has a bare-button test fixture (`setup()`); see
    // `three_pixel_wiggle_without_immediate_drag_still_resolves_to_click` and
    // `request_immediate_drag_allows_one_pixel_move_to_begin_drag` there.

    /// One app frame's post-process rebuild: `overlay_dirty` (set whenever an
    /// overlay consumes an event) drives a VISUAL `rebuild_scroll_panels` — the
    /// exact path `app_render.rs` takes, which re-runs `build_overlays` and
    /// re-mints the Audio Setup chrome.
    fn settle(ui: &mut UIRoot) {
        if ui.overlay_dirty {
            ui.overlay_dirty = false;
            ui.rebuild_scroll_panels(ScrollDirty {
                visual: true,
                ..ScrollDirty::default()
            });
        }
    }

    fn open_audio_panel_with_send(ui: &mut UIRoot) {
        ui.audio_setup_panel.open();
        // Open the dock column too (the real toggle sets both — D1); without a
        // width the dock rect is ZERO and the body clips to nothing.
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.audio_setup_panel.configure(
            None,
            vec![manifold_ui::panels::audio_setup_panel::AudioSendRow {
                id: manifold_core::AudioSendId::new("s1"),
                label: "Audio 1".into(),
                channels: vec![0],
                channel_label: "Channel 1".into(),
                gain_db: 0.0,
                floor_db: manifold_ui::types::FLOOR_DB_OFF,
                driven_count: 0,
                routings: vec!["Capture: Channel 1".into()],
                has_clip_triggers: false,
                feeding_layers: Vec::new(),
                consumers: Vec::new(),
            }],
            None,
        );
        ui.audio_setup_panel.set_scope_bands(200.0, 2000.0, 20.0, 20_000.0);
        ui.build();
    }

    /// THE double-click regression (the Audio Setup buttons-need-two-clicks bug).
    /// Drives TWO real clicks on the Floor `−` stepper through the exact app
    /// frame loop — Down → process_events → overlay-dirty rebuild → Up →
    /// process_events — and asserts each fires exactly one `AudioSendFloorStep`.
    ///
    /// Root cause it guards: the Audio Setup panel is the only overlay that
    /// consumes `PointerDown` (BUG-059 leak stopgap), so a press on one of its
    /// buttons marks the overlay dirty and rebuilds the tree BETWEEN press and
    /// release. `rebuild_scroll_panels` → `UITree::truncate_from` used to
    /// recompute `root_count` from the current root-parented survivors, which
    /// undercounts once a root has been reparented (the inspector wraps its
    /// subpanels under a ClipRegion) — so the rebuilt overlay chrome root got a
    /// DIFFERENT salt than at press, its `WidgetId` (and every child's) churned,
    /// and the release resolved to a different widget than the press → no
    /// `Click`. Fixed by salting truncate's root count from `root_minted`
    /// (mint-time parentage). Pre-fix this test sees `steps1 == 0`.
    #[test]
    fn floor_stepper_fires_on_a_single_click_across_overlay_rebuild() {
        let mut ui = new_root();
        open_audio_panel_with_send(&mut ui);

        let floor0 = ui
            .audio_setup_panel
            .floor_minus_id()
            .expect("floor stepper builds when a send is selected");
        let fb = ui.tree.get_bounds(floor0);
        assert_ne!(fb, Rect::ZERO, "floor button must be live in the built tree");
        let p = Vec2::new(fb.x + fb.width * 0.5, fb.y + fb.height * 0.5);
        let w_at_build = ui.tree.widget_of(floor0);

        // ── Click 1 ─────────────────────────────────────────────
        ui.pointer_event(p, PointerAction::Down, 0.0);
        let _ = ui.process_events();
        settle(&mut ui); // the consumed PointerDown rebuilt the overlay
        let w_after_rebuild = ui.audio_setup_panel.floor_minus_id().map(|n| ui.tree.widget_of(n));
        ui.pointer_event(p, PointerAction::Up, 0.05);
        let click1 = ui.process_events();
        settle(&mut ui);

        // ── Click 2 (same pixel) ────────────────────────────────
        ui.pointer_event(p, PointerAction::Down, 0.20);
        let _ = ui.process_events();
        settle(&mut ui);
        ui.pointer_event(p, PointerAction::Up, 0.25);
        let click2 = ui.process_events();

        let steps = |acts: &[PanelAction]| {
            acts.iter()
                .filter(|a| matches!(a, PanelAction::AudioSendFloorStep(..)))
                .count()
        };

        // The button's identity must survive the mid-click rebuild — the whole
        // failure was this WidgetId churning between press and release.
        assert_eq!(
            w_after_rebuild,
            Some(w_at_build),
            "the Floor button's WidgetId must survive the overlay rebuild that a \
             consumed PointerDown triggers (build={:?} after-rebuild={w_after_rebuild:?})",
            Some(w_at_build),
        );
        assert_eq!(
            steps(&click1),
            1,
            "the FIRST click on the Floor stepper must fire one step — a second \
             click should not be needed. click1={click1:?}"
        );
        assert_eq!(steps(&click2), 1, "the second click must also fire one step: click2={click2:?}");
    }
}
