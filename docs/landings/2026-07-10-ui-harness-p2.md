# Landing — UI_HARNESS_UNIFICATION P2 (repoint runner at the seam + agent-legible captures)

**Date:** 2026-07-10 · **Phase:** P2 · **Branch:** `feat/ui-harness-p2` → `main` · **Orchestrator:** Opus (1M), Sonnet worker

## What shipped

The drift the whole design exists to kill is gone: the script Runner no longer keeps its own parallel copy of the redraw decision.

- **Runner repointed at the seam, parallel `rebuild()` deleted.** The Runner now owns a `RenderState` (persistent `UICacheManager` + composited offscreen) and drives every frame through `apply_ui_frame_invalidations` + `composite_main_ui_frame` — the identical functions the live app and `cache_path_full_render` use. One update+composite path, three callers.
- **`render_ui_to_png`'s panel pass repointed at the seam** (was a fresh `render_tree` per call — the exact full-repaint lookalike D1 kills). Its immediate-mode passes factored into a shared `draw_immediate_passes`.
- **D9 captures:** Runner-level filmstrip (a tile per stepped frame → contact sheet) and CPU-side pointer stamps (crosshair at each synthesized gesture point, drawn on the readback copy only, never a texture an assertion reads).
- **Consolidation** serving the "one path" goal: `composite_resources.rs` + `render.rs` `pub(super)` helpers now shared by `render_ui_to_png`, the Runner, and the P0 test (were privately duplicated); `sync_build` split into `sync_data` + `reconcile_state` so the Runner gates its rebuild through the seam.

## Gates (verbatim)

- **Negative gate (drift gone) — re-run by orchestrator:** `rg "needs_rebuild|invalidate_layers" script.rs` → 0 hits; `rg "fn rebuild\(|\.rebuild\(" script.rs` → 0 hits.
- **L3 — scripted flows drive the real input path, re-run by orchestrator:** `cargo run -p manifold-app --features ui-snapshot -- ui-snap timeline --script scripts/ui-flows/drag-clip.json` — all 7 steps `ok`, both `RectWithin` assertions pass (clip moves x=230→x=314 through the seam, pointer acted at the synthesized point). `select-and-inspect.json` exits 0 (worker).
- **Artifacts read by orchestrator:** the pointer-stamp demo (crosshair chain along the drag path on a faithful full timeline render) and the 12-tile drawer filmstrip — both faithful, captures working.
- **Tests / lint:** `cargo test -p manifold-app --features ui-snapshot` green (165 passed); full `cargo test --workspace` green; `cargo clippy --workspace -- -D warnings` clean.

## Level reached

**L3** — scripted click-flows drive the real input path through the shared seam; the orchestrator ran a flow and read the capture artifacts.

## Honest gaps / escalations

- **BUG-097 (logged, not fixed; renumbered from BUG-094 — concurrent-session ID collision with fluidsim3d):** `render_ui_to_png`'s overlay pass (Pass 5) uses `render_tree_range` where the live app uses `render_sub_region`, so an overlay range excluding its own region root can render nothing. **Pre-existing** (surfaced by P2's mandated VERIFY-AT-IMPL audit, not introduced here), **latent** (not reproduced against a failing scene; the two shipped flows and the drag demo all render overlays correctly), and outside P2's scope. Fixing needs a repro scene that doesn't exist yet — left tracked rather than fixed-by-guess.
- **BUG-073 → PARTIAL:** the per-frame tick mechanism now exists but is opt-in per script; existing flows weren't retrofitted.
- `Step`'s per-frame `thread::sleep(DT)` is real wall-clock (needed so clock-driven tweens advance); harmless to the two shipped gates, uncapped for future large-`Step` scripts — same tradeoff P0 accepted.

## What's owed

P3 — generalize the scaffolding to the cacheless graph-editor window with its own invariant (no atlas cache). BUG-097 (ex-094) as a follow-up if an overlay render bug is ever reported.
