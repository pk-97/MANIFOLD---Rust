# DRAG_CAPTURE P3 (final phase) — landed 2026-07-08 @ `f23fa1f1`

**Branch:** `wave/drag-capture-p3` (feat `2fc4cfbd`, merge `f23fa1f1`) · **Level reached:** L1 / target L1 (§5 P3 — the 1px feel is L4 by nature, tracked as VD-019)
**Doc status line (quoted verbatim):** `**Status:** SHIPPED 2026-07-08 (all phases) — **P1 @ `9bb8ca86`** (ownership D1–D4 + D9, L3); **P2 @ `12683746`** (z-aware seams D5 + `swallow_drag` retired, L1; VD-017/018); **P3 @ `f23fa1f1`** (immediate-drag threshold D6, L1; VD-019 = Peter's crossover-nudge feel pass) · design 2026-07-07 (approved same day by Peter) · Fable`

## What landed

Immediate-drag threshold for precision surfaces (D6 / §3.4). `UIInputSystem::request_immediate_drag()`
(`input.rs:458`) sets `immediate_drag_armed` (`input.rs:394`), which makes `process_pointer`'s Move
arm use an effective threshold of `0.0` for the current press (`input.rs:561`), cleared on the next
Up (`input.rs:672`) — the global `DRAG_THRESHOLD`/`DRAG_THRESHOLD_PX` constant is untouched.
`wants_immediate_drag()` overlay hook (default false, `overlay.rs:176`); the audio panel overrides it
to `self.dragging_band.is_some()` (`audio_setup_panel.rs:2413`); `UIRoot::any_overlay_wants_immediate_drag()`
(`ui_root.rs:1185`) polls open overlays after a consumed `PointerDown` and arms the input system.
The band dividers now begin dragging on the first pixel instead of needing a 4px pull.

## Seam resolution

`route_overlay_event` returns `bool`, not which overlay consumed the `PointerDown`. Resolved with
the brief's preferred (non-escalating) approach: `any_overlay_wants_immediate_drag` polls every open
overlay's `wants_immediate_drag()` via the same `Z_ORDER` walk `broadcast_gesture_end` uses. Sound
because the only field the hook reads (`dragging_band`) can only have just been set by whichever
overlay's `on_event` actually ran this event, and it is cleared at every gesture end — no stale
positive. `route_overlay_event`'s public return shape was not changed.

## Gate results (verbatim, run by orchestrator in worktree)

```
cargo test --workspace           → every crate "test result: ok", 0 failed anywhere
                                    (manifold-app bin 991 passed, manifold-ui lib 633 passed)
cargo clippy --workspace -- -D warnings → exit 0, Finished (manifold-media objc build-script
                                    warnings pre-existing, non-clippy)
rg -n "DRAG_THRESHOLD_PX\s*=\s*0|DRAG_THRESHOLD\s*=\s*0" crates/ → 0 hits (negative gate)
named tests:
  input::tests::request_immediate_drag_allows_one_pixel_move_to_begin_drag → pass
  input::tests::three_pixel_wiggle_without_immediate_drag_still_resolves_to_click → pass (regression)
  ui_root::drag_capture_tests::divider_grab_requests_immediate_drag_and_one_pixel_move_yields_crossover_changed → pass
```

## Deviations from brief

None. The seam was resolved with the brief's named preferred approach; no escalation.

## Shortcuts confessed (rolled up from phase report)

None.

## Verification debt

VD-019 opened — band-divider immediate-drag feel is L1 (mechanism proven by tests); the ~2px
crossover-nudge feel is L4 by nature, owed to Peter.

## Concurrency note

Landed into a shared checkout where a concurrent session had advanced main to `56a6bad0`
(AUDIO_ANALYSIS_ACCURACY / BUG-069 license-audit docs) after P2. The `--no-ff` merge took
`56a6bad0` as a parent (verified `git merge-base --is-ancestor 56a6bad0 f23fa1f1`); zero file
overlap with P3's code, so the pre-merge workspace gate held byte-for-byte.

## Click-script for Peter (≤2 minutes) — the whole pass, end to end

1. Trim a clip and release the mouse over the inspector, then immediately grab and move a second
   clip — expect: both behave, no stuck move/trim cursor (P1: drags never wedge).
2. Open the Audio Setup panel over the timeline and grab a crossover line sitting over the
   video/timeline split zone — expect: the line moves, the panels do not resize (P2: a missed/covered
   grab edits nothing underneath).
3. Grab a band-divider line and nudge it a couple of pixels — expect: it responds from the first
   pixel, on the first click, with no sticky lag (P3 + D9: the L4 feel Peter confirms live).
