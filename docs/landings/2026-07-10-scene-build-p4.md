# Scene Build + Group Params — P4 landing

**Phase:** P4 — group boxes render their exposed param rows (D6) · **Level reached: L2** (faithful editor-window capture on the REAL glTF import, orchestrator-verified)
**Orchestrator:** Opus · **Worker:** Sonnet
**Base:** `de35da32` (P3) · merged `origin/main` `1721a943` (audio-dock P3a + build-speed infra).

## What shipped

- **Group-face param rows** — `graph_canvas/model.rs`: `build_group_param_rows` (recursive over `outer_routings`) + `group_param_summary` ("N params" chip); `interaction.rs`/`hit.rs`/`render.rs`: scrub/hit/render for group rows, expose glyph suppressed on group rows. A group box now shows a live slider for every card param whose binding targets a node inside it (transitively); collapsed → "N params" chip, expanded → the rows.
- **Command parity** — a group-row scrub emits `GraphEditCommand::SetOuterParam { outer_param_id, new_value }` (the canvas's own context-free vocabulary), which `app_render.rs` translates to the identical `PanelAction::ParamChanged(GraphParamTarget, ParamId, f32)` → `ChangeGraphParamCommand` the perform-card slider uses (via the same `ui_bridge::dispatch`). One value, three surfaces (card, group face, inner node). The `SetOuterParam` indirection is required, not optional: GRAPH_EDITOR_INSPECTOR_UNIFICATION "Change 3" decoupled the canvas from `watched_graph_target`, so the canvas can't safely construct the target itself — `app_render` resolves it explicitly.
- **BUG-103 (the payoff enabler, FIXED @ `9384d080`)** — `outer_routings_from_view` built its `node_id → handle` map from top-level `canonical_def.nodes` only, never recursing into group bodies. The glTF importer puts each object's material node *inside* its group, so 4 of 13 bindings (the per-object Metallic/Roughness) were silently dropped — group-face rows AND the "↳ outer" hint were inert for exactly the imported scenes the wave targets. New `collect_node_handles` recurses into `n.group` bodies (handles are unique per display def, so no false matches); the diverged arm (`content_thread.rs:1376`) reused the same helper (class fix). Real azalea import: 9/13 → 13/13 routings resolve.

## Orchestrator scope intervention

The worker initially shipped the mechanism and logged the pristine-import gap as out-of-scope (display+dispatch only). Correct at the phase level, but at the wave level it left the headline feature inert on real imports. I sent it back to complete the bounded snapshot fix. The worker's re-diagnosis corrected my prescribed fix (there are no per-instance bindings on a pristine import — the bug was the non-recursive handle map), used the escape hatch I gave it, and delivered the real root fix.

## Gate (orchestrator-verified on the merged tree)

- Full `cargo test --workspace`: 65 binaries ok, 0 failures. `cargo clippy --workspace -- -D warnings`: clean.
- New tests: `loaded_preset_view::tests::gltf_import_group_material_bindings_resolve_through_groups` (drives the REAL importer + resolver, asserts 13/13), plus manifold-ui canvas suite (rows appear, command-parity equality, wire-driven lockout, collapsed chip, nested-level rule).

## Demo (L2) — read on the REAL import, not the synthetic fixture

- `/tmp/scene-build-p4/gltfeditor-fixed.png` + `-crop.png` — the real azalea import: both object group boxes ("QS1694-W02-1-1" red, "Material.001" blue) carry Metallic/Roughness slider rows on the group face, matching the sectioned card lane. Orchestrator read both; pre-fix crop showed interface ports only.
- `groupdemo-expanded.png` / `groupdemo-collapsed.png` — synthetic routing fixture: rows expanded, "2 params" chip collapsed.

## Merge-resolution fixes (greened main, not P4 defects)

Integrating origin/main surfaced two concurrent-landing breakages that scoped gates let slip onto main:
- **dispatch arg count** — audio-dock P3a cut `ui_bridge::dispatch` 19→18 args (removed `audio_send_sensitivity_drag_snapshot`); P4's new call site inherited the old 19-arg form (semantic merge conflict). Fixed.
- **design-token ratchet** — P3a's Triggers-matrix deletion removed ~14 `Color32::new` literals, dropping the count to 187 while baseline stayed 201; **origin/main was red on `design_tokens`**. Ratcheted baseline 201→187 per its own rule.
- (Earlier this session I also hotfixed a separate P3a breakage — `AudioSendRow.triggers`→`has_clip_triggers` in two ui_root.rs test fixtures, `3fbbac70` — that broke the default manifold-app test compile on main.)

## Process note

Re-ran the FULL sweep after each fix rather than assuming isolation — the docs-index failure was masking the design-token failure behind cargo's fail-fast short-circuit (the same P1 trap). Two fixes deep before the tree was actually green.

## Owed / unverified

- The `app_render` `SetOuterParam`→`ParamChanged` translation is value-tested at the canvas boundary but not separately unit-tested in `app_render` (deterministic reuse of the card's own `watched_graph_target` path; verified end-to-end by the real-import render). Minor coverage gap.
