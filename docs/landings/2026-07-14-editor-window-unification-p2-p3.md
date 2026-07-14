# EDITOR_WINDOW_UNIFICATION P2+P3 — landed 2026-07-14 @ 9ea241dc

**Branch:** feat/editor-window-unification · **Level reached:** P2 L2 / P3 L1 (both at target, §10)
**Doc status line (quoted verbatim):** `**Status:** APPROVED design · SHIPPED 2026-07-14 — P1 (D1/D2/D4, BUG-151 fixed), P2 (D6 redraw-keepalive aggregate), P3 (D7 structural guard) all landed · Fable 5 (with Peter in the room) · Sonnet-executable`

## Gate results (verbatim)

P2 unit tests (`cargo test -p manifold-app --features ui-snapshot --bin manifold`): 180 passed, 0 failed, 2 ignored (177 P1 tests + 3 new).

P2 negative gate: `rg -c "offscreen_dirty = true" crates/manifold-app/src/window_input.rs` → 29 (≤ 29 baseline measured on the P1 tip before P2 started — unchanged, since none of the 29 sites were overlay-animation-driven).

P3 structural guard, re-run independently by the orchestrator (not trusted from the worker report):
```
cargo test --test tree_render_call_sites -p manifold-app
running 3 tests
test tree_passes_branches_on_input_presence_not_caller_identity ... ok
test guard_walk_finds_source_files ... ok
test tree_render_call_sites_are_allowlisted ... ok
test result: ok. 3 passed; 0 failed
```

Red-then-green probe, executed by the orchestrator directly (a claimed-but-unverified guard is a hope, not a gate — per §5's own rule): inserted a duplicate `ui_renderer.render_tree_range(&ui_root.tree, 0, ui_root.overlay_region_start);` into `editor_frame.rs`, re-ran the guard test:
```
thread 'tree_render_call_sites_are_allowlisted' panicked:
Structural guard (EDITOR_WINDOW_UNIFICATION_DESIGN.md D7) tripped:
editor_frame.rs: found 2 raw render_tree_range/render_sub_region call site(s), expected 1
test result: FAILED. 2 passed; 1 failed
```
Reverted (`git checkout -- crates/manifold-app/src/editor_frame.rs`), working tree confirmed clean, re-ran: `test result: ok. 3 passed; 0 failed`.

Full workspace sweep, run by the orchestrator in the main checkout after merging two concurrent origin/main landings (fusion-sota batch 3, twice) into the branch first:
```
cargo nextest run --workspace → 3342 tests run: 3342 passed (1 leaky), 12 skipped (30.2s)
cargo clippy --workspace -- -D warnings → clean
cargo deny check bans → bans ok
```

## Deviations from brief

**P2 membership re-derivation diverged from the design doc's guess.** D6's prose speculated the redraw-keepalive survivor set would be "toast timers and any remaining overlay tween." The re-derivation (`rg "is_animating|tick\(" crates/manifold-ui/src/panels/`) found the popup professional pass had already deleted every popup's entrance tween — `browser_popup`/`ableton_picker`/`settings_popup` all hardcode `is_animating() -> false` with comments saying so. Only `ToastPanel` is actually live; the worker correctly did not include the three dead stubs in the aggregate (the brief's own "unit test per member ⇒ animating flips true" requirement is unsatisfiable for a predicate that can never observe `true`).

**P3's call-site inventory diverged from D7's design-time baked list**, because P1 (which ran before P3, in the same wave) already did more of the seam extraction than D7 assumed would happen at P3-impl time. D7's doc text expected `ui_frame.rs:702,710` to still need extracting at P3; P1 had already moved them into `tree_passes.rs`. Re-derived inventory: 3 files need allowlist entries (`tree_passes.rs`, `editor_frame.rs`, `ui_snapshot/mod.rs`), not 4 — `ui_frame.rs` needs zero. This is the intended behavior of the "re-derive, don't trust the baked inventory" rule (DESIGN_DOC_STANDARD §8.3), not a defect.

**Landing-time discovery, not part of either phase's brief:** a concurrent session's fusion-sota wave landed on `origin/main` twice while P2/P3 were being built and gated, and independently claimed bug ID `BUG-154` for an unrelated bug (`removing-group-with-slider-bound-nodes-leaves-stale-effect-card`) at the same time P2's worker claimed `BUG-154` for the editor-perf-hud finding. The orchestrator resolved the resulting merge conflict by renumbering this design's bug to `BUG-157` (the next free ID after the incoming session's 154–156) rather than overwriting theirs — same duplicate-ID class BUG-148 already documents, caught here before it landed instead of after.

## Shortcuts confessed (rolled up from phase reports)

None on the landed code, both phases. P2's worker noted BUG-157 (editor perf HUD would never tick if opened) as found-not-fixed, correctly out of P2's scope (redraw-keepalive aggregate, not overlay data-plumbing) and currently unreachable (no UI path opens the editor's own perf HUD today).

## Verification debt

None opened, none carried.

## Click-script for Peter (≤2 minutes)

1. Open the graph editor, open the node-browser popup (as in P1's click-script) — expect: no change from P1's landing, still renders correctly.
2. If a perf-HUD toggle is ever wired to the editor window specifically (not yet — see BUG-157), opening it there would show the panel chrome but frozen `"—"` values; this is documented, not blocking, and out of scope for this landing.
3. No other user-visible surface — P2's redraw-keepalive change and P3's structural guard are both internal/policy mechanisms with no direct UI affordance to click. P3's "surface" is the meta-test itself (proven to fire, see the red/green transcript above).
