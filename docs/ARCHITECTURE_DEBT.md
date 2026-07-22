# Architecture Debt Register — god-file campaign inventory

**This file is inventory and pointers, never status.** Wave status lives on each wave design doc's `**Status:**` line (the design-status board watches those). Baseline sizes/churn measured 2026-07-21; re-verify fresh at each wave's Phase 0 (`wc -l`, `git log --since --name-only`).

Campaign origin: Peter, 2026-07-21 — *"I want god files gone, I want proper software architecture, designs, and boundaries."* Operating plan discussion + adversarial review same day. Godhood = size × churn × mixed concerns; cohesive large files are NOT targets.

## Layer vocabulary (fixed, campaign-wide — Wave 1 doc D2)

| Layer | Owns | Never does |
|---|---|---|
| **Projection** | snapshot → view-model, per domain, dirty-checked | send commands, build tree nodes |
| **Surface** | manifests/VMs → widget tree (the widget-tree layer) | resolve targets, mutate |
| **Routing** | gesture → typed intent (per-domain enums, `RowIndex`) | touch `Project` |
| **Bridge** | intent → `ContentCommand`/`EditingService` | build UI |
| **Frame** | drain → events → sync → push → present orchestration | contain domain logic |
| **Geometry** | laid tree bounds only (widget-tree D6) | snapshots/caches |

## Wave 1 — UI/app funnel (design: `docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md`)

| File | Lines (2026-07-21) | Commits since May | Target |
|---|---|---|---|
| `manifold-app/src/ui_bridge/inspector.rs` | 6,012 | 124 | `dispatch/` per-domain handlers + tests move along |
| `manifold-app/src/ui_bridge/state_sync.rs` | 4,177 | 158 | `projection/` per-domain modules |
| `manifold-app/src/ui_bridge/mod.rs` | 1,255 | 115 | thin entry + `context.rs` + `scrub.rs` |
| `manifold-app/src/app_render.rs` | 6,571 | 261+ | `frame/` stages + `editor_bridge.rs` |
| `manifold-app/src/app.rs` | 3,687 | 155 | slimmed struct; scrub fields → `ScrubState`; WGSL out |
| `manifold-app/src/ui_root.rs` | 4,080 | 104 | panel wiring / overlay+drag / dropdown builders |
| `manifold-ui/src/panels/param_card.rs` | 6,946 | 118 | migrate bespoke infra → `param_surface`; split renderer/routing/state |
| `manifold-ui/src/panels/scene_setup_panel.rs` | 3,584 | — | same layer lines (P-S) |
| `manifold-ui/src/panels/inspector.rs` | 4,231 | 81 | same (P-S) |
| `manifold-ui/src/panels/param_slider_shared.rs` | 3,166 | 92 | same (P-S) |
| `manifold-ui/src/panels/mod.rs` (`PanelAction`, 303 variants) | — | 131 | per-domain intent enums under one wire type |

## Wave 2 — model/command layer (design: `docs/MODEL_COMMAND_DECOMPOSITION_DESIGN.md`)

Layer vocabulary for this crate pair (design D2): **Model** (type + impl + serde, per domain) · **Migration** (load-time normalization) · **Query** (read-only traversal) · **Command** (mutation + undo, per domain).

| File | Was (2026-07-21) | Now (P2-G/E/P landed 2026-07-22) |
|---|---|---|
| `manifold-editing/src/commands/graph.rs` | 9,538 lines, 80 commits | `commands/graph/` — 8 modules, facade at mod.rs |
| `manifold-core/src/effects.rs` | 5,594 lines, 104 commits | `effects/` — 11 modules |
| `manifold-core/src/project.rs` | 2,922 lines, 74 commits | `project/` — 6 modules |

Non-target (assessed at Wave 2, cohesive): `manifold-editing/src/service.rs` (1,541 — clip/selection orchestration) and `undo.rs` (159).

## Wave 3 — renderer runtime (design: `docs/RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md`; cohesion verdicts recorded there)

**COMPLETE 2026-07-22** — status detail lives ONLY on the design doc's Status line; the rows below point to the landed directories.

| Was (single file, pre-Wave-3 lines) | Now | Note |
|---|---|---|
| `node_graph/freeze/codegen.rs` (6,374) | `node_graph/freeze/codegen/` (mod/types/uniforms/entry_points/standalone/fused + tests) | `FREEZE_COMPILER_MAP.md` §2 stays authoritative for module sizes; D2 named ceiling `fused.rs` |
| `preset_runtime.rs` (8,175) | `preset_runtime/` (mod/core/build/errors/segments/bindings/instrumentation + `tests/` #[path] modules) | D3 named ceiling `core.rs` (~2k, Peter-sanctioned); `PresetRuntime` stays one type |
| `node_graph/gltf_import.rs` (8,502) | `node_graph/gltf_import/` (mod/assembly/animation/materials/cards/object_group/scene/merge/report + tests) split by feature; P3-D tabled `MAP_FAMILIES`/`MATERIAL_PARAMS`; P3-A `ImportCtx`/`ObjectAssembly` | importer cohesive — split, not redesigned |

**Campaign house rules (D7, adopted Wave 3 P3-Z as register policy):** tests live in-file until ~500 lines of test code, then move to a sibling `tests.rs` (single mod) or a `tests/` directory-module (multiple mods, `#[path]` declarations); code files target ~1.5k lines, and crossing that needs a *named* ceiling with rationale in the owning design doc, pinned by `godfile_regrowth`.

## Named design items (from the Wave-1 close audit, 2026-07-22 — each is a Peter-in-the-room design session, scheduled work with an owner, never a revival trigger)

| Item | Scope | Origin |
|---|---|---|
| **CHROME_PARAMS** | App-level built-in performance params (master opacity, layer gain, crossovers) declared via a small static manifest, hosted on ParamSurface; exposure/automation/perform-surface semantics | D-39 (Peter: chrome hard-coding is an accident of history) |
| **GESTURE_ENTRY** | Unify the frame-resident gestures (editor mapping range/affine, node-face drags) onto one designed gesture entry path; kills VD-037's two-entry-point risk class by construction | P-I Fork-2 compromise |
| **ROW_MODEL_EDGES** | Decide end-states for relight rows outside RowHost (historical special-casing) and RowHost's params-by-reference routing surface. RowHost's `row_action` wide-arg surface shares the bloated-call disease class Wave 3 P3-A fixed with context structs (`ImportCtx`, `ObjectAssembly`, `ChainBuildInputs`, `StandaloneKernelSpec`, `FrameContextInputs` — plain structs bundling params that travel as one fact); that is the precedent when this item is designed | P-S2/P-S3 seam compromises |
| **VERIFICATION_INFRA (priority-one prerequisite for any future UI wave)** | The flow-driver blind-spot family (BUG-234/293/294/296/300) behind all 7 known-red flows — the oracle every UI wave leans on | Wave-1 close audit |

## Explicit non-targets (cohesive; do not split for size)

`render_scene.rs` (one primitive) · `freeze/proof.rs`, `freeze/region.rs` (single compiler stages) · `wgsl_compute.rs` (one primitive) · test files except where their code moves.

## Crate-promotion candidates (evidence gathered during waves; per-seam judgment, later)

- `projection/` (pure, one-directional) — record compile-time evidence at P-P landing.
- `frame/present` vs content pipeline boundary — assess after P-F.

## Enforcement

- Pure moves: `scripts/move_identity_check.py` (zero non-wiring residue; test-mod wiring class added Wave 2 D7a).
- Regrowth: `crates/manifold-app/tests/godfile_regrowth.rs` — per-file line ceilings, rides nextest (BUILT at Wave 2 P2-Z, 2026-07-22 — Wave 1 D11 specified it but never built it; Wave-1 rows carry interim ceilings that tighten at Wave 1 close).
