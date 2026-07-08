# DRAG_CAPTURE P3 — read-back + seam resolution

Base verified: `1fdf15e0` (P2 landing report + status + VD-017/018).

## Read-back — D6 + §3.4

**D6 restated.** Precision surfaces — controls whose entire job is sub-4px
adjustment — can opt out of the global `DRAG_THRESHOLD_PX = 4.0` click-vs-drag
disambiguation, per press, not globally. A `PointerDown` consumer calls
`UIInputSystem::request_immediate_drag()` while routing that press; this arms
a zero-threshold override for the CURRENT press only, so the very next `Move`
(any nonzero distance) emits `DragBegin` instead of waiting for 4px of
travel. The audio panel's band-crossover dividers use it: arm on press when
the press hit a divider line, first pixel of movement drags.

**Click-forfeiture consequence, stated plainly.** Once a press is armed for
immediate drag, that press can never resolve to a `Click` — literally any
movement before `Up` becomes a drag, so there is no press → tiny-jiggle →
release path left that produces a `Click` event for that press. This is
*correct* for the band dividers because they have no click behavior at all —
nothing is lost. It would be the *wrong* trade for a control that has both a
click action and a drag action (e.g. a button that also supports drag-reorder)
— arming it would silently delete the click path for that widget. That's why
the hook is opt-in per-press, decided by the consumer's own `PointerDown`
handling of *this* event, not a static property of the widget.

**Forbidden moves, restated.**
1. Lowering `DRAG_THRESHOLD_PX` globally — breaks click-vs-drag disambiguation
   everywhere (buttons, clip selection, layer headers) for the sake of one
   surface's need; a hand-tremor on a button would start a drag.
2. Per-widget style flags instead of the per-press hook — a static flag on a
   widget type would apply unconditionally regardless of whether that
   widget's `PointerDown` handler actually armed a drag-worthy grab this
   press, and has no natural per-gesture clear; the per-press call
   (`request_immediate_drag`, cleared on `Up`) is what keeps the override
   scoped to exactly one gesture, mirroring D1's "resolved once, at the
   gesture's first event" discipline instead of adding a sixth ambient flag.

## What I found at the entry anchors

- `crates/manifold-ui/src/input.rs`: `UIInputSystem` struct (~L375);
  `DRAG_THRESHOLD` const = `crate::color::DRAG_THRESHOLD_PX` (L401);
  `process_pointer`'s `Move` arm gates `DragBegin` on
  `dist >= DRAG_THRESHOLD` (L543); the `Up` arm (L582) clears
  `pressed_widget`/`is_dragging` — the natural place to also clear a new
  per-press immediate-drag flag.
- `crates/manifold-app/src/ui_root.rs`: `process_events` (L1516);
  `route_overlay_event` (L1018, returns `bool`, not overlay identity — the
  seam); its `PointerDown`-relevant consumption site in the main loop
  (L1577–1580: `if self.route_overlay_event(event, &mut actions) { ...
  continue; }`); `DragOwner` (L57), `resolve_drag_owner` (L1096),
  `broadcast_gesture_end` (L1165) already landed by P1/P2 — `resolve_drag_owner`
  already has a Z_ORDER-walk pattern over `overlay_mut` I can reuse for the
  new poll.
- `crates/manifold-ui/src/panels/overlay.rs`: `claims_drag` (L156) and
  `gesture_ended` (L167) trait hooks already landed (P1/P2), both with
  design-doc-citing comments. `wants_immediate_drag` does not exist yet —
  new hook for P3, same shape (default no-op / `false`).
- `crates/manifold-ui/src/panels/audio_setup_panel.rs`: `dragging_band`
  field (L429); armed at `PointerDown`'s `divider_at` hit (L2289); cleared
  on `DragEnd`/`PointerUp` (L2367) and by `gesture_ended` (L2403);
  `claims_drag` override (L2394) already reads
  `dragging_band.is_some() || calibration_drag.is_some() || point_in_panel(origin)`.

## Seam resolution — `route_overlay_event` / `wants_immediate_drag`

`route_overlay_event` returns `bool` only (consumed or not), not which
overlay consumed — so there's no direct handle to the "the overlay that just
armed a divider grab" for the wiring in §3.4 to query.

**Resolved via the doc's Preferred approach — no escalation needed.** After a
`PointerDown`'s `route_overlay_event` call returns `true`, poll every OPEN
overlay's `wants_immediate_drag()` (a new `any_overlay_wants_immediate_drag`
helper on `UIRoot`, same `OverlayId::Z_ORDER` walk `broadcast_gesture_end`
already uses) and call `self.input.request_immediate_drag()` if any returns
`true`.

This is sound, not just convenient: `dragging_band` (the only thing
`wants_immediate_drag` reads) can only have just been set by whichever
overlay's `on_event` actually ran and consumed this exact `PointerDown` —
every other open overlay never saw this event, so its own per-press state is
untouched. And `dragging_band` cannot be stale from a previous gesture: it's
cleared unconditionally by `gesture_ended` at every terminal broadcast and by
the panel's own `DragEnd`/`PointerUp` arm. So polling the whole open set and
OR-ing the answer together identifies the same overlay `route_overlay_event`
already picked, without changing its public return shape and without adding
a second identity-tracking field. No change to `route_overlay_event`'s
signature was needed.
