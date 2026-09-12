# Scene modifier foundation — implementation plan

<!-- index: Baseline qualification and eight phases for unified photoscan presets, coordinates, migration, catalog, file-only authoring and release proof. -->

**Status:** APPROVED · 2026-09-12 · Codex lead · F0/F1–F8 pending. The shipped photoscan slice supplies working inputs; it does not implement the unified foundation.
**Prerequisites:** [Preset Architecture](SCENE_MODIFIER_PRESET_ARCHITECTURE.md); existing graph/preset/editor infrastructure.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8. Architecture decisions A/D references below refer to the companion architecture; test policy is [Validation](SCENE_MODIFIER_VALIDATION_PLAN.md).

The next release makes the existing photoscan journey reliable and generic: loaded GLB → file preset → repeat/reorder/target → live gesture → graph edit → save/reopen/share. F0 captures the working baseline; F1–F6 replace the adapter with the shared architecture; F7 proves independent file-only authorship; F8 closes release evidence. New geometry families do not substitute for unresolved foundation work.

## 1. Audit and re-derivation

Historical inventory: `28c8486f0`, 2026-09-10. Refresh it against the shipped photoscan implementation before F1; counts and line numbers below are historical. The architecture owns data definitions; this plan owns migration call sites. Current additions are `scene_modifier_mesh.rs`, core `MeshStageSplice`, generic editing preflight/removal, three JSON files, two GPU atoms and the photoscan plan/editing/UI fixtures. Preserve those tests as migration inputs.

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

**D1 — Build in dependency order.** F0 → F1 → F2 → F3 → F4 → F5 → F6 → F7 → F8. F1 adds support without changing old projects. F4–F6 are separate work packages/commits but one coherent activation landing: migration, replacement cards/catalog and old-runtime deletion ship together. Do not auto-migrate a user's graph before the replacement controls exist. No permanent dual runtime or migration feature flag survives that landing.

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
pub fn resolve_modifier_mesh_frames(
    owner: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
) -> Result<Vec<SceneMeshReferenceFrame>, SceneModifierExpandError>;
```

The frame resolver validates stored calibration and captures only newly selected targets during an undoable structural edit. Fresh instances have no frames; invalid stored/source-changed entries return diagnostics rather than recalibrating. It is not called to mutate a graph during expansion or a live gesture. Preset export excludes host frames; applied-instance duplication copies them. F2 owns this shared resolver, F3 owns its atomic command integration, and F4 supplies legacy captured values directly.

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

**D4 — Keep source semantics honest.** Loop/Fog remain source presets with their existing restrictions and off behaviour; the three photoscan modifiers supply existing chainable identity-bypass examples. General camera controllers, atmosphere blending and replication of an existing instance set are later extensions.

## 3. Phasing and common execution rules

**F0 — capture the baseline and close the narrow qualification gap.** Before changing schema, retain immutable v2 graph fixtures for each photoscan look and a three-stage stack, with non-default values, Enabled and representative saved mappings. Reuse `photoscan_modifier_plans`, `scene_mesh_modifier_roundtrip`, the nine `photoscan_modifier` GPU proofs and `scene-photoscan-modifiers.json`. Record fixture asset hashes and observed settings. Peter already reports visual/LFO success; do not describe that as untested or rerun a basic slider demo to replace it. Add one bounded production save → reopen → LFO/driver/audio gesture with a visible playing clip, plus V8's prepared performance sequence on the scan. Compare stored mappings and post-reload effective geometry. Audio observation uses a deterministic available input or records the missing input as a concrete gap. Record BUG-e3p6.5 results; no long-show soak. The lead resolves a failed baseline before claiming migration parity; dynamic RT BUG-e3p6.4 remains outside M1. This is an entry work package, not a ninth new framework subsystem.

**Lane and landing schedule:** the lead owns schema/coordinate/migration decisions and review. Use Luna high-effort workers with prepared exact-path briefs. F1 schema/IO is one owner; after its interfaces settle, F2 expansion and independent negative-fixture authoring may run in parallel. F3 owns command/binding code; F4 migration and disjoint fixture work can then run separately. F5 owns the shared UI surface; F6 catalog conversion may run alongside independent validation tooling only after its loader interface is fixed. F4–F6 share one leased workstream until the activation gate is green. F7 is an independent recipe author lane with no permission to change runtime code. Do not dispatch all phases concurrently or split one shared module between workers. Each phase returns focused checks; the lead runs required landing gates once per coherent landing. Split an oversized phase at a named seam before dispatch, preserving its exit criteria.

Each phase begins by reading its entry files and restating binding decisions, forbidden shortcuts and inventory results. Acquire one slot through `scripts/agent-worktree.py`, verify its base, and follow existing build-lock/landing mechanics. No app builds are needed for this documentation delivery.

Commands below run from the phase worktree with an explicit absolute `--manifest-path "$MODIFIER_WORKTREE/Cargo.toml"` on Cargo commands. `MODIFIER_WORKTREE` is the assigned slot path, never an assumed shared checkout. GPU tests use cargo test through `gpu_proofs_gate.py`. Use focused tests once after edits; at most two attempts per exact command. The landing script preserves required touched-crate gates. New tests and flows in these briefs must be created before their commands can pass; zero selected tests is a failed gate.

## 4. F1 — core schema and lossless files

**Entry:** baseline audit commands; read architecture sections 2–4, core graph/preset/project types, `preset_file.rs` and version validation. No dependency on F2.

**Deliverables:** architecture structs including preparationParams and typed coordinate contexts; graph v3 support; SceneModifier preset kind and metadata; BindingTarget variant; GraphTarget variant with typed errors at unresolved new targets. Update constructors compiler-first. File/project embedding preserves v3 and rejects recursive/unknown recipe versions. Add `scene_modifier_v3_roundtrip` and invalid-schema fixtures, including a held-out authored JSON preset. Validate unique preparation parameter names and their declared types. New enum paths may return explicit unsupported-execution errors until F2/F3; never masquerade as Generator. Reuse existing asset dependency traversal for nested local snapshots.

Include authored meshFrames roundtrip and validation. Calibration survives save/reopen and is excluded from exported bare preset recipes. The format distinguishes absent fresh-instance frames (creation resolver input) from an incomplete saved instance (load diagnostic); saved validation requires frames for every selected mesh target needing context.

**Gate:** `cargo test -p manifold-core -p manifold-io scene_modifier_v3` and focused clippy for core/io with `--tests -- -D warnings`; serialize → reload → equal canonical definition. Negative: no new `Arc<Mutex`/`Arc<RwLock` in touched code. A1/A7 tests are deliverables. **Demo:** none — L1. **Scope fence:** data/file support only; no picker or GPU work.

## 5. F2 — typed expansion

**Entry:** F1 tests; read scene_object, graph_loader, bound_graph, freeze/install, generator registry. Re-derive all preparation entrances.

**Deliverables:** expansion and applicability functions; stable identity allocation; stage routing; reference/current distinction; admission limits; expanded binding fan-out. Use one shared preparation function at entrances that currently flatten directly: `graph_loader.rs:771`, `bound_graph.rs:290,495`, and pre-fusion generator entry (`generators/registry.rs:267`). Follow callers for viewport, thumbnail, warmup and export. Add A3/A4/A5/A10 tests, including two noncommuting stages, renamed groups, duplicate labels, reordered numeric IDs and explicit missing targets. Add neutral CPU fixture nodes for structural tests; do not introduce production test-only fallbacks.

Also deliver A11/A13: source-frame resolver, SceneRadius/SourceOffset context, actual registry port/binding validation, checked expansion/buffer admission and shared renderer capability diagnostics. Test nested multi-material imports, missing frame data, nonuniform/unsupported transforms, punctuation in IDs, wrong patch input port, duplicate interface names and radius normalization. No atom-type-specific setup remains in the new compiler. Graph expansion runs once per structural preparation; parameter gestures cannot invoke it. Pass the three existing recipe bodies through this path before broadening source support. Do not retrofit dynamic RT in this phase.

`scene_modifier_coordinate_context` must move an object after application, trigger an unrelated rebuild and reopen the graph: its saved frame/output relationship remains unchanged. A changed mesh source yields a calibration diagnostic; a newly selected target gets one captured frame without changing surviving frames. Test actual f32 context wire values and geometry, not just equality of stored metadata.

**Gate:** `cargo test -p manifold-renderer scene_modifier_expand` and renderer clippy; `scene_modifier_preparation_parity` explicitly exercises fused preparation, unfused preparation and preview resolution. Negative: expanded graphs contain no `sceneModifiers` and no modifier-only BindingTarget. These are parsed assertions, not a source-string guess. **Demo:** none — L1; geometry operation not introduced yet. **Forbidden:** new Object producer/consumer chain, direct RT writes, per-frame expansion.

## 6. F3 — commands and parameter identity

**Entry:** F2; read GraphTarget resolution, existing graph snapshot commands, host manifest/mapping pruning and param bindings. Inventory all new enum matches.

**Deliverables:** four commands in section 2, shared nested graph-target resolver, stable host macro addresses, fan-out through the existing runtime binding path. Add insert/delete/move/retarget undo/redo tests and mapping restoration after reload. Two instances of the same preset with identical titles must remain independent. Changing selection recompiles without changing surviving target/node IDs. Delete-object command checks references as specified in architecture section 6.

A14 delivers `scene_modifier_parameter_mutability`: live controls update existing slots; preparation controls produce undoable reprepare and reject modulation mapping. Invalid/stale requests leave graph, mappings, installed output and undo history unchanged. Retargeting AllObjects/Explicit has the architecture's exact membership semantics, including newly added and deleted targets. Preserve IDs across duplicate labels, undo, reorder and graph export/reimport.

**Gate:** `cargo test -p manifold-editing scene_modifier` plus `cargo test -p manifold-renderer scene_modifier_binding`; touched-crate clippy. A2/A3/A7 tests mandatory. Negative: removal implementation has zero comparisons against display-name/section strings for ownership. **Demo:** none — L1. **Forbidden:** add new modulation vectors, synthesize unstable parameter indices, silently clamp Move index.

## 7. F4 — legacy migration

**Entry:** F3 and F0 snapshots; read all Loop migrations, actual five-kind descriptor registrations, photoscan group/control ownership and saved fixtures. Resolve Mirror's header/source discrepancy through code/history; do not change its implementation during this phase.

F4 builds and tests migration in the leased workstream. Register production load-time migration only in the F4–F6 activation landing, with the new cards, all five stock files and deletion checks ready. Earlier F1–F3 landings preserve the old user journey and keep unsupported new authoring routes explicitly unavailable.

**Deliverables:** load-only `migrate_legacy_scene_modifiers` after existing loop migrations; known-shape extraction; stable control remap; preserve partial/custom graphs with diagnostics. Keep current Loop phase/cell/camera values and Fog neutral defaults. Maintain fixture copies of old v1/v2 documents before changing any fixture. Migration produces the same result twice and round-trips. New custom held-out legacy fixture exercises non-default bindings, renamed handles and unrelated atmosphere/camera wiring.

Migrate Elastic Sculpture, Surface Peel and Vortex Fragments using architecture section 6. Preserve actual group contents, names/defaults, Phase mappings, stack order/reference edges and material-target offsets; never regenerate from factory defaults. Include edited per-target clones and conflicting target orders as explicit preserved-with-diagnostic cases. Recognising one kind inside an ambiguous connected stack does not permit partial conversion. The scope is lossless adoption, not merging Peel/Vortex into one card. Add `scene_modifier_photoscan_migration` tests and compare old/new GPU output for nonzero gestures plus exact bypass; render the same fixed scene/settings for appearance parity. Reuse existing independent atom proofs rather than rewriting their expected formulas.

**Gate:** `cargo test -p manifold-renderer --test scene_modifier_legacy_migration`; old Loop structural/wrap/roundtrip tests selected by exact target names from current Cargo inventory; focused GPU filter `scene_modifier_legacy` for old versus new evaluated geometry and frames. Negative: new apply path never calls legacy trace; partial signatures remain byte-preserved in the test. A6 mandatory. **Demo:** saved comparison frames — L2 artifact, human review separately recorded. **Forbidden:** restore by first type match, drop unrecognised nodes, infer absent takeover history.

## 8. F5 — cards, browser and graph editor

**Entry:** F3/F4; read `param_surface.rs`, `ui_bridge/projection/cards.rs`, `ui_bridge/project.rs`, dropdowns and shared graph-editor target resolution.

**Deliverables:** SceneModifier catalog filtering, applied instance cards in actual stack order, add/remove/reorder/selection affordances, Enabled binding, Open Graph to local snapshot, Save As/import/export through existing preset transport. Explicit source-order errors visible before apply. Calibrated-default editing follows architecture section 3: atomically remove that default's initializer/calibration, store the explicit value, and expose the fixed-default status. UI stays foundation-only in dependency direction; new action payload IDs use existing foundation types and translate into core at app boundary. Existing `GraphParamTarget::GeneratorOf(layer_id)` continues to route host controls, so no parallel slider implementation is needed.

Metadata supplies all card controls; remove hardcoded photoscan row whitelists as well as Loop/Fog rows. Show actionable unsupported-frame, render-mode and capacity reasons, including loading a saved project with an incompatible mode; preserve authored data. Preparation-only controls must be visibly distinct and unavailable in modulation targeting. Include a real playing clip in the acceptance flow: the old botanical fixture's empty viewport is not enough for this vertical slice.

**Gesture:** duplicate Elastic Sculpture, map one Phase to an LFO, then reorder and remove the other card; the first mapping stays live. Reopen the project and change the mapped range. Use a playing photoscan clip so the same flow observes both controls and changing geometry.

**Gate:** `cargo test -p manifold-app scene_modifier_surface`; focused app/UI clippy. Deliver `scripts/ui-flows/scene-modifier-preset.json`, using current semantic action assertions. Build `manifold-app` with `ui-snapshot`, then run `target/debug/manifold ui-snap gltfscene --script scripts/ui-flows/scene-modifier-preset.json`; extend the fixture with a playing clip and recheck its selection at execution. A8 counts rebuilds while a parameter gesture runs; expects zero structural rebuilds and zero pipeline compiles. **Demo:** flow capture — target L3. **Forbidden:** title-based ownership, hardcoded Loop/Fog rows, writing model fields from UI.

## 9. F6 — all stock files and old runtime deletion

**Entry:** F5; read current Loop/Fog builders and photoscan adapter end-to-end plus the corridor contract. Verify F1/F2 initializer/calibration tests before dispatch. All admitted defaults and controls must be represented in files and shared infrastructure.

**Deliverables:** `assets/scene-modifier-presets/SceneLoop.json` and `SceneFog.json`; adopt ElasticSculpture/SurfacePeel/VortexFragments JSON with explicit recipe/context ports; catalog discovery/validation; preserved names and control identities; graph-based shared values; old runtime descriptor/builders removed. Legacy recognisers move into a narrowly named migration module. check-presets gains modifier-specific boundary validation and host-fixture expansion, since modifier files intentionally have no final texture output. Preserve source restrictions rather than fabricating universal bypass.

**Gate:** focused `check-presets` scene-modifier mode (new CLI `--kind sceneModifier`, delivered here); `cargo test -p manifold-renderer --test scene_modifier_stock`; focused migration/legacy GPU proof after file conversion. Negative: production code has no `SceneModifierDescriptor`, `SceneModifierDescriptorEntry`, `build_scene_loop_plan`, `build_scene_fog_plan`, `RecipeKind`, photoscan row/trace tables or atom-name-specific attachment patching. Exact source searches exclude only the named load-time migration module where legacy constants are required; do not rename a runtime callback to make a grep pass. Existing tests move to file loading, not duplicated in a legacy runtime harness. **Demo:** all five kinds load from files through the same picker; saved photoscan controls still work — L3 via the F5 flow extended here. **Forbidden:** per-preset Rust callback or display-name switch.

## 10. F7 — independent file-only authoring proof

**Entry:** F6, existing atom registry and documented recipe boundary. No new geometry algorithm is required. The wave pilot already shipped position/field/displacement primitives; photoscan atoms already supply shear and rigid patch motion. Audit reuse before any future primitive proposal.

**Deliverables:** an independent worker authors one conformance preset from existing nodes, controls, typed context and attachment metadata using only a preset file. Freeze its runtime dependencies before dispatch. Add/import through the real user/project catalog and picker; duplicate, reorder, edit the local graph, Save As, export/import and reopen with a mapped control. It must produce a nonzero visible effect on an already loaded photoscan. No descriptor, per-kind enum, control table, include_str registration, shader or app change is permitted to make this particular file work. A discovered generic framework defect returns to the lead and the owning earlier phase.

**Gate:** `scene_modifier_file_authoring` integration test through catalog → preparation → runtime, modifier `check-presets --kind sceneModifier`, F5 flow extension and focused checks only for any generic fix. Diff audit confirms recipe addition contains no runtime code. Negative fixtures: wrong port, unsupported source frame, unresolved binding and capacity violation produce structured errors before install. Existing standalone/fused atom proofs remain the numeric authority; compose a nonzero two-stage case to verify routing.

**Demo:** file imported through picker, visible gesture, then saved/reopened mapped instance — L3 target. Apply [programme section 2c](SCENE_MODIFIER_PROGRAMME.md#2c-september-12-direction-and-creative-admission): the conformance preset may remain a test/user asset if it is only a variation. Technical authorship success is not permission to publish another similar stock card. **Forbidden:** another travelling-wave kernel project, baked animation, private runtime callback, changing existing photoscan defaults to fake novelty.

## 11. F8 — release proof and contract reconciliation

**Entry:** F0/F1–F7 green at the reviewed tip. Read validation V1–V9 and rederive only necessary fixture/build commands. Preserve prior passed evidence when code and conditions have not changed.

**Deliverables:** complete photoscan journey and migrated-project reopen; duplicate/reorder/retarget with independent modulation; graph edit and preset export/import; malformed preset diagnostics; measured prepared scan/instance budgets. Qualify raster and each supported depth/shadow/motion-vector path explicitly. Dynamic RT is a separate tracked capability, not a requirement to implement before this raster release. Verify unsupported-mode diagnostics instead. Use one held-out static photoscan for the final journey, selected after implementation; do not repeat a broad GLB sweep.

Update the old framework contract to the replacement, resolve Mirror documentation with evidence, and reconcile statuses and beads. Existing Elastic/Peel/Vortex names, output and mappings survive; Loop/Fog retain their source semantics. All production consumers share expansion and no legacy runtime descriptor remains. Capture exact launch command and bounded comparison artifacts. Record Peter's existing visual/LFO feedback separately from newly measured persistence/audio/performance evidence.

**Gate:** relevant V1–V9, then `scripts/land_branch.py` with all required gates. Zero tests selected is failure. The new recipe loads without registration; deterministic baseline/modified checks are nonempty and visibly nonzero. Resolve BUG-e3p6.5 only for checks actually completed; leave dynamic RT BUG-e3p6.4 open. **Demo:** one scripted end-to-end playing-scene flow plus bounded rendered comparisons — L3 target; Peter's later review recorded separately. **Forbidden:** claiming whole programme shipped, silent rendering-mode switches, unmeasured throughput, or describing another waveform as a new behaviour family.

## 12. Invariants & enforcement

Architecture A1–A14 have phase owners above. F0 owns baseline evidence; F1 schema; F2 composition/coordinates/admission; F3 identity/mutability; F4 migration; F5 performance routing/diagnostics; F6 no Rust recipe callbacks; F7 file-only authoring; F8 production acceptance. No behaviour is complete solely because its horizontal phase compiles. Every phase introducing serialized state includes reload and post-reload modulation where applicable.

## 13. Decided — do not reopen

One canonical stack, one graph preparation path, one parameter surface, exact-ID ownership, load-only legacy recognisers and measurable geometry proofs. No app changes on main; workers return edits/checks and the lead lands.

## 14. Deferred

The [programme map](SCENE_MODIFIER_PROGRAMME.md) assigns later functionality to its owning contract. Advanced camera control, lighting/material endpoints and contextual default extensions beyond the specified expression vocabulary need fresh seam specifications.
