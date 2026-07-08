# DRAG_CAPTURE P2 — landed 2026-07-08 @ `12683746`

**Branch:** `wave/drag-capture-p2` (feat `c22f26fe`, merge `12683746`) · **Level reached:** L1 / target L2 (§10 — gap tracked as VD-017)
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — **P1 LANDED 2026-07-08 @ `9bb8ca86`** (ownership D1–D4 + D9, L3); **P2 LANDED 2026-07-08 @ `12683746`** (z-aware seams D5 + `swallow_drag` retired, L1; VD-017/018); P3 pending · design 2026-07-07 (approved same day by Peter) · Fable`

## What landed

Z-aware window seams (D5) + `swallow_drag` stopgap retirement. New
`UIRoot::overlay_contains_point(pos)` walks `Z_ORDER` top-down over `overlay_rects` (recorded
in `build_overlays` from each overlay's actual placement rect); the split-handle and
inspector-edge press interceptors in `window_input::primary_mouse_input`
(`window_input.rs:289,331`) each gain `&& !overlay_contains_point(cursor_pos)`, so a seam
visually under a floating overlay (the Audio Setup panel docked over the timeline, BUG-059)
no longer steals the press — it falls through to normal routing and the overlay gets it. The
handles are NOT converted to widget routing (Deferred §7); `split_dragging`/
`inspector_resize_dragging` stay put. The audio panel implements the P1-defaulted trait hooks:
`claims_drag(origin) = armed || point_in_panel(origin)` and `gesture_ended()` clearing
`dragging_band`/`calibration_drag`. `swallow_drag` (field, arms, two tests, all comments) is
deleted — its test scenarios moved to `claims_drag`/`gesture_ended` tests.

## Gate results (verbatim)

```
cargo test -p manifold-ui --lib            → 631 passed; 0 failed; 0 ignored
cargo test -p manifold-app --bin manifold  → 156 passed; 0 failed; 2 ignored
cargo clippy -p manifold-ui -p manifold-app -- -D warnings → clean (manifold-media objc build-script warnings pre-existing, non-clippy)
rg -n "swallow_drag" crates/               → 0 hits (negative gate)
new test overlay_contains_point_true_where_audio_panel_overlaps_split_handle → asserts the
  panel and split-handle actually overlap at today's constants, THEN that the guard fires
```

## Deviations from brief

Two judgment calls, both genuine gaps between the doc's prose and the code, documented in the
worktree NOTES rather than improvised:
1. `route_overlay_event` does no reusable rect-hit, so `overlay_contains_point` needed a new
   `overlay_rects` cache populated in `build_overlays` from the placement rect. The three
   `SelfManaged` overlays (dropdown, browser popup, Ableton picker) get a placeholder rect,
   not their true footprint — tracked as VD-018; the dropdown is hit-tested accurately upstream
   so it is unaffected, and the audio panel (the BUG-059 case) gets an exact rect.
2. The `on_event` missed-grab arm is dropped, not replaced — tracing `process_events` showed
   leak-prevention runs through `resolve_drag_owner`/`should_stash_for_tracks` (ownership), so
   `claims_drag`/`gesture_ended` are the true replacement one layer up.

## Shortcuts confessed (rolled up from phase reports)

None. (No stubs, no hardcodes; both judgment calls are documented decisions, not shortcuts.)

## Verification debt

VD-017 opened — audio-panel-over-timeline seam demo is L1 (unit tests only); no snapshot scene
opens the panel over the timeline and the brief forbade inventing one; collapses into the P3
L4 feel pass. VD-018 opened — SelfManaged overlay rect fidelity in `overlay_contains_point`
(structural note, carried).

## Concurrency note

Landed into a shared checkout where a concurrent session had advanced main to `8499457a`
(AUDIO_ANALYSIS_ACCURACY docs, BUG-069) after P1. The `--no-ff` merge took `8499457a` as a
parent (verified `git merge-base --is-ancestor 8499457a 12683746`); zero file overlap with
P2's code.

## Click-script for Peter (≤2 minutes)

1. Open the Audio Setup panel so it floats over the timeline, and grab a crossover/band
   divider line that sits directly over the video/timeline split zone — expect: the divider
   line moves, the panels do NOT resize (the seam yields to the overlay).
2. Close the panel, then grab the same split zone with nothing over it — expect: the split
   handle drags normally (the seam still works when no overlay covers it).
