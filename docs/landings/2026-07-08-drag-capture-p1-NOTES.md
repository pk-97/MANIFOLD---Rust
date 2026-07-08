# DRAG_CAPTURE_DESIGN P1 — execution notes (worktree `wave/drag-capture-p1`)

Not a landing report (P1 isn't landed to main by this session) — working notes per
the phase brief's read-back + escalation requirements. The orchestrator's landing
report is a separate file at merge time.

## Read-back (before any code)

**D1** — Ownership for a drag gesture is resolved exactly once, at the gesture's
first `DragBegin`, and recorded on `UIRoot` (now `drag_owner: Option<DragOwner>`).
Every later `Drag`/`DragEnd` of that same gesture routes to whatever was recorded —
never re-decided per event. This replaces five places that today re-answer "who
owns this" independently and can disagree.

**D2** — The terminal event (`DragEnd`/`PointerUp`) is a broadcast, not a routable/
consumable message: the owner gets the full event, then *every* other drag-state
holder gets an unconditional, idempotent "gesture over" signal. No single consumer
can eat another's terminal event and leave it stuck (BUG-058's whole mechanism).

**D3** — The dropdown never claims a drag it didn't originate. Its old
eat-everything-while-open arm (`RightClick|DragBegin|Drag|DragEnd => close+consume`)
is gone. A drag beginning outside an open dropdown still dismisses it (matches
today's UX) but as a side effect of ownership resolution, not by consuming the
event — ownership passes through to whoever actually owns the gesture.

**D4** — A modal overlay claims a drag unconditionally, regardless of where it
started. This is today's behavior, now expressed as an ownership decision instead
of an accidental side effect of "ignored but captured."

**D9** — `DragBegin`/`Drag` are emitted unconditionally now, exactly like
`DragEnd`/`PointerUp` already were (`859bbceb`). `node_id` on all four event kinds
is `Option<NodeId>` — best-effort context, never a delivery precondition. Before
this, a rebuild dropping the pressed widget between `Down` and the 4px threshold
crossing silently killed the *entire* motion stream (Peter's live "first band-line
click is always dead" trace). Ownership routing, not node identity, now decides
delivery.

## Forbidden moves (§3.5) — respected

1. Did not add the newly-relevant overlay to the existing unconditional
   inspector/layer-headers second-loop pattern as a "fix" — the whole point of this
   design is that pattern doesn't scale; ownership generalizes it once.
2. Did not keep `overlay_drag_active` "as a fast path" — deleted outright, rg-zero
   gated (see below).
3. Did not give any overlay its own private copy of "am I the owner" — there is
   exactly one `Option<DragOwner>` on `UIRoot`.
4. Did not touch `window_input`'s winit seam or reach for OS-level capture.

## Entry-state check

`rg -n "overlay_drag_active" crates/manifold-app/src/ui_root.rs` returned **7**
sites at start (`336, 461, 1461, 1464, 1472, 1483, 2493`), not the doc's baked 5.
Per the orchestrator's pre-approved resolution: the two extras (`1472`, `1483`)
were `eprintln!` reads inside the stash-gate's trace block, which §3.2's rewrite
deletes wholesale — they went with it. The `MANIFOLD_INPUT_TRACE` tap itself stayed
alive, retargeted to print `drag_owner` instead of the old latch (see the retargeted
`eprintln!` in `process_events`'s stash decision).

## D7 finding — Ruler kept

`rg -n "Drag" crates/manifold-ui/src/panels/viewport/interaction.rs` shows ruler
scrubbing IS `Drag`-event-based: `ViewportDragMode::RulerScrub` is armed on
`DragBegin` (origin inside `ruler_rect`) and consumed on `Drag`/`DragEnd`. Per D7's
own instruction ("if it does, it's a `DragOwner` variant"), **`Ruler` is kept** —
implemented as `DragOwner::Ruler`, resolved at step 4 (before `TimelineTracks`,
after layer headers), a pure `ruler_rect().contains(origin)` check.

## `DragBegin { / Drag {` consumer inventory (re-derived, `rg -n "DragBegin \{|Drag \{" crates/ -g '*.rs'`)

Classified by how each site was handled:

**No change needed (position-based, ignore `node_id`):**
- `crates/manifold-ui/src/panels/viewport/interaction.rs` — marker/ruler/overview/
  scrollbar drag modes, all keyed on `origin`/`pos`.
- `crates/manifold-ui/src/panels/audio_setup_panel.rs` (non-test `on_event` arms) —
  keyed on `origin`/`pos` only.
- `crates/manifold-app/src/ui_snapshot/script.rs` — drives `InteractionOverlay` off
  `origin`/`pos`.
- `crates/manifold-app/src/app_render.rs` (editor window's overlay feed) — same
  shape, `origin`/`pos` only.
- `crates/manifold-app/src/ui_root.rs`'s `is_event_in_tracks_area`/`trace_kind`/
  `trace_worthy` — position/wildcard matches, rewritten anyway per the seam brief.
- `crates/manifold-ui/src/interaction_overlay.rs` (test) — wildcard `{ .. }` match.

**Mechanical `Option`-wrap (literal test constructors):**
- `crates/manifold-ui/src/panels/audio_setup_panel.rs` — 4 sites constructing
  `UIEvent::DragBegin`/`Drag` with a bare `NodeId` (`p.bg_id`, `gain_value`,
  `sens_value`) now wrap `Some(...)`.

**Genuinely needed the node — adapted, not shimmed:**
- `input.rs` `process_pointer` — the D9 change itself: gate removed, emits
  `tree.node_for_widget(pw)` (now `Option`) unconditionally for both `DragBegin`
  and `Drag`.
- `input.rs` test module — `drag_survives_tree_rebuild` unwraps the now-`Option`
  `node_id`; added `d9_motion_stream_survives_pressed_node_dying_before_threshold`
  (the D9 repro).
- `inspector.rs` `handle_event`'s `DragBegin` arm — `find_target_for_node` only
  called `if let Some(node_id) = *node_id`; `None` just means no NEW target
  resolves (matches the existing `Some(id)`-resolves-to-`None` case — `pressed_target`
  is normally already armed from `PointerDown`, which D9 doesn't touch).
- `inspector.rs` `try_begin_card_drag(node_id: NodeId, ...)` → `Option<NodeId>`:
  `None` short-circuits to `false` (no card drag can be identified without a node —
  same net effect as today when the event didn't fire at all).
- `layer_header.rs` `handle_drag_begin(tree, node_id: NodeId)` → `Option<NodeId>`:
  the existing `pending_drag_layer` fallback (built for "a rebuild invalidated the
  id between `PointerDown` and `DragBegin`") already covers exactly the `None`
  case — `None` now skips straight to that fallback instead of attempting an exact
  match first.
- `crates/manifold-app/src/ui_root.rs`'s own `route_inspector_events` (editor
  window) — same `try_begin_card_drag` call, mechanically adapted; per the design's
  §7 Deferred note this window doesn't get its own `DragOwner` registry in P1, only
  the shared D9/D3 fixes (which it inherits for free, same types).

No misfit site required escalation — every consumer fell cleanly into one of the
three buckets above.

## Two things found, not blocking, flagged for the orchestrator

1. **Pre-existing clippy failure behind `ui-snapshot` feature, unrelated to this
   phase.** `cargo clippy -p manifold-ui -p manifold-app --features
   manifold-app/ui-snapshot -- -D warnings` fails on `crates/manifold-app/src/
   ui_snapshot/render.rs:760` (`make_blit_pipeline` never used, dead-code deny).
   Confirmed present at the base commit `b9304330` (I never touched `render.rs`;
   `git diff --stat b9304330 -- .../render.rs` is empty) and reproduces in a
   throwaway worktree built straight from `b9304330`. The brief's actual clippy
   gate (`cargo clippy -p manifold-ui -p manifold-app` — no feature) is clean; I'm
   flagging this because the L3 flow gate needs the feature to run `cargo xtask
   ui-snap`, so anyone chaining `clippy --features ui-snapshot` into one command
   will trip over it. Worth a `docs/BUG_BACKLOG.md` entry (next free id) — I didn't
   add one myself since it's outside this phase's file list and touching the
   shared backlog from an unlanded worktree branch risks a conflict with whatever
   else is live on `main`.
2. **`inspector` scene fixture artifact (pre-existing, unrelated to ownership):** a
   full-width clip's reported center (from `visible_clip_rects`) can land inside
   the Inspector panel's real hit-test rect when `inspector_width=600` narrows the
   tracks column at this zoom — confirmed identical on `b9304330` too (same
   `EffectCardClicked(2)` side effect, same clip left un-moved). This is why the new
   L3 flow uses the `timeline` scene (unique clip labels, proven geometry) with a
   drag destination past the tracks' right edge instead of the `inspector` scene's
   real estate — see "Shortcuts taken" below.

## Shortcuts taken

- **The new L3 flow doesn't literally render over inspector chrome.** The `timeline`
  scene (matching `drag-clip.json`'s proven geometry, unique clip labels) sets
  `inspector_width = 0.0` — there's no rendered inspector column to release over.
  The flow instead drags to `x=1500` (well past every clip's max extent of 1382,
  i.e. definitively outside `tracks_rect`) as the position-independence proxy the
  design actually cares about, then drags a second, unrelated clip (`Video 3`) to
  prove no wedge. This is a deliberate substitution, not an oversight — the
  `inspector` scene's clips don't safely support both requirements (a real
  inspector column AND a uniquely-labeled clip whose reported center stays inside
  the narrower tracks column) at once, per finding #2 above. The `resolve_drag_owner`
  mechanism itself is scene-agnostic; the ui_root unit tests (`modal_overlay_claims_
  drag_unconditionally`, `dropdown_open_at_drag_start_dismisses_without_consuming_
  and_falls_through`) exercise the literal overlay-vs-tracks ownership question
  directly without depending on any scene fixture.
- Nothing else. No stubs, no hardcoded IDs, no deferred TODOs in the shipped code.
