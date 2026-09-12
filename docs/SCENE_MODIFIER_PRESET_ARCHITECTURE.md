# Scene modifier presets — canonical graphs and ordered expansion

<!-- index: Approved unified modifier schema, typed attachment and coordinate contracts, photoscan migration, bindings and runtime deletion. -->

**Status:** APPROVED · 2026-09-12 · Codex lead · unified architecture not implemented. Shipped photoscan recipes are migration inputs, not completion of this schema.
**Prerequisites:** Existing graph/preset/parameter infrastructure and the shipped Loop/Fog/photoscan modifiers. No new backend.
**Execution contract:** Read [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8; phases are in [Foundation Plan](SCENE_MODIFIER_FOUNDATION_PLAN.md).

Peter wants modifiers to work like effects and generators, with presets as files. **A modifier is an authored graph plus a typed attachment recipe; an applied modifier is an ordered, independently identified copy of that graph.** The compiler expands it into the existing scene graph before normal flattening and fusion.

## 1. Audit — verified 2026-09-10

Base audit: `28c8486f0`. Re-derive anchors at implementation; source takes priority over dated design headers. Extend existing infrastructure.

| Piece | Source | Finding |
|---|---|---|
| Graph and stable node identity | `crates/manifold-core/src/effect_graph_def.rs:82`, `:108` | `EffectGraphDef` has nodes/wires/metadata; `NodeId` survives document renumbering |
| Nested graph bodies | `effect_graph_def.rs:255`, `:269` | `GroupInterface` and `GroupDef` already describe reusable typed boundaries |
| Preset kind | `crates/manifold-core/src/preset_def.rs:29` | Effect and Generator only; SceneModifier is new |
| Shared file format | `crates/manifold-io/src/preset_file.rs:1` | Complete graph + metadata; JSON import/export, `.manifoldpreset` |
| Project-contained definitions | `crates/manifold-core/src/project/mod.rs:32`; `project/presets.rs:7` | Existing embedded preset registry; reuse catalog tiers and saved origins |
| Modifier registry | `crates/manifold-renderer/src/node_graph/scene_modifier.rs:32`, `:162` | Rust function pointers, fixed trace strings, Loop/Fog submissions |
| Singleton discovery | `scene_modifier.rs:238`; `scene_vm.rs:547` | Presence inferred from kind trace, not an instance stack |
| Undoable graph surgery | `crates/manifold-editing/src/commands/graph/scene_modifier.rs:90`, `:419` | Current apply/remove snapshot and refresh precedent |
| Object boundary | `crates/manifold-renderer/src/node_graph/primitives/scene_object.rs:39` | Vertices/transform/material/maps/instances → Object; no Object chaining |
| Existing parameter surface | `crates/manifold-ui/src/param_surface.rs:1` | One manifest-backed row model for scene/effect/generator cards |
| Compile pipeline | `crates/manifold-renderer/src/node_graph/graph_loader.rs`; `freeze/install.rs` | Existing group flattening, binding retargeting, GPU fusion and install |

## 2. Decisions

**September 12 implementation audit:** `scene_modifier_mesh.rs:23–35` embeds three JSON files; `:200–285` still registers per-kind descriptors, trace IDs and row lists; `:295`, `:323`, `:398`, `:455`, `:587` resolve radius/targets/offsets, patch atom parameters and create outer controllers. Core `scene_modifier.rs:57` supplies `MeshStageSplice`; editing `commands/graph/scene_modifier.rs:125,239` preflights and inversely removes stages. These seams already solve the pilot, but per-look registration, atom-type checks and inferred ownership are the work to retire. Anchors verified 2026-09-12; rederive at implementation tip. `photoscan_modifier_plans.rs` and `scene_mesh_modifier_roundtrip.rs` are existing regression inputs. The preceding table is the historical pre-pilot inventory, not the current list of stock kinds.

- **D1 — Persist an ordered stack in the canonical graph.** Add `sceneModifiers` to `EffectGraphDef`. Its entries contain their authored graph snapshot and stable target identities. This replaces the old framework's "list is never stored" rule at the F4–F6 activation landing, when migration and replacement controls are ready together. Graph topology and this stack are the authored source; expanded nodes are derived and never saved beside it. Rejected: infer arbitrary repeatable presets from fixed node-name signatures.
- **D2 — Same graph/file vocabulary, one new preset kind.** Add `PresetKind::SceneModifier`. Preset metadata carries a declarative recipe. Use ordinary graph nodes/groups/params/bindings. Rejected: one Rust descriptor, callback or shader bundle per creative look.
- **D3 — Expansion is an authoring/build operation.** Content state owns the canonical graph. CPU expansion runs on the existing graph-build/preparation path and creates a disposable ordinary graph. No traversal, JSON parsing, topology reconstruction or allocation per frame. No new thread, channel or shared lock.
- **D4 — Typed attachment endpoints, no `Scene`/chainable `Object` port.** Insert into vertices, transforms, instances, camera or atmosphere paths before `scene_object`/`render_scene`. Preserve the Object single-hop invariant.
- **D5 — One selected scene per owner graph in v1.** Resolve the single `render_scene` by stable path; reject ambiguous graphs. Multiple scene nodes are a later extension, not a fallback to the first node.
- **D6 — Structural edits are undoable; gestures are param writes.** Add/remove/reorder/retarget and graph edits rebuild through the existing preparation path. Amount, phase and enable do not. UI reads snapshots and sends existing command routes.
- **D7 — Reference and current are explicit.** A stage can sample unmodified source data for placement/weights while transforming the result of earlier stages. No frame-to-frame accumulation unless a stateful primitive explicitly owns it.
- **D8 — All newly applicable stages preserve their input at bypass.** Camera/atmosphere sources without a meaningful predecessor use a declared identity/default. Scene Loop is a singleton source with the documented legacy bypass exception in section 5; its copies are not secretly deleted by a new migration.
- **D9 — Schema v3 and lossless rejection.** New fields require graph version 3 at every containing definition. v1/v2 remain readable. Add `EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS = 3`, update maximum-version checks, and make `with_preset_metadata()` promote without downgrading v3. Add the next project migration rung (1.15.0 at this audit; re-derive if another rung lands first) so older project loaders reject new saves before parsing away fields. Current standalone IO only deserializes serde data; add recursive version validation there before import/export. We cannot retrofit safe rejection into arbitrary old third-party JSON tools: downgrade/export-to-old-version is unsupported, and old binaries must not be used to resave these files.
- **D10 — Metadata is the single authoring surface.** Catalog discovery, card names, control order/ranges, enable identity and applicability come from the file and typed recipe. Remove Rust row whitelists, traces and per-kind parsing/registration. Use existing JSON document/preset transport; do not introduce a parallel JSONL format. A look made from existing atoms requires no app, renderer or UI code change.
- **D11 — Shared coordinate resolution replaces atom-specific setup.** EachObject + Vertices is the mesh attachment; “EachMesh” in pilot prose is shorthand, not another scope. Resolve imported source offsets and a common source radius once through typed context. The compiler does not branch on `node.wave_shear_mesh` or `node.transform_mesh_patches` to set their parameters.
- **D12 — Safe publication is transactional.** Validate the complete candidate graph, bindings and resource admission before replacing the installed graph. Invalid authoring remains diagnosable through existing editor error state; no partial application or silently skipped targets. Geometry controls stay on the current parameter runtime; preparation-only controls are explicitly declared and unavailable as modulation targets.

## 3. Canonical core schema

New types live in `manifold-core/src/scene_modifier_preset.rs` and are re-exported through core. All serialize with camelCase fields, enums with camelCase variants; IDs use existing transparent `NodeId`. No GPU types. Types below derive Debug/Clone/PartialEq/Serialize/Deserialize; Eq where all fields permit. Omitted optional fields default to None, omitted vectors to empty. Do not put runtime caches in serialized types.

```rust
// Existing EffectGraphDef gains:
pub scene_modifiers: Vec<SceneModifierInstanceDef>,
// Existing PresetMetadata gains:
pub scene_modifier: Option<SceneModifierRecipe>,
// Existing PresetKind gains: SceneModifier
// Existing BindingTarget gains:
// SceneModifier { modifier_id: NodeId, param_id: String }

pub struct SceneNodeRef {
    pub scope: Vec<NodeId>, // stable ancestor group IDs, root to leaf
    pub node: NodeId,
}
pub enum SceneTargetSelection {
    AllObjects,
    Explicit { objects: Vec<SceneNodeRef> },
}
pub struct SceneModifierInstanceDef {
    pub id: NodeId,
    pub scene: SceneNodeRef,
    pub targets: SceneTargetSelection,
    pub mesh_frames: Vec<SceneMeshReferenceFrame>, // default empty; authored calibration
    pub graph: Box<EffectGraphDef>,
}
pub struct SceneMeshReferenceFrame {
    pub target: SceneNodeRef,
    pub source: SceneNodeRef,
    pub source_definition_hash: String,
    pub source_offset: [f64; 3],
    pub scene_radius: f64,
}
pub struct SceneModifierRecipe {
    pub schema_version: u32, // 1 for this recipe ABI
    pub singleton: bool,
    pub enabled_param: String, // names one declared preset param
    pub preparation_params: Vec<String>, // default empty; remaining numeric controls are live
    pub stages: Vec<SceneModifierStageDef>,
    pub initializers: Vec<SceneNodeInitializer>,
    pub calibrations: Vec<SceneParamCalibration>,
}
pub enum SceneAxis { X, Y, Z }
pub enum SceneScalarExpr {
    Constant { value: f64 },
    BoundsMin { axis: SceneAxis },
    BoundsMax { axis: SceneAxis },
    Add { a: Box<SceneScalarExpr>, b: Box<SceneScalarExpr> },
    Subtract { a: Box<SceneScalarExpr>, b: Box<SceneScalarExpr> },
    Multiply { a: Box<SceneScalarExpr>, b: Box<SceneScalarExpr> },
    Max { a: Box<SceneScalarExpr>, b: Box<SceneScalarExpr> },
}
pub struct SceneNodeInitializer {
    pub target: SceneNodeRef, // local path inside the preset
    pub param: String,
    pub value: SceneScalarExpr,
}
pub struct SceneParamCalibration {
    pub param_id: String,
    pub min: SceneScalarExpr,
    pub max: SceneScalarExpr,
    pub default_value: SceneScalarExpr,
}
pub enum SceneStageScope { Scene, EachObject }
pub enum SceneEndpoint { Camera, Atmosphere, Transform, Instances, Vertices }
pub enum SceneContextValue {
    Beat, Time, TriggerCount, ObjectOrdinal, ObjectCount,
    ObjectSeed, SceneMin, SceneMax,
    SceneRadius, SourceOffsetX, SourceOffsetY, SourceOffsetZ,
}
pub enum SceneStageSource {
    Previous { endpoint: SceneEndpoint },
    Reference { endpoint: SceneEndpoint },
    Context { value: SceneContextValue },
    StageOutput { stage: NodeId, port: String },
}
pub struct SceneStageInput {
    pub port: String,
    pub source: SceneStageSource,
}
pub struct SceneStageOutput {
    pub port: String,
    pub endpoint: SceneEndpoint,
}
pub struct SceneModifierStageDef {
    pub group: NodeId, // top-level group in this preset graph
    pub scope: SceneStageScope,
    pub inputs: Vec<SceneStageInput>,
    pub outputs: Vec<SceneStageOutput>,
}
```

Stage groups use existing `system.group_input`/`system.group_output` and `GroupInterface`. Pure shared producers may live at preset root and feed groups using ordinary wires. The modifier preset has no `render_scene` or `final_output`. The recipe is the boundary contract instead of the effect texture/generator output contract. Existing graph nodes and internal groups remain editor-visible.

`preparationParams` names unique declared numeric parameters. They use undoable authoring edits/reprepare, not live binding slots; reject MIDI/LFO/audio/driver bindings to them with a reason. This initial vocabulary covers topology/count/cell-membership controls if exposed. Existing photoscan Phase, Bend, Lift, Curl, Orbit, Rise and Enabled remain live. Their fixed patch cell size remains internal preparation data. String/asset controls retain existing authoring semantics. A recipe claiming a live control must pass the no-rebuild and capacity tests; metadata is not permission to recompile on every gesture.

Initializers/calibrations execute only on fresh application, against finite imported scene bounds, before the snapshot is inserted. Reload, duplication, reorder and gestures never re-evaluate them. Max expression depth is 16 and total nodes 64 per expression; reject NaN/Inf, overflow, duplicate targets, missing params, min > max or defaults outside [min,max]. Evaluate in f64, range-check conversion to the existing serialized numeric param type. A calibration changes manifest min/max/default; initializers set its bound node defaults consistently. Shared live controls remain ordinary bindings and graph arithmetic. This finite expression vocabulary expresses Loop cell size `2 * (maxZ - minZ)`, proportional camera defaults and curated ranges without a Rust Loop callback. It is not a per-frame expression interpreter. Save As preserves these declarations for fresh application to another scene. Changing a calibrated default through the editor removes that param's calibration and bound initializers in the same undo unit, then stores the explicit default; an informational row explains that it is now fixed rather than scene-scaled. Raw JSON authors can still author expressions. No expression-tree UI is required in M1.

Stage order is explicit. A `StageOutput` can only reference an earlier stage; Scene → EachObject broadcasts, EachObject → EachObject pairs the same target, EachObject → Scene is rejected (a reduction is not implied). Ordinary shared wires into replicated stage groups broadcast; ordinary wires out to a shared consumer are rejected. Every output endpoint has exactly one writer within a stage. No cross-target state sharing.

Camera/Atmosphere endpoints belong to Scene stages. Transform/Instances/Vertices belong to EachObject stages. M1 implements these five endpoints; Material/Light/Splat endpoints require additive recipe versions and the later contracts. No generic arbitrary string path into Rust state.

**Static mesh coordinate context:** Vertices are in the source mesh's local coordinates; material submeshes must sample one common scan frame. For the admitted production GLB shape, `scanPosition = localPosition + sourceOffset`. At fresh application, SceneRadius is the finite positive half-diagonal of imported source bounds; the existing maximum finite positive source-provenance-radius fallback is admitted only when bounds are absent, never camera distance. SourceOffsetX/Y/Z and SceneRadius are ScalarF32 context inputs supplied from the instance's authored `meshFrames`. Offsets are admitted only for EachObject stages with a verified static imported translation route. The radius is common across all selected parts within an instance. Missing/ambiguous frame data yields `UnsupportedCoordinateFrame` with a target path; do not guess identity.

`meshFrames` is saved calibration, not a runtime cache. Fresh apply captures the source route, current static placement offset and scene radius once, matching the shipped adapter. Migration copies the actual saved per-target constants. Reload, reorder, duplication, parameter gestures and unrelated graph edits never recalculate them from the current object transform or camera. This preserves the field while the performer moves the object after adding a modifier. Validate finite offsets, positive radius, unique target entries and f32 conversion. The source-definition fingerprint is deterministic over canonical source type/asset path, mesh/primitive/material selectors and fit/recenter/translate settings, independent of document numeric IDs and downstream object motion. A source change invalidates its calibration with a diagnostic; explicit removal/reapplication captures a new frame. No automatic rebase changes a saved look.

For AllObjects membership changes and Explicit retargeting, the same undoable structural edit captures frames only for new targets, retains surviving entries and prunes removed ones. Reuse the stored instance radius for new targets; do not rescale surviving targets. The content-owned candidate update and frame validation precede installation. Pure graph expansion consumes this snapshot and never mutates it; an incomplete external file gets a diagnostic instead of silent calibration. Bare preset export excludes host target/frame snapshots, so fresh application in another scene calibrates that new scene. Duplicating an applied instance preserves its frame until explicitly retargeted.

The files wire these context inputs to scalar shadows using ordinary arithmetic: shear origin is negative source offset, patch source offset is positive, and both use SceneRadius as scale. Cell size stays dimensionless (the shipped value is 0.15 radii), avoiding double normalization. Existing material, map, transform and instance edges remain attached. General rotated/nonuniform transform chains and animated skin/morph reference frames require separate qualification; F2 must reject unsupported frame requirements rather than silently apply different spaces to different materials. No whole-scene baking or GPU readback is introduced by this resolver.

Identity is the instance ID, never display name or preset ID. Duplicate application mints a new ID. Reordering preserves all IDs. Namespacing uses length-prefixed tuple components `(modifier ID, stage group ID, target stable path, local node ID)`; it must be injective even when IDs contain punctuation. The generated numeric document IDs are allocator-owned and never persisted as target references.

The modifier instance graph is a full canonical local snapshot, excluding derived expansion. Catalog updates do not mutate a show. Save As exports this authored recipe graph plus its manifest; sharing uses the existing standalone preset transport. Instance graphs are already self-contained definitions, so project snapshot/prune logic traverses nested asset dependencies without making an additional competing copy of their definitions. Stock/user/project catalog tiers remain the source when adding a new instance. The containing EffectGraphDef belongs to its existing Generator GraphTarget; `instance.scene` locates render_scene inside that owner. The new nested GraphTarget is an editor address into `owner.sceneModifiers`, not another stored owner or duplicate scene reference.

Preserve existing enum encodings. `PresetKind` currently uses lowercase: give only the new SceneModifier variant explicit serde name `sceneModifier`; do not rename existing Effect/Generator variants. All nested modifier definitions participate in version and asset validation.

## 4. Expansion and binding contract

New renderer module `node_graph/scene_modifier_expand.rs` owns:

```rust
pub fn expand_scene_modifiers(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<EffectGraphDef, SceneModifierExpandError>;
```

`PrimitiveRegistry` refers to the existing renderer registry type, imported from its existing module. Errors are structured variants: UnsupportedVersion, MissingScene, AmbiguousScene, MissingTarget, DuplicateIdentity, InvalidRecipe, UnsupportedEndpoint, UnsupportedCoordinateFrame, UnsupportedRenderMode, MissingInput, ConflictingSource, RecursiveModifier, InvalidBinding and CapacityExceeded, each carrying the offending stable path and human-readable detail. No panics for authored input. Validate actual registry port names and types, group interfaces, wire endpoints and binding leaves before install; parsing JSON alone is insufficient.

Algorithm:

1. Validate graph/recipe versions, unique IDs, stage boundary types and binding targets. Reject modifier graphs with their own nonempty `sceneModifiers` (no recursive modifier nesting in v1).
2. Resolve the canonical scene/object routes through existing group structure. Build an attachment table keyed by stable object path plus endpoint. Record both the original producer (Reference) and current producer (Previous). Resolution is structural, not dependent on current visibility or GPU readback.
3. Walk the stack, then stages, then targets in stable path order. Copy template nodes into an expanded graph, namespace IDs and resolve ports. Update the current producer table. AllObjects follows scene object membership on structural rebuild; Explicit keeps its authored set. Missing explicit targets fail validation; they are never silently dropped.
4. Flatten stage controls: a host `BindingTarget::SceneModifier` resolves through that instance's metadata bindings to each generated leaf consumer. One macro ID may fan out to many targets. Preserve scale/offset/conversion and string bindings. Reject unresolved targets before install.
5. Rewire scene inputs once to the final producers, clear `sceneModifiers` in the derived graph and remove recipe-only metadata there. Preserve root metadata and all unrelated topology. Pass to ordinary flatten → validate → freeze → install.

No previous-frame geometry is read for Reference. Reference means the producer before this stack at the current evaluation time; for an animated source it remains animated. Bind/rest geometry for assembly is a separate explicit source in the mesh contract.

Every production consumer must see the same expansion: editor validation/preview, live rendering, warmup, export, thumbnails and check-presets. F2 installs it at the common preparation seam and proves both fused and unfused paths. Do not expand twice; a derived graph's empty stack makes expansion idempotent, but tests must also prove no double namespace/binding change.

Host manifests expose modifier params using stable `(instance ID, param ID)` identity. The new BindingTarget variant is an authoring address resolved before runtime, not a new runtime modulation engine. Parameter commands resolve it back to the local snapshot and existing manifest slot; live effective values continue through the current binding fan-out. Removing a modifier prunes only its own bindings, slots and mappings. Undo restores them together. Labels can collide harmlessly.

## 5. Sources, identity and bypass

Unwired Transform has existing identity semantics. Unwired Instances means one original object: materialize an identity instance only when a stage needs an array. Unwired Vertices is not a drawable target and fails applicability for a mesh stage. No invented geometry. Missing/degenerate bounds reject fresh bounds-dependent calibration or an unresolved bounds context; saved valid meshFrames do not require recalibration on reload. Recipes not using bounds remain applicable. Removal must remain possible when a source or its metadata is missing.

Scene Loop preserves the current corridor atom, camera window and phase wiring. It is singleton per scene, must precede other instance stages, and a fresh apply refuses an already-authored instance producer until explicit replication composition exists. It consumes the previous camera through the same camera switch. **Legacy Loop off restores the camera while leaving corridor instances:** preserve that saved behaviour in M1 and label the control "Camera Travel" in new curation without changing stored binding identity. Full loop removal restores the canonical base; do not claim old enable was an identity bypass.

Fog is an atmosphere source, singleton in M1. Fresh apply still refuses an existing atmosphere producer. Density zero is its declared neutral output; no unverified claim that a zero-density atmosphere is byte-identical to every possible absent-atmosphere rendering path. Migration keeps the current neutral tint/shaft defaults.

Transforming stages are chainable and may appear repeatedly. Enabled=0 must produce exact current input data. Where a recipe declares an overall Amount, Amount=0 is also exact identity; do not mistake zero Lift alone for bypass while Curl remains nonzero. Preserve existing neutral-control semantics. A branch bypass may avoid GPU work through existing liveness; disabling must not trigger pipeline creation. Count-expanding operations define a separate capacity contract, not a transform bypass hidden by truncating output.

## 6. Migration and editor semantics

Migration is transactional and idempotent, after existing fixed-row/corridor migrations. Recognise only complete known legacy Loop/Fog signatures and verified wiring shapes. Extract their actual node params, wires, control IDs and mapping state into snapshots; derive the base using recorded camera-switch input and known owned edges, not "first node of this type". Preserve current saved behaviour, including legacy lost takeover history that cannot be reconstructed.

Partial or custom legacy shapes remain ordinary editable graphs with a visible migration diagnostic. Do not hide, delete or guess their semantics. They may be manually adopted later; they are not displayed as an active new preset instance. Legacy recognisers remain load-only, covered by migration fixtures; the runtime descriptor registry/builders are removed.

**Photoscan migration is mandatory in F4.** Recognise complete connected groups and their owned controllers/bindings for all three kinds; IDs alone are insufficient. Preserve the actual authored contents, outer parameter identities, current values, Enabled, ranges and all MIDI/driver/LFO/audio/envelope mappings. Derive stage order from current-input edges, never enum/catalog order. Preserve each stage's original reference source, target subset, radius, offsets and fixed cell size. Convert only if per-target clones reduce losslessly to one local recipe plus typed context; differing edited bodies, incompatible calibration, contradictory order across targets or ambiguous ownership keep the affected connected stack as an ordinary graph with a diagnostic. Do not partially migrate that stack and change its meaning.

The conversion is performed on a candidate snapshot and checked before publication. Keep golden v2 fixtures for all three alone and together, middle removal, disabled state, non-default controls, punctuation in stable IDs, renamed handles, multiple materials and custom inner edits. A mapping is retargeted to the new authored address while its identity and value survive; no permanent runtime alias map. Source groups/controllers are removed only after their replacement and all references are validated. Preserve Loop/Fog names and source semantics too. Catalog updates do not replace migrated snapshots with factory defaults.

Open Graph navigates to the canonical instance snapshot. Graph editing never edits generated per-target copies. A recipe boundary is shown as a typed group input/output; unsupported boundary edits produce validation errors without destroying the last valid installed graph. Save persists invalid authoring data only if the existing editor does so with its explicit error state; playback must not silently present it as valid.

Add/remove/reorder/retarget commands operate on `sceneModifiers` and associated manifest entries in one existing graph-target snapshot command. A stale command validates owner/instance identity before mutation. Reorder recalculates legality (Loop source first, singleton sources); do not display arbitrary reorder success when compilation rejects the order. Object deletion that breaks Explicit selection is rejected unless the same undo unit explicitly retargets/removes that reference. This is distinct from load-time preservation of a broken external file.

## 7. Consequences and performance

Persisting a stack is a deliberate schema change, not a small descriptor refactor. New bindings touch core, editing, app projection and preparation. Those changes earn independent repeated instances and reliable reorder/removal. Per-object expansion can grow graph size; M1 admits at most 256 selected object bindings and 16 applied modifiers per scene. Limits are proposed admission budgets, not measured throughput claims. Larger scenes keep loading their ordinary graph; adding a modifier beyond the limit fails explicitly.

Instance/vertex math runs through manifold-gpu and the existing native Metal backend. Pure per-element operations must use freeze codegen with standalone/fused proofs. Prepare pipelines and capacities before performance. Keep source resources immutable and use existing generation/retirement semantics so raster, shadows, RT, motion vectors and export consume coherent geometry.

**Rendering capability boundary:** dynamic mesh RT is currently unsupported because acceleration updates wait for settled vertex generations (BUG-e3p6.4). M1 conservatively treats any Vertices-writing modifier stage as requiring dynamic-vertex support for RT, even if currently bypassed. Surface the incompatibility through shared renderer admission; preserve the authored request and show the reason without silently switching settings. This endpoint rule is independent of preset names and live values; any future relaxation needs proof. Raster M1 can ship independently. Do not claim shadow/motion-vector/RT coherence without its specific check. Enable gestures neither reclassify capability nor compile pipelines.

## 8. Invariants & enforcement

All named tests below are new deliverables, not currently passing tests. Shared commands and proof levels are in [Validation](SCENE_MODIFIER_VALIDATION_PLAN.md).

| ID | Contract | Machine check / owning phase |
|---|---|---|
| A1 | Canonical data round-trips; derived data not saved | `scene_modifier_v3_roundtrip`, F1 |
| A2 | Duplicate labels/kinds have independent controls | `scene_modifier_duplicate_identity`, F3/F5 |
| A3 | Order is semantic, remove reconnects exact predecessors | `scene_modifier_order_and_remove`, F2/F3 |
| A4 | Object stays single-hop | existing Object invariant + `scene_modifier_no_object_chaining`, F2 |
| A5 | Expansion identical across consumers and idempotent | `scene_modifier_preparation_parity`, F2 |
| A6 | Migration preserves known legacy behaviour and mappings | `scene_modifier_legacy_migration`, F4 |
| A7 | Invalid input never partially applies | `scene_modifier_invalid_atomic`, F1/F3 |
| A8 | No live structural rebuild for gestures | `scene_modifier_live_binding_no_rebuild`, F5 |
| A9 | Geometry changes reach every claimed render path; unsupported dynamic RT is diagnosed | `scene_modifier_geometry_generation` and mode-admission test, F2/F8; RT parity owned by BUG-e3p6.4 |
| A10 | Source-only restrictions are explicit and tested | `scene_modifier_source_order`, F2 |
| A11 | Common scan frame across material groups; no atom-specific compiler setup | `scene_modifier_coordinate_context`, F2/F4 |
| A12 | Files own all curation and attachment; new recipe needs no Rust registration | `scene_modifier_file_authoring`, F6/F7 |
| A13 | Invalid ports, unsupported frames/modes and excess capacity fail before install | `scene_modifier_admission`, F2/F5 |
| A14 | Live vs preparation controls have enforceable mapping semantics | `scene_modifier_parameter_mutability`, F1/F3/F5 |

## 9. Phasing

F0 baseline evidence; F1 schema; F2 expansion/coordinate context/admission; F3 commands/bindings; F4 migration of all five kinds; F5 cards/editor/library; F6 stock files and old runtime deletion; F7 independent file-only authorship proof; F8 release proof. Exact briefs live in the [Foundation Plan](SCENE_MODIFIER_FOUNDATION_PLAN.md).

## 10. Decided — do not reopen during implementation

Canonical ordered snapshots; JSON documents; typed endpoints; no Object chaining; expansion before fusion; existing parameter/command infrastructure; beat-derived stateless motion first; legacy behaviour preserved; runtime kind callbacks removed.

## 11. Deferred

Multiple scene owners in one graph, nested modifier stacks inside a preset, arbitrary cross-target reductions, dynamic topology during performance, general source replication composition, Material/Light/Splat endpoints and live automatic catalog upgrades. Revival: a later named contract supplies the missing typed semantics and proofs. Do not silently broaden the v1 schema during a worker phase.
