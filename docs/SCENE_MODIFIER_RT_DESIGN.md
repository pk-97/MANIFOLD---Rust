# Scene modifier RT — one evaluated geometry for raster and rays

<!-- index: Implementation contract for automatic ray tracing of scene-modified meshes: geometry revisions, fusion, ordered BLAS updates, emissive sampling, export, and K3 phase briefs. -->

**Status:** IN PROGRESS · 2026-09-18 · P0–P4b landed; P5–P8 production implementation is complete in `0e95bb5dd` and `d7e054595`. Current-frame, refit, catalog, export and bounded performance evidence is in acceptance §A11. Full acceptance remains open for the listed qualification gaps and Peter’s L4 review.
**Prerequisites:** existing scene-modifier foundation and native Metal RT on main. Work item: `BUG-e3p6.4`.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md), sections 5–6 and 8; repository `AGENTS.md` takes precedence over older workflow guidance.

Peter: “I want to get RT working as a ‘trait’ of our scene modifiers so it ‘just works’ for all current and future scene modifiers.” This design makes that a geometry-output contract. **Raster and RT consume the same final mesh, instances, appearance, and material state for the same evaluated frame.** Modifier recipes never implement ray tracing. Geometry primitives describe changes; the graph preserves that description; the renderer performs one shared update policy for live playback and export.

Companions: [acceptance definitions](SCENE_MODIFIER_RT_ACCEPTANCE.md), [source inventory](SCENE_MODIFIER_RT_INVENTORY.md), [modifier architecture](SCENE_MODIFIER_PRESET_ARCHITECTURE.md), [RT contract](RAYTRACING_DESIGN.md), [instancing contract](RT_INSTANCING_DESIGN.md), [GPU architecture](MANIFOLD_GPU_ARCHITECTURE.md), [freeze compiler map](FREEZE_COMPILER_MAP.md).

## 1. Audit — verified 2026-09-16

Snapshot: `2a356c5b18966084245bd4d9885cd788f78b78cb`. Anchors are a dated inventory, not a claim that future main has identical lines. Re-run the inventory before each phase and review changed sites before editing. No LSP service was available in this session; trait declarations, blanket implementations, constructors, and consumers were inspected by source search. Extend these mechanisms; do not replace the graph or renderer.

| Piece | Evidence | Classification and consequence |
|---|---|---|
| Final mesh already feeds raster and RT | `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:5270` | Exists. `RtObjectGeometry` borrows the draw's final vertex buffer. No second deformation implementation is needed. |
| Continuous changes deliberately do not rebuild | same file `:2834`, `:2960` | Existing limitation. The content-settle policy cannot represent dynamic geometry. Remove it when P5 engages the new path. |
| Per-object BLAS + instance TLAS | `crates/manifold-gpu/src/metal/raytrace/accel.rs:49`, `:356`, `:777` | Exists. BLAS uses `Refit` usage but retains only its structure. Retained descriptor/scratch are currently TLAS resources. |
| Structural guard | `accel.rs:139` | Exists. Identity, layout, count, index identity, alpha mode and instance capacity are checked. This does not detect in-place content/index changes. |
| Content generations and pending state | `node_graph/execution.rs:240`, `:1449`, `:1524`; `node_graph/bindings.rs:39` | Exists. Extend the commit point, including alias/skip paths; do not infer changes from time or transport. |
| Cut-map revision | `node_graph/scene_object.rs:57`, `:121`; `primitives/scene_object.rs:131`; `scene_modifier_expand/fragment_cuts.rs:600` | Exists. Preserve the authored topology port and use its revision as an additional conservative invalidation input. |
| Fusion becomes dynamic WGSL nodes | `node_graph/freeze/install.rs:135`, `:422`, `:643`, `:1323`; `primitives/wgsl_compute.rs:1902` | New metadata propagation required. Raw/fused def accessors currently discard non-serialized sidecars. |
| GPU ordering entry point | `crates/manifold-gpu/src/metal/encoder.rs:205` | Exists. `raw_cmd_buf()` ends the active encoder. Encode AS operations here; no separately committed AS command buffer. |
| Emissive geometry is CPU-cached | `raytrace/emissive.rs:147`, `:392` | New GPU preparation required. CPU mapped reads cannot observe this frame's queued modifier writes. Transforming old CPU copies is insufficient. |
| Matrix motion and cut invalidation | `render_scene.rs:5788`, `:5911` | Exists, but not general vertex correspondence. Use conservative invalidation for deformation in this scope. |
| Export has completion/error discipline | `crates/manifold-app/src/content_export.rs` | Exists. Reuse the production frame pipeline and final completion checks; details in the inventory. |
| Numerical GPU proof infrastructure | `crates/manifold-renderer/tests/gpu_proofs/rt_instancing.rs`, `rt_emissive_light_table.rs`, `scene_modifier_legacy.rs` | Exists. Extend production helpers; do not use beauty-image similarity as the ray-hit oracle. |
| Admission accounts current + candidate memory | `scene_modifier_expand/buffer_budget.rs:310`; `manifold-gpu/src/lib.rs:26` | Exists. Include acceleration and emissive scratch in the same aggregate policy. |

Binding constraints: content-thread hot path, GPU command order, asynchronous source readiness, shared buffer lifetime, and memory at scene scale. No new thread, channel, lock, persistent project field, or UI switch is required. Beats remain authoritative; this work does not change frame-time evaluation.

## 2. Decisions

**D1 — Describe geometry writes, not modifier names.** Add output change semantics to `Primitive`/`EffectNode`. Runtime mesh revisions distinguish topology, positions, and remaining attributes. Unsupported declarations default to a rebuild on a write, preserving correctness for future/custom triangle-mesh producers. A new primitive may earn refit through an explicit contract and proof. An `supports_rt: bool` on recipes is rejected: it duplicates renderer policy and cannot describe stacks.

**D2 — One current frame, one GPU order.** Modifier writes → RT table uploads → changed BLAS builds/refits → TLAS update → emissive preparation → ray dispatch, on the caller's `GpuEncoder`. Raster consumes the same geometry version. No CPU wait inside scene evaluation; no one-frame-old BLAS paired with current raster geometry. `ready` means completion for warmup/diagnostics, not permission to encode an ordered later consumer.

**D3 — Correct rebuild path first; refit is an optimization of that path.** P5 enables all mesh changes using selective BLAS rebuilds; P6 replaces eligible rebuilds with refits. Live and export share the same planner and encoder. No export-only geometry evaluator and no “rebuild the entire scene every export frame” implementation.

**D4 — Structural change is about the representation.** Triangle count, connectivity/index contents, descriptor layout, object membership, and instance capacity matter. Equal addresses/counts do not prove equal topology. Cuts use cut-map revisions even when output capacity stays fixed. A stable cut map followed by deformation is eligible for refit; a changed cut map rebuilds. Large deformation may be correct under refit yet slower to trace; measure both update and trace cost. Apple describes refit as reusing a hierarchy for moved primitives, with quality best for smaller changes: [Metal guide](https://developer.apple.com/videos/play/wwdc2023/10128/), [refit usage](https://developer.apple.com/documentation/metal/mtlaccelerationstructureusage/refit).

**D5 — Preserve rendering policy while making its inputs current.** Keep existing opaque/masked RT participation, material/texture semantics, cast-shadow masks, instance transforms, and emissive selection/estimator constants. Appearance weights/gain must affect RT exactly where the raster shader treats them as coverage. Do not add transparent-glass ray transport or change the GI estimator in this work.

**D6 — GPU geometry never returns to the CPU to maintain RT.** Emissive tables are prepared from current GPU geometry and current instance descriptors. CPU reads of same-frame mesh data, a copy of the modifier math, and “disable emissive sampling for modified meshes” are forbidden.

**D7 — Conservative temporal correctness is the v1 policy.** Any topology/position change, and any content/material/appearance change that makes accumulated shading stale, resets all affected scene RT histories, denoiser history, and temporal-upscale history on that frame. Seed from fresh samples, never black. Static frames resume accumulation. Existing transform-only motion handling stays. No claim of deformation motion vectors is made. Cost: deforming scenes are noisier; accurate deformation reprojection is separately deferred, not silently assumed.

**D8 — Correctness does not imply arbitrary-scene 60 fps.** Heavy live scenes may miss the frame budget; they must not silently drop meshes, lower samples, switch to raster, or trace old geometry. Export waits at the existing frame-completion boundary and retains existing GPU fault deadlines. A scene that exceeds those deadlines fails explicitly; “offline” is not unlimited GPU execution time.

**D9 — All derived state is runtime-only.** Existing `NodeId`, `ResourceId`, `Slot`, rebuild epoch, object order, and instance-slot order remain the addressing systems. No parallel mesh registry or serialized RT declarations. Fusion sidecars are owned by existing prepared views and are regenerated from canonical graphs after reload.

**D10 — Admission before publication, reuse afterward.** Retain BLAS build/refit scratch, descriptors and emissive workspaces. Prepare during existing scene warmup/candidate preparation, including RT-off scenes that expose the RT toggle, so toggle-on pays no shader compile or geometry allocation. Charge those resources to admission. This deliberately increases prepared memory and load-time work. Unexpected structural growth must enter existing candidate preparation; it cannot allocate unchecked on the live path.

Relationship to `RAYTRACING_DESIGN.md` D17: the shared caller-ordered update path supersedes its former settle-and-private-submit mechanism. Its no-CPU-wait requirement remains. Initial construction is moved into candidate warmup before publication; D10 accounts for that cost. This design does not claim that putting a large first build on a live frame is free.

Rejected alternative: rebuild all geometry on every write without metadata. It is a useful test reference but wastes static meshes and cannot deliver the Surface Waves live target. Rejected alternative: a new worker/queue rebuilding RT asynchronously. It adds ownership and stale-frame reconciliation while the graph already provides ordered GPU dataflow.

## 3. Geometry change contract

### 3.1 Exact types and ownership

New renderer module: `crates/manifold-renderer/src/node_graph/mesh_change.rs`. No serde derives. Types are renderer-owned; `manifold-gpu` does not depend on them.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshAspect { Topology, Positions, Content }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshDependency {
    pub input: std::borrow::Cow<'static, str>,
    pub aspect: MeshAspect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshRevisionRule<'a> {
    Written,
    Fixed,
    Dependencies(&'a [MeshDependency]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshOutputRule<'a> {
    pub topology: MeshRevisionRule<'a>,
    pub positions: MeshRevisionRule<'a>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshRevision {
    pub topology: u64,
    pub positions: u64,
    pub content: u64,
}
```

Add `fn mesh_output_rule(&self, port: &str) -> MeshOutputRule<'_>` to both `Primitive` and `EffectNode`; blanket forwarding belongs beside `array_output_capacity` in `primitive.rs`. Default is `{ topology: Written, positions: Written }`. Query only outputs with the `MeshVertex` channel layout. No general array is assumed to be a triangle mesh. `MeshVertex` also represents grids, points and morph deltas in existing primitives; revision tracking on those arrays is conservative metadata only. Only the existing validated scene-object triangle geometry enters AS construction. New non-triangle representations require their own intersection contract. `PrimitiveSpec` and macro syntax need not change: ordinary `impl Primitive` overrides are the existing precedent.

Rules mean:

* `Written`: revise when output content changes; honor memo skips and truthful unchanged-content declarations.
* `Fixed`: stable across writes while resource identity/layout and executor epoch are stable. Only use when the primitive guarantees fixed triangle connectivity independent of all its live controls.
* `Dependencies`: revise when any named input aspect changes; an empty dependency list is `Fixed`. Input `Content` works for non-mesh controls such as a cut map. Missing required dependencies are preparation errors, not fixed revisions.
* Content revises on semantic output changes, including normals/UV-only changes. A physical recopy of identical content is not a semantic change. A rule does not suppress data writes or schedule GPU work.

Static primitive declarations use `Cow::Borrowed` names; fused declarations own `Cow::Owned` names. `WgslCompute` can return borrowed slices of its owned rule vectors without self-referential storage or frame allocations. Examples:

| Producer | Topology rule | Position rule |
|---|---|---|
| Unknown mesh source/custom WGSL | Written | Written |
| Ordinary deformer, e.g. normal wave | input `in.Topology` | Written |
| Morph between meshes | `in.Topology` + `b.Topology`; preserve structural minimum-capacity check | Written |
| Patch/recon after cut expansion | both deformed and reference mesh input topologies | Written |
| Normals/tangents-only operation | input `in.Topology` | input `in.Positions` |
| Exact pass-through/mesh snapshot of current input | input topology | input positions |
| Cut remap | `in.Topology` + `map.Content` | Written |
| Fixed triangle source with audited invariant | Fixed | Written |

`Fixed` does not permit NaN/invalid vertices. Finite zero-area triangles and their revival are explicit Metal acceptance cases. If the target backend fails that proof, P6 cannot qualify those operations for refit: keep the already-correct rebuild path and escalate the failed acceptance to the lead. Do not invent an inactive-triangle representation to pass the test.

### 3.2 Logical content and executor mechanics

`ContentVersion` is an opaque, runtime-only `(executor epoch, ResourceId, revision)` stamp. `StorageRevision` records writes to a physical slot. They are different Rust types: semantic consumers cannot accidentally substitute a write counter for a content stamp. Both use existing executor addressing and revision storage; there is no second resource registry.

The executor proves output retention from physical identity, slot ownership and storage revision before evaluation. Producers require `outputs_retained()` before skipping a write; unchanged allocation identity alone is insufficient after another tenant overwrites it. A producer may call `mark_output_content_unchanged()` after safely copying identical cached data into a new destination. Storage still changes; logical content does not. `mark_outputs_unchanged()` means no physical write and implies unchanged content. First publication, pending-to-ready, and output shape changes must publish a fresh version. Missing metadata is unknown, never a fabricated zero or evidence for a cache hit.

Pure and fused transforms also retain logical content when their complete input versions and parameter epoch match, even if transient output storage requires another dispatch. This reuses memo dependency state without retaining more GPU allocations. RT appearance, lighting, IBL, shadow, instance, weight and topology-hint consumers use logical content stamps. Physical identities remain necessary for actual GPU bindings and acceleration-structure representation checks. Shared RT change classification drives history invalidation and emissive refresh; transform-only motion retains the existing reprojection policy. The optional `MANIFOLD_RT_SOURCE_TRACE` reports geometry, appearance, instance and reset decisions without readback.

Extend `ExecutionPlan` with compiled rules indexed by existing output `ResourceId`; compiled dependencies are `(ResourceId, MeshAspect)`. Extend `Executor` with pre-sized revision state and dependency snapshots, plus one monotonically increasing revision counter. Tokens are unique within its existing rebuild epoch. Do not use a hash as the equality oracle for revision dependencies.

At the existing output-commit choke point:

1. Snapshot input revisions before overwriting aliased output metadata.
2. Propagate pending from every actually selected/read dependency; a pending source remains pending through deformers, fusion, scene bundles, and mesh boundaries. No AS work may consume it.
3. On a semantic change, issue a content token. Issue topology/position tokens according to the compiled rules. Position changes also imply content changes. Structural identity/layout changes issue all three regardless of rule.
4. Memo/hoist skips and declared identical-content recopies retain logical content tokens. Selected-input aliases and copies preserve pending status and retain their output token while the selected logical input version is unchanged; source selection changes revise it. Physical storage movement alone does not revise logical content. Mesh representation changes still revise topology/positions where the AS binding requires it. An in-place output uses captured input tokens, not its just-written output tokens.
5. Late capture/feedback uses the same commit helper when new bytes become the next observable output. A previous-frame feedback value is valid graph semantics; RT must match the raster's version of it.

Use logical resource state as the authority and publish its snapshot into physical-slot metadata alongside `slot_generations`/`slot_pending`. Pool reuse must never inherit another logical resource's revision. Allocation/resizing happens during plan/resource preparation, not in the frame loop.

Add to `NodeInputs`:

```rust
pub fn mesh_revision(&self, port: &str) -> Option<MeshRevision>;
pub fn mesh_revision_of(&self, slot: Slot) -> Option<MeshRevision>;
```

Thread metadata via a crate-private `with_mesh_revisions` builder, following `with_pending`; retain the existing three-argument test constructor. A mesh without metadata is conservative `Written`, issuing fresh revision tokens on each evaluated frame until its producer supplies metadata; it is never classified unchanged. This is an explicit compatibility rule for externally prebound test/source buffers, not a fallback from malformed prepared metadata.

### 3.3 Fusion and graph loading

Fusion composes rules at preparation time using the same member/wire map that rewrites the graph. Substitution rules are mechanical: an internal dependency expands to that member's rule; `Written` dominates its dependent aspect; `Fixed` contributes no dependency; external dependencies are renamed to the emitted input port. Sort/deduplicate external leaves. Content dependency on an internal written value is `Written`. No per-frame walk of authored modifier graphs.

Owned forms live in `mesh_change.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparedMeshRevisionRule {
    Written,
    Fixed,
    Dependencies(Vec<MeshDependency>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedMeshOutputRule {
    pub output: String,
    pub topology: PreparedMeshRevisionRule,
    pub positions: PreparedMeshRevisionRule,
}
pub type PreparedMeshRules =
    ahash::AHashMap<manifold_core::NodeId, Vec<PreparedMeshOutputRule>>;
```

Add `mesh_rules: PreparedMeshRules` to `FusedDef`, `FusedGeneratorView`, `LoadedPresetView`, and `SegmentView`. Unfused views use an empty map; node declarations supply their rules. The map uses existing generated node IDs. Segment concatenation uses existing card prefixes. Cache values retain it; include a mesh-rule schema revision in compiled fusion cache keys. It is not serialized into `EffectGraphDef`, WGSL, or project files.

Add `fn install_mesh_output_rules(&mut self, rules: &[PreparedMeshOutputRule]) -> Result<(), String>` to `EffectNode`, default rejecting a nonempty override. `WgslCompute` accepts compiler-provided overrides after checking output/input layouts and names and owns their copies. Reparse clears overrides. Authored WGSL cannot assert a trusted refit declaration through a comment. Generated fused nodes missing expected metadata fail preparation rather than silently reverting to rebuild.

Add one shared loader helper:

```rust
pub(crate) fn install_prepared_mesh_rules(
    graph: &mut Graph,
    def: &EffectGraphDef,
    id_map: &ahash::AHashMap<u32, NodeInstanceId>,
    rules: &PreparedMeshRules,
) -> Result<(), GraphBuildError>;
```

Call it inside `graph_loader::instantiate_def`, after parameter/source installation and before returning `NodeInstantiation`. Resolve each document node's stable `NodeId` using the existing `node_id`-then-handle rule at `graph_loader.rs:913`; look up its numeric document ID in `NodeInstantiation.id_map`. Do not use effect handle names as stable node IDs. Add `GraphBuildError::MeshRules { node_id: manifold_core::NodeId, reason: String }` and map it through the existing `LoadError` conversion. Unknown, duplicate or uninstalled sidecar entries are preparation errors. Sidecars describe the final prepared definition; any later graph rewrite must retarget them alongside existing bindings.

Append `mesh_rules: &PreparedMeshRules` to these existing signatures, preserving their other arguments/returns: `instantiate_def(graph, def, registry, handle_scope, boundary, mesh_rules)`, `EffectGraphDefExt::into_graph(self, registry, mesh_rules)`, `PresetRuntime::from_render_def(doc, registry, manifest, mesh_rules)`, and `splice_def_into_chain(graph, source, def, registry, relight, mesh_rules)`. Raw canonical callers pass a shared empty map. Propagate maps through generator, modifier, effect-card and segment preparation before compilation, array preparation and admission. Segment/copy/relight rewrites retarget sidecars with their existing ID maps; never fuse an already fused `.def` while discarding its provenance. All executable prepared-view consumers must forward the sidecar, including graph tools, preview, thumbnails and profiling. Installation errors fail candidate publication, not a silent canonical fallback.

Rename def-only fusion accessors to view-returning accessors and migrate their callers compiler-first: `fused_generator_def_for` → existing `fused_generator_view_for`; `fused_generator_def_by_id` → `fused_generator_view_by_id`; `fuse_generator_def`/`fuse_generator_def_masked` → `fuse_generator_view`/`fuse_generator_view_masked`. Return `FusedGeneratorView` (cached APIs return `Arc<FusedGeneratorView>`). Remove obsolete def-only functions. A caller that only prints JSON may explicitly read `.def`; a caller that executes must install `.mesh_rules`.

Pin the real entrypoint flow (`preset_runtime/modifier_runtime.rs:51–118`): `from_def` → `from_def_for_render` → `from_def_for_render_view` receive **canonical** definitions and retain their current signatures. `prepare_scene_modifiers`/`prepare_scene_modifier_math_view` expand canonical primitive graphs before fusion, so `PreparedSceneModifierGraph` needs no mesh-rule sidecar in this design. After expansion, `from_def_for_render_view` obtains `FusedGeneratorView`; it must forward that view's rules to `from_render_def`, and use an empty map only when fusion did not occur. The Math View isolated/chained factories at `preset_runtime/math_view.rs:117` follow this same sequence. Generator registry retains its call to `from_def_for_render`; its old def-only-fusion comment is stale and must be corrected.

For external proof/tool callers that already hold a fused view, add this public factory in `preset_runtime/build.rs`:

```rust
pub fn from_prepared_generator_view(
    view: &crate::node_graph::freeze::install::FusedGeneratorView,
    registry: &PrimitiveRegistry,
    manifest: Option<&ParamManifest>,
) -> Result<Self, JsonGeneratorLoadError>;
```

It invokes `from_render_def((*view.def).clone(), registry, manifest, &view.mesh_rules)` and installs existing `retarget` data exactly as the current modifier runtime does. It requires an already expanded standalone definition (no unexpanded scene modifiers); it does not repeat expansion/fusion. Share this installation helper with `from_def_for_render_view`, which continues to attach its canonical authoring/routes/guards/event state afterward. Migrate every executable `from_def(fused_def)`/`into_graph(fused_def)` caller to the prepared factory or the explicit rules-taking `into_graph`; canonical callers remain canonical. Thus no sidecar needs to pass backward through the modifier expander, and no prepared view is unwrapped and silently reloaded as canonical. Acceptance tests cover both entrypoint families.

## 4. Native Metal acceleration lifecycle

### 4.1 Public seam

The `ShadowRayTracer` trait in `manifold-gpu/src/metal/raytrace/tracer.rs` replaces its independent `build_accel`/`refit_accel` methods with:

```rust
fn plan_accel(
    &self, device: &GpuDevice, resident: Option<&Self::Accel>,
    objects: &[RtObjectGeometry<'_>],
) -> Result<RtAccelPlan, RtAccelError>;
fn prepare_accel(
    &self, device: &GpuDevice, resident: &mut Option<Self::Accel>,
    plan: RtAccelPlan,
) -> Result<(), RtAccelError>;
fn encode_accel_update(
    &self, encoder: &mut GpuEncoder, accel: &mut Self::Accel,
    objects: &[RtObjectGeometry<'_>], changes: &[RtGeometryChange],
    materials: &[GiMaterial], instance_data_changed: bool,
    emissive_data_changed: bool,
) -> Result<RtAccelUpdate, RtAccelError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RtGeometryChange { Reuse, Attributes, Refit, Rebuild }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtAccelUpdate {
    pub blas_builds: u32,
    pub blas_refits: u32,
    pub tlas_builds: u32,
    pub tlas_refits: u32,
    pub emissive_refreshes: u32,
}
pub struct RtAccelPlan { /* private, owns prepared descriptors and sizes */ }
impl RtAccelPlan {
    pub fn additional_peak_bytes(&self) -> u64;
}
#[derive(Debug)]
pub enum RtAccelError {
    InvalidGeometry { object: usize, reason: String },
    NeedsPreparation,
    Allocation { bytes: u64, resource: &'static str },
    Encode(&'static str),
}
```

`plan_accel` is CPU-only sizing; it must not inspect vertex contents. `additional_peak_bytes` includes replacement overlap, BLAS/TLAS storage, both scratch kinds, descriptor/source/material tables, emission candidates/sort/alias workspaces, and lifetime pins. Already-live resources are in the device snapshot; do not charge them twice. Renderer passes this number to existing `admit_candidate_bytes` before `prepare_accel`. No retained scene is destroyed on admission/allocation failure.

`prepare_accel` allocates/reuses capacity but never commits a GPU command. `encode_accel_update` validates parallel lengths, descriptors and capacity before encoding anything; structural mismatch without preparation returns `NeedsPreparation`. It performs no resource allocation. Initial/unbuilt BLAS always builds regardless of requested change. Until P6, `Refit` intentionally executes the same rebuild branch and reports `blas_builds`; P6 replaces that branch, not the caller policy.

`RtObjectGeometry` keeps its geometry/material texture fields. Extend it with optional per-vertex appearance buffer plus gain, using the same coverage semantics as the raster shader (section 5.2). GPU types stay behind `manifold-gpu`. Scene object order remains the current canonical order shared by material and normal-source tables. On membership/order changes rebuild the TLAS and re-establish that table order; conservatively rebuild the BLAS list if matching cannot be proven from the existing list. Stable membership must update only dirty BLAS entries. A new cross-scene BLAS identity/deduplication system is out of scope.

### 4.2 Resident storage and command order

Each `Blas` retains its primitive descriptor, triangle descriptor, structure, build scratch and refit scratch. Build/refit scratch is sized from Metal's reported requirements, never guessed. Structural replacements are prepared atomically. Stable-topology rebuilds reuse structure/storage. The TLAS retains corresponding build/refit storage. No compaction in this scope.

`encode_accel_update` ends the current compute/render encoder through `GpuEncoder::raw_cmd_buf()`, writes current instance descriptors through the existing GPU descriptor builder, opens an AS encoder on that command buffer, encodes changed BLAS entries followed by the TLAS, ends it, then encodes emissive preparation. The non-instanced path uses the same descriptor builder with an implicit identity instance combined with the object transform; avoid CPU mutation of mapped descriptors while an earlier frame reads them. Reuse current descriptor composition math and its numeric tests. Keep the existing `any wired instance buffer` semantic flag for emissive local/world conventions; using one descriptor encoder does not change that flag.

Dirty decision order:

1. New object/list/layout/index content/alpha-class/topology revision → affected BLAS build; list/capacity changes → TLAS build.
2. Position revision only → affected BLAS refit; TLAS refit even if object transforms stayed fixed, because BLAS bounds moved.
3. Instance transform/content or cast mask only → TLAS refit.
4. Attribute/material/appearance changes → refresh tables/history; rebuild BLAS only when geometry descriptor opacity changes.
5. No changes → no AS encoder or emissive preparation.

Retain readiness for warmup completion using existing atomic completion machinery, but remove `rt_accel_built` as an admission latch. Encoding a current AS update successfully allows a later trace on that same encoder. Completion failures go through the existing GPU-fault machinery. A successful encode is not reported as a completed GPU frame.

### 4.3 Lifetime and immutable input snapshots

Encoding order alone does not protect CPU writes. Every CPU-authored RT input read by the GPU (normal sources, materials, object motion, descriptor parameters, trace uniforms) must be snapshotted at encode time. Reuse `GpuBinding::Bytes` for small records. For persistent GPU tables, use an internal prewarmed copy kernel that takes at most 4 KiB of inline bytes per dispatch and writes the destination table on the same encoder. Chunk from retained CPU scratch; no per-frame staging allocation, mutable mapped uploads, or new ring allocator.

All AS resources and indirectly referenced buffers/textures are declared with `useResource` and pinned through the actual consuming command buffer's completion. Package resident handles in an immutable `Arc` resource set created only at preparation/replacement; callbacks clone that existing `Arc`. There is no new mutex. Update/trace completion pins must survive graph teardown **before the caller commits**, not only teardown after submission. Keep the established queue-retirement protection for already-submitted consumers. Superseded completion callbacks must not mark a newer prepared structure ready: readiness belongs to that resource set, not an owner-global flag.

The no-allocation gate covers new Rust scratch collections and Metal buffers/AS/pipelines during prepared steady state. Existing Metal command-encoder/completion-block creation is reported separately; do not claim the framework allocates nothing.

## 5. Shading, emission, history, and export

### 5.1 GPU emissive preparation

Replace `build_emissive_table`/`refit_emissive_table` CPU geometry reads with `encode_emissive_table` in `raytrace/emissive.rs`, called only by the shared AS update path. This is renderer infrastructure, not a new user graph primitive. Its signature is crate-private:

```rust
pub(crate) fn encode_emissive_table(
    tracer: &MetalShadowRayTracer, encoder: &mut GpuEncoder,
    accel: &mut RtAccel, objects: &[RtObjectGeometry<'_>],
) -> Result<(), RtAccelError>;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct EmissiveTableStats {
    pub entry_count: u32,
    pub entries_are_local: u32,
    pub mean_power: f32,
    pub total_area: f32,
}
```

The table owns a 16-byte GPU stats buffer instead of CPU-authoritative count/mean/area. Keep the existing 80-byte triangle and 8-byte alias records and 4096-entry cap. Preserve current non-instanced/world-space versus instanced/local-space estimator conventions. The arithmetic is current `emissive.rs`/`shadow_rays.msl`, not a redesigned importance sampler.

GPU algorithm: enumerate `(object, instance slot, triangle)` into preallocated candidate records; fetch current indexed/flat vertices and UVs through the same checked source layout as hit shading; compute current local area and material-factor power for ranking (the current policy in both modes); reject nonfinite/zero-area/nonpositive-power candidates; select the largest 4096 powers; gather triangle records; construct the alias table and stats. Use deterministic ties `(object, slot, triangle)` ascending. Sorting is a GPU radix sort over positive-float power keys with deterministic identity tie order; candidate and ping-pong buffers are sized during preparation. The bounded 4096-entry alias construction may use a single GPU thread with preallocated small/large stacks; benchmark its cost, do not move it to CPU readback.

Refresh when positions, topology, material emission, relevant UVs/appearance, or instance data change; unchanged scenes dispatch none of these passes. Nonemissive scenes skip candidate work but still have valid zero stats. Emission changing from zero to positive must work without topology edits. Current pending sources never enter candidate generation.

Trace kernels and firefly-clamp kernels read the stats buffer directly. Delete CPU-derived `emissive_table_mean_power`, `emissive_table_count`, `emissive_table_total_area`, and `emissive_entries_are_local` fields from `ShadowRayParams`; update Rust/MSL layouts and offset tests together. `FireflyClampParams.floor` remains the fixed minimum, and the kernel computes `max(floor, stats.mean_power)`. Add `emissive_stats: &GpuBuffer` immediately after the `accel` argument of `ShadowRayTracer::dispatch_shadow_rays`; add it immediately after `encoder` in `firefly_clamp`. Every caller supplies the prepared zero-stats buffer when emission is absent. Debug proof entry points use the same kernels and stats layout.

Delete CPU local-triangle retention and production mapped vertex/index reads. Static-scene tests must retain existing lighting behavior within declared floating-point tolerances; deterministic equal-power truncation is the only intentional selection-order change.

### 5.2 Appearance and hit attributes

Current `MeshVertex` positions, normals and UVs come from the same final buffer for ray hits and emitter sampling. Preserve material/instance IDs and the existing RT shading policy; a current buffer does not imply complete raster/RT material parity. Remove the explicit RT rejection of wired weights/nonunit gain in `render_scene.rs:1977` only after these proofs pass.

Pin the appearance math to `shaders/render_scene.wgsl:889`: interpolate vertex weight (unwired = 1), compute `level = gain * weight`, `coverage = clamp(level, 0, 1)`, `brightness = max(level, 1)`. Nonfinite input follows existing validation; it must not become an unchecked buffer access. Keep the existing material alpha-mask cutoff test first. Reject `coverage == 0`; accept `coverage == 1`; for fractional coverage accept when a deterministic uniform variate is below coverage. Use the existing ray RNG, with a separate hash stream keyed by sample seed, ray/bounce and candidate identity so appearance tests do not shift unrelated sampling sequences. This matches the *expected appearance coverage* of raster alpha-to-coverage, not its exact MSAA mask. Do not change existing base-material alpha transport or add Blend participation. Tests assert both the formula and sampling distribution, not stochastic image equality.

Extend `RtNormalSource` with `appearance_weights_addr: u64`, `appearance_weight_count: u32`, `appearance_gain: f32`; use address/count zero for unwired. Extend `RtObjectGeometry<'a>` with `appearance_weights: Option<&'a GpuBuffer>` and `appearance_gain: f32`; the checked weight count is the mesh vertex count. Pin weights and snapshot source tables. Also add `index_base_addr: u64` and `vertex_count: u32` to `RtNormalSource`, with explicit Rust/MSL padding/offset assertions. Zero index address means flat triangles; otherwise the existing backend representation is uint32 indices at offset zero. One shared triangle-index helper resolves corners for normal/UV/appearance fetch, normal-map frame derivation and emissive generation. Preserve vertex offsets in the base address. Thread and pin the existing optional index buffer; do not add a new indexed mesh format to scene-object authoring. A nonzero address with insufficient weights is a structured geometry error.

All candidate-hit walkers (visibility, GI and reflection) use the same appearance helper. Descriptor nonopaque property is `alpha_mask || translucent || appearance_weights.is_some() || appearance_gain != 1.0`; a change in that property rebuilds the affected BLAS. A subsequent fractional-to-fractional gain change updates the source table only. Accepted hits multiply evaluated reflected/emitted surface radiance by `brightness`; coverage has already been applied by stochastic acceptance and must not multiply it twice. Explicit emitter samples multiply radiance by `coverage * brightness` at the sampled barycentrics because those samples have not passed the hit test. Preserve current area×material-luma candidate ranking/alias probabilities; do not use a different hidden appearance-dependent importance estimator. Gain=1 with no weights retains existing alpha/material behavior.

Material-boundary note (updated 2026-09-26): the former inherited-limit list is
superseded by the current material contract in
[GLTF_MATERIAL_EXTENSIONS_DESIGN.md section 7](GLTF_MATERIAL_EXTENSIONS_DESIGN.md#7-material-fidelity-corrections-2026-09-26).
RT now carries per-map UV transforms/addressing, authored tangent handedness,
inverse-transpose normals, and base-alpha × texture-alpha × colour-factor
transport; raster shadow-depth alpha follows the same corrected contract.

The actual hybrid RT limits remain: secondary hits omit clearcoat, sheen,
iridescence, subsurface and full Phong/Cel evaluation; glass and Blend surfaces
are absent from acceleration; ray-hit texture sampling has no ray-cone
minification; environment anisotropy uses a bent-normal approximation; and
reflections do not provide recursive specular transport. The texture table is
limited to 64 unique textures, and arbitrary nested dielectric transport and
spectral transport remain outside the supported model. These limits preserve
the scope of this geometry contract and are not claims of full material parity.

### 5.3 Renderer integration and history

Extend `ObjectDraw` with `mesh_revision: MeshRevision` and `topology_hint: Option<(Slot, u64)>`. `vertices_generation` remains for existing raster shadow cache consumers; do not replace unrelated keys. Build per-object RT change decisions from revision comparisons plus structural validation. A changed explicit topology hint forces a rebuild even if generic metadata says otherwise; once both paths describe a cut they must not schedule duplicate work.

Refactor `validate_topology_and_author_flags` into collection/validation and a later author-flags step. Order in `evaluate`: collect draws → determine changes/prepare if required → encode AS/tables → decide RT flags → depth and shading consumers → trace/accumulate. RT flags must reflect the update that will execute for this frame, not last frame's completion latch. AS work may precede the depth pass; tracing remains after its current depth input is produced.

Remove `rt_deferred_build_decision`, `rt_refit_eligible`, content-pending keys and the topology-only trace gate. Replace them with one successful-current-update result plus pending/error handling. Pending sources propagate through existing warmup; an incomplete RT scene does not quietly trace a subset as complete. A currently published scene stays visible while a replacement warms through existing candidate publication. If a previously active source becomes unavailable, surface the existing structured render error; never silently use stale geometry.

Enable the same path through authoring and loading: vertex modifiers must not
lock `rt_enabled` in `scene_modifier_parameter_lock_reason`, reject authored or
live RT in the modifier compiler, or install a raster-only prepared-parameter
guard. This includes legacy fragment graphs. Keep calibrated source-selector
locks and source-frame validation. The regression
`rt_dynamic_current_frame_stock_modifier_combo_accepts_rt_and_dispatches` loads
the stock Vortex Fragments → Ordered Recon stack and exercises RT on/off/on;
this establishes dispatch and frame validity, not full-project export acceptance.

Fold geometry/attribute/material/appearance invalidation into the single reset decision used by RT accumulation, moments, SVT, denoiser and temporal upscaler. `MeshTopologyHistory` consumes the general topology revisions plus the existing explicit hint rather than maintaining an independent cut-only authority. Deformation resets temporal consumers but does not pretend to supply true deformation velocity. On unchanged frames, no forced reset. World transforms retain existing reprojection behavior.

### 5.4 Frame validity and export

`EffectNodeContext::error` is currently diagnostic-only (`effect_node.rs:430–455`); it does **not** abort the frame. A GPU completion fence alone cannot reject a magenta/error frame. Add an explicit allocation-free validity result in renderer module `frame_status.rs`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameRenderStatus {
    #[default]
    Complete,
    PendingGeometry,
    Failed(FrameRenderFailure),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRenderFailure {
    InvalidGeometry,
    RtNeedsPreparation,
    RtAllocation,
    RtEncode,
}
impl FrameRenderStatus {
    pub fn merge(&mut self, next: Self);
}
```

`merge` severity is Failed > PendingGeometry > Complete; first failure wins and a later success cannot clear it. Add owned `frame_status: FrameRenderStatus` to the **renderer wrapper** `gpu_encoder::GpuEncoder<'a>` (not a global GPU fault counter), initialized Complete in its constructors. Add `pub fn frame_status(&self) -> FrameRenderStatus` and `pub fn merge_frame_status(&mut self, status: FrameRenderStatus)`. Native `checkpoint`/command-buffer splitting leaves this wrapper status intact. No new lock, channel or per-frame vector.

`render_scene` records pending/failed status through `ctx.gpu_encoder()` and continues existing node diagnostics via `ctx.error`. Mark outputs pending on source readiness gaps; do not trace an incomplete object list. Other nodes propagate pending through the executor as §3.2 specifies. Runtime failures invalidate that produced frame; they cannot be made successful by a deterministic diagnostic clear.

`ContentPipeline` owns `last_frame_status: FrameRenderStatus`, resets it once at the beginning of `render_content`, and merges status from the generator wrapper before dropping it (`content_pipeline.rs:2228`), plus the compositor wrapper after rendering. `LayerCompositor` must merge every nested clip/layer/master wrapper into its caller's wrapper before dropping it, including the internal encoder paths at `layer_compositor.rs:937` and `:1182`; early returns preserve status. Thumbnail/probe callers inspect their own wrapper status. `GeneratorRenderer::render_all` already shares its caller's wrapper; no new generator trait is needed. Add `pub fn frame_render_status(&self) -> FrameRenderStatus` to `ContentPipeline`.

Warmup treats PendingGeometry as incomplete preparation through its existing `WarmupOutcome`; candidate publication requires Complete. The currently published scene remains during replacement warmup. A failed active live frame follows existing visible diagnostics, explicitly marked failed; it never silently draws old RT geometry. In `export_one_frame`, inspect `frame_render_status()` after rendering/background flush and before selecting/encoding the output texture. Anything except Complete returns the existing export error path and prevents `encode_frame`. Include the reason and frame index in diagnostics; GPU completion/fault checks still run for otherwise Complete frames. Pending at the export boundary is an error, not permission to tick/evaluate again.



The export loop must not issue its own AS calls. It evaluates one production frame at the existing beat/time, encodes the shared scene path, then uses the existing `ContentPipeline::wait_for_export_complete` boundary before feeding the video encoder. Do not rerun the graph at the same beat just to settle RT: that advances stateful modifiers twice. Warmup may initialize resources, but exported simulation advances once per output frame.

Preserve first-frame source readiness, section boundaries, seek/reset behavior, cancellation, output resize, fault propagation, partial-export cancellation, and the existing finite completion deadline. Export frame one must already use current deformed geometry. Error tests assert no affected frame reaches the media encoder. No export setting, saved-field migration, or alternate geometry cache.

## 6. Invariants and enforcement

| Invariant | Required machine check |
|---|---|
| Same evaluated frame in raster and AS | `rt_dynamic_same_frame_gpu_write_then_hit` with alternating disjoint geometry; one submission, no intermediate wait |
| In-place topology change rebuilds | `rt_dynamic_same_capacity_cut_rebuilds` and indexed mutation case |
| Static peers cost no BLAS work | `rt_dynamic_selective_updates_and_idle` counters |
| Metadata survives fusion/cache/reload | `mesh_change_fused_rules_match_unfused`, production fused modifier proof, cache round-trip |
| Pending propagates | `mesh_change_pending_through_fusion_and_alias`, first-ready ray test |
| Instance transforms follow BLAS bounds | `rt_dynamic_deform_then_instance` analytical hit oracle |
| Current emission, no CPU geometry readback | `rt_dynamic_emissive_gpu_geometry`, negative source check in acceptance runner |
| Appearance parity | `rt_dynamic_coverage_and_attributes` value probes |
| Lifetime and snapshot correctness | `rt_dynamic_unsubmitted_teardown_and_multiframe` |
| Export exactly once/current | `rt_dynamic_export_first_frame_and_state_steps` |
| No stale temporal samples | `rt_dynamic_history_reset_and_resume` |
| No new per-frame resource allocation | prepared steady-state allocation/capacity counters in `rt_dynamic_bounded_perf` |
| No unsupported future primitive opt-out | unknown producer test; catalog coverage requires every mesh output to resolve a rule |
| No partial publication on admission failure | `rt_dynamic_admission_is_atomic` |

Exact fixtures, numerical thresholds, commands and failure interpretation are in the acceptance document. Test names there are deliverables, not claims that they already exist.

## 7. Phasing — K3 execution briefs

All phases: fresh K3 session, read back this phase's decisions/forbidden moves before coding; use the existing slot ring from a verified `origin/main` tip. Lead owns diagnosis/review/landing; workers do not delegate or land. Re-run the inventory commands; a count/site change requires a written delta review before edits. Use `scripts/codex_prepare.py` for worker briefs and `scripts/codex_checks.py` for changed-file checks. No Claude/K3 configuration changes are part of this design.

Checks below use an absolute shell variable `RT_WORKTREE` for the acquired slot. No app implementation is performed by this design-authoring task. Each phase updates its marker and this header when actually landed. No phase may claim acceptance from its own report alone: the lead reviews code and gate output, runs the required landing gate, and owns Peter's demo handoff.

### P0 — Establish failing numerical witnesses (LANDED)

Entry: audit base plus current `BUG-e3p6.4`; read sections 1–2 and acceptance A0–A3. Scope: new `tests/gpu_proofs/rt_dynamic_geometry.rs`, proof module registration, minimal production-helper debug ray query in `manifold-gpu`.

Deliver: deterministic ray-query helper and CPU triangle-intersection oracle, small canonical fixture builder, counters, pending-source and emission witnesses. Existing unsupported behavior is demonstrated by an explicit baseline probe whose expected observation is stale/missing current geometry; do not commit ignored/red tests. The passing implementation gates are introduced with their owning fixes. Record baseline values, not a screenshot judgment.

Gate: originally `rt_dynamic_baseline`, now `gpu_proofs_gate.py --filter rt_dynamic_oracle` after replacing the unsupported-state witness; ray oracle must distinguish the two geometry states and a deliberately wrong hit result. Negative: no alternate modifier math in production, no ignored tests. Demo: numeric report and diagnostic PNG, L1 plus Peter artifact. Gesture: alternate Surface Waves phase at a held camera. This is the one baseline reproduction; do not run broad RT experiments.

### P1 — Mesh revision metadata (LANDED)

Read-back: section 3.1–3.2, `bindings.rs`, `execution.rs`, `execution_plan.rs`, `effect_node.rs`, `primitive.rs`. Deliver exact traits/types, compiled rules, revision commit helper and pending propagation, including alias, recycled slots, memo/hoist, in-place and feedback cases. Existing node declarations remain conservative until catalog declarations are added. No RT behavior change yet.

Gate: renderer CPU tests filtered `mesh_change_`; focused renderer clippy. Negative: no serde fields and no new identity registry/locks. Demo: none — L1. Invariants: revision uniqueness, pending/unchanged independence, metadata conservatism.

### P2 — Fusion and primitive declarations (LANDED)

Read-back: section 3.3 and inventory. Deliver prepared-view sidecars, shared installation helper, compiler-first accessor migration, the audited declarations needed by stock modifiers and explicit conservative defaults for other catalogued mesh writers, cache invalidation, custom-WGSL conservative behavior. Use actual shader semantics, not names or equal buffer capacity. Keep all current fusion opportunities; do not insert boundaries to dodge metadata.

Gate: `mesh_change_` CPU suite; `gpu_proofs_gate.py --filter rt_dynamic_fusion`; focused renderer clippy. Negative: old def-only accessor symbols absent and no serialized mesh-rule/WGSL trust markers. Demo: fused/unfused numerical report, L1 plus PNGs for Peter. Gesture: reorder two modifiers then drive the outer phase control. Acceptance requires a fused Surface Waves path to select the same eventual update class as unfused.

### P3 — Caller-ordered acceleration and resident storage (LANDED)

Read-back: section 4; existing `accel.rs`, tracer trait, encoder lifetime and GPU fault code. Deliver new trait/API, retained build/refit scratch and descriptors, sizing/admission hooks, safe input snapshots and completion pins; migrate all build/refit call sites. Initially all dirty mesh updates build. Preserve the currently unsupported deformation boundary until P5; this phase does not claim modifier RT completion.

Gate: `gpu_proofs_gate.py --filter rt_dynamic_ordering`, existing `rt_instancing` filter as selected by checks; focused gpu/renderer clippy and tests. Negative: no private AS `commit`, `waitUntilCompleted`, or CPU geometry reads introduced. Demo: deterministic same-command-buffer moving-triangle report, L1 plus artifact. Gesture: transform a single instance. Completion faults cannot become successful readiness.

### P4a — Current GPU emission (LANDED)

Read-back: section 5.1 and existing emissive tests. Deliver GPU candidate/sort/gather/alias/stats preparation, stats-buffer consumers including firefly clamp, and descriptor/texture/resource pins. Static emissive behavior is the reference; remove CPU geometry caches, not merely their callers. Production order is shared with P3.

Gate: `gpu_proofs_gate.py --filter rt_dynamic_shading`, required existing emission/alpha/normal tests; focused gpu/renderer clippy. Negative: `EmissiveTriangleCpu`, `local_triangles`, and production `refit_emissive_table` absent. Demo: current-emitter vertex/area/UV/coverage report and PNG, L1 plus Peter artifact. Gesture: deform an emitter, then return emission from zero to positive. All sample data must refer to the current frame.

### P4b — Appearance and indexed hit attributes (LANDED)

Read-back: section 5.2, current candidate walkers and raster appearance formula. Deliver checked weight/index source fields, shared index resolution, deterministic fractional appearance coverage and current emitter appearance; preserve existing material limitations explicitly. Remove the weights/gain rejection only with the passing proof. No new material model.

Gate: `gpu_proofs_gate.py --filter rt_dynamic_shading` plus changed alpha/normal tests selected by the gate; focused GPU/renderer clippy. Negative: no duplicated appearance logic across ray walkers and no per-frame mapped source-table writes. Demo: coverage distribution and indexed UV/normal report, L1 plus diagnostic PNG. Gesture: vary gain through zero, fractional and HDR values. Readback values must match the current mesh.

Landed: `rt_dynamic_coverage_and_attributes` (9 sections) green plus full gpu_proofs gate 206/206; the one root-cause fix was restoring the any_hit terminal committed-hit check after the walker rewrite.

### P5 — Enable the shared dynamic scene path (IMPLEMENTED; acceptance evidence and limits in A11; after P2, P4b)

Read-back: sections 4–5, render-scene collection/flags/reset order, export inventory. Deliver revision-driven selective rebuilds, unified current-frame flags, pending/error handling, general topology-history integration, conservative deformation resets, frame-validity propagation through nested encoders/export, aggregate resource admission and warmup. Remove content-settle/deferred-ready behavior and update authoritative RT/modifier contracts. This is the first complete user-visible milestone: correct dynamic geometry with rebuild costs, live and export.

Gate: `gpu_proofs_gate.py --filter rt_dynamic_current_frame`; renderer/gpu CPU tests and clippy; current-state/first-frame export test. Negative: old defer/gate/latch symbols absent. Demo: same-frame RT/raster raw hit report plus production modifier UI flow, target L3. Gesture: turn RT on, animate Surface Waves continuously, pause and resume. Budget here is correctness/resource safety, not a claim of 60 fps rebuilds.

### P6 — Selective BLAS refit (IMPLEMENTED; acceptance evidence and limits in A11; after P5)

Read-back: sections 2 D4, 3 rules, 4 dirty order; Apple refs; A3/A4. Deliver actual in-place BLAS refit for position-only changes, followed by TLAS update, using retained scratch. Reference remains a freshly rebuilt AS from the identical GPU output. No new runtime mode/switch. Same descriptor list and instance builder.

Gate: `gpu_proofs_gate.py --filter rt_dynamic_refit`; degenerate/revival and bounds-expansion tests mandatory; focused gpu/renderer clippy. Negative: no full-list BLAS rebuild for a single proven deformation. Demo: exact action counts and hit parity, L1 plus artifact. Gesture: increase wave amplitude beyond the original mesh bounds. Failure of a backend capability proof keeps the correct rebuild path and blocks fast-path qualification; report it to lead after the bounded attempt budget.

### P7a — Production catalog and saved-project acceptance (IMPLEMENTED; acceptance evidence and limits in A11; after P6)

Read-back: complete acceptance matrix; existing modifier journey and export repro. Deliver discovery-driven stock catalog cases, composition cases, authored custom producer, and project save/reload + undo/redo. Include all documented modifier recipes, hidden stock recipes included; count-match the catalog.

Gate: `gpu_proofs_gate.py --filter rt_dynamic_catalog`; renderer/editing tests selected by `codex_checks.py`. Negative: no skipped hidden recipe and no implementation branches on modifier recipe IDs. Demo: catalog numerical report and saved-project artifacts, L1. Gesture: reorder Waves + cuts + echoes, save/reopen and modulate phase.

### P7b — Production export and performer flow (IMPLEMENTED; acceptance evidence and limits in A11; after P7a)

Read-back: acceptance A7–A8, export inventory and existing journey harness. Deliver the acceptance runner, production export frame-one/state-count/failure/cancellation/HDR assertions and one registered UI flow using existing controls. Do not change export simulation semantics to settle RT.

Gate: focused app tests `rt_dynamic_export_` with `journey-proofs`; execute registered UI flow and deterministic export artifact; focused app clippy and diff-selected checks. Negative: no affected export frame reaches the media encoder before successful GPU completion. Demo: L3 plus video/PNGs for Peter; L4 stays pending until Peter tests. Gesture: enable RT, animate/reorder modifiers, save/reopen and export.

### P8 — Bounded performance and final delivery (IMPLEMENTED; acceptance evidence and limits in A11; after P7b)

Read-back: acceptance A9; current warmup and admission behavior. Deliver one bounded measurement run on this Mac with reference and held-out scenes, preparation/steady-state timings, allocation counts and memory peak. Fix only evidenced failures within this design; do not tune unrelated shaders. This phase does not expand into a general renderer optimization campaign.

Gate: acceptance runner `--mode perf`, repeat only after changed code or new evidence; `MANIFOLD_RENDER_TRACE=1`, required landing gate through `scripts/land_branch.py`, diff-scoped GPU proofs. Exact bounds are in A9. Negative: no hidden sample/resolution changes, no fresh steady-state AS/scratch/table allocations. Demo: report plus exact worktree release launch command for Peter, L3 reached/L4 pending as appropriate. Gesture: sweep wave phase while RT is on, then pause; static AS work and resets must stop.

Final lead pass: cover every invariant and acceptance row, update `BUG-e3p6.4`, update this status/phase markers and existing contracts, commit exact paths, land/push through the gate, and release the slot. If unfinished, preserve through the repository's reviewed retirement/archive workflow. A handoff note alone is not preservation.

## 8. Decided — do not reopen

1. One post-modifier geometry path; no modifier-specific RT implementation.
2. Conservative unknown-producer rebuild; explicit, tested refit declarations.
3. Revisions and readiness survive fusion, aliasing, reload and graph replacement.
4. GPU-ordered AS work on the caller's encoder, without a CPU mid-frame wait.
5. Current GPU emission/appearance and safe input snapshots are required scope.
6. Rebuild correctness precedes refit optimization; live/export share both.
7. Deformation invalidates temporal history in v1; noise is an explicit cost.
8. Existing memory admission and GPU-failure deadlines remain binding.
9. No new serialized field, RT modifier toggle, shared lock, or background worker.

## 9. Deferred

* Accurate per-vertex deformation motion vectors and local temporal rejection: revive after this contract passes, if measured noise warrants the added previous-geometry storage and correspondence work.
* Adaptive rebuild-after-many-refits quality heuristics: revive only from measured total update-plus-trace regression. V1 does not guess a deformation-distance threshold.
* BLAS deduplication across scene objects/views and reuse across changed membership: separate optimization after correctness/action-count gates.
* Non-triangle procedural RT primitives, splat/volume integration, and additional transparent transport: require their own representation/intersection contract. Future triangle-mesh modifiers already inherit this one.
* New emissive estimator/cap policy and importance-sampling quality improvements: unchanged here; GPU preparation preserves current policy.

There are no product-choice blockers for P0. Hardware behavior and performance are acceptance gates with explicit outcomes, not unrecorded architectural forks.
