# EDITOR_WINDOW_UNIFICATION P1 — landed 2026-07-14 @ c8584b8d

**Branch:** feat/editor-window-unification · **Level reached:** L2 / target L2 (§10)
**Doc status line (quoted verbatim):** `**Status:** APPROVED design · P1 LANDED 2026-07-14 (BUG-151 FIXED) · P2–P3 not built · Fable 5 (with Peter in the room) · Sonnet-executable`

## Gate results (verbatim)

`cargo clippy -p manifold-app -- -D warnings` (both plain and `--features ui-snapshot`): clean, no clippy warnings (only unrelated pre-existing Objective-C deprecation notices from `manifold-media`'s native build).

`cargo test -p manifold-app --features ui-snapshot --bin manifold`:
```
test ui_snapshot::editor_window_harness::node_the_fixture_places_renders_at_its_declared_screen_rect ... ok
test ui_snapshot::overlay_fidelity_proof::bug097_render_sub_region_draws_root_excluding_overlay_that_render_tree_range_blanks ... ok
test ui_snapshot::cache_path_full_render::cache_path_full_render ... ok
test result: ok. 177 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 1.23s
```

Negative gates (re-run by the orchestrator independently, not trusted from the worker report):
```
rg "overlay_draw.iter" crates/manifold-app/src/ui_frame.rs        → zero hits
rg "render_tree_range\(&ui_root.tree, 0, usize::MAX\)" crates/manifold-app/src/editor_frame.rs → zero hits
rg "WorkspaceKind|is_graph_editor" crates/manifold-app/src/tree_passes.rs → one hit, a doc-comment stating the forbidden pattern, not an instance of it
```

Byte-identical main-window readback (I4): confirmed via sha256 match of `timeline`/`states` PNGs between the D2-fix commit and the D1-only baseline commit. `inspector` scene skipped — pre-existing nondeterminism unrelated to this change, logged as BUG-153.

Full workspace sweep, run by the orchestrator in the main checkout after merge, before push:
```
cargo nextest run --workspace → 3331 tests run: 3331 passed, 12 skipped (28.5s)
cargo clippy --workspace -- -D warnings → clean
cargo deny check bans → bans ok
```

## Deviations from brief

D2 as literally written in the original design doc turned out to be unimplementable: the doc's §1 audit claimed the editor's `UIRoot::build()` already populated `overlay_draw` "exactly like the main window's." This was false — only the main window ever calls `UIRoot::build()`; the editor's tree is assembled by hand each frame and never recorded overlays at all, so `overlay_draw`/`overlay_region_start` were permanently empty for the editor. That gap, not a flat-scan traversal quirk, is BUG-151's actual root cause.

P1's first worker hit this correctly as a must-escalate line and stopped rather than improvising past it (landed D1 + D4 only, commit `9e3d710e`). The orchestrator brought it to Peter, who preferred "make the editor run the same overlay-registration step the main window runs, scoped to what it actually has." A design-reviewer pass (Fable, review-only, no code) confirmed the direction and found the correct granularity: not the whole `UIRoot::build()` (which lays out main-window-only panels and would corrupt editor state), but a new minimal `UIRoot::build_overlays_for_screen()` wrapper around the existing private `build_overlays()`. A second Sonnet worker implemented that spec, corrected the design doc's §1 audit and D2 text, and corrected the BUG-151 backlog entry (commit `1c26b219`).

Net effect: the phase's substance (D1, D2, D4, BUG-151 fix) is exactly what was promised, but the mechanism inside D2 differs from the doc's original prose, and the doc itself was corrected in the same landing per the "don't compound doc inaccuracy" concern Peter raised.

## Shortcuts confessed (rolled up from phase reports)

None on the landed code. One process shortcut, explicitly allowed by the orchestrator: the acceptance demo produced only an "after" PNG (`docs/landings/BUG-151_editor_after_open_picker.png`), not a "before" PNG, because reproducing the old hand-rolled broken path inside the harness would have required re-implementing dead code — a written description of the prior broken state (cells with no container) was used instead, per the phase brief's own fallback allowance.

## Verification debt

None opened, none carried. BUG-153 (pre-existing `ui-snap inspector` scene nondeterminism, found while re-running the I4 readback) was logged to `docs/BUG_BACKLOG.md` as a new, separate, unrelated bug — not verification debt of this phase.

## Click-script for Peter (≤2 minutes)

1. Launch the app, open the graph editor for any layer with a generator or effect chain. — expect: the editor opens normally, canvas and inspector render as before.
2. Click "+ Add Effect" (or any control that opens the node browser/picker popup). — expect: the popup opens with an opaque dark container, a search bar, and a full grid of node names — legible over the graph behind it, not bare text floating with the graph bleeding through.
3. Toggle the perf HUD (if bound) while the editor is open. — expect: no visual regression; this is exercised further in P2, not blocked by P1.
4. Switch back to the main window's timeline/inspector. — expect: no visible change from before this landing — pixels are byte-identical per the I4 gate.
