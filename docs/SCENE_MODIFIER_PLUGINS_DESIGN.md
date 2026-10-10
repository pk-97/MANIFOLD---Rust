# Scene Modifier Plug-ins — the engine runs the pipeline, families own the rewrites

**Status:** PROPOSED · 2026-10-10 · Fable · awaiting Peter's yes on D1–D9 · four stages, none started · Section 6 (Phasing).
**Prerequisites:** BUG-hkbdp.6.6 (CPU FLIP removal) landed; stage 1 inside BUG-hkbdp.6.15 (true engine-to-water couplings) under the epic BUG-hkbdp (renderer crate split epic); stages 2–4 inside BUG-bfktg (rendering-side decomposition epic). WATER_CRATES_DESIGN.md stage 2 (rigid crate carve) may land before or after stage 1; section 3.4 (Homes) names the file under both layouts.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before any stage. Lead: Opus. Lanes make one commit then stop; the lead lands with `scripts/land_branch.py`.

<!-- index: Scene-modifier expansion as registered plug-ins: the engine keeps the generic pipeline (stage order, routing, bindings, namespacing, validation); Shatter, fragment cuts, Math View, force recipients and mesh-frame rules move to the families that own their nodes, with prepared-graph snapshots proving nothing changes on stage. -->

**The governing insight: the scene-modifier compiler is two things glued together — a generic pipeline and five product recipes written into it.** The pipeline (`prepare_scene_modifiers_impl`: capacity preflight, instance cloning per target, endpoint chain state, routes, bindings, flattening, validation) names no family. The recipes do: Shatter clones rigid bodies into `node.physics_world` slots, the fragment pass inserts `node.cut_mesh_*` maps along a mesh lineage, Math View builds `node.render_mesh_diagram` overlays, the force resolver knows `pose_N → body_acceleration_N`, and frame capture knows which glTF source and transform are "static". Together they spell about 50 scene, water and image node ids and port grammars inside `manifold-node-engine`, which must never depend on a family crate. The coupling audit on BUG-hkbdp.6 (post-T2 water cleanup epic), 2026-10-10, marked Shatter NEEDS-DESIGN because no registered seam exists between the route builder and `into_graph`. This design adds that seam once, in the shape the engine already uses for every other family-owned callback, and moves each recipe to its owner.

Peter, 2026-10-10: *"plug-in style design and architecture sounds much stronger, safer, and easier to work with than custom graph re-writes and custom edits."*

Stage translation: nothing changes in what a modifier does to a scene. Shatter still releases the same pieces into the same body slots; Surface Peel still cuts the same bands; Math View still draws the same diagram. What changes is where a modifier bug gets fixed and what it can break: a Shatter fix builds `manifold-nodes` and reruns Shatter's four contracts and one GPU proof, not the engine and every family. And a new modifier that needs a graph rewrite is a file beside its nodes plus one `inventory::submit!`, not an edit to a 1962-line engine file.

Binding constraints (DESIGN_AUTHORING.md section 1 (The intake)): *Persistence* — none. Recipes, instances and frames keep their serialized shape; the only new serialized-adjacent fact is that a recipe feature with no linked plug-in is a load error instead of a silent skip (D6). *Hot path* — none. Expansion runs at load, attach and structural edit; `prepare_scene_modifiers` is already off the frame path (`runtime/modifier_runtime.rs:102` prepares once and caches). *Thread residency* — untouched; expansion is a pure function of `EffectGraphDef`. *Time model* — untouched.

Companion docs: `docs/WATER_CRATES_DESIGN.md` (D4: anything naming two families lives in the registration layer — applied here to decide homes), `docs/RENDERER_CRATE_SPLIT_DESIGN.md` (D4 no facades, D6 linking proven, D12 layering as a test), `docs/SCENE_MODIFIER_FRAMEWORK_DESIGN.md` and `docs/SCENE_MODIFIER_PRESET_ARCHITECTURE.md` (the recipe schema this design does not change), `docs/SCENE_MODIFIER_RT_DESIGN.md` (mesh revision rules the fragment pass must keep honoring).

---

## 1. Audit — what exists (verified 2026-10-10 at `c35147931`, worktree slot-1)

### 1.1 The generators in `crates/manifold-node-engine/src/load/expand/`

Every site that spells a family's node ids or port grammar, what it rewrites, and who owns the ids. Engine-owned ids (`node.value`, `system.*`, `node.render_scene` as the host type, `node.morph_mesh` and `node.sample_triangle_grid` which the engine also constructs) are not coupling and are not listed.

| Generator | Where | What it rewrites | Family ids spelled | Owner |
|---|---|---|---|---|
| Shatter | `compiler/shatter.rs:112` `prepare` (454 lines, whole file); called `compiler.rs:476` | Per enabled Shatter instance: finds the target's `node.physics_world` pose wire (`:156-168`), allocates free `body_N` slots (`:206-233`), distributes Pieces across material parts (`:236-261`), clones the glTF mesh source, the rigid body and the scene object per piece (`:294-398`), wires `body_N`/`body_acceleration_N`/`pose_N` and new `object_N` scene ports, bumps the scene `objects` count (`:445-450`), fans out bindings (`:416-431`), wires the trigger to `release_count` (`:203`) | `node.physics_world`, `node.gltf_mesh_source`; ports `body_`, `body_acceleration_`, `pose_`, `release_count`; rigid-body params `density`, `motion`, `enabled`, `fragment_count/index/parent`, `collider_parts`, `compound_materials`; source params `path`, `mesh_index`, `primitive_index`, `material_index`, `fit`, `recenter`, `translate_*`, `max_capacity`, `source_vertex_count`; `RigidImpulseTargets::BODY_CAPACITY` | water + scene |
| Force recipients | `acceleration.rs:31` `resolve`, `:89` `trace`, `:177` `authoring_objects`, `:192` `selected`, `:238` `recipient_key`, `:249`/`:265` `impulse_recipients*`; used by `compiler.rs:311-323,1559-1564`, `impulses.rs`, `expand.rs:142` | Resolves a scene object to the one physical input that receives acceleration. `trace` is generic (follows same-typed inputs through the registry). The grammar is not: `pose_N → (body_N, body_acceleration_N)` and `instances → (copies, copies_acceleration)` at `:117-134`; liquids resolve by the registry already (`acceleration_field` input, `:81`); the impulse mapping `copies_acceleration → RigidImpulseTargets.copies`, `body_acceleration_N → bodies bit N`, liquid → `ImpulseTarget::Fluid` at `:284-302` | `node.physics_world`; ports above; `acceleration_field` | water |
| Fragment cuts | `fragment_cuts.rs:362` `apply`, `:156` `contains_fragments` (847 lines); called `compiler.rs:211-222` (legacy no-instance path) and `:475` | Topologically walks the flattened graph, inserts one `node.cut_mesh_bands`/`cells` map per fragment node and `node.remap_mesh_cut`/`remap_cut_weights` at every multi-mesh seam, carries a lineage, wires `topology` into scene objects, fans out control bindings to the cutter | `node.ordered_recon_mesh`, `node.transform_mesh_patches` (`:127-132`), 19 unary mesh deformers (`:173-196`), `node.morph_mesh`, three weight sources (`:198-203`), `node.scene_object`, the four generated cut/remap ids, `BANDS_PORTS`/`CELLS_PORTS` | scene |
| Fragment grammar, runtime side | `buffer_budget.rs:121-124`, `value_writes.rs:174-186`, `parameter_guards.rs:85-110` | Identify the generated cut nodes for budget accounting, cutter baseline for value writes, and guard scope from fragment nodes to `node.render_scene` | same cut/remap/fragment ids | scene |
| Mesh frame capture | `frames.rs:79` `needs_mesh_frame`, `:112` `resolve_modifier_mesh_frames`, `:271` `source_route`, `:319` `static_offset`, `:374` `scene_radius`; `selected_objects` (`:25`) and `validate_saved_frames` (`:205`) are generic | Decides which vertex source counts as a static import, reads its translate/fit/bbox params, decides which transform is an unwired static translation, reads `pos_*`/`rot_*`/`scale_*`/`billboard`; special-cases Shatter at `:98` and `:174-183` (Shatter frames follow the live pose, offset = source translate) | `node.gltf_mesh_source`, `node.gltf_skinned_mesh_source`, `node.transform_3d` and their param names | scene |
| Math View | `compiler.rs:712` `seed_math_view`, `:786` `capture_math_view_input`, `:859` `finish_math_view`, `:1064-1234` controls/compose, plus `compiler/math_events.rs:76` `prepare` (338 lines); interleaved with the instance loop at `compiler.rs:347-400` and run post-routes at `:471` | Seeds a sampler per target, captures chain state at the view's position, builds a diagram and a surface-depth pass per target, composes them with `node.mix` and `node.set_alpha` into `system.final_output`; math_events adds a spatial mask, triangle samples and a mesh export boundary per frame | `node.sample_triangle_grid`, `node.render_mesh_diagram`, `node.mesh_spatial_mask`, `node.sample_mesh_triangles`, `node.transform_mesh_patches` (scene); `node.mix`, `node.set_alpha` (image) | scene + image |
| Endpoint identity producers | `compiler.rs:1270` `endpoint_input`: Transform → `node.transform_3d` (`:1290`), Instances → `node.arrange_copies` with `max_capacity=1, active_count=1, extent=0, base_scale=1` (`:1293-1303`), Acceleration → `node.uniform_vector_field` with `x=y=z=0` (`:1306-1309`); SceneMin/Max context → `node.compose_vec3` (`:1471`) | When a stage reads an endpoint the host never wired, the compiler mints the identity element for that endpoint | three scene/water/image constructors | scene, water, image |

Not generators (STAYS, audit verdict): `endpoint_port`/`endpoint_scope` (`compiler.rs:46-65`) map core's `SceneEndpoint` to the host scene's port names — a core-owned contract (`manifold-core/src/scene_modifier_preset.rs:287`); `resolve_camera_anchor` (`:1587`) uses `PortType::Camera` only; `namespace.rs`, `routes.rs`, `bindings.rs`, `impulses.rs`, `event_state.rs`, `control_state.rs`, `value_sources.rs` are generic.

### 1.2 Mechanisms that exist

| Piece | Where | Shape | Verdict |
|---|---|---|---|
| Family-owned graph augmentation | `load/augmentation.rs:13-16` `RelightAugmentation { augment: fn(&EffectGraphDef, &PrimitiveRegistry, &RelightParams) -> EffectGraphDef, targets }`, `inventory::collect!`; exactly one provider asserted (`:18-24`); submitted at `manifold-nodes-scene/src/node_graph/relight.rs:36-41` | A family rewrites a def through a fn-pointer struct the engine collects | EXTEND — the registered form copies this shape |
| Instantiation hooks | `load/instantiation.rs:20-25` `GraphInstantiationHook { name, apply }`; run sorted by name, duplicate names asserted (`:34-43`); submitted `manifold-nodes-water/src/graph_install.rs:89-94` | Named callbacks after wires, before rules | EXTEND — ordering and uniqueness rule copied verbatim |
| Migrations at stages | `load/migration.rs:7-23` `GraphMigration { name, stage: MigrationStage, order: u16, apply }`; `ordered()` sorts by `(order, name)`; submitted from scene (`gltf_import/migration.rs:399-416`) and water (`liquid/migration.rs:303-347`, `runtime/gpu_flip_surface.rs:168`, `primitives/blob_bounds.rs:235`) | Stage enum + order + name | EXTEND — the expansion stage enum copies `MigrationStage` |
| Declared per-node rules | `exec/extent.rs:55-61` `ExtentRule { type_id, check }` beside each node in `primitives/*/extent.rs`; `DuplicateRule`/`NoRule` errors (`:66-71`) | Data row keyed by `type_id`, collected by inventory | EXTEND — declared rows copy this shape and its error pair |
| Recipe schema | `manifold-core/src/scene_modifier_preset.rs:186-204` `SceneModifierRecipe { …, stages, initializers, calibrations, shatter: Option<SceneShatterRecipe> }`; `:217-221` `SceneShatterRecipe { fragments_param, trigger_node, trigger_port }`; validated `:981-1004` | Serialized; the `shatter` field is the only feature flag | KEEP — no schema change; gains one non-serialized helper (D6) |
| Math View recipe test | `manifold-core/src/scene_modifier_math_view.rs` `is_math_view_recipe`, `CONTROLS`, `MAX_MATH_VIEW_MODIFIERS` | Core knows a Math View by its control nodes | KEEP |
| Liquid domain predicate | `manifold-core/src/liquid_domain.rs:23,94` `is_liquid_domain`, `liquid_domain_of` | Core, by design (audit STAYS) | KEEP |
| Builder internals exposed to tests | `compiler.rs:22-35,551-570,675-710` `testkit_visible!` on `PortAddress`, `EndpointKey`, `Builder`, `current_for_test`, `set_current_for_test`, `attachment_key` | The chain-state table is already reachable from `manifold-nodes/tests/.../expand_compiler_tests.rs` (20 tests) | EXTEND — becomes the `ChainView` the Math View plug-in reads (section 3.3) |
| Namespacing | `namespace.rs:14` `namespace_node_id(parts)`, `:26` `clone_template_node`; `pub(super)` | Stable generated ids | EXTEND — `pub` for plug-ins (compiler-driven: the move fails to compile until it is) |
| Family registration crate | `manifold-nodes/src/lib.rs:6-9` links image, scene, water (`use … as _`); header: "tests that span families" | The only crate below the app that sees two families | The home for two-family plug-ins (WATER_CRATES D4) |

### 1.3 Proofs that exist

| Proof | Where | Covers |
|---|---|---|
| Expand contracts | `manifold-nodes/tests/contracts/node_graph/catalog_tests/expand_{shatter 4, acceleration 1, frames 5, impulses 4, parameter_guards 1, compiler_tests 20, compiler_conformance 7, compiler_parameter_guard_tests 4}.rs` | Shape of every generator's output on hand-built fixtures |
| Stock files prepare | `manifold-nodes/tests/scene_modifier_stock.rs:173` `all_stock_files_prepare_through_canonical_host_path` (7 of the 18 bundled recipes), `scene_modifier_inv_gate.rs`, `scene_modifier_fragment_masks.rs`, `scene_force_presets.rs` | Bundled recipes attach and prepare; no byte-level snapshot of the prepared def exists today (`rg -n "sha256\|snapshot\|golden" manifold-nodes/tests/scene_modifier_*.rs` → 0) |
| Pre-launch validator | `manifold-nodes/src/bin/check_presets.rs` | Every `assets/scene-modifier-presets/*.json` (18 files: 7 fragment, 3 force, 1 Shatter, 1 Math View, 6 plain) loads and prepares |
| Shatter on the GPU | `manifold-app/tests/renderer_contracts/gpu_proofs/physics_solids.rs:26` `physics_imported_flower_shatter_release_preserves_authored_row_and_materials` | The tiger-lily import, physics enabled, Shatter attached, 120 frames, release, materials preserved |
| Fragments on the GPU | `manifold-app/tests/renderer_contracts/gpu_proofs/rt_dynamic_catalog.rs` (calls `prepare_scene_modifiers`) | Fragment stages under ray tracing |
| Water forces on the GPU | `manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs` (calls `prepare_scene_modifiers`) ⚠ VERIFY-AT-IMPL: `rg -n "prepare_scene_modifiers" crates/manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs` names the member |
| Engine word ratchet | `manifold-app/tests/crate_layering.rs:134` `ENGINE_WATER_WORD_FILES = 54`, `engine_water_vocabulary_only_shrinks` | Water words in engine files only go down |

### 1.4 Classification

*Exists:* the inventory pattern (four precedents), stage ordering (`MigrationStage`), name uniqueness (`instantiation.rs`), declared per-node rows (`ExtentRule`), the registration crate, every test fixture. *One wire away:* `namespace_node_id`, `selected_objects`, `Builder` chain-state access are `pub(super)`/testkit-only and become `pub`. *Genuinely new:* one registered plug-in collection, three declared row collections, one prepared-graph snapshot test, one core helper that names a recipe's features. Zero new id schemes, zero new shared state, zero new threads.

---

## 2. Decisions

**D1 — Two plug-in forms, declared preferred.** A *declared* plug-in is a `&'static` data row the engine collects by inventory and reads (no family code runs during expansion; checkable at first use; the engine owns every error message). A *registered* plug-in is a fn-pointer struct the engine calls at a fixed stage of the pipeline. Every generator in section 1.1 is assigned one form in section 3.2; a generator takes the registered form only where section 3.5 shows data cannot express it. Rejected: *registered-only* — the force grammar and identity producers are pure tables, and a table the engine validates at first use is safer than code the engine trusts. Rejected: *declared-only (templates for everything)* — Shatter's slot allocation, material distribution and parameter inheritance, the fragment pass's lineage walk, and Math View's chain capture are algorithms over the live graph; a template language rich enough to express them is a second compiler (section 3.5).

**D2 — One registered collection, `SceneModifierExpansion`, shaped like `RelightAugmentation` + `GraphInstantiationHook`.** Name-sorted, unique names asserted, fixed stage enum. Signature in section 3.1. Rejected: *one trait object per feature with `dyn` dispatch* — fn-pointer structs are the house shape for every inventory callback in the engine (`rg "inventory::collect!" crates/manifold-node-engine/src` → 19, all structs of `fn` pointers or data). Rejected: *extending `GraphMigration` with a new `MigrationStage`* — migrations take `&mut EffectGraphDef` alone and return `bool`; expansion needs the scene index, routes, registry and binding sources and returns typed errors. Forcing that through `GraphMigration` would make its contract lie.

**D3 — Three declared collections, shaped like `ExtentRule`: `IdentityProducer`, `ForceRecipient`, `MeshFrameRule`.** Each is a `type_id`-keyed (or slot-keyed) static row submitted beside the node it describes. Rejected: *one `SceneModifierDeclaration` enum collection* — one grab-bag collection is shorter to declare and harder to read; three rows with three error pairs copy `ExtentRule` exactly. Rejected: *fields on `primitive!`/`NodeDescriptor`* — `NodeDescriptor` (`descriptor.rs:186-206`) is documentation metadata ("Documentation / AI-composition metadata"); loading behaviour into it couples the catalog generator to expansion. Rejected: *reading `mesh_output_rule` for the fragment node roles* — only 5 scene primitives implement it (`rg -l mesh_output_rule crates/manifold-nodes-scene/src` → 5), the 19-name list needs all of them; see Deferred.

**D4 — Homes follow WATER_CRATES_DESIGN D4.** A plug-in naming one family's ids lives beside that family's nodes; a plug-in naming two families' ids lives in `manifold-nodes`. Shatter (water + scene) and Math View (scene + image) → `manifold-nodes/src/scene_modifiers/`. Fragment cuts, mesh frame rules, the scene identity rows → `manifold-nodes-scene`. Force recipient rows and the acceleration identity row → `manifold-nodes-water` (the rigid crate after the carve). The `node.compose_vec3` identity row → `manifold-nodes-image`. Rejected: *Math View in `manifold-nodes-scene` treating `node.mix`/`node.set_alpha` as "just atoms"* — D4 is a mechanical rule precisely so homes are never argued; two image ids are two image ids. Consequences, stated honestly: the Math View plug-in sits two crates away from `node.render_mesh_diagram`, whose owner will most often be the one fixing it. The cost is a `cargo check -p manifold-nodes` instead of `-p manifold-nodes-scene` for that fix.

**D5 — Fixed stage order, name order within a stage.** `ExpansionStage::{Diagnostics, MeshLayout, Bodies}` runs in that order after routes are built and before `into_graph` — exactly today's sequence `math_events::prepare` (`compiler.rs:471`) → `fragment_cuts::apply` (`:475`) → `shatter::prepare` (`:476`). Within a stage, name order; the engine asserts unique names once at first use. Rejected: *a numeric `order` field like `GraphMigration`* — three stages with one plug-in each is the whole known population; a number invites tuning instead of naming the dependency.

**D6 — A recipe feature is claimed by exactly one plug-in, and a feature nobody claims is an error.** Core gains `SceneModifierRecipe::features(&self) -> impl Iterator<Item = &'static str>` (`"shatter"` when `shatter.is_some()`, `"mathView"` when `is_math_view_recipe`). Each `SceneModifierExpansion` names the one feature it `claims` (or `None` for a graph-shape plug-in like fragment cuts, which claims nodes through `owns_node`). At first use the engine asserts no two plug-ins claim one feature. During preparation, a recipe whose feature has no registered plug-in fails with `InvalidRecipe { path: "<instance>.presetMetadata.sceneModifier.<feature>", detail: "no scene-modifier plug-in registered for '<feature>'; the binary does not link the crate that owns it" }`. Rejected: *skip silently when no plug-in is linked* — that is the silent-fallback forbidden move; a Shatter that prepares to an intact graph in a binary missing `manifold-nodes` would hide a linking bug the way RENDERER_CRATE_SPLIT D6 warns against.

**D7 — Nothing changes on stage, and a machine says so.** Before stage 1 lands, a new test records a SHA-256 of the canonical JSON of `PreparedSceneModifierGraph { def, routes, binding_sources }` for every bundled scene-modifier preset on the canonical hosts, plus the Shatter and force fixtures of the expand contracts. Every stage asserts equality against the committed file. Re-baselining needs Peter's yes in the bead and is done with one named env var, never by editing the file. Rejected: *relying on the expand_* contracts alone* — they assert shapes ("one remap", "four pieces"), not identity; a reordered wire or a renamed generated id passes them and still changes fusion grouping. Section 5 (Proof) has the mechanics.

**D8 — Compiler-driven moves, no parallel path, no re-export.** Each stage deletes the engine file (or block) and submits the plug-in in the same commit; the build errors are the checklist; the stage's negative gate is `rg` zero hits for the moved ids in `crates/manifold-node-engine/src/load/expand/`. RENDERER_CRATE_SPLIT D4 applies: no `pub use` of a moved item from the engine.

**D9 — The runtime-side fragment grammar (budget, value writes, parameter guards) reads the plug-in's `owns_node` and `namespace`, not type ids.** `buffer_budget.rs:121-124` already falls back to the `fragment_cut` namespace prefix for fused members (`:128-130`); it uses the prefix for everything. `value_writes.rs:174-186` already resolves the cutter by its namespaced id and checks the parameter exists; the type-id match is redundant and goes. `parameter_guards.rs:85-110` asks the plug-ins `owns_node(type_id)` for guard roots. Rejected: *a second declared row "GeneratedNodeKinds"* — the plug-in that generates the nodes is the authority on which nodes it generated; the namespace it declares already encodes that.

---

## 3. Design body

### 3.1 The seam — committed types

File: `crates/manifold-node-engine/src/load/expand/expansion.rs` (new, engine-owned, generic).

```rust
//! Registered scene-modifier plug-ins. The engine owns the pipeline; a family
//! owns every rewrite that names its nodes. Shape: load/augmentation.rs +
//! load/instantiation.rs (name-sorted, unique names asserted at first use).

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::SceneEndpoint;
use crate::persistence::PrimitiveRegistry;
use super::{SceneModifierExpandError, SceneModifierNodeRoute};
use super::bindings::SceneModifierBindingSource;

/// Fixed pipeline positions after routes are built and before `into_graph`.
/// Order is the enum order. (Today: math_events → fragment_cuts → shatter.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExpansionStage { Diagnostics, MeshLayout, Bodies }

/// Everything a plug-in may read. Nothing here is mutable but `def` and
/// `binding_sources`, which it receives separately.
pub struct ExpansionContext<'a> {
    pub owner: &'a EffectGraphDef,
    pub index: &'a FlatSceneIndex,
    pub routes: &'a [SceneModifierNodeRoute],
    pub registry: &'a PrimitiveRegistry,
    /// `Some` when preparing a Math View request; plug-ins that keep the
    /// sparse diagnostic graph intact return early (today: compiler.rs:474).
    pub math_view: Option<&'a manifold_core::NodeId>,
}

/// How a feature's instances capture mesh frames (frames.rs:79,174-183 today).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameCapture {
    /// Static calibration against the authored transform (the default rule).
    StaticTransform,
    /// Follow the live pose; capture only the source's own translate.
    SourceTranslate,
}

pub type ExpansionApply = fn(
    &ExpansionContext<'_>,
    &mut EffectGraphDef,
    &mut Vec<Option<SceneModifierBindingSource>>,
) -> Result<(), SceneModifierExpandError>;

pub struct SceneModifierExpansion {
    /// Unique across the binary; sorts the stage.
    pub name: &'static str,
    /// The recipe feature this plug-in alone may serve (D6), or `None` for a
    /// graph-shape plug-in that claims nodes through `owns_node`.
    pub claims: Option<&'static str>,
    pub stage: ExpansionStage,
    /// Namespace head for every node this plug-in generates
    /// (`namespace_node_id(&[namespace, ..])`). Budget and value-write code
    /// identify generated nodes by this prefix (D9).
    pub namespace: &'static str,
    /// Authored node types whose layout this plug-in rewrites. Used by
    /// parameter guards and by the legacy no-instance path to decide whether
    /// the pass must run at all.
    pub owns_node: fn(type_id: &str) -> bool,
    /// True when this owner graph needs the pass (today: `contains_fragments`,
    /// `recipe.shatter.is_some()`, `is_math_view_recipe`).
    pub applies: fn(owner: &EffectGraphDef) -> bool,
    pub frame_capture: FrameCapture,
    pub apply: ExpansionApply,
    /// Chain hooks for a plug-in that must observe the per-instance endpoint
    /// chain (Math View). `None` for every other plug-in.
    pub chain: Option<ChainHooks>,
}
inventory::collect!(SceneModifierExpansion);
```

Chain hooks exist for Math View alone (section 3.3). Their shape is committed at the signature level and marked for verification because stage 4 is last:

```rust
/// Read/write access to the compiler's endpoint chain state for one
/// instance. Backed by `Builder` (compiler.rs:553); the testkit methods
/// `current_for_test`/`set_current_for_test` become these.
pub struct ChainView<'b, 'a> { /* private: &'b mut Builder<'a> */ }
impl ChainView<'_, '_> {
    pub fn current(&self, key: &EndpointKey) -> Option<PortAddress>;
    pub fn reference(&self, key: &EndpointKey) -> Option<PortAddress>;
    pub fn set_current(&mut self, key: EndpointKey, value: Option<PortAddress>);
    pub fn attachment_key(&mut self, instance: &SceneModifierInstanceDef, target: Option<&SceneNodeRef>, endpoint: SceneEndpoint) -> Result<EndpointKey, SceneModifierExpandError>;
    pub fn add_node(&mut self, parts: &[&str], type_id: &str, title: Option<&str>, params: BTreeMap<String, SerializedParamValue>) -> Result<u32, SceneModifierExpandError>;
    pub fn wire(&mut self, from: PortAddress, to: u32, port: &str);
    pub fn derived(&self) -> &EffectGraphDef;
}
pub struct ChainHooks {
    /// Before the first instance of the requested view's scene (compiler.rs:347-370).
    pub seed: fn(&mut ChainView<'_, '_>, &SceneModifierInstanceDef, &[SceneNodeRef]) -> Result<(), SceneModifierExpandError>,
    /// At the view's (or legacy carrier's) position (compiler.rs:373-400).
    pub capture: fn(&mut ChainView<'_, '_>, &SceneModifierInstanceDef, &[SceneNodeRef]) -> Result<(), SceneModifierExpandError>,
    /// After endpoint writes are applied (compiler.rs:431-433).
    pub finish: fn(&mut ChainView<'_, '_>, &EffectGraphDef, &BTreeMap<String, LeafMap>) -> Result<(), SceneModifierExpandError>,
}
```
⚠ VERIFY-AT-IMPL (stage 4): the exact `ChainView` method list is whatever `rg -n "self\.(current|reference|written|camera_anchors|contexts|math_|derived|next_id)" crates/manifold-node-engine/src/load/expand/compiler.rs` shows the Math View methods touching at that time; the list above is the 2026-10-10 reading. Adding a method is in scope; adding a second chain-hook consumer is not.

Declared rows, same file:

```rust
/// Which node the compiler mints when a stage reads an endpoint the host never
/// wired (compiler.rs:1270-1325) or a vector context (compiler.rs:1471).
pub struct IdentityProducer {
    pub slot: IdentitySlot,
    pub type_id: &'static str,
    pub params: &'static [(&'static str, SerializedParamValue)],
    pub port: &'static str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdentitySlot { Endpoint(SceneEndpoint), SceneBoundsVector }
inventory::collect!(IdentityProducer);

/// A node whose outputs name physical bodies that take scene acceleration and
/// impulses (acceleration.rs:117-134, 284-302 today). Liquid domains need no
/// row: they declare an `acceleration_field` input and the registry already
/// resolves them (acceleration.rs:81).
pub struct ForceRecipient {
    pub type_id: &'static str,
    pub rows: &'static [RecipientRow],
}
pub enum RecipientRow {
    /// `{output_prefix}{n}` ⇄ `{body_prefix}{n}` / `{acceleration_prefix}{n}`
    /// for `n < capacity`; impulses hit body bit `n`.
    Slots { output_prefix: &'static str, body_prefix: &'static str, acceleration_prefix: &'static str, capacity: usize },
    /// One output ⇄ one body input / one acceleration input; impulses hit copies.
    Copies { output: &'static str, body: &'static str, acceleration: &'static str },
}
inventory::collect!(ForceRecipient);

/// How frame capture reads a vertex source or a transform (frames.rs:271-372 today).
pub struct MeshFrameRule { pub type_id: &'static str, pub role: FrameRole }
pub enum FrameRole {
    /// A static imported mesh: fresh capture allowed when `fit` is unset or 0.
    StaticSource { translate: [&'static str; 3], fit: &'static str, bbox_radius: &'static str },
    /// Animated or skinned: never a fresh capture.
    AnimatedSource,
    /// An unwired static translation when rotation is 0, scale is 1, billboard off.
    StaticTransform { position: [&'static str; 3], rotation: [&'static str; 3], scale: [&'static str; 3], billboard: &'static str },
}
inventory::collect!(MeshFrameRule);
```

Lookup is one `LazyLock<BTreeMap<..>>` per collection built at first use (precedent: `manifold-nodes-water/src/node.rs` `PhysicsNode` registry); duplicates fail there with the `ExtentRule` error pair (`DuplicateRule`/`NoRule` → `SceneModifierExpandError::InvalidRecipe` with the colliding names). Immutable static data is not the shared state CLAUDE.md forbids.

Visibility changes in the engine (compiler-driven; each is red until flipped): `namespace::namespace_node_id` and `clone_template_node` → `pub`; `frames::selected_objects` → `pub`; `compiler::{PortAddress, EndpointKey}` → `pub` (drop `testkit_visible!`); `SceneModifierBindingSource` → `pub`; `SceneModifierExpandError` constructors already `pub`.

### 3.2 Assignment — every generator, its form, its home

| Generator (section 1.1) | Form | Plug-in | Home |
|---|---|---|---|
| Shatter | Registered, `Bodies`, claims `"shatter"`, namespace `"shatter"`, `frame_capture: SourceTranslate` | `scene_modifiers::shatter` — `compiler/shatter.rs` moved whole; its `control`/`generated` helpers come with it | `manifold-nodes/src/scene_modifiers/shatter.rs` |
| Force recipients | Declared `ForceRecipient` | `ForceRecipient { type_id: "node.physics_world", rows: &[Slots { "pose_", "body_", "body_acceleration_", RigidImpulseTargets::BODY_CAPACITY }, Copies { "instances", "copies", "copies_acceleration" }] }` | beside `primitives/physics_world.rs` in `manifold-nodes-water` (→ `manifold-water-rigid` after the carve) |
| Fragment cuts | Registered, `MeshLayout`, claims `None`, namespace `"fragment_cut"`, `owns_node` = `is_fragment`, `applies` = `contains_fragments` | `scene_modifier_fragments` — `fragment_cuts.rs` moved whole with its two unit tests; the `is_mesh_unary`/`is_mesh_multi`/`is_weight_source` lists move with it (Deferred: declared roles) | `manifold-nodes-scene/src/node_graph/scene_modifier_fragments.rs` |
| Fragment grammar, runtime side | Engine code re-keyed to `namespace`/`owns_node` (D9) | — | stays in engine, zero family ids |
| Mesh frame capture | Declared `MeshFrameRule` × 3 | `StaticSource` on `node.gltf_mesh_source` (`translate_x/y/z`, `fit`, `source_bbox_radius`), `AnimatedSource` on `node.gltf_skinned_mesh_source`, `StaticTransform` on `node.transform_3d` (`pos_*`, `rot_*`, `scale_*`, `billboard`); the Shatter special case becomes `frame_capture` on the Shatter plug-in | beside each primitive in `manifold-nodes-scene` |
| Math View | Registered, `Diagnostics`, claims `"mathView"`, namespace `"math_view"`, `chain: Some(..)` | `scene_modifiers::math_view` — `math_events.rs` becomes `apply`; `seed_math_view`/`capture_math_view_input`/`finish_math_view`/`math_control_sources`/`wire_math_controls`/`add_math_node`/`compose_math_diagrams` become the three chain hooks | `manifold-nodes/src/scene_modifiers/math_view.rs` |
| Endpoint identity producers | Declared `IdentityProducer` × 4 | `Endpoint(Transform)` → `node.transform_3d` and `Endpoint(Instances)` → `node.arrange_copies` in scene; `Endpoint(Acceleration)` → `node.uniform_vector_field` in water; `SceneBoundsVector` → `node.compose_vec3` in image | beside each primitive |

The engine after all four stages: `compiler.rs` keeps the pipeline, `Builder`, `endpoint_input` (now a row lookup), `context` (now a row lookup for the vector case), `resolve_camera_anchor`, `append_instance`, `validate_binding_leaves`, `preflight_expansion`; `acceleration.rs` keeps `resolve`/`trace`/`selected`/`impulse_recipients` reading rows; `frames.rs` keeps `selected_objects`, `validate_saved_frames`, `resolve_modifier_mesh_frames` reading rows and `frame_capture`. Negative oracle for the whole design: `rg -n '"node\.(physics_world|gltf_|cut_mesh|remap_|ordered_recon|transform_mesh_patches|render_mesh_diagram|mesh_spatial_mask|sample_mesh_triangles|arrange_copies|uniform_vector_field|transform_3d|compose_vec3|set_alpha|mix)"' crates/manifold-node-engine/src/load/expand/` → 0 (excluding `#[cfg(test)]` bodies, which the ratchet in section 4 excludes too).

### 3.3 The pipeline, with the plug-in points marked

`prepare_scene_modifiers_impl` (compiler.rs:165) keeps its order. Plug-in points, in place of today's hard-wired calls:

1. Capacity preflight, schema validation, standalone-recipe refusal (`:175-209`) — unchanged.
2. **No-instance path** (`:210-230`): today `if !contains_fragments(owner) { return clone }`. Becomes: if no registered plug-in with `claims == None` has `applies(owner)`, return the clone; else flatten and run the `MeshLayout` stage only. Generic: the engine asks, the plug-in answers.
3. **Feature check** (new, D6): for every instance, every `recipe.features()` entry must match exactly one plug-in's `claims`; otherwise the D6 error.
4. Index, preflight, Math View target resolution (`:253-267`) — unchanged.
5. Per-instance loop (`:295-403`): frames validation (now reads `frame_capture` of the claiming plug-in, default `StaticTransform`); force targets via `acceleration::selected` (rows); **`chain.seed`** at the position of `:347-370` and **`chain.capture`** at `:373-384`, for the plug-in whose `chain` is `Some` and whose `claims` matches the requested view; `append_instance` unchanged (`endpoint_input` reads `IdentityProducer`).
6. Endpoint writes (`:410-430`) — unchanged. **`chain.finish`** at `:431-433`.
7. Bindings, flatten, index, local defaults, routes (`:436-470`) — unchanged.
8. **Stages** (`:471-477`): build `ExpansionContext`; for each stage in enum order, for each plug-in in that stage in name order, `if (applies)(owner) { (apply)(&ctx, &mut prepared, &mut binding_sources)? }`. The `math_view.is_none()` guard that today skips fragments and shatter (`:474`) moves into those two plug-ins' `apply` as an early return on `ctx.math_view.is_some()` — the plug-in knows why it keeps the sparse graph; the engine does not.
9. Capacity recheck, `into_graph`, impulses, binding-leaf validation, graph validation (`:478-491`) — unchanged.

`validate_modifier_runtime` (`:69`), `validate_modifier_attachment` (`:99`), `prepare_scene_modifier_math_view` (`:140`) keep their signatures; callers in `graph_loader.rs:479`, `modifier_runtime.rs:102`, `loaded_preset_view.rs:102`, the app (`scene_modifier_edit.rs`, `scene_modifier_journey.rs`, `scene_modifier_transfer.rs`, `ui_bridge/projection/cards.rs`) and `check_presets.rs:38` are untouched.

### 3.4 Homes and dependency direction

| Crate | Gains | Depends on (unchanged) |
|---|---|---|
| `manifold-node-engine` | `load/expand/expansion.rs`; loses `compiler/shatter.rs`, `compiler/math_events.rs`, `fragment_cuts.rs`, the Math View methods of `compiler.rs`, the grammar blocks of `acceleration.rs` and `frames.rs` | core, foundation, gpu, playback |
| `manifold-nodes-scene` | `node_graph/scene_modifier_fragments.rs` (+ `inventory::submit!`), three `MeshFrameRule` and two `IdentityProducer` submits beside `gltf_mesh_source.rs`, `gltf_skinned_mesh_source.rs`, `transform_3d.rs`, `arrange_copies.rs` | engine |
| `manifold-nodes-water` / `manifold-water-rigid` | one `ForceRecipient` beside `physics_world.rs`; one `IdentityProducer` beside the vector-field source | engine |
| `manifold-nodes-image` | one `IdentityProducer` beside `compose_vec3` | engine |
| `manifold-nodes` | `src/scene_modifiers/{mod,shatter,math_view}.rs` with their submits; `lib.rs` gains `pub mod scene_modifiers;` | engine + three families (already) |

No crate gains a dependency. The plug-ins name the other family's nodes by type-id string and port name, exactly as the engine does today; they never import a family crate's Rust items (INV-P3). If the water carve lands first, `physics_world.rs` is in `manifold-water-rigid` and the row goes there; the design is indifferent (WATER_CRATES D6: registration lives with the type it registers).

Consequences, stated honestly: a test binary that links the engine but not `manifold-nodes` cannot prepare a Shatter or Math View recipe (D6 error) and cannot mint an endpoint identity (NoRule error). Today no such binary does — the expand contracts live in `manifold-nodes/tests` and every caller of `with_builtin` links the families (RENDERER_CRATE_SPLIT D6). The error names the missing crate so the first one to try learns in one line.

### 3.5 Where declared templates cannot express a generator

Stated plainly, per generator, so nobody re-derives it:

- **Shatter**: the piece count is a live control read from the prepared route (`shatter.rs:134`); body slots are allocated from whatever the host left free (`:206-233`); pieces are distributed to material parts by vertex-count weight (`:250-261`); `max_capacity` per piece is a halving bound (`:307-319`); density inherits from the parent or the registered default (`:192-202`); the scene's `object_N` port is renumbered and the `objects` count rewritten (`:399-450`); bindings targeting the original are duplicated to every piece (`:416-431`). A template that can say "clone these three nodes N times, wire them so" cannot say any of those seven things without becoming a language.
- **Fragment cuts**: a lineage is computed by a topological walk over the whole flattened graph and remaps are inserted where lineages differ (`fragment_cuts.rs:205-347`). Templates add nodes at known anchors; this pass decides anchors from the walk.
- **Math View**: the capture reads the compiler's chain state at a position between instances (`compiler.rs:786-857`); no data form can observe compiler state.
- **Force recipients, identity producers, mesh frame rules**: pure tables — declared.

### 3.6 Ordering, determinism, errors

- Stage order is the enum; plug-in order within a stage is `name`; `inventory::iter` order is irrelevant. Same-named plug-ins panic at first use with both names (programming error, caught by `expansion_names_unique` in section 4 before any binary ships).
- Two plug-ins claiming one feature: panic at first use naming both (same class). A feature with no plug-in: `InvalidRecipe` to the user (D6), surfaced through the existing attach/load error path (`validate_modifier_attachment` is what the editor calls before accepting an attachment: `manifold-app/src/scene_modifier_edit.rs`).
- A plug-in that writes a node id already present fails the same way today's code does (`shatter.rs:52` "duplicate generated identity", `fragment_cuts.rs:93`); `FlatSceneIndex::build(&flat)` after the stages (`compiler.rs:468`) remains the collision backstop for authored-vs-generated ids.
- A bad declared row (duplicate `type_id`, two rows for one `IdentitySlot`, a `Slots` row with `capacity == 0`, a `StaticSource` naming a param the primitive does not declare) fails at first use with the row's `type_id` and the colliding names. Row-vs-registry checks (param names exist on the primitive) run once in a test over `PrimitiveRegistry::with_builtin`, not per preparation (section 4, INV-P5).
- Determinism: every plug-in receives `BTreeMap`-ordered inputs and produces ids through `namespace_node_id`; nothing reads `HashMap` iteration order (negative gate in section 4).

---

## 4. Invariants and enforcement

| Invariant | Enforcement |
|---|---|
| INV-P1 — The engine's `load/expand/` spells no family node id or family port grammar. | New ratchet in `manifold-app/tests/crate_layering.rs`: `engine_expand_family_ids_only_shrink` counts matches of the section 3.2 negative-oracle regex outside `#[cfg(test)]`; pinned at today's count before stage 1, lowered per stage, asserted `== 0` after stage 4. Same shape as `engine_water_vocabulary_only_shrinks` (`:134-171`). |
| INV-P2 — Prepared graphs are byte-identical before and after every stage. | `manifold-nodes/tests/scene_modifier_prepared_snapshots.rs` (section 5) against the committed `tests/fixtures/scene-modifiers/prepared_snapshots.txt`. |
| INV-P3 — A plug-in imports no other family crate. | Cargo: `manifold-nodes-scene` and `manifold-nodes-water` have no edge to each other (`crate_layering.rs` `LAYERS`); `manifold-nodes` plug-ins are checked by `rg -n "use manifold_nodes_(scene|water|image)" crates/manifold-nodes/src/scene_modifiers/` → 0, asserted in `scene_modifier_plugins_name_nodes_by_id_only` (manifold-nodes test). |
| INV-P4 — Plug-in names and feature claims are unique; every core feature has a plug-in. | `manifold-nodes/tests/scene_modifier_plugins.rs::expansion_names_unique`, `::every_recipe_feature_has_one_plugin` (iterates `inventory::iter::<SceneModifierExpansion>` and the core feature list `["shatter", "mathView"]`). |
| INV-P5 — Declared rows name real primitives and real params. | `::declared_rows_match_registry`: for each `ForceRecipient`/`MeshFrameRule`/`IdentityProducer`, `registry.construct(type_id)` succeeds and every named param/port exists; for `IdentityProducer`, `params` are accepted by the primitive's `parameters()` types. |
| INV-P6 — No plug-in depends on hash iteration order. | Negative gate per stage: `rg -n "HashMap|HashSet|AHashMap|AHashSet" <plug-in files>` → 0 (the engine's own `namespace.rs:1` `HashSet` is a preflight set, never iterated into output). |
| INV-P7 — The old path is gone, not paralleled. | Per stage: `rg -n "mod shatter|mod math_events|mod fragment_cuts|fn seed_math_view|fn finish_math_view" crates/manifold-node-engine/src/load/expand/` → 0 for the moved items; no `pub use` of a moved symbol in the engine (`rg -n "pub use .*(shatter|math_events|fragment_cuts)" crates/manifold-node-engine/src` → 0). |

---

## 5. Proof — nothing changes on stage

**The snapshot test** (`manifold-nodes/tests/scene_modifier_prepared_snapshots.rs`, stage 1 deliverable, recorded on main *before* stage 1's move commit):

- Hosts: the synthetic cube host of `scene_modifier_stock.rs:51` and the mushroom import (`assemble_import_graph(MUSHROOM_FIXTURE)`, `:22`), as that file already builds them; the Shatter fixture of `expand_shatter.rs:4` (a hand-built `node.physics_world` graph — no GPU, no editing crate); the force fixtures of `expand_acceleration.rs:11` and `expand_impulses.rs:21`.
- For every file in `assets/scene-modifier-presets/` (18 today; the test globs the directory so a new preset fails until its hash is recorded): attach through `prepare_new_scene_modifier` + `insert_scene_modifier` on each host where the recipe prepares today (a recipe that cannot attach to a host records `unattachable` for that pair, so the matrix is total), `prepare_scene_modifiers`, serialize `def` + `routes` + `binding_sources` with `serde_json::to_vec` on a `BTreeMap`-ordered form, SHA-256, compare to the committed line `"<preset>@<host>" = <hex>`. Math View presets additionally snapshot `prepare_scene_modifier_math_view`.
- `MANIFOLD_RECORD_PREPARED_SNAPSHOTS=1` rewrites the file; the bead for the stage records Peter's yes when any hash legitimately changes (none should in this design).
- Why hashes, not stored JSON: the prepared defs are tens of thousands of nodes for the fragment presets on the mushroom; hashes keep the fixture one line per pair. A mismatch prints both serialized forms to the scratchpad for diffing (the test writes `before.json`/`after.json` on failure).

**The expand contracts** (section 1.3) run unchanged at every stage: they are the shape proofs; the snapshot is the identity proof.

**GPU proofs, one run each through `scripts/gpu_queue.py`**, confirming the stage closest to pixels:

| Stage | Proof |
|---|---|
| 1 (Shatter, forces) | `physics_imported_flower_shatter_release_preserves_authored_row_and_materials` (`manifold-app` `renderer_contracts`, `gpu_proofs/physics_solids.rs:26`); the one `liquid_conformance` member that prepares a force modifier (section 1.3 ⚠) |
| 2 (fragments) | one `rt_dynamic_catalog` member that prepares a fragment preset ⚠ VERIFY-AT-IMPL: `rg -n "fn .*fragment\|SurfacePeel\|OrderedRecon" crates/manifold-app/tests/renderer_contracts/gpu_proofs/rt_dynamic_catalog.rs` |
| 3 (frames) | none — frame capture is CPU; `expand_frames.rs` (5) + snapshots are the oracle. `Demo: none — L1`. |
| 4 (Math View) | `manifold-nodes/tests/scene_modifier_fragment_masks.rs` (CPU) + snapshots; GPU: the Math View preset thumbnail through `fluid_capture OUT_DIR --preset` is an artifact Peter looks at (L2) |

---

## 6. Phasing

One stage = one lane session, one commit, landed green on its own. Every stage: read-back first (this doc sections 2–3 plus the files named), re-run the anchors it depends on (`rg -n "fn prepare\|fn apply\|fn resolve" <file>` must find them at the lines in section 1.1 or the lane stops and lists the drift), batch the work, verify once at the end, `cargo clippy -p <touched>`, `scripts/landing_gate.py` at landing. Forbidden moves for every stage: a `pub use` from the engine of a moved item; keeping the engine copy "for now"; a `match type_id` fallback in the engine when no row is found; widening into god-file splits of `compiler.rs` (that is the rendering epic's own bead); re-baselining a snapshot without Peter's yes in the bead.

### Stage 1 — the seam, Shatter and the water rows (BUG-hkbdp.6.15 items 7 and 10)

*Entry:* main after `dc1a0ee56`; `crates/manifold-node-engine/src/load/expand/compiler/shatter.rs` exists with `prepare` at `:112`; `acceleration.rs:117-134` holds the `physics_world` grammar.
*Deliverables:* `expansion.rs` with every type in section 3.1 except `ChainHooks` (declared as the struct with its three fn fields, no consumer yet — the field exists so stage 4 adds no engine type); `SceneModifierRecipe::features()` in core; the four `IdentityProducer` rows (scene ×2, water, image); the `ForceRecipient` row beside `physics_world.rs`; `manifold-nodes/src/scene_modifiers/{mod.rs, shatter.rs}` with `shatter.rs` moved whole (`git mv`) and submitted with `claims: Some("shatter")`, `stage: Bodies`, `frame_capture: SourceTranslate`; `compiler.rs` points 2, 3, 8 of section 3.3 (fragments and Math View still hard-wired at their positions, called around the stage loop — explicitly a two-commit interim inside this stage's landing, not a kept path: the stage's negative gate covers shatter only); `frames.rs:98,174-183` read `frame_capture` from the claiming plug-in; `acceleration.rs` reads rows; the snapshot test recorded as the first commit of the branch, before any move; INV-P1 ratchet pinned; INV-P4/P5 tests.
*Gate (positive):* `cargo nextest run -p manifold-nodes expand_shatter expand_acceleration expand_impulses scene_modifier_prepared_snapshots scene_modifier_plugins scene_modifier_stock scene_force_presets`; `cargo nextest run -p manifold-node-engine load::expand`; `cargo run -p manifold-nodes --bin check_presets -- scene-modifier` ⚠ VERIFY-AT-IMPL the subcommand spelling (`sed -n 60,120p crates/manifold-nodes/src/bin/check_presets.rs`); `crate_layering` green with `ENGINE_WATER_WORD_FILES` lowered by the files that lost water words (`shatter.rs`, `acceleration.rs`: expect 52) and the new ratchet pinned; one `gpu_queue.py` run of the two section 5 stage-1 proofs.
*Gate (negative):* `rg -n '"node\.physics_world"\|body_acceleration_\|"pose_"\|release_count\|compound_materials' crates/manifold-node-engine/src/load/expand/` → 0; `rg -n "mod shatter" crates/manifold-node-engine/src/load/expand/` → 0; INV-P6 on `manifold-nodes/src/scene_modifiers/`.
*Demo:* `Demo: none — L1` plus the GPU proof run (L2 artifact is the proof's readback diff, a number).
*Test scope:* `-p manifold-node-engine`, `-p manifold-nodes`, `-p manifold-nodes-water`, `-p manifold-nodes-scene`, `-p manifold-nodes-image` (one-line submits), `-p manifold-core` (`features()`); `crate_layering` in `manifold-app`. No workspace sweep: nothing outside these crates compiles differently.

### Stage 2 — fragment cuts to the scene family (rendering epic)

*Entry:* stage 1 on main; `fragment_cuts.rs:362 apply` and `:156 contains_fragments` present; `buffer_budget.rs:121`, `value_writes.rs:181`, `parameter_guards.rs:89` spell cut ids.
*Deliverables:* `manifold-nodes-scene/src/node_graph/scene_modifier_fragments.rs` (`git mv` of `fragment_cuts.rs`, submit with `claims: None`, `stage: MeshLayout`, `namespace: "fragment_cut"`, `owns_node: is_fragment`, `applies: contains_fragments`); `compiler.rs` point 2 generic (no-instance path asks the plug-ins); D9 re-keying of the three runtime-side files; ratchet lowered.
*Gate:* `cargo nextest run -p manifold-nodes expand_compiler scene_modifier_prepared_snapshots scene_modifier_fragment_masks scene_modifier_stock expand_parameter_guards`; `-p manifold-nodes-scene scene_modifier_fragments` (its two unit tests); negative: `rg -n '"node\.(cut_mesh|remap_|ordered_recon_mesh|transform_mesh_patches|shatter_mesh|twist_mesh|morph_targets_blend)' crates/manifold-node-engine/src/load/expand/` → 0; one `gpu_queue.py` run of the section 5 stage-2 proof.
*Demo:* L2 — the proof's readback; Peter may also open Surface Peel on the mushroom in the app (main rebuild).

### Stage 3 — mesh frame rules (rendering epic)

*Entry:* stage 2 on main; `frames.rs:271 source_route`, `:319 static_offset`, `:374 scene_radius` present.
*Deliverables:* three `MeshFrameRule` rows beside the scene primitives; `frames.rs` reads rows (an unknown source type is `UnsupportedCoordinateFrame` with today's messages, which the `expand_frames.rs` contracts pin); ratchet lowered.
*Gate:* `cargo nextest run -p manifold-nodes expand_frames scene_modifier_prepared_snapshots scene_modifier_stock`; negative: `rg -n '"node\.(gltf_mesh_source|gltf_skinned_mesh_source|transform_3d)"\|source_bbox_radius\|"billboard"' crates/manifold-node-engine/src/load/expand/` → 0. `Demo: none — L1`. Round-trip gate (standard section 5): `manifold-nodes/tests/scene_modifier_file_authoring.rs` (save → reload → frames still validate) in the filterset.

### Stage 4 — Math View (rendering epic; conformance treatment, section 3.1 ⚠)

*Entry:* stage 3 on main; `compiler.rs` Math View methods at the section 1.1 lines (re-derive: `rg -n "fn (seed_math_view|capture_math_view_input|finish_math_view|math_control_sources|wire_math_controls|add_math_node|compose_math_diagrams)" crates/manifold-node-engine/src/load/expand/compiler.rs` → 7 hits, or stop and list).
*Deliverables:* `ChainView` over `Builder` (replacing the `*_for_test` methods, which the 20 `expand_compiler_tests` then call through `ChainView` — a seam brief with the before/after signatures is written into the bead at stage start from the re-derivation above); `manifold-nodes/src/scene_modifiers/math_view.rs` (`math_events.rs` as `apply`, the seven methods as the three hooks); `compiler.rs` points 5–6 call `chain.*` of the claiming plug-in; INV-P1 ratchet reaches 0 and becomes `== 0`.
*Gate:* `cargo nextest run -p manifold-nodes expand_compiler scene_modifier_prepared_snapshots scene_modifier_fragment_masks scene_modifier_stock`; `-p manifold-app legacy_param_id_resolution` (Math View legacy carriers); negative: the section 3.2 full regex → 0; `rg -n "math_view\|MathView" crates/manifold-node-engine/src/load/expand/compiler.rs` → only the `math_view: Option<..>` request plumbing (list the surviving lines in the report).
*Demo:* L2 — Math View preset thumbnail via `fluid_capture`, Peter looks.

Phasing-completeness check: every row of section 3.2 appears in exactly one stage above; the Deferred items below are the only affordances the body names and no stage builds.

---

## 7. Decided — do not reopen

1. Two forms; declared where a table suffices, registered otherwise (D1).
2. One registered collection, fn-pointer struct, name-sorted, unique (D2). No trait objects, no `GraphMigration` reuse.
3. Three declared collections shaped like `ExtentRule` (D3). Not `NodeDescriptor`, not `primitive!` fields.
4. Homes by WATER_CRATES D4: Shatter and Math View in `manifold-nodes`; fragments and frame rules in `manifold-nodes-scene`; force rows in water (D4).
5. Stage order is the enum; Diagnostics → MeshLayout → Bodies (D5).
6. Unclaimed feature = user-facing `InvalidRecipe`; never a silent skip (D6).
7. Snapshot hashes recorded before the first move; re-baseline only with Peter's yes (D7).
8. Compiler-driven moves, no re-exports (D8).
9. Runtime-side fragment code keys on `namespace`/`owns_node` (D9).
10. The recipe schema does not change; `features()` is a helper, not a field.

## 8. Deferred

- **Declared mesh roles for the fragment pass** (`is_mesh_unary` etc. as rows beside each deformer). Trigger: a second plug-in needs the same classification, or the scene-crate design under the rendering epic carves a mesh-deformer crate and the list straddles it.
- **Declared Shatter template for the per-piece triple** (clone source/body/object with fixed wires, leaving only the algorithmic parts in code). Trigger: a second modifier needs "clone N objects into physics slots" — then the shared part is a template both read.
- **`node.render_scene` as the host type id in the engine** (`compiler.rs:1741`, `parameter_guards.rs:110`). It is the modifier host contract and core already resolves scenes by it (`scene_index`); naming it in core as a constant is a one-line chore when the rendering epic decides the host contract's home.
- **A second chain-hook consumer.** Trigger: a modifier other than Math View must observe chain state; then `ChainHooks` grows a `stage`/`name` and the hook loop generalises. Until then one consumer, asserted by INV-P4's feature list.

## 9. Questions only Peter can answer

1. **Math View's home**: `manifold-nodes` by D4's letter (two image atoms), or `manifold-nodes-scene` by ownership of the diagram node? The design defaults to D4; say so if you want the exception.
2. **Snapshot hosts**: is the mushroom import the right photoscan for the "before" hashes, or should the tiger lily (the Shatter GPU proof's fixture) be in the matrix too? Default: mushroom + synthetic cube + the three hand-built fixtures; tiger lily only in the GPU proof.
3. **Stage 1 scope**: it touches `manifold-nodes-scene` and `manifold-nodes-image` for four one-line identity rows so the engine never carries a half table. Acceptable inside the water cleanup bead, or do you want identity rows as their own tiny stage first?
