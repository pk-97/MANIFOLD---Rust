# Drag Capture — one owner per pointer gesture, from press to release

**Status:** IN PROGRESS — **P1 LANDED 2026-07-08 @ `9bb8ca86`** (ownership D1–D4 + D9, L3); **P2 LANDED 2026-07-08 @ `12683746`** (z-aware seams D5 + `swallow_drag` retired, L1; VD-017/018); P3 pending · design 2026-07-07 (approved same day by Peter) · Fable
**Prerequisites:** none (BUG-058 instrumentation + BUG-059 stopgap landed 2026-07-07 @ `fb2bdc07`; P2 deletes the stopgap)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight: MANIFOLD has no single notion of who owns an in-flight drag.
Four uncoordinated layers each re-decide per event — raw-position interceptors in
`window_input.rs`, a first-match-wins consumable overlay gauntlet, the input system's
implicit capture, and per-panel private "am I dragging" flags — and any disagreement
between them either **wedges** a gesture (BUG-058: timeline stuck in move/trim because
an overlay consumed the terminal `DragEnd`) or **leaks** it (BUG-059: a missed
band-line grab fell through a modeless panel and silently moved clips underneath).
On stage both are the same failure: the instrument's touch surface stops being
trustworthy mid-set. The fix is single drag-capture ownership: **at drag start, one
owner is resolved once and recorded; every subsequent `Drag`/`DragEnd` routes to that
owner by identity, never by position; the end of a gesture is a broadcast no one can
eat.** This is the pattern every windowing system converged on (Win32
`SetCapture`, macOS mouse-tracking, browsers' `setPointerCapture`) — the codebase's
`UIInputSystem` already does it correctly at the widget level
([input.rs:490](../crates/manifold-ui/src/input.rs) — `pressed_widget` pins the
target, terminals fire unconditionally per `859bbceb`); this design extends the same
discipline one level up, to the surface arbitration in `UIRoot` and `window_input`.

Companions: `docs/BUG_BACKLOG.md` BUG-058/BUG-059 (the incident record and root-cause
analysis this design closes); `docs/UI_AUTOMATION_DESIGN.md` (the flow driver the
gates use); `docs/OVERLAY_SYSTEM_DESIGN.md` (the overlay driver being extended).

---

## 1. Audit — what exists (verified 2026-07-07, tip `fb2bdc07`)

| Piece | Where | State |
|---|---|---|
| Widget-level capture (correct for terminals, NOT for motion) | `crates/manifold-ui/src/input.rs:490` `process_pointer` | `pressed_widget` pinned at Down; `DragEnd`/`PointerUp` emitted unconditionally on Up (fix `859bbceb`) — but `DragBegin` and every `Drag` are emitted ONLY while the pressed widget resolves to a live node; a rebuild that drops the node silently swallows the whole motion stream (D9, live-trace confirmed 2026-07-07). P1 touches this. |
| Consumable routing gauntlet | `crates/manifold-app/src/ui_root.rs:1372` `process_events` | Per event: overlay gauntlet → intent → panel handlers → positional stash. First consumer wins; a consumed `DragEnd` never reaches later stages. **The wedge mechanism.** |
| Overlay gauntlet | `ui_root.rs:982` `route_overlay_event` | Z-order walk ([ui_root.rs:42](../crates/manifold-app/src/ui_root.rs) `Z_ORDER`); `Consumed` stops routing; a `Modal` captures even events it `Ignored`s. |
| Dropdown eats all drags | `crates/manifold-ui/src/panels/dropdown.rs` (`on_event`, RightClick/Drag* arm) | An open dropdown consumes `DragBegin`/`Drag`/`DragEnd` unconditionally and dismisses. Confirmed `DragEnd`-eater (BUG-058). |
| Tracks-area stash gate + latch | `ui_root.rs:2486` `is_event_in_tracks_area` + `ui_root.rs:336` `overlay_drag_active` | Raw-position classification with zero z-awareness, patched by a boolean latch approximating capture. **The leak mechanism** (BUG-059) and the thing ownership replaces outright. |
| Timeline drag state | `crates/manifold-ui/src/interaction_overlay.rs:1460/1607` `on_begin_drag`/`on_end_drag` | `DragMode` — EXCLUSIVE owner of clip move/trim/region/automation gestures; cleared only by `on_end_drag`, which only runs if the `DragEnd` survived the gauntlet and the gate. |
| Broadcast precedent (partial) | `ui_root.rs` `process_events`, second loop (search `"PointerUp handling: Unity's OnPointerUp ALWAYS fires"`) | Inspector + layer_headers already receive `DragEnd\|PointerUp` UNCONDITIONALLY in a dedicated loop, because routed delivery burned them before. The house already learned broadcast-terminals — twice — but per-consumer. This design generalizes it. |
| Window-seam interceptors | `crates/manifold-app/src/window_input.rs:236` `primary_mouse_input` | Dropdown-dismiss, split-handle (6px full-width band), inspector-edge (±4px full window height) intercept presses on raw position BEFORE any hit-testing — straight through overlays floating above them. Both handles already have tree nodes (built in `UIRoot::build`), but input ignores them. |
| Per-panel drag flags | inspector `pressed_target`/card-drag; layer_headers `is_dragging`/gain; audio panel `dragging_band`/`calibration_drag`/`swallow_drag` ([audio_setup_panel.rs:439](../crates/manifold-ui/src/panels/audio_setup_panel.rs)) | Each panel privately re-answers "is this my drag". `swallow_drag` is the 2026-07-07 stopgap this design supersedes (P2 deletes it). |
| Diagnostic tap | `MANIFOLD_INPUT_TRACE=1`, six seams (landed `556578c3`) | Prints per-transition routing lines. Keep — it verifies this design's phases too. |
| Graph-editor canvas | `crates/manifold-ui/src/graph_canvas/interaction.rs` | Does its own internal capture inside one panel; sound, out of scope (scope fence §6). |

Instruction: **extend, don't redesign.** The input layer is correct; the overlay
driver keeps its shape; `InteractionOverlay` keeps `DragMode`. Only the arbitration
between surfaces changes.

## 2. Decisions

- **D1 — Ownership is resolved once, at `DragBegin`, and recorded on `UIRoot`.**
  The first `DragBegin` after a press runs one resolution pass (§3.2); the winner is
  stored in `UIRoot::drag_owner`. Every subsequent `Drag`/`DragEnd` of that gesture
  routes to the owner by identity. Rationale: today five mechanisms re-answer
  ownership per event and drift; one recorded answer cannot drift.
  Rejected: *resolving at `PointerDown`* — presses that never become drags (clicks)
  would churn owner state for nothing, and every existing consumer already arms its
  internal state at Down; the drag is the thing needing an owner, so the drag's
  first event is the resolution point.
- **D2 — Terminal events are broadcasts, not routed messages.** When the input
  system reports the gesture over (`DragEnd`/`PointerUp`), the owner gets the full
  event, and every other drag-state holder gets an idempotent end-of-gesture clear.
  No consumer can eat another's terminal. Rationale: this kills BUG-058's whole
  eater class in one move; the second unconditional loop in `process_events` proves
  the pattern already works here — it just never covered the timeline.
  Rejected: *broadcast-only without ownership* (no D1) — cheaper, but leaves two
  consumers actionable on the same Drag stream (the BUG-059 leak class) and keeps
  the latch + positional gate alive.
- **D3 — The dropdown never claims a drag it didn't originate.** Its current
  eat-everything drag arm is deleted. A `DragBegin` originating outside an open
  dropdown dismisses it (same UX as today) but does NOT consume — ownership passes
  to the real owner. A drag originating inside it (scroll-thumb, future) it may
  claim. Rationale: the eat-arm is the one *confirmed* BUG-058 eater; "dismiss"
  and "consume" were conflated.
- **D4 — Modals claim all drags unconditionally.** A modal owner that ignores the
  events makes everything beneath it inert, which is what modal means. This is
  today's behavior, now expressed as ownership instead of capture-by-side-effect.
- **D5 — Window-seam interceptors yield to overlays.** The split-handle and
  inspector-edge press checks run only if no open overlay contains the press point
  (new `UIRoot::overlay_contains_point`, walking `Z_ORDER` top-down). Rationale:
  the seams are visually UNDER a floating panel; input must agree with the eye.
  Rejected: *fully converting the handles to widget-intent routing* — they have
  nodes already, but their drag loops live in `window_input` state
  (`split_dragging`) and work; converting is a bigger refactor with no additional
  safety beyond the z-check. Deferred (§7) with trigger.
- **D6 — Precision surfaces can opt out of the 4px drag threshold.** A `PointerDown`
  consumer may arm immediate drag (`UIInputSystem::request_immediate_drag`), making
  the next Move emit `DragBegin` at distance 0. The audio panel's band dividers use
  it: arm on press, first pixel drags. Rationale: `DRAG_THRESHOLD_PX = 4.0` is right
  for click-vs-drag disambiguation on buttons and clips, wrong for a control whose
  entire job is sub-4px adjustment (BUG-059 "sticky" feel). Rejected: *lowering the
  global threshold* — breaks click detection everywhere for one surface's need.
- **D7 — `Ruler`/scrub is an owner like any other.** ⚠ VERIFY-AT-IMPL: whether ruler
  scrubbing consumes Drag events via `viewport.handle_event` —
  `rg -n "Drag" crates/manifold-ui/src/panels/viewport.rs`. If it does, it's a
  `DragOwner` variant; if scrubbing is Move-based, it isn't and the variant is
  dropped. Default: include the variant; drop it if the check says otherwise.
- **D9 — the drag event stream is emitted unconditionally; node identity is
  best-effort context (added 2026-07-07, same day, from Peter's live trace).**
  `DragBegin` and `Drag` today are emitted only while `tree.node_for_widget
  (pressed)` resolves; the terminal events already lost that gate in `859bbceb`.
  Peter's `MANIFOLD_INPUT_TRACE=1` repro of the "first band-line click is always
  dead" bug caught the consequence live (observed): the press armed the band drag
  and was consumed by the panel, then `DRAG-BEGIN … resolves=false` — the pressed
  node was gone from the build by threshold-crossing — and the ENTIRE motion
  stream was silently swallowed; the armed, position-based band drag never
  received a single move. The dead click and the working click carried different
  WidgetIds. Probable path for the node death (inferred, not observed): the
  panel's own consume sets `overlay_dirty` → overlay rebuild between Down and
  threshold; the design does not depend on which rebuild killed the node — ANY
  node death mid-gesture must not kill the stream. Under this design the gate is
  removed: `DragBegin`/
  `Drag` carry `node_id: Option<NodeId>` (the same best-effort contract `DragEnd`
  has), position is the payload, and ownership routing (D1) — not node identity —
  decides delivery. Without D9, D1 is inert in exactly this case: ownership is
  resolved at `DragBegin`, and a swallowed `DragBegin` means no owner and a dead
  gesture. Consequences, stated honestly: consumers that used `Drag.node_id` as a
  guaranteed live node must handle `None`; the compiler finds them all when the
  field type changes (seam brief in P1).
- **D8 — BUG-058's live-repro confirmation is not a prerequisite.** The
  instrumentation stays shipped and useful, but this design removes the eater class
  wholesale, so the design does not block on naming which eater fired in Peter's
  exact stuck-trim repro. (D8's original claim that a trace finding would "never
  change this design" was falsified within hours — Peter's first-click-dead trace
  produced D9. Corrected rule: a trace finding inside the drag pipeline amends
  this design; only a finding outside it — e.g. winit-macOS losing the Release at
  the OS seam, BUG-028 precedent — becomes a separate bug entry.)

## 3. Design body

### 3.1 The owner type (committed)

```rust
// crates/manifold-app/src/ui_root.rs — UI thread only, no serialization.
/// Who owns the in-flight pointer drag. Resolved once per gesture (D1) by
/// `UIRoot::resolve_drag_owner`, cleared by the terminal broadcast (D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragOwner {
    /// An open overlay claimed it (modal, or origin inside its rect — §3.2).
    Overlay(OverlayId),
    /// The timeline tracks surface → stashed to `InteractionOverlay`.
    TimelineTracks,
    /// Inspector slider / effect-card drag (`pressed_target` / card-drag).
    Inspector,
    /// Layer-header reorder or gain drag.
    LayerHeaders,
    /// Timeline ruler scrub (D7 — ⚠ VERIFY-AT-IMPL, may be dropped).
    Ruler,
}
```

`UIRoot` gains `drag_owner: Option<DragOwner>` (replacing
`overlay_drag_active: bool` at `ui_root.rs:336` — same slot in spirit, identity
instead of boolean). `window_input`'s `split_dragging` / `inspector_resize_dragging`
stay where they are: they intercept at press, before the UI event stream exists, and
already have sound release handling; D5 only makes them z-aware.

### 3.2 Ownership resolution (committed order)

On the first `DragBegin` of a gesture, `UIRoot::resolve_drag_owner(origin, node_id)`
asks, in this fixed order, first claim wins:

1. **Open overlays, z-top-down** (`Z_ORDER.iter().rev()`, same walk as
   `route_overlay_event`): a modal claims unconditionally (D4); a modeless overlay
   claims iff a new trait hook says so —
   ```rust
   // manifold-ui overlay trait (same trait `on_event`/`modality` live on):
   /// Does a drag ORIGINATING at `origin` belong to this overlay? Default:
   /// no. Modeless overlays with drag surfaces override (audio panel:
   /// armed band/calibration drag OR origin inside panel_rect — replaces
   /// the BUG-059 `swallow_drag` stopgap).
   fn claims_drag(&self, origin: Vec2) -> bool { false }
   ```
   A dropdown open at this moment that does NOT claim (origin outside it) is
   dismissed here as a side effect, without consuming (D3).
2. **Inspector**: `inspector.has_pressed_target() || inspector.is_card_drag_active()`
   (both armed during `PointerDown`/`DragBegin` routing, as today).
3. **Layer headers**: `layer_headers.is_dragging() || is_gain_dragging()`.
4. **Ruler** (D7, if kept): press originated in `viewport.ruler_rect()`.
5. **TimelineTracks**: origin inside `viewport.tracks_rect()` — the fallback that
   today's stash gate approximated.
6. Nobody: `drag_owner = None`; the drag routes nowhere (same net effect as today's
   un-stashed drags on dead space).

Delivery afterward is mechanical: `Drag` events go only to the owner (`Overlay(_)` →
`ov.on_event`; `TimelineTracks` → stash for `InteractionOverlay`, unconditionally, no
position check; `Inspector`/`LayerHeaders` → the existing calls in the second loop,
now gated on ownership instead of their private flags). `is_event_in_tracks_area`
keeps ONLY its non-drag arms (Click/DoubleClick/RightClick/HoverEnter/PointerDown
classification for the stash) — the Drag/DragEnd/latch arms are deleted.

### 3.3 Terminal broadcast (committed)

On `DragEnd`/`PointerUp` in `process_events`:

1. Owner gets the event first (full data, may emit actions — crossover commit, clip
   drag end via stash, card reorder).
2. Then `UIRoot::broadcast_gesture_end()` runs **unconditionally** — after the owner,
   regardless of what any overlay returned, even with no owner recorded: every
   overlay's `fn gesture_ended(&mut self)` (new trait hook, default no-op, panels
   with drag state override with an idempotent clear), plus the existing inspector /
   layer_headers end-calls (which are already idempotent — that's why the current
   second loop works). `drag_owner = None`.
3. `TimelineTracks` owner ⇒ the `DragEnd` is stashed so `InteractionOverlay::
   on_end_drag` runs exactly as today — but now nothing upstream can have eaten it.

The broadcast is the invariant the whole design buys: **a gesture that began always
ends, for every consumer, exactly once.**

Failure story — the owner record's termination is triple-covered, so a stale owner
cannot outlive one gesture: (a) the terminal broadcast clears it (normal path);
(b) the next `PointerDown` clears any leftover owner AND fires the same
`broadcast_gesture_end` first (self-heal for a release the window never received —
focus loss, the BUG-028-class winit seam); (c) `resolve_drag_owner` overwrites
unconditionally at the next `DragBegin`. There is no state in which two gestures
share an owner record, and no path where a lost OS release wedges anything past the
user's next press.

### 3.4 Immediate-drag threshold (D6, committed seam)

```rust
// crates/manifold-ui/src/input.rs
impl UIInputSystem {
    /// Arm zero-threshold drag for the CURRENT press only (cleared on Up).
    /// Call while routing the PointerDown of a precision surface (D6).
    pub fn request_immediate_drag(&mut self) { /* threshold = 0 for this press */ }
}
```

Wiring: after `route_overlay_event` consumes a `PointerDown`, `UIRoot` asks the
consuming overlay `fn wants_immediate_drag(&self) -> bool` (trait hook, default
false; audio panel returns true iff it just armed `dragging_band` — a divider grab).
True → `self.input.request_immediate_drag()`. Consequences, stated honestly: an
immediate-drag press can never become a `Click` (any movement is a drag). For the
band dividers that is correct — they have no click behavior; any future surface
opting in must make the same trade knowingly, which is why the hook is per-press
opt-in and not a style flag.

### 3.5 The plausible-wrong architecture, forbidden by name

- **You will want to add the newly-found eater to the unconditional second loop**
  (the inspector/layer_headers pattern) and call it fixed. No — that pattern
  per-consumer is exactly how the codebase accreted four arbiters; the count of
  special-cased consumers goes DOWN in this design, not up.
- **You will want to keep `overlay_drag_active` as a fast-path alongside
  `drag_owner`.** No — the latch is the bug. It's deleted in P1, with an rg-zero
  gate. Parallel old paths are the house's most-observed escape.
- **You will want to give each overlay its own copy of the owner flag** ("am I the
  owner") instead of one `UIRoot` field. No — distributed ownership state is the
  disease being cured; there is exactly one `Option<DragOwner>`.
- **You will want `winit`-level capture or an OS API.** No — macOS already delivers
  the whole gesture to the window; the arbitration problem is entirely inside our
  routing. Nothing in this design touches `window_input`'s winit seam except D5's
  z-check.

## 4. What this means on stage

A drag can no longer wedge (stuck move/trim cursor mid-set) because no surface can
eat the release; a drag can no longer leak (clips silently moved under a calibration
panel, timeline region-selects while adjusting a slider) because exactly one surface
owns the gesture; a grab can no longer die because the UI repainted under your
finger (D9 — the "first band-line click is always dead" bug); and the band dividers
become a real precision control (first pixel responds). The performer-visible contract: **whatever you grabbed is what you're
dragging, until you let go — no matter where your hand travels.**

## 5. Phasing

### P1 — Ownership + terminal broadcast (the vertical slice) — ✅ LANDED 2026-07-08 @ `9bb8ca86` (L3; report `docs/landings/2026-07-08-drag-capture-p1.md`)

- **Entry state:** tip contains `556578c3` (instrumentation + stopgap):
  `git log --oneline -5` shows it; `rg -n "overlay_drag_active" crates/manifold-app/src/ui_root.rs`
  returns the 5 current sites (re-derive; if the count differs, stop and list).
- **Read-back:** this doc §2–§3 + `ui_root.rs` `process_events` end-to-end +
  `dropdown.rs` `on_event` + BUG-058 backlog entry. Restate D1–D4, D9, the
  forbidden moves (§3.5), and the entry-check counts before any code.
- **Deliverables:** `DragOwner` + `UIRoot::drag_owner` + `resolve_drag_owner` +
  `broadcast_gesture_end`; `claims_drag`/`gesture_ended` trait hooks (defaults);
  dropdown eat-arm deleted per D3; stash gate's Drag/DragEnd/latch arms deleted;
  second loop's inspector/layer_headers calls gated on ownership; the
  `PointerDown` stale-owner self-heal (§3.3 failure story); **D9: `DragBegin`/
  `Drag` emission made unconditional in `input.rs` `process_pointer` — their
  `node_id` becomes `Option<NodeId>`, same contract as `DragEnd`**; unit tests in
  `ui_root` (or the panel crates where state lives) covering: owner resolution
  order, dropdown-open-at-release no longer wedges `DragMode`, modal claims all,
  timeline drag released outside tracks rect still reaches `on_end_drag`, and
  **the D9 repro: press, remove the pressed node from the tree, cross the
  threshold and move — DragBegin + Drag events still arrive with `node_id:
  None` and correct positions** (this is Peter's first-click-dead trace as a
  unit test).
- **Seam brief:** two seams. (a) Stash gate:
  `is_event_in_tracks_area(Drag*|DragEnd) → latch → stash` becomes
  `drag_owner == Some(TimelineTracks) → stash`. Call-site inventory (2026-07-07):
  `overlay_drag_active` ×5 (`ui_root.rs:336,461,~1461,~1464,~2493`), dropdown drag
  arm ×1, stash gate ×1. Re-derivation:
  `rg -n "overlay_drag_active|is_event_in_tracks_area" crates/manifold-app/src`.
  Compiler-driven: delete the `overlay_drag_active` field first; the errors are the
  checklist. (b) D9 event shape: `UIEvent::DragBegin { node_id: NodeId, … }` and
  `UIEvent::Drag { node_id: NodeId, … }` become `node_id: Option<NodeId>` —
  change the field type FIRST; every consumer that assumed a live node is then a
  compile error and gets an explicit `None` decision (most consumers are
  position-based and ignore the node; a consumer that genuinely needs the node
  treats `None` as "keep last known", per the DragEnd precedent). Re-derivation:
  `rg -n "DragBegin \{|Drag \{" crates/ -g '*.rs'`. Misfit sites escalate, never
  adapt.
- **Gate (positive):** `cargo test -p manifold-ui --lib` and the new ui_root tests
  green; existing L3 flow `scripts/ui-flows/drag-clip.json` passes; NEW L3 flow
  `drag-clip-release-over-inspector.json` — drag a clip from tracks to inspector
  coordinates, assert the moved rect AND that a subsequent click-drag on a second
  clip moves that second clip (the no-wedge proof driven through the real input
  path).
- **Gate (negative):** `rg -n "overlay_drag_active" crates/` → **0 hits**;
  `rg -n "DragEnd" crates/manifold-ui/src/panels/dropdown.rs` → **0 hits** in a
  consuming arm (the dismiss-without-consume path is Move/Begin-side).
- **Acceptance demo:** the new flow's `result.json` + PNG, L3.
- **Performer gesture:** trim a clip, release over the inspector, immediately grab
  and move a second clip — both behave.
- **Forbidden moves:** §3.5 all four; plus keeping any latch "temporarily".
- **Test scope:** focused (`-p manifold-ui --lib`, ui_root tests); workspace sweep
  deferred to P3 (final phase of the pass).

### P2 — Z-aware window seams + stopgap retirement — ✅ LANDED 2026-07-08 @ `12683746` (L1; VD-017/018; report `docs/landings/2026-07-08-drag-capture-p2.md`)

- **Entry state:** P1 landed (`rg "drag_owner" crates/manifold-app/src/ui_root.rs`
  non-empty, `overlay_drag_active` zero).
- **Read-back:** D5, audit rows for window-seam interceptors + stopgap; restate.
- **Deliverables:** `UIRoot::overlay_contains_point(pos)` (Z_ORDER top-down,
  open overlays only); split-handle + inspector-edge press branches in
  `window_input.rs` guarded by `!overlay_contains_point`; audio panel implements
  `claims_drag` (armed || `point_in_panel(origin)`) and `gesture_ended`
  (clears `dragging_band`/`calibration_drag`); `swallow_drag` field + arms +
  its two tests DELETED (the tests' scenarios move to `claims_drag` tests —
  same assertions, new seam).
- **Gate (positive):** new unit tests: press on a band line whose y is inside the
  split-handle band while the panel overlaps it arms the band drag, not the split
  (buildable headlessly: build panel at a viewport where the rects overlap, drive
  `primary_mouse_input`-level routing via the ui_root test seam or panel-level
  claims tests). **Gate (negative):** `rg -n "swallow_drag" crates/` → **0 hits**.
- **Acceptance demo:** headless PNG of the audio panel over the timeline +
  the claims-test output; L2 (the seam interaction isn't flow-driver reachable —
  window-level press interception happens above `ui-snap`'s entry point).
- **Performer gesture:** grab a crossover line positioned over the timeline-split
  zone; the line moves, the panels don't resize.
- **Forbidden moves:** widening into full widget-intent conversion of the handles
  (Deferred, §7); keeping `swallow_drag` "for safety".
- **Test scope:** focused.

### P3 — Immediate-drag threshold for precision surfaces

- **Entry state:** P1+P2 landed.
- **Read-back:** D6 + §3.4; restate the click-forfeiture consequence.
- **Deliverables:** `request_immediate_drag` on `UIInputSystem` (threshold override
  for the current press, cleared on Up); `wants_immediate_drag` overlay hook;
  audio panel returns true on divider arm; unit test: Down on a divider + 1px Move
  emits `DragBegin`+`Drag` and yields `AudioCrossoverChanged`; regression test:
  buttons still need 4px (a 3px wiggle before Up still Clicks).
- **Gate:** the two named tests + `cargo test --workspace` (final-phase sweep) +
  `cargo clippy --workspace -- -D warnings`.
- **Acceptance demo:** none beyond tests — L1, stated explicitly (the 1px feel is
  L4 by nature; see below).
- **Performer gesture / L4:** Peter nudges a crossover by ~2px and it tracks —
  owed to him as the feel pass, listed in VERIFICATION_DEBT at landing.
- **Forbidden moves:** lowering `DRAG_THRESHOLD_PX` globally; per-widget style
  flags instead of the per-press hook.
- **Test scope:** full workspace sweep + clippy (this is the pass's final phase).

Phasing-completeness check (§5 of the standard): every §3 commitment maps — 3.1/3.2/3.3 → P1;
D5 + stopgap retirement → P2; 3.4/D6 → P3; D7's variant → P1 (with its
VERIFY-AT-IMPL); D9 unconditional emission → P1; handle widget-conversion →
Deferred. No affordance is phase-less.

## 6. Decided — do not reopen

1. One `Option<DragOwner>` on `UIRoot`; no distributed owner flags.
2. Resolution at first `DragBegin`, fixed order §3.2, first claim wins.
3. Terminal events broadcast after owner delivery; broadcast runs unconditionally.
   A stale owner is cleared by the next `PointerDown` (which also broadcasts) —
   no timeout, no poll; the user's next press is the recovery path.
4. Dropdown dismisses foreign drags without consuming; modals claim everything.
5. Seam interceptors yield to overlays via `overlay_contains_point`; they are NOT
   converted to widget routing in this design.
6. Precision threshold is per-press opt-in (`request_immediate_drag`), never a
   global or per-style change.
7. The graph-editor canvas's internal capture is out of scope; scope fence.
8. `swallow_drag` is a stopgap and dies in P2 — do not extend it.
9. `DragBegin`/`Drag` emit unconditionally with `node_id: Option<NodeId>` (D9);
   the motion stream never depends on the pressed node surviving a rebuild.

## 7. Deferred

- **Widget-intent routing for split/inspector handles** — revive if a third
  window-seam gesture appears (e.g. a draggable footer) or D5's z-check proves
  insufficient in practice (a press-through bug survives P2's gate).
- **Editor-window ownership** — verified 2026-07-07: what the editor needs, it
  already gets. D9 (unconditional motion stream) and D3 (dropdown eat-arm
  deletion) are shared-code fixes — the editor's `UIInputSystem` instance and
  `Dropdown` type are the same types, so both windows are covered by P1
  automatically. The editor inspector already has its own unconditional
  `DragEnd|PointerUp` loop (`route_inspector_events`, second loop), and the graph
  canvas never touches the event pipeline at all — `window_input.rs` feeds it
  presses/moves directly and it does its own internal capture
  (`editor_mouse_input`, `window_input.rs:706` onward), so the eater and
  fall-through classes structurally cannot reach it. What is NOT built for the
  editor: the `DragOwner` registry itself. The editor has its own trio of
  position-based pre-canvas interceptors — the node-picker modal bypass, the
  dock column-divider drag (`ed.dock.begin/end`), and the mini-timeline scrub —
  the same architectural pattern as the primary window's seam handles,
  currently exclusive-by-press and sound. Revive editor ownership if any of
  those three ever mis-claims a canvas gesture in practice, or if an editor
  overlay hosts a drag surface (trigger: a `claims_drag` override on an overlay
  the editor window opens).
- **Naming which eater fired in Peter's original repro** — the trace is shipped;
  if he reproduces before P1 lands, the log names it (D8). Not blocking.
- **BUG-059 feel item "hover-glow only when grabbable"** (scope-dark deadness) —
  cosmetic, revive with the next Audio Setup UX pass.
