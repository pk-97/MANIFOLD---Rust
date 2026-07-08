# DRAG_CAPTURE P1 — landed 2026-07-08 @ `9bb8ca86`

**Branch:** `wave/drag-capture-p1` (feat `6e4bddcb`, merge `9bb8ca86`) · **Level reached:** L3 / target L3 (§10)
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — **P1 LANDED 2026-07-08 @ `9bb8ca86`** (single drag-capture ownership D1–D4 + D9, L3); P2–P3 pending · design 2026-07-07 (approved same day by Peter) · Fable`

## What landed

Single drag-capture ownership (D1–D4 + D9). One `UIRoot::drag_owner: Option<DragOwner>`
resolved once at the first `DragBegin` (`resolve_drag_owner`, order Overlay > Inspector >
LayerHeaders > Ruler > TimelineTracks > None, first claim wins) replaces the
`overlay_drag_active` latch, the dropdown eat-arm, and the per-panel positional stash.
Terminal events broadcast (`broadcast_gesture_end`, unconditional after owner delivery;
self-heal on next `PointerDown`) so every gesture that began ends exactly once — kills
BUG-058's eater class by construction. `DragBegin`/`Drag` now emit unconditionally with
`node_id: Option<NodeId>` (D9) — the motion stream survives a mid-drag tree rebuild
dropping the pressed widget (Peter's "first band-line click is always dead" trace).
`claims_drag`/`gesture_ended` trait hooks added with defaults only (audio-panel overrides
are P2). D7: `Ruler` variant KEPT — ruler scrub is `Drag`-event-based.

## Gate results (verbatim)

```
cargo test -p manifold-ui --lib            → 630 passed; 0 failed; 0 ignored
cargo test -p manifold-app --bin manifold  → 155 passed; 0 failed; 2 ignored (GPU)
cargo clippy -p manifold-ui -p manifold-app -- -D warnings → clean (no-feature gate, per brief)
rg -n "overlay_drag_active" crates/                       → 0 hits (negative gate)
rg -n "DragEnd" crates/manifold-ui/src/panels/dropdown.rs → 0 hits (negative gate)
L3 (existing regression)  cargo xtask ui-snap timeline --script scripts/ui-flows/drag-clip.json → exit 0
L3 (new no-wedge proof)   cargo xtask ui-snap timeline --script scripts/ui-flows/drag-clip-release-over-inspector.json → 12/12 steps ok
```

New-flow assertions (run in main checkout by the orchestrator): Plasma 1 moves x 230→824;
then Video 3 (a second, unrelated clip) moves x 902→524 — the second drag taking effect
proves the first didn't wedge `DragMode`. Final PNG `run-drag-clip-release-over-inspector/09.png`
visually confirms both clips at their new positions.

## Deviations from brief

New L3 flow uses the `timeline` scene (proven geometry, unique clip labels) with a drag
destination past the tracks' right edge as the position-independence proxy, instead of
literally rendering over the `inspector` scene's inspector column — that scene can't
safely support both a real inspector column and a uniquely-labeled, safely-positioned
draggable clip at once (see finding BUG-068). The assertion the flow proves — a drag
released *outside* `tracks_rect` still completes, and a following unrelated drag works —
is exactly the no-wedge guarantee the brief asked for.

## Shortcuts confessed (rolled up from phase reports)

None beyond the scene-choice deviation above. No stubs, no hardcoded IDs, no parallel
old path (both negative gates zero).

## Verification debt

None opened, none carried for P1. (The pass-level L4 feel item — Peter's ~2px
crossover nudge — is owed at P3, not P1.)

## Click-script for Peter (≤2 minutes)

1. Open a project, grab a clip and trim/move it, then release the mouse while the cursor
   is over the inspector panel (or any non-tracks area) — expect: the clip lands where you
   dropped it, no stuck move/trim cursor.
2. Immediately grab a second clip and move it — expect: it moves normally (no wedge from
   the previous gesture).
3. Open a dropdown, then start a drag somewhere else on the timeline — expect: the dropdown
   dismisses and the drag works (dropdown no longer eats the gesture).
