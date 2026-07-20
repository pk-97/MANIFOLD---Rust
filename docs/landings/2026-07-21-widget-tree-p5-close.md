# 2026-07-21 — WIDGET_TREE P5 close + scene-convergence BUG-295 landing

Closes `docs/WIDGET_TREE_DESIGN.md` (P1–P5 complete) and lands the scene-convergence lane's final code: BUG-295's live manifest refresh.

## What landed (main, `d3ad7502` + `fbd017a5` lineage)

- **BUG-295 fix:** `PresetInstance::refresh_manifest_from_graph` (effects.rs) — the live twin of the load-path reconcile; value preservation via the same `ParamEntryWire` encode the file serializer uses. Wired into execute AND undo of all five scene-structural commands (AddSceneObject/AddSceneLight/AddSceneEnvironment/AddSceneFog/InsertMeshModifier) via `refresh_target_manifest` (commands/graph.rs). On stage: adding a fog/light/object/modifier live shows its rows immediately — no save+reload.
- **Acceptance (L3):** 3 fog flows re-pointed at the converged card rows and green (`scene-setup-add-fog-drag` 17/17, `scene-setup-fog-density-card-row` 12/12, `scene-setup-fog-undo-removes-fog` 17/17), driving add → rows appear live → drag → undo → rows vanish → redo. Two new value-level tests pin refresh + undo/redo restore and value preservation.
- **Gate at landing:** full workspace clippy clean, `cargo deny check bans` ok, `cargo nextest run --workspace` — 3846/3846 passed.

## P5 accounting (the count-match, BUG-252 rule)

All 15 `scene-setup-*` flow files on disk ran 2026-07-21: **14 green** (13 under `gltfscene`; `scene-setup-empty-states` under `timeline`), **1 blocked**: `scene-setup-modifier-stack` fails headless because the `--script` driver drops context-menu actions (BUG-293) → **VD-035**, deliberately not weakened or re-pointed.

Inspector surface: pinned by the untouched `undo_baseline`/`mapping_undo_baseline`/`bug_266_tab_pin` suites + the P4 `row_dispatch` harness families. The VD-034 card-drag L3 flow was **attempted and is blocked on new BUG-296**: the drag fires the real input path (`dispatched EffectReorder(0, 2) (structural=true)`, `inspector` fixture scene) but `advance_frame` never rebuilds the cached inspector cards after a structural dispatch, so no honest post-drag assert exists. VD-034 burn-down re-pointed at BUG-296.

Editor surface: L2 per VD-030 (no flow-driver routing to the editor window) — unchanged, still open.

Negative gate: `rg 'ParamCardConfig|match_param_row_click' crates/` → zero hits. Stale doc guidance fixed the same session (`HEADLESS_UI_HARNESS.md`, `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` supersession note).

## Open behind this close

- **BUG-296** (new): script driver's inspector cards never rebuild after a structural dispatch. Family: BUG-234/293/294 — one verification-infra lane should take all four.
- **Scene convergence P3/P4 remain** (outliner slimming; supersession sweep of the four prior scene design docs) — `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` status header is the truth.
- Widget-tree deferred items (dispatch context struct, `PanelAction` decomposition, unified scrub wire, scratch-buffer alloc) stay deferred with their named revival triggers; the god-file decomposition wave follows this design per the upgrade plan.
