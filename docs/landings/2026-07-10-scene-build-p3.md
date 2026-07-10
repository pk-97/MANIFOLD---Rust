# Scene Build + Group Params — P3 landing

**Phase:** P3 — card sections from group names · **Level reached: L3** (faithful-render capture + field-level fold proof, orchestrator-verified on the post-harness-seam tree)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `5c0f86f2` (P2) · merged `origin/main` `c4ae2d4c` (harness-fidelity seam + verbs) into the branch before landing.

## What shipped

- **Expose-time seeding** — `manifold-editing/src/commands/graph.rs`: `innermost_group_display_name` (:1746) resolves the group name from `scope_path`; `ToggleNodeParamExposeCommand::execute` threads it through `mirror_effect_side`'s new `inner_section` into `UserParamBinding.section` (new field, `manifold-core/src/effects.rs:98`). Top-level (empty scope) → `None`. Static-slot toggles never touch section.
- **Rename-sweep** — `RenameGroupCommand` (graph.rs:2017) gained `swept: Vec<(String, Option<String>)>`; on rename it rewrites `section` on every spec whose value equals the old name AND whose binding target resolves inside the renamed subtree (`collect_node_ids` at :1762); `undo` restores each. Hand-edited sections (different string) untouched.
- **Card header + fold** — `manifold-ui/src/panels/param_card.rs`: `section_runs` (:2151) groups contiguous same-section rows; `build_section_header` (:2174) draws the clickable header (fold triangle + name + row-count chip when folded); `PanelAction::SectionFoldToggled` triggers a pure rebuild (no model write). Fold state is UI-local workspace state — no second home in the model.
- **Mapping editor** — `BindingMappingEdit.section` (`manifold-editing/src/commands/effects.rs:754`); `EditParamMappingCommand` writes it to the **manifest spec only** (BOUNDARIES D4). The popover text-input widget itself is deferred (BUG-101; the command-side write is real + tested).
- **Root-cause fix (found, not asked):** `ParamDef.section` (`manifold-core/src/effects.rs:83`) — the registry-facing twin of `ParamSpecDef` was missing the `section` mirror, so every glTF-imported generator (which tracks its embedded preset via the registry, `graph: None`) would silently lose every seeded section on catalog round-trip. Threaded through `to_spec`/`param_def_from_spec`/`param_spec_def_to_param_def`. Without this, P3 does nothing for the exact case it exists to serve.

## Gate (orchestrator-verified on the merged tree)

- Full `cargo test --workspace`: 65 test binaries ok, 0 failures. `cargo clippy --workspace -- -D warnings`: clean.
- New tests: manifold-core +2 (serde skip + round-trip), manifold-editing +7 (expose/section, rename-sweep both directions, JSON round-trip, mapping write), manifold-ui +3 (section_runs, header render, fold-skips-rows).
- Serde guard: a no-section spec re-serializes without a `section` key.
- **Negative gate**: `state_sync.rs:2002` reads `section: p.spec.section.clone()` — manifest spec only, no graph-def read. Confirmed by inspection.

## Demo (L3) — faithful-render capture, verified through the post-seam path

Re-ran the worker's `gltfscene` scene + `scripts/ui-flows/gltf-import-card-sections.json` on the MERGED tree (post harness-fidelity seam). Real import path (`assemble_import_graph` + `ImportModelLayerCommand` on the azalea CC0 scan). All 7 flow steps ok.
- `/tmp/scene-build-p3/gltfscene-unfolded.png` — sectioned card: real glTF material names `QS1694-W02-1-1` and `Material.001` as headers with their own Metallic/Roughness rows, plus `Camera`/`Sun`/`Environment` blocks. Orchestrator read it.
- `/tmp/scene-build-p3/gltfscene-folded.png` — after clicking QS1694's header: its rows gone, header shows `(2)`, others unaffected. Orchestrator read it.
- **Field-level fold proof** (tree dumps 01 vs 06): before fold — 2 Metallic + 2 Roughness rows, both headers present; after folding QS1694 — 1 Metallic + 1 Roughness, both headers still present. The fold removed exactly QS1694's two child rows and nothing else.

## First-consumer harness finding

`cargo xtask ui-snap diff <a.tree.json> <b.tree.json>` is dominated by the volatile per-node `gen` (generation) counter: any fold/toggle triggers a full rebuild that bumps every node's `gen`, so the diff reports all ~hundreds of nodes as changed and buries the real structural delta (rows added/removed). It is currently unusable for the most common UI-verification question — "what changed across an interaction that rebuilds." Logged in `docs/UI_HARNESS_UNIFICATION_DESIGN.md` Deferred/gaps: `diff` should skip `gen` (and any pure rebuild-bookkeeping fields) by default, or offer `--ignore-gen`. Worked around here by counting labels in each dump directly.

## Shortcuts / owed

- BUG-101 (mapping-popover section text field not wired — label editing was already deferred for the same reason) logged, command-side write is real + tested.
- Fold-state persistence across restart: Deferred per §9, unchanged.
