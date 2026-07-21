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

## Wave 2 — model/command layer (design doc: not yet authored)

| File | Lines | Commits | Note |
|---|---|---|---|
| `manifold-editing/src/commands/graph.rs` | 9,538 | 80 | largest file in repo; every graph edit funnels through it |
| `manifold-core/src/effects.rs` | 5,594 | 104 | Unity-port sediment concentration |
| `manifold-core/src/project.rs` | 2,922 | 74 | assess at Wave 2 Phase 0 |

## Wave 3 — renderer runtime (design doc: not yet authored; cohesion judgment per file)

| File | Lines | Commits | Note |
|---|---|---|---|
| `manifold-renderer/src/node_graph/freeze/codegen.rs` | 6,374 | 106 | `FREEZE_COMPILER_MAP.md` stays authoritative |
| `manifold-renderer/src/preset_runtime.rs` | 8,175 | 63 | |
| `manifold-renderer/src/node_graph/gltf_import.rs` | 8,502 | 77 | importer — may be largely cohesive |

## Explicit non-targets (cohesive; do not split for size)

`render_scene.rs` (one primitive) · `freeze/proof.rs`, `freeze/region.rs` (single compiler stages) · `wgsl_compute.rs` (one primitive) · test files except where their code moves.

## Crate-promotion candidates (evidence gathered during waves; per-seam judgment, later)

- `projection/` (pure, one-directional) — record compile-time evidence at P-P landing.
- `frame/present` vs content pipeline boundary — assess after P-F.

## Enforcement

- Pure moves: `scripts/move_identity_check.py` (zero non-wiring residue).
- Regrowth: `godfile_regrowth` invariant test — per-file line ceilings for every file listed here (ceilings set at each wave's final landing; rides nextest).
