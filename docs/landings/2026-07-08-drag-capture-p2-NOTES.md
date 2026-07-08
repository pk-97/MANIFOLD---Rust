# DRAG_CAPTURE_DESIGN P2 — notes (worktree `.claude/worktrees/drag-capture-p2`, base `95dc1e39`)

## Read-back

**D5 in my own words:** the split-handle and inspector-edge press interceptors in
`window_input.rs::primary_mouse_input` run *before* any hit-testing — they check raw
cursor position against a thin band, full-width (split) or full-height (inspector edge),
and start a resize drag straight through anything floating on top. The Audio Setup panel
docks top-right, 38% width, full height, so its rect visually overlaps both bands. Today
input disagrees with the eye: a click on a divider line that happens to sit inside either
band starts a panel resize instead of a crossover drag (part of BUG-059). The fix is a
new `UIRoot::overlay_contains_point(pos)`, walking `Z_ORDER` top-down over OPEN overlays
only, and gating both press branches on `!overlay_contains_point(pos)` — if a floating
overlay visually occupies the press point, the seam yields and the press falls through to
normal routing (which reaches the overlay via the existing gauntlet).

**Forbidden moves for this phase (§3.5, D5 note, §6.5/6.8, P2 "Forbidden moves"):**
- Do NOT convert the split/inspector handles to widget-intent routing — they keep their
  `window_input`-side state (`split_dragging`/`inspector_resize_dragging`); D5 only adds a
  z-check in front of the existing raw-position branches. Full conversion is Deferred §7.
- Do NOT keep `swallow_drag` "for safety" alongside `claims_drag`/`gesture_ended` — it dies
  entirely this phase, negative gate is zero `rg` hits anywhere in `crates/`.
- Do NOT widen this into a general audit of every hover/cursor call site that uses the
  same seam predicates (see observation below) — P2's deliverable is the two *press*
  branches in `window_input.rs::primary_mouse_input`, not hover-cursor feedback.

## Entry-check counts (re-derived)

- `rg -n "swallow_drag" crates/` → **16 hits**, exactly matching the brief's inventory:
  field (439), its doc comment (441), `panel_rect`'s comment referencing it (750, 1751),
  Default init (497), six arm assignments/reads (2341, 2348, 2377, 2387, 2390, 2399), four
  tests (2635, 2652, 2670, 2683), plus one mention in the `Overlay` trait's `claims_drag`
  doc comment (`overlay.rs:153`). All in `audio_setup_panel.rs` except the last. Shape
  matches — proceeding.
- `rg -n "is_near_split_handle|is_near_inspector_edge" crates/manifold-app/src` → 6 hits:
  the two press-interception branches this phase guards
  (`window_input.rs:288` split-handle, `window_input.rs:318` inspector-edge — matches the
  brief's "the two seam branches D5 guards" exactly); the definition site
  (`ui_root.rs:2470` `is_near_inspector_edge`) and a doc-comment mention (`ui_root.rs:677`);
  and **two additional call sites NOT named by the brief**, both in `app.rs`
  (`app.rs:864`, `app.rs:879`) inside what turns out to be a hover-cursor-icon updater —
  it sets `TimelineCursor::ResizeHorizontal`/`ResizeVertical` when the cursor is *near*
  either seam, independent of any press. This is a materially different concern (visual
  hover feedback, not press-ownership arbitration) that D5 does not mention.

**Observation, not a stop:** because D5's guard only lands in `window_input.rs`'s press
branches (per this phase's explicit deliverable), the hover-cursor logic in `app.rs` is
now slightly inconsistent with press behavior — hovering over the audio panel where it
overlaps a seam will still show a resize cursor, even though a press there now correctly
falls through to the panel instead of starting a resize. This is cosmetic (cursor icon
only; no functional/data-loss consequence — the actual drag/resize ownership is correct
after this phase's fix) and out of scope for P2 as briefed. Flagging it in the landing
report as a candidate follow-up rather than silently fixing or silently ignoring it.

## Implementation choice worth recording: how `overlay_contains_point` gets its rects

The design says the method should "model its open-check + rect-hit exactly on how
`route_overlay_event` decides an overlay is live and hit" — but `route_overlay_event`
does no generic rect-hit at all; it dispatches to each overlay's own `on_event` and lets
the overlay's internal state decide Consumed/Ignored. There is no existing per-overlay
rect cache anywhere in `UIRoot` (`overlay_draw` is node index ranges for drawing, not
rects). The one place a screen rect gets computed per overlay is `build_overlays`
(`anchor()` + `size_policy()` + `compute_overlay_rect`), and it's discarded after use.

Chose to add a small cache, `UIRoot::overlay_rects: Vec<(OverlayId, Rect)>`, populated in
`build_overlays` from the same `rect` value already computed there (so `overlay_contains_
point` reads exactly the rect the overlay was last actually placed at, not a fresh
recomputation — this is what "input must agree with the eye" means literally: use what
was drawn). Documented limitation: three overlays (`Dropdown`, `BrowserPopup`,
`AbletonPicker`) use `Anchor::SelfManaged` and position themselves *inside* `build_at`
from raw screen size — `compute_overlay_rect` returns a placeholder `(0,0,w,h)` rect for
those, not their true click-anchored footprint, so the cache is not meaningful for them.
None of the three are relevant to this design's motivating case (BUG-059 is specifically
the Audio Setup panel, `Anchor::Corner`, which resolves correctly), and their placeholder
rect sits at the screen origin — far from where the split-handle/inspector-edge bands
live in practice — so the risk of a false "blocked" seam from this gap is low but not
zero. Recording this rather than silently treating the 3 self-managed overlays as fully
covered by D5; a real fix would need a new trait hook exposing each overlay's actual
on-screen rect, which is bigger than this phase's deliverable and not requested by it.

## `on_event`'s DragBegin/Drag/DragEnd arms — how they simplify without `swallow_drag`

Read `process_events` end-to-end before deciding this: the routed cascade
(`route_overlay_event`, loop 1) and the ownership resolution (`resolve_drag_owner`,
called from a *separate* loop 2 over the same events) are two independent mechanisms.
`resolve_drag_owner`'s result — not `route_overlay_event`'s Consumed/Ignored return — is
what `should_stash_for_tracks` reads to decide whether a drag reaches the timeline. That
means the actual harmful consequence BUG-059 named (a missed grab silently moving clips
underneath) is prevented by ownership (`claims_drag` → `drag_owner = Overlay(AudioSetup)`
→ `should_stash_for_tracks` sees a non-`TimelineTracks` owner and refuses to stash),
**not** by the panel's own `on_event` consuming the event. So `on_event`'s missed-grab
branch (previously: consume via the old drag-swallow flag when `point_in_panel(origin)`
and nothing armed) is deleted outright, not replaced with equivalent logic — it returns
`Ignored` now for that case, same as it would if the panel weren't there. The armed cases
(`dragging_band`/`calibration_drag` `Some`) are unchanged; they already return `Consumed`
directly without ever consulting the old flag. `claims_drag`/`gesture_ended` are the real
replacement, operating one level up (ownership), which is what "replaces the BUG-059
stopgap" means in the trait doc comment — confirmed by re-reading it, not assumed.

## Gate results (all green)

- `cargo test -p manifold-ui --lib` → **631 passed, 0 failed** (includes the 3
  claims_drag/gesture_ended tests replacing the two deleted swallow tests:
  `missed_grab_origin_inside_panel_is_claimed_by_ownership`,
  `claims_drag_false_for_origin_outside_panel_with_nothing_armed`,
  `gesture_ended_clears_armed_band_and_calibration_drags`).
- `cargo test -p manifold-app --bin manifold` → **156 passed, 0 failed, 2 ignored**
  (pre-existing ignores, unrelated). `ui_root::drag_capture_tests` (6 tests, including
  P1's 5 pre-existing ones — no regression) includes the new
  `overlay_contains_point_true_where_audio_panel_overlaps_split_handle`.
- `cargo clippy -p manifold-ui -p manifold-app -- -D warnings` → clean (only pre-existing
  Obj-C deprecation warnings from `manifold-media`'s native build, unrelated to this
  phase and not clippy findings).
- Negative gate `rg -n "swallow_drag" crates/` → **0 hits** (one self-introduced hit in a
  new doc comment was caught and reworded before the final pass).

## Acceptance demo — honest fallback, not invented

No existing `scripts/ui-flows/*.json` flow or `ui_snapshot::fixtures` scene positions the
Audio Setup panel open and overlapping the timeline/split-handle — the one audio-related
fixture (`audio_sends_scene`, key `"audiosends"`) only seeds `project.audio_setup` data
(sends/routes/triggers) for testing send routing, never opens the panel overlay itself.
Per this phase's explicit instruction not to invent a scene fixture without escalating,
the acceptance demo for this phase is the passing test output above — in particular
`overlay_contains_point_true_where_audio_panel_overlaps_split_handle`, which asserts (with
a self-checking sanity gate, `overlap_x_min < overlap_x_max`, so the test would fail loudly
rather than silently pass if the panel/handle stopped overlapping at today's constants)
that the docked panel's real, build-resolved rect overlaps the split handle's band at the
current default viewport, and that `overlay_contains_point` correctly reports the overlap.
This is L2 per the phase brief (window-level press interception is above the ui-snap flow
driver's entry point). A follow-up scene fixture that actually opens the Audio Setup panel
over the timeline and renders a PNG would be a good addition for a future L3+ pass, but is
out of scope here.
