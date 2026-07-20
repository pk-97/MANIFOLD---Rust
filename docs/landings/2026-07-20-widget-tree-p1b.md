# Landing — WIDGET_TREE P1b · 2026-07-20

**Merged:** `lane/widget-tree-p1b` (`f53ff1f0` types by Fable · `ecb109fb` swap by Sonnet lane · `8b5a33f8` doc status), `--no-ff`. 13 files, +925/−883.

## What landed

`ParamCardConfig` and `ParamInfo` are **deleted**. The card model is now `manifold_ui::param_surface::{ParamSurface, ParamRow, RowSpec, RowValue, RowMapping}` — id-keyed rows, derived badge aggregates (`has_drv()`/`has_env()`/`has_abl()` — three stored mirrors gone), `target()` derived from kind+index. One projection, `state_sync::param_surface()`, replaces `preset_to_config` + `rows_from_manifest`/`SpecRow`: a single manifest walk builds identity+spec+value+mapping per row; `build_card_modulation`/`build_audio_card_state` zip in unchanged. `id_to_index` is gone (local `row_index_of` closure inside the projection only). The §5b agent recipe ships as the module doc of `param_surface.rs`.

As-built deviations from the design §3 sketch (committed code is authoritative): no stored `target` field (method), `audio` remains `AudioCardState` on the surface (rows inside it, P1a shape) rather than absorbed per-row — interior-partitioning freedom the doc grants. `driven` is `false` outside the editor for now (wired when a caller knows).

## Gates (run by orchestrator)

- Worktree: `cargo nextest -p manifold-ui -p manifold-app` → `1173 passed, 3 skipped`; clippy `-D warnings` clean (independently re-run by orchestrator, same results).
- Main after merge: `cargo nextest run --workspace` → `3836 passed (10 slow), 13 skipped` · `cargo clippy --workspace -- -D warnings` clean · `cargo deny check bans` → ok.
- Flows: `select-and-inspect.json` (inspector) and `drag-clip.json` (timeline) both exit 0 through the swapped model — **L3**.
- Negative: `rg 'ParamCardConfig|\bParamInfo\b|id_to_index|rows_from_manifest|SpecRow'` over manifold-ui + manifold-app → 0; remaining repo-wide hits verified pre-existing unrelated substrings (`layer_id_to_index` in playback, a registry doc-comment in renderer).
- No test assertion values changed; only accessor paths where the type forced it (lane report, spot-verified).

**Level:** L3 (flows) over an interior model swap; behavior contract = the untouched 1173-test suite incl. `undo_baseline`.

## Click-script for Peter (≤1 min)

1. Open the inspector on a layer with effects; scrub a slider — value follows, one undo entry.
2. Check badges: a card with an armed driver shows DRV; with an Ableton mapping shows ABL (now derived live from rows, not stored flags).

## Notes

- `scene_setup_panel.rs` consumed `ParamInfo` and was mechanically re-pointed; it has its own unrelated `RowValue` type (scene addressing) — the lane qualified paths rather than renaming anything (correct; that file belongs to the convergence lane).
- The in-flight scene-convergence lane will hit rename conflicts (`preset_to_config`→`param_surface` etc.) when it next merges origin/main; the field map lives in this report and the P1b brief if that session needs it.
- `Shortcuts taken:` none (lane report + review).

**VD opened:** none. **VD carried:** VD-034 (card-drag flow → P5).
