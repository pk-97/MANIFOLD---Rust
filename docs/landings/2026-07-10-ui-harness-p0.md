# Landing — UI_HARNESS_UNIFICATION P0 (faithful full-app headless render)

**Date:** 2026-07-10 · **Phase:** P0 · **Branch:** `feat/ui-harness-p0` → `main` · **Orchestrator:** Opus (1M), Sonnet worker

## What shipped

The reframed P0 (see the design doc's "Reframe 2026-07-10" block): a headless test that renders the **whole main window through the real `UICacheManager` + `UIRenderer` + atlas** — the same code the live app runs — and saves inspectable artifacts. Not a differential, not a regression gate; a faithful-render-and-look tool.

- New test module `cache_path_full_render` (`crates/manifold-app/src/ui_snapshot/mod.rs`), feature `ui-snapshot`. Constructs a real `UICacheManager` (`set_scale_factor(1.0)` + `ensure_atlas` at the fixture's logical size 1536×1216), drives the §5 gesture sequence (scroll · drawer tween · tab swap) through the real input paths, and saves a full-app PNG + a drawer-tween filmstrip contact sheet (D9a). Smoke check only: readback non-empty and not a uniform clear colour.
- Heavy synthetic fixture `bug060heavy` (Plasma generator + 3× Color Compass + Color Grade + Depth of Field, audio/LFO modulation). No BUG-060-specific tuning; a realistically heavy scene for general coverage.
- **BUG-071 dump fix** (D9c): `dump.rs` serializes the live `tree.parent_of(n.id)`, not the mint-time `parent_id`. Backlog entry closed.
- Retired the old differential entirely — deleted the `cache_path_footer_differential` and `incremental_path_modulation_differential` byte-equality tests (D2/D4a retired by the reframe).
- Feature-clippy debt cleared: deleted the unused `make_blit_pipeline` (BUG-057/067, duplicates) and dropped two redundant `as i32` casts (BUG-093). All three closed.

## Gates (verbatim)

- **Render gate (the acceptance gate) — L2, orchestrator read the artifacts:** the full-app PNG shows the complete real main window — transport bar, timeline with the selected PLASMA track, and the full inspector effect stack with audio/LFO badges drawing. A faithful render of the real path. The filmstrip is 5 real composited frames tiled into one sheet (drawer motion is subtle at fixture zoom — flagged for a more legible capture when P2 adds richer capture, not a P0 blocker).
  - `…/crates/manifold-app/target/ui-snapshots/bug060heavy/full_render/frame.png` (1536×1216)
  - `…/crates/manifold-app/target/ui-snapshots/bug060heavy/full_render/drawer_filmstrip.png` (5 tiles)
- **Focused test:** `test ui_snapshot::cache_path_full_render::cache_path_full_render ... ok` — `1 passed; 0 failed; 166 filtered out`.
- **Negative gate (D1):** `rg "render_ui_to_png|traverse_flat_range"` scoped to the new module → zero hits. The render goes through the real cache, not a lookalike.
- **Clippy:** `cargo clippy -p manifold-app --features ui-snapshot -- -D warnings` → clean (the only output is Objective-C deprecation warnings from the `manifold-media` build script, not Rust lints).

## Level reached

**L2** — a headless render whose artifact the orchestrator looked at and compared against the real app. No user click-script for P0 (headless, no live-app interaction). The live-app L4 gate arrives at P1, which moves the show's per-frame render path.

## Shortcuts / honest gaps

- The full-app PNG reads the atlas texture directly rather than replaying `present_all_windows`'s clear + full-atlas-blit + video-band composite. Justified: video is `None` in the harness and the atlas blit is fullscreen 1:1, so the atlas readback equals the offscreen minus the (absent) video band. **P1 must verify this equivalence against the real `composite_main_ui_frame` once that seam exists.**
- Filmstrip drawer motion is subtle at 1× fixture zoom — the mechanism is proven; a more obviously-animated capture is a P2 polish item.
- D8 gaps unchanged and known: 1× only (no Retina), video band absent, stepped-clock time.

## What's owed

P1 (seam extraction, byte-identical gate, live-app L4 click-script — pause for Peter), P2 (repoint the script Runner at the seam + pointer stamps + richer filmstrip), P3 (editor window). Anchor caution: the 2026-07-10 class-kill refactored the renderer flush machinery — P1–P3 re-derive their §Audit/§3/§4 anchors against current main.
