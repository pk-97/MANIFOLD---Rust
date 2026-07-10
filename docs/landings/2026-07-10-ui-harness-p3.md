# Landing — UI_HARNESS_UNIFICATION P3 (editor window; lookalike found + killed) — WAVE COMPLETE

**Date:** 2026-07-10 · **Phase:** P3 (final) · **Branch:** `feat/ui-harness-p3` → `main` · **Orchestrator:** Opus (1M), Sonnet worker

## What happened

P3's first attempt escalated correctly: the existing headless `editor` scene was itself a **lookalike**. The live editor (`present_graph_editor_window`, `app_render.rs:3706`) builds the sidebar + inspector into ONE merged `ws.ui_root.tree` and issues one `render_tree_range`; the headless `render_graph_editor_to_png` built THREE separate scratch trees and self-asserted "same paint order" without proof — the exact drift this wave kills, in the last window. Peter chose **option A** (fix it for real, not accept a waiver — the design's D1 rules out a lookalike).

## What shipped

- **Editor seam extracted** into `crates/manifold-app/src/editor_frame.rs` (mirrors P1's `ui_frame.rs`), two behavior-preserving moves:
  - `build_editor_preview_column(...)` — the sidebar tree-building (was `app_render.rs:3449–3562`).
  - `composite_editor_frame(...)` — clear + `canvas.render` + `render_tree_range` + `dock.draw` + mini-timeline + overlays + `prepare`/`render` into `offscreen` (was `app_render.rs:3694–3751`). 4 documented signature deviations (e.g. `ui_renderer: Option<&mut UIRenderer>`, mirroring P1's `Option` cache) — the live-only inputs were already reduced to plain values before the block, so the cut is clean, no `Application` dependency, no escalation needed.
- **Lookalike killed:** `render_graph_editor_to_png` now builds the merged tree the live way and calls `composite_editor_frame` — the live editor and the headless editor render the IDENTICAL code.
- **P3 harness entry:** `editor_window_harness::node_the_fixture_places_renders_at_its_declared_screen_rect` — renders through the seam, reads the fixture node's declared screen rect from `GraphCanvasTargets`, asserts internal pixel-color variety (self-contained, no hardcoded theme color). The worker's first idea (center-pixel-≠-clear-color) would have false-passed AND was avoided — the dark theme's node fill ≈ canvas grid, so that check was unreliable; switched to internal-variance.

## Gates (verbatim, re-run by orchestrator)

- **Live inline block gone (behavior-preserving move):** `rg "render_tree_range\(&ws.ui_root.tree" app_render.rs` → 0 hits; live now calls `build_editor_preview_column` (app_render.rs:3453) + `composite_editor_frame` (:3601).
- **Lookalike gone:** `render_graph_editor_to_png` calls `composite_editor_frame` (render.rs:540); no scratch `tree`/`editor_ui.tree`.
- **Negative (D5):** `rg "UICacheManager"` in the editor path → 0 hits (editor is cacheless; never borrows the atlas model).
- **Structural test:** `editor_window_harness::node_the_fixture_places_renders_at_its_declared_screen_rect ... ok`; RED-verified by the worker (disable `canvas.render` → `is flat (1 distinct color)` → revert → green).
- **Editor render read by orchestrator (L2):** `target/ui-snapshots/editor/editor.png` — faithful full editor (node canvas with real nodes + wires, preview sidebar, Fluid Sim 2D inspector card lane, mini-timeline). Matches the live editor layout.
- **Tests / lint:** `cargo test -p manifold-app --features ui-snapshot` (167 passed); full `cargo test --workspace` green; `cargo clippy --workspace -- -D warnings` clean.

## Level reached

**L2** — headless render + structural assertion, read by the orchestrator. No live L4: the editor is authoring UI (not the show's per-frame output), and the extraction is a behavior-preserving move — Peter waived L4 for this window.

## Wave complete

Every render the harness touches — main window (P0/P1) and editor window (P3) — now goes through the app's real code, proven by construction (shared seams) and by read artifacts. The script Runner's parallel drift is deleted (P2). The design's thesis holds end to end: the harness cannot go green on a path the performer never runs.

## Open / deferred

- **BUG-097** (from P2, renumbered from BUG-094 to resolve a concurrent-session collision with fluidsim3d) remains: `render_ui_to_png`'s overlay pass traversal divergence — latent, tracked. (Note: P3's editor overlays go through the extracted seam now, so they are faithful; BUG-097 is the *main-window* `render_ui_to_png` overlay pass, a separate immediate-mode path.)
- Deferred (design §Deferred, unchanged): monitor/output windows, CI wiring, Retina 2x, perform-mode surfaces, playback-stepped capture.
