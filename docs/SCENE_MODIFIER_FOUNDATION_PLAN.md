# Scene modifier foundation — implementation plan

<!-- index: Eight bounded phases for v3 modifier presets, expansion, bindings, legacy migration, cards, first wave and release proof. -->

**Status:** PROPOSED · 2026-09-10 · Codex lead · no phases implemented.
**Prerequisites:** [Preset Architecture](SCENE_MODIFIER_PRESET_ARCHITECTURE.md); existing graph/preset/editor infrastructure.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8. Architecture decisions A/D references below refer to the companion architecture; test policy is [Validation](SCENE_MODIFIER_VALIDATION_PLAN.md).

The first release makes one complete journey work: scene → file preset → performance gesture → graph edit → save/reload. F1–F6 build the support; F7 adds the first mathematical look; F8 proves the complete journey. Do not begin later geometry work to avoid an unresolved foundation seam.

## 1. Audit and re-derivation

Verified at `28c8486f0`, 2026-09-10. The architecture audit owns data definitions. This inventory owns migration call sites.

```sh
rg -n 'scene_modifier::(descriptors|descriptor_for|build_plan|trace_modifier)' crates/manifold-app/src crates/manifold-renderer/src
rg -n 'ApplySceneModifier|RemoveSceneModifier|SceneModifierApply|SceneModifierRemove|SceneModifierToggle' crates
rg -n 'flatten_groups|fused_generator_def_for|into_graph' crates/manifold-renderer/src/node_graph crates/manifold-renderer/src/generators
rg -n 'PresetKind::|BindingTarget::|GraphTarget::' crates
rg -n 'SceneModifierDescriptorEntry|reflect_array|scene_mirror' crates/manifold-renderer/src docs/SCENE_MIRROR_DESIGN.md
```

The first command produces eight qualified occurrences at this audit (imports included): `ui_bridge/projection/cards.rs:480`; `ui_bridge/project.rs:432,1460,1477,1478`; `scene_vm.rs:547,551`; `corridor_acceptance.rs:144`. Additional unqualified calls live inside `scene_modifier.rs` and `cards.rs`; compiler-driven removal catches those. Do not count the primitive palette's unrelated `descriptor_for` in `preview_encoding.rs` as a scene modifier lookup.

| Surface | Before | Migration category |
|---|---|---|
| `manifold-app/src/ui_bridge/project.rs:379` | Plan-driven apply | New instance creation and one graph snapshot command |
| Same file `:398` | Re-derived plan removal | Remove by instance ID; no descriptor |
| Same file `:432` | Kind-specific enable resolution | Host manifest param from recipe `enabledParam` |
| Same file `:1460` | `build_plan(kind_id, def, u32)` | Catalog recipe validation + insertion |
| `manifold-renderer/src/node_graph/scene_vm.rs:547` | Iterate kinds and trace presence | Iterate canonical instances and resolved applicability |
| `manifold-app/src/ui_bridge/projection/cards.rs:480` | Registry rows and kind labels | Preset catalog + stable instance identity |
| `manifold-ui/src/panels/actions.rs:287` | Kind-based actions | Apply by preset ID; remove/reorder by instance ID |
| `manifold-app/src/ui_root/dropdowns.rs:505` | Kind apply dropdown | SceneModifier catalog filter |
| `manifold-app/src/corridor_acceptance.rs:144` | Legacy trace | Fixture migrated to canonical instance + expansion |
| `manifold-core/src/graph_target.rs:33` | Effect/Generator graph targets | Nested scene-modifier authoring target |

Before F1, capture the full enum call-site inventories from the commands above in the phase brief. Before F3/F5, re-run and compare; changed or newly discovered sites return to the lead for an updated seam brief. The unbounded enum sweep is deliberately re-derived instead of freezing hundreds of unrelated matches as a false completeness claim.

## 2. Decisions and seam contracts

**D1 — Build in dependency order.** F1 → F2 → F3 → F4 → F5 → F6 → F7 → F8. F1 adds support without changing old projects. Transitional old code is permitted only through F5, and F6's deletion gate closes it. No permanent dual runtime.

**D2 — The canonical graph stays the editing authority.** Old signatures:

```rust
build_plan(kind_id: &str, def: &EffectGraphDef, render_scene_node_id: u32)
    -> Option<SceneModifierPlan>;
// GraphTarget = Effect(EffectId) | Generator(LayerId)
```

Replacement renderer seams in `node_graph/scene_modifier_expand.rs`:

```rust
pub fn validate_modifier_attachment(
    owner: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
    registry: &PrimitiveRegistry,
) -> Result<(), SceneModifierExpandError>;
pub fn expand_scene_modifiers(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<EffectGraphDef, SceneModifierExpandError>;
```

Replacement authoring address in core `graph_target.rs`:

```rust
// Additional GraphTarget variant:
SceneModifier { owner: Box<GraphTarget>, modifier_id: NodeId }
```

Only `owner = Generator(layer_id)` is accepted in v1. Nested SceneModifier owners and Effect owners fail target resolution. `preset_kind()` returns SceneModifier for this variant. Existing Effect/Generator variants retain their on-disk representation. Resolve the modifier snapshot in one shared graph-target resolver, not independent implementations in editor/export/fork.

New editing commands in `commands/graph/scene_modifier.rs` use existing Command/EditingService and graph snapshot undo:

```rust
pub struct InsertSceneModifierCommand {
    // private undo state; constructor is the public seam
}
impl InsertSceneModifierCommand {
    pub fn new(owner: GraphTarget, index: usize,
               instance: SceneModifierInstanceDef) -> Self;
}
pub struct DeleteSceneModifierCommand { /* private undo state */ }
impl DeleteSceneModifierCommand {
    pub fn new(owner: GraphTarget, modifier_id: NodeId) -> Self;
}
pub struct MoveSceneModifierCommand { /* private undo state */ }
impl MoveSceneModifierCommand {
    pub fn new(owner: GraphTarget, modifier_id: NodeId, index: usize) -> Self;
}
pub struct RetargetSceneModifierCommand { /* private undo state */ }
impl RetargetSceneModifierCommand {
    pub fn new(owner: GraphTarget, modifier_id: NodeId,
               targets: SceneTargetSelection) -> Self;
}
```

`index` is the final zero-based position after removal for Move; out-of-range is an error, never clamped. Renderer validation happens during preparation before dispatch. Editing rechecks structural identity and source-order invariants from core data before mutation, so a stale UI plan cannot modify a different instance. Error transport reuses existing Command results; no renderer dependency in editing. Invalid commands have no undo entry and change no state.

**D3 — Generic defaults replace bespoke Loop callbacks.** Architecture section 3 defines bounded `SceneScalarExpr` initializers and calibrations for apply-time bounds-derived values/ranges. Implement them in F1/F2 and author their declarations in F6. Shared live values use ordinary bindings and graph arithmetic, never coupled UI writes. No per-preset callback survives.

**D4 — Keep source semantics honest.** Loop/Fog remain source presets with their existing restrictions and off behaviour; Wave is the first fully chainable identity-bypass modifier. General camera controllers, atmosphere blending and replication of an existing instance set are later extensions.

## 3. Phasing and common execution rules

Each phase begins by reading its entry files and restating binding decisions, forbidden shortcuts and inventory results. Acquire one slot through `scripts/agent-worktree.py`, verify its base, and follow existing build-lock/landing mechanics. No app builds are needed for this documentation delivery.

Commands below run from the phase worktree with an explicit absolute `--manifest-path "$MODIFIER_WORKTREE/Cargo.toml"` on Cargo commands. `MODIFIER_WORKTREE` is the assigned slot path, never an assumed shared checkout. GPU tests use cargo test through `gpu_proofs_gate.py`. Use focused tests once after edits; at most two attempts per exact command. The landing script preserves required touched-crate gates. New tests and flows in these briefs must be created before their commands can pass; zero selected tests is a failed gate.

## 4. F1 — core schema and lossless files

**Entry:** baseline audit commands; read architecture sections 2–4, core graph/preset/project types, `preset_file.rs` and version validation. No dependency on F2.

**Deliverables:** architecture structs; graph v3 support; SceneModifier preset kind and metadata; BindingTarget variant; GraphTarget variant with typed errors at unresolved new targets. Update all struct constructors compiler-first. File parsing and project embedding accept and preserve v3; recursive/unknown recipe versions reject. Add `scene_modifier_v3_roundtrip` and invalid-schema fixtures, including a held-out independently authored JSON preset. New enum paths may return explicit unsupported-execution errors until F2/F3; never masquerade as Generator.

**Gate:** `cargo test -p manifold-core -p manifold-io scene_modifier_v3` and focused clippy for core/io with `--tests -- -D warnings`; serialize → reload → equal canonical definition. Negative: no new `Arc<Mutex`/`Arc<RwLock` in touched code. A1/A7 tests are deliverables. **Demo:** none — L1. **Scope fence:** data/file support only; no picker or GPU work.

## 5. F2 — typed expansion

**Entry:** F1 tests; read scene_object, graph_loader, bound_graph, freeze/install, generator registry. Re-derive all preparation entrances.

**Deliverables:** expansion and applicability functions; stable identity allocation; stage routing; reference/current distinction; admission limits; expanded binding fan-out. Use one shared preparation function at entrances that currently flatten directly: `graph_loader.rs:771`, `bound_graph.rs:290,495`, and pre-fusion generator entry (`generators/registry.rs:267`). Follow callers for viewport, thumbnail, warmup and export. Add A3/A4/A5/A10 tests, including two noncommuting stages, renamed groups, duplicate labels, reordered numeric IDs and explicit missing targets. Add neutral CPU fixture nodes for structural tests; do not introduce production test-only fallbacks.

**Gate:** `cargo test -p manifold-renderer scene_modifier_expand` and renderer clippy; `scene_modifier_preparation_parity` explicitly exercises fused preparation, unfused preparation and preview resolution. Negative: expanded graphs contain no `sceneModifiers` and no modifier-only BindingTarget. These are parsed assertions, not a source-string guess. **Demo:** none — L1; geometry operation not introduced yet. **Forbidden:** new Object producer/consumer chain, direct RT writes, per-frame expansion.

## 6. F3 — commands and parameter identity

**Entry:** F2; read GraphTarget resolution, existing graph snapshot commands, host manifest/mapping pruning and param bindings. Inventory all new enum matches.

**Deliverables:** four commands in section 2, shared nested graph-target resolver, stable host macro addresses, fan-out through the existing runtime binding path. Add insert/delete/move/retarget undo/redo tests and mapping restoration after reload. Two instances of the same preset with identical titles must remain independent. Changing selection recompiles without changing surviving target/node IDs. Delete-object command checks references as specified in architecture section 6.

**Gate:** `cargo test -p manifold-editing scene_modifier` plus `cargo test -p manifold-renderer scene_modifier_binding`; touched-crate clippy. A2/A3/A7 tests mandatory. Negative: removal implementation has zero comparisons against display-name/section strings for ownership. **Demo:** none — L1. **Forbidden:** add new modulation vectors, synthesize unstable parameter indices, silently clamp Move index.

## 7. F4 — legacy migration

**Entry:** F3; read all Loop migrations, actual descriptor registrations and saved fixtures. Resolve Mirror's header/source discrepancy through code/history; do not change its implementation during this phase.

**Deliverables:** load-only `migrate_legacy_scene_modifiers` after existing loop migrations; known-shape extraction; stable control remap; preserve partial/custom graphs with diagnostics. Keep current Loop phase/cell/camera values and Fog neutral defaults. Maintain fixture copies of old v1/v2 documents before changing any fixture. Migration produces the same result twice and round-trips. New custom held-out legacy fixture exercises non-default bindings, renamed handles and unrelated atmosphere/camera wiring.

**Gate:** `cargo test -p manifold-renderer --test scene_modifier_legacy_migration`; old Loop structural/wrap/roundtrip tests selected by exact target names from current Cargo inventory; focused GPU filter `scene_modifier_legacy` for old versus new evaluated geometry and frames. Negative: new apply path never calls legacy trace; partial signatures remain byte-preserved in the test. A6 mandatory. **Demo:** saved comparison frames — L2 artifact, human review separately recorded. **Forbidden:** restore by first type match, drop unrecognised nodes, infer absent takeover history.

## 8. F5 — cards, browser and graph editor

**Entry:** F3/F4; read `param_surface.rs`, `ui_bridge/projection/cards.rs`, `ui_bridge/project.rs`, dropdowns and shared graph-editor target resolution.

**Deliverables:** SceneModifier catalog filtering, applied instance cards in actual stack order, add/remove/reorder/selection affordances, Enabled binding, Open Graph to local snapshot, Save As/import/export through existing preset transport. Explicit source-order errors visible before apply. Calibrated-default editing follows architecture section 3: atomically remove that default's initializer/calibration, store the explicit value, and expose the fixed-default status. UI stays foundation-only in dependency direction; new action payload IDs use existing foundation types and translate into core at app boundary. Existing `GraphParamTarget::GeneratorOf(layer_id)` continues to route host controls, so no parallel slider implementation is needed.

**Gesture:** duplicate Wave fixture, map one Amount to an LFO, then reorder and remove the other card; first mapping stays live. The fixture can use existing simple scalar/transform nodes until F7's look ships.

**Gate:** `cargo test -p manifold-app scene_modifier_surface`; focused app/UI clippy. Deliver `scripts/ui-flows/scene-modifier-preset.json`, using current semantic action assertions. Run `cargo xtask ui-snap gltfscene --script scripts/ui-flows/scene-modifier-preset.json` using the verified `ui_snapshot/mod.rs:98` script option; recheck the scene fixture name at execution. A8 counts rebuilds while a parameter gesture runs; expects zero structural rebuilds and zero pipeline compiles. **Demo:** flow capture — target L3. **Forbidden:** title-based ownership, hardcoded Loop/Fog rows, writing model fields from UI.

## 9. F6 — stock Loop/Fog files and old runtime deletion

**Entry:** F5; read current Loop/Fog builders end-to-end and the corridor contract. Verify F1/F2 initializer/calibration tests before dispatch. All admitted defaults and controls must be represented in files and shared infrastructure.

**Deliverables:** `assets/scene-modifier-presets/SceneLoop.json` and `SceneFog.json`; catalog discovery/validation; preserved outer control IDs; graph-based shared values; old runtime descriptor/builders removed. Legacy recognisers move into a narrowly named migration module. check-presets gains modifier-specific boundary validation and host-fixture expansion, since modifier files intentionally have no final texture output. Preserve source restrictions rather than fabricating universal bypass.

**Gate:** focused `check-presets` scene-modifier mode (new CLI `--kind sceneModifier`, delivered here); `cargo test -p manifold-renderer --test scene_modifier_stock`; focused migration/legacy GPU proof after file conversion. Negative: `rg -n 'SceneModifierDescriptor|SceneModifierDescriptorEntry|build_scene_loop_plan|build_scene_fog_plan' crates/*/src` returns zero. Existing tests move to file loading, not duplicated in a legacy runtime harness. **Demo:** Loop/Fog controls after save/reload — L3 via F5 flow extended here. **Forbidden:** per-preset Rust callback or display-name switch.

## 10. F7 — first travelling wave

**Entry:** F6; mathematical fields W1/W2 signatures and proofs; complete primitive audit at the exact current tip. Only the instance-position response is required here.

**Deliverables:** minimal coordinate/weight/instance displacement operations not already present; `TravellingWave.json`; Loop → Wave fixture plus a dense low-poly field fixture. Scene Loop emits semantic cell identity if needed by the recipe: resident buffer slot must not become identity. All new pure GPU operations ship codegen/standalone/fused proofs. Support phase, amplitude, direction and wavelength through shared bindings.

**Gesture:** park camera, raise Amount, change phase and wavelength; then enable Loop travel. Wave remains stable as the corridor window moves. **Gate:** `gpu_proofs_gate.py --filter scene_modifier_wave`; `cargo test -p manifold-renderer --test scene_modifier_stock`; focused renderer clippy. Tests assert zero amount/disable exact identity, nonzero effect, phase periodicity and semantic-ID continuity. **Demo:** parked-camera and travelling comparison — L2 artifact; live card flow — L3. **Forbidden:** read back same-frame GPU positions to CPU, use slot index as a stable copy ID, promise arbitrary source-array compatibility without validation.

## 11. F8 — release proof and contract reconciliation

**Entry:** F1–F7 gates green at reviewed branch tip. Read validation contract; re-derive build and fixture commands.

**Deliverables:** full journey fixture; import a held-out photoscan scene; save/reload and modulate; preset export/import; malformed preset preservation; measured low-poly instance budgets; raster/shadow/RT and motion-vector checks. Update old framework contract to reference the replacement, resolve the Mirror documentation discrepancy with evidence, and update build order/status headers. Preserve existing unrelated engineering beads.

**Gate:** validation V1–V8, then `scripts/land_branch.py` with its existing required gate. Run no optional broad render sweep. Repeat only failed checks after changed code/evidence. New preset variation must be file-only and load through the real picker. **Demo:** one scripted end-to-end flow, emitted comparison frames and exact worktree launch command for Peter; target L3, L4 recorded only after Peter tests. **Forbidden:** calling whole programme shipped, inventing throughput from compile success, carrying a new red check without explicit named disposition.

## 12. Invariants & enforcement

Architecture A1–A10 have phase owners above. F1 owns schema; F2 composition; F3 identity; F4 migration; F5 performance routing; F6 no Rust recipe callbacks; F7 geometry; F8 production acceptance. No behaviour is complete solely because its horizontal phase compiles. Every phase introducing serialized state includes reload and post-reload modulation where applicable.

## 13. Decided — do not reopen

One canonical stack, one graph preparation path, one parameter surface, exact-ID ownership, load-only legacy recognisers and measurable geometry proofs. No app changes on main; workers return edits/checks and the lead lands.

## 14. Deferred

The [programme map](SCENE_MODIFIER_PROGRAMME.md) assigns later functionality to its owning contract. Advanced camera control, lighting/material endpoints and contextual default extensions beyond the specified expression vocabulary need fresh seam specifications.
