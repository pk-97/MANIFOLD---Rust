# Landing — UI_HARNESS_UNIFICATION P1 (shared render-seam extraction)

**Date:** 2026-07-10 · **Phase:** P1 · **Branch:** `feat/ui-harness-p1` → `main` · **Orchestrator:** Opus (1M), Sonnet worker

## What shipped

The two seam functions (D3) extracted into a new App-internal module `crates/manifold-app/src/ui_frame.rs`, so the live app and the headless harness render through **identical** code:

- `apply_ui_frame_invalidations(ui_root, cache, signals)` — owns the invalidate/rebuild decision block (the scroll-in-place `invalidate_inspector` + the rebuild/structural + `invalidate_all` + `invalidate_scroll_panels` block). Moved out of `tick_and_render`, replaced by one call at `app_render.rs:2832`.
- `composite_main_ui_frame(...)` — owns dirty-panel atlas render + clear + full-atlas blit + optional video-band blit. Moved out of `present_all_windows`, replaced by one call at `app_render.rs:4023`. The fast path, drawable acquire/present stay in `present_all_windows`.
- The P0 harness (`cache_path_full_render`) now renders its frame by calling `composite_main_ui_frame` — same code as the live app.

Five forced signature deviations from the §4 sketch, all documented in the `ui_frame.rs` module doc and the design doc's §4 AS-BUILT note, each argued behavior-preserving: `Option<&mut cache>` (pre-GPU-init), `&mut ui_root` (real mutations), the App-owned pipelines/samplers/scale/video-dims threaded as params (hot-path discipline — not recreated per frame), one-shot `scrolled_in_place` clear, and `render_dirty_panels` moving to the non-fast-path branch.

## Gates (verbatim)

- **Byte-identical (the core proof) — reproduced independently by the orchestrator, not just the worker:** `frame.png` sha256 = `5fe4ff45779082d1192fc58ff0d82129aaba2009a281668cff85fd7ec70490b3` from THREE renders: the worker's before/after, the pre-extraction path (`a56f641a`, direct atlas readback), and the post-extraction path (offscreen via `composite_main_ui_frame`). Identical pixels across two genuinely different code paths.
- **No new shared state:** `git diff` over `crates/manifold-app/src` shows no `Arc::new`/`Mutex`/`RwLock` added (CLAUDE.md hard rule).
- **Inline blocks gone (not duplicated):** `rg "cm.invalidate_all\(\)"` in `app_render.rs` → the only site is inside the call to `apply_ui_frame_invalidations`; `"Atlas Blit"`/`"Blit Compositor"` labels → 0 in `app_render.rs`, moved to `ui_frame.rs`.
- **Tests:** `cache_path_full_render` passes; `cargo test -p manifold-app` green; `cargo test -p manifold-renderer --features gpu-proofs` green (1253 + gpu_proofs binary 5); full `cargo test --workspace` green; `cargo clippy --workspace -- -D warnings` clean.
- **L4 — live app, Peter:** built from `feat/ui-harness-p1`, driven through idle (fast path), inspector scroll + settle, Layer⇄Master tab, drawer expand/collapse, clip playback. **"P1 good"** — no stale chrome, flicker, or missing repaint. This is the gate the headless proof cannot reach (the real fast path + frame pacing).

## Level reached

**L4** — the live app, driven by a human, confirming the show's per-frame render path still draws correctly after the extraction.

## Honest gaps

- `MANIFOLD_RENDER_TRACE=1` produced no output from the headless unit test — expected: that var gates the content-thread trace, and the harness never builds a live `Application`. Frame-cost preservation is covered structurally (the move adds no heap allocation) and by the L4 (Peter saw no pacing regression).
- The P0 atlas-readback vs. P1 offscreen-composite equivalence (the P0 shortcut) is now positively confirmed: both hash identically, so the offscreen composite equals the atlas readback for this fixture (video `None`, fullscreen 1:1 blit).

## What's owed

P2 (repoint the script Runner at the seam, delete its parallel rebuild logic, ship pointer stamps + richer filmstrip), P3 (editor window). Anchor caution stands: re-derive against current main.
