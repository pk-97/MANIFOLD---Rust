# Physics engine boundary — concrete engines, shared authoring

**Status:** APPROVED · 2026-10-06 · review amendments folded; implementation pending.
**Prerequisites:** none for P1 or G1a; later phase entries name their dependencies. Water F1b/F2 shipped.
**Execution contract:** read [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5–6 before starting a phase. This task authorizes documentation only.

<!-- index: Engine, graph, and authoring boundaries for physics; Water is the first consumer, with shared insertion, controls, and lifecycle. -->

The engine owns simulation, not projects, graph editors, or scene panels. Graph adapters turn authored inputs into simulation work. Authoring owns groups, exposures, and undo. These are three boundaries within the existing infrastructure. GPU numerical moves use a private workspace crate; external extraction and public release remain separate decisions.

Peter's direction, verbatim, 2026-10-06:

> if this is a large build it's worthwhile to also consolidate these into sensible API boundaries and interfaces and groups in our existing infra. You might need to refactor some other areas to get this working well and simple. This is an extremely complex system we are building with our custom physics-api. The infrastructure, UI, and UX around it needs to be unified too and exist as a cohesive system that is largely independent where possible. I might want to give back to the community one day in the future and provide access to the engine for other projects.

Companions:

- [PHYSICS_DIRECTION.md](PHYSICS_DIRECTION.md): approved physics direction and coupling requirements.
- [LIQUID_SOLVER_SEAM_DESIGN.md](LIQUID_SOLVER_SEAM_DESIGN.md): concrete solver seams, captures, clock, and coupling. Its owning session controls its open phases; this design does not amend them.
- [FLUID_ENGINE_INTEGRATION_PLAN.md](FLUID_ENGINE_INTEGRATION_PLAN.md): native integration, provenance, recording, and outstanding acceptance.
- [WATER_FAMILY_DESIGN.md](WATER_FAMILY_DESIGN.md): D1–D10 remain binding. F1b and F2 shipped on that document; this document later converges their implementation.
- [NODE_GROUPS_DESIGN.md](NODE_GROUPS_DESIGN.md) and [GROUPING_GRAPHS.md](GROUPING_GRAPHS.md): existing group interface and identity rules.
- [WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) section 5b (param-surface recipe): the only manifest-backed control surface.

## 1. Audit — what exists

Verified 2026-10-06 against `c67c1e9ad84a51db2dc3f433247a1d6100cd5eb7`, branch `feat/physics-boundary-design`. HEAD and clean working state were checked before reading. This is a static source audit, not a runtime or visual verification. **Extend the listed infrastructure; do not redesign it.** Line numbers are snapshot anchors and must be re-resolved before implementation. Amendments, dependency metadata, and duplication were checked at `16dd977ab` (reviewed boundary-design commit), with a clean worktree before this edit.

Path abbreviations below are exact repository-relative prefixes: `P = crates/manifold-physics/src/`, `F = crates/manifold-fluids/src/`, `R = crates/manifold-nodes/src/node_graph/`, `C = crates/manifold-core/src/`, `E = crates/manifold-editing/src/commands/graph/`, `A = crates/manifold-app/src/`, `U = crates/manifold-ui/src/`.

### 1.1 Engine and host code

| Piece | Source used by the executor | Keep or separate |
|---|---|---|
| CPU rigid world and Box3D | `P/lib.rs:207`, `:248`, `:322`, `:350`, `:690`; `crates/manifold-physics/native/box3d/include/box3d/box3d.h` (`b3CreateWorld`, `b3World_Step`) | Concrete owned world, body handles, typed errors. Keep the wrapper and existing native lock. |
| CPU FLIP and coupling | `F/lib.rs:43`, `:524`, `:903`, `:920`; `F/coupling/owner.rs:23`, `:107`; `crates/manifold-fluids/build.rs:10`, `:150` | `flip_engine` is vendored C++, not a Rust crate. Keep its concrete API and coupling owner; no native solver changes. |
| Clock and completion | `P/clock.rs:94`; `P/stepping.rs:29`, `:149`, `:316`; `R/liquid/clock.rs:2` | Reuse SimulationClock, StepInterval, CompletionLedger, and LiveStepSchedule. |
| Graph adapters and worker | `R/physics.rs:2`–`:27`; `R/fluid.rs:9`–`:29`, `:480` (`Worker`) | Core time, geometry, roles, caches, transforms, and worker handoff remain host code. `fluid.rs:91` is domain_size, not worker state. |
| Liquid records and graph services | `R/liquid/bodies.rs:11`, `:35`, `:199`; `lattice.rs:5`, `:60`; `fields.rs:28`–`:40`; `coupling.rs:16`–`:25` | Separate numerical records from contexts, scene inputs, and renderer preparation only in deferred moves. |
| Shared graph recognition and validation | `C/liquid_domain.rs:21`, `:114`; `R/liquid/extent.rs:22`–`:32`; `conformance.rs:217`, `:256`; `primitives/liquid_state.rs:50` | Reuse the single domain list/index, extent checks, and capture contract. Keep graph validation outside the engine. |
| GPU FLIP stages | `R/primitives/gpu_flip_step.rs:660`, `:1056`, `:1282`; `gpu_flip_pressure.rs:26`; `gpu_flip_bodies.rs:30`; `gpu_flip_clock.rs:17`; `gpu_flip_narrow_band.rs:9`; `gpu_flip_sheeting.rs:42` | Encode already takes manifold-gpu's encoder. Graph constants, allocation admission, and generated kernels still cross the proposed boundary. |
| Whitewater stages | `R/primitives/whitewater_step.rs:593`, `:666`, `:1121`, `:1147`, `:1691`; `whitewater_type.rs:9` | Split numerical scratch/dispatch from fence and display publication. WhitewaterSpawn is a production manifold-fluids dependency. |
| GPU and generated-kernel dependencies | `R/liquid/grid.rs:10`–`:13`; `crates/manifold-node-engine/src/gpu/gpu_encoder.rs:18`, `:35`; `R/primitives/standalone_pipeline.rs:8`; `whitewater_step.rs:745`, `:914` | Renderer encoder carries host services; generated atom kernels depend on descriptors/codegen. Supply manifold-gpu resources and prepared kernels, never copied shader implementations. |
| Allocation dependency | `R/scene_modifier_expand/buffer_budget.rs:310`, `:343`; `primitives/gpu_flip_step.rs:601`; `gpu_flip_narrow_band.rs:208`; `gpu_flip_sheeting.rs:196`; `whitewater_step.rs:954` | Preserve admission while separating checked arithmetic from scene policy/errors. |
| Shatter | `R/scene_modifier_authoring.rs:29`; `scene_modifier_expand/compiler/shatter.rs:156`, `:174`, `:233`; `C/scene_modifier_preset.rs:195` | A modifier recipe compiled into rigid participants, not a solver. |

Cargo manifests and `cargo metadata --offline --format-version 1` were checked at the reviewed commit, including normal, build, dev, and target-conditioned workspace edges. Physics depends on foundation; fluids on foundation/physics; GPU and UI on foundation. None of these four has a host dependency. Renderer depends on physics, fluids, GPU, core, native, playback, and UI (`crates/manifold-nodes/Cargo.toml:8`–`:17`). The exact ban allowlists and the cargo-deny check are in section 3.2. No implementation dependency fix is needed today.

The production source import search `rg -n 'use .*manifold_(renderer|app|ui|core)|crate::(app|ui)' crates/manifold-{physics,fluids}/src` returned zero. GPU primitives and liquid/whitewater atoms still use graph contexts, descriptors, and freeze codegen; those are deferred separation work, not evidence of an independent GPU engine today. MPM keeps its concrete `matter_*` stages. Remove FLIP's `matter_domain::closed_faces` dependency (`gpu_flip_domain.rs:25`) during the numerical moves, without a universal solver trait.

### 1.2 All scene entry paths in scope

| Entry | Graph, metadata, exposure, and undo path | Duplication to remove |
|---|---|---|
| Add Fluid | `A/ui_bridge/project.rs:17`–`:36`, `:652`–`:659` selects the GPU recipe. `E/scene/fluid/template.rs:16`, `:28`, `:46`, `:83` defines six exposure categories and the CPU recipe. `E/scene/fluid.rs:66`, `:215`, `:322`, `:371`, `:390`, `:448` renumbers, stamps, wraps one output, wires World controls, and snapshots graph/instance state. | The command takes separate metadata vectors, has its own transaction, and assumes one object output. `exposed_type_id` (`E/scene/fluid/template.rs:59`) searches only top-level body nodes. The template is liquid-specific despite doing ordinary group insertion. |
| Add object in an existing physics scene | `E/scene.rs:239`; `E/scene/physics.rs:5` (`append_physics_scene_object`) | Separate transform/body/mesh/material/object construction and slot/handle allocation. This path exists as well as explicit Enable Physics. |
| Enable/disable Box3D physics | `A/ui_bridge/project.rs:851`–`:884`; `E/scene/physics.rs:647`, `:1049`, `:1141`, `:1182`, `:1245`, `:1268`, `:1344` | Group and loose-object recipes plus exposure stamping. Existing-body enable uses `SetGraphNodeParamCommand`; first enable uses `EnableSceneObjectPhysicsCommand`. Disabling in the UI writes `enabled`; it is not deletion of the engine world. `DisableSceneObjectPhysicsCommand` also exists at `:1399`; do not conflate these operations. |
| Split/imported object physics | `E/scene/split.rs:379` reuses `add_group_physics`; `R/physics_mesh.rs:38`, `:75`, `:233` resolves assets and prepares colliders. | Asset loading/cooking inputs are host preparation. The numerical world should receive immutable mesh/hull data, never a project path or an import card. |
| Duplicate a physics object | `E/scene/duplicate.rs:119`–`:179`, `:234`, `:453`, `:515`, `:530`; `E/scene/physics_match.rs:6` (`PhysicsSceneObject`), `:748` (`first_free_physics_body_slot`); `scripts/ui-flows/scene-physics-duplicate-paused.json` | Validates ownership and a free world slot, clones bodies/object outputs with fresh IDs, remaps numeric/string metadata and fluid routes, then keeps its own graph/instance snapshots for undo/redo. Nested physics, copies objects, malformed ownership, and full worlds reject. Converge this transaction in P6a; preserve paused duplicate behavior and Water's no-duplicate rule. |
| Model import | `R/gltf_import/mod.rs:92`; `merge.rs:35`, `:379`; `A/import_worker.rs:162`; `A/ui_bridge/project.rs:1058`, `:1077`; `E/scene.rs:2666`, `:2690`, `:2721` | Import produces mesh/material/object graph and exposure lists. It does **not** automatically create a physics world/body. A later Enable Physics or role assignment makes colliders relevant. Import's graph/metadata commit is another bespoke transaction to converge. |
| Shatter | `A/scene_modifier_edit.rs:52`, `:96`, `:101`, `:112`, `:470`; `R/scene_modifier_authoring.rs:29`; `E/modifier_stack.rs:264` | Prepare against the selected imported physics objects, then insert a serialized modifier with atomic stack undo. The compiler requires one active shatter and supported static undeformed imported geometry. Do not remove these admission checks to unify the UI. |
| Fluid roles | `A/ui_bridge/project.rs:601`; `E/scene/fluid/roles.rs:34`, `:99`, `:219`, `:595`, `:616`, `:657`, `:805`; `roles/lifecycle.rs:126`, `:158` | Assignment creates/reroutes source/role graph nodes, stamps role controls, and snapshots candidate state. Remove and retarget have separate lifecycle work. The source scene object is not owned by Water merely because Water consumes its role. |
| Bundled/test recipes | `R/primitives/gpu_flip_preset.rs:76`, `:364`, `:707`, `:984`; `E/scene/fluid/template.rs:83` | Preset construction and Add Fluid assemble related metadata separately. One recipe must supply both. CPU FLIP remains compatibility/reference coverage, not an extra new Water choice. |

The better transaction already exists: `E/modifier_stack.rs:119` (`StackTransaction`), `:133` (`prepare`), `:200` (`execute`), `:230` (`undo`). It checks the expected owner and preserves instance-layer state. Its pure result is `C/scene_modifier_edit.rs:20` (`SceneModifierGraphEdit`). Generalize that transaction, rather than adding a PhysicsEditingService.

### 1.3 Scene panel and persistence

| Piece | Source | Assessment |
|---|---|---|
| Object ownership | `R/scene_vm.rs:196`, `:253`–`:271`, `:570`, `:967`, `:1344` | `SceneObjectKnownRow` carries optional physics, domain, and `fluid_controls`; group projection synthesizes rows. Ownership discovery is renderer-specific. |
| Card selection | `A/ui_bridge/projection/scene.rs:39`, `:56`, `:80`, `:102`–`:122` | App adds object, liquid controls, role nodes, body, transform, material, and modifier ancestry. This is a second ownership assembly, not just projection. |
| Manifest controls | `U/param_surface.rs:1`–`:19`, `:291`, `:318`; `U/panels/scene_setup_panel.rs:847`, `:998`, `:1017`, `:1275`, `:2240` | ParamSurface already renders and routes the numeric controls. Reuse it. Do not create Water sliders beside it. |
| Bespoke action rows | `U/panels/scene_setup_panel/material_inspector.rs:1052`; `fluid_roles.rs:1`; `scene_setup_panel.rs:2802`–`:2823` | Enable/Disable Physics and role target/remove/add are action chrome. They need common capability/ownership data, but are not duplicate numeric editors. |
| Force/modifier cards | `U/panels/scene_setup_panel/forces.rs:12`; `object_modifiers.rs:46` | Already ParamSurface consumers. Keep shared card hosts. |
| Authored storage | `C/effect_graph_def.rs:84`, `:106`, `:114`, `:275`, `:385`, `:425`, `:450`; `C/scene_modifier_preset.rs:26` | Nested groups, stable NodeIds, exposures, strings, modifier recipes, and scoped references are serialized. No new physics document store is needed. |
| Load/save | `crates/manifold-io/src/saver.rs:12`, `:41`; `crates/manifold-io/src/loader.rs:41`, `:206`, `:238`; `A/project_io.rs:515`, `:532`–`:534`, `:734`, `:766` | Project decoding, manifest reconciliation, and scene migrations are existing host responsibilities. Engine handles are rebuilt; they are not durable scene identity. |
| Simulation takes | `R/fluid_cache.rs:61`, `:206`, `:425`, `:485`; integration plan's provenance/collected-project checkpoints | Separate versioned cache binding and authenticated state, not ordinary graph serialization. Old cache decoding is not proof that a cache is compatible with the current graph. No Bake UI is authorized here. |

Plainly: there are duplicate graph transactions, per-feature exposure routing, and two layers discovering control ownership. There are also legitimate differences: native workers versus GPU dispatch, imported collider preparation versus simulation, and Shatter recipes versus solvers. Unification removes the first set without erasing the second.

### 1.4 Concurrent work and evidence limits

⚠ **VERIFY-AT-IMPL:** Water F1a is not at this HEAD. Peter's current scope is one Water group, four object outputs, and the obstacle source **inside** the group. Read `R/primitives/gpu_flip_preset.rs` and the landed F1a tests before P2/P3. Do not reinstate the older external obstacle-source input from the checked-in Water design. Confirm actual port spellings with `rg -n 'GroupInterface|GroupPortDef|group_output|obstacle' crates/manifold-node-engine/src/water/primitives/gpu_flip_preset.rs`.

The liquid seam document marks P7a unaudited, P8 owing L3 acceptance, P10 open, and P5/P6 retired. Source is ahead of parts of the document; this audit does not mark its open phases complete. P1–P3, P6a/P6b, P7/P8, and G1a need no unlanded seam phase. G1b/G2/G3 are deferred under the explicit trigger in section 9. Water F1b/F2 shipped under their own approved design. No nested regions are introduced.

The integration plan still names incomplete recording/provenance and collected-take acceptance: BUG-vglg (Integrate CPU FLIP liquids with shared scene physics), child BUG-vglg.17 (Complete coupled input-take identity and paired cache playback). Nothing here relaxes those guards. Water D8 keeps simulation advancing when its display is hidden.

## 2. Decisions

**D1. Enforce dependencies now; package GPU numerical code when it moves.** P1 adds Cargo bans for the existing engine/UI boundaries. Deferred G1b creates the private workspace crate `manifold-physics-gpu`, depending on foundation, gpu, physics, and fluids; renderer depends on it. Cargo's acyclic dependency graph checks the boundary. This is workspace packaging, not external extraction or release. Rejected: a renderer submodule plus lexical import checks or a cargo-from-test compile probe; those leave the boundary weaker than the crate graph. The lead made this amendment with Peter informed.

**D2. Use concrete engines, not a universal solver trait.** Keep `PhysicsWorld`, `FluidWorld`, `RigidFluidCoupling`, and concrete GPU stages. Shared data, time, completion, and coupling protocols are enough. Rejected: `dyn PhysicsSolver`, `dyn LiquidSolver`, one universal domain node, or a solver enum threaded through the UI. They hide different state and coupling obligations and contradict the liquid seam.

**D3. A physics object is an ordinary group with an analyzed ownership boundary.** Use existing typed ports, captures, NodeIds, and `FlatSceneIndex`. The common contract is structural; there is no mandatory universal list of solver buffers. Water is the first multi-output instance. Rejected: a second scene/entity registry or label-derived family membership.

**D4. One graph transaction and one template insertion algorithm.** Generalize the existing modifier transaction. Recipes and pure edits remain different functions; all commit through it and EditingService. Metadata is complete before commit. Rejected: sequential Add Water/Add Foam/Add Spray/Add Bubbles commands, because partial undo and orphan exposures are inevitable.

**D5. One derived scene-object model selects all physics cards and actions.** Core analyzes authored structure; app projects immutable rows into UI-owned types. ParamSurface owns every manifest-backed control and its modulation. Action chrome remains ordinary shared scene-panel chrome. Rejected: a Water panel, a physics inspector tree, or UI imports from core/renderer.

**D6. Identity, visibility, enablement, and deletion are separate.** Stable scoped NodeIds identify authoring objects. Engine handles are runtime-only. Hide preserves simulation; disabling a rigid body retains its authored configuration; deleting Water removes its owned graph and exposures and detaches external roles atomically. Child visibility is independent. No family duplication, including keyboard/context dispatch.

**D7. Preserve the numerical and temporal contracts.** Beats and the host tempo map remain transport authority. Existing clock/interval/coupling code supplies typed seconds to engines. Preserve one owner and one completed publication per stamp, body mass/inertia inside the incompressible solve, and matched whitewater/liquid captures. Rejected: render-delta stepping, a UI clock, and advancing a shared rigid world independently in each object group.

**D8. Preserve Water D1–D10 exactly.** Water is a real parent with its own look; Foam/Spray/Bubbles are look-only children with material, Size, and visibility. Emission and Amount stay on Water; Amount defaults to 1. Dust remains simulated/captured but has no rendered child. Reuse `gpu_flip_preset`; regenerate grouped presets; no old-project upgrade or version bump just for Water. Particle View replaces only Water's display. No duplicate action.

**D9. Converge existing paths in separate phases.** Add Fluid and Water converge first. Rigid enable, import, fluid roles, and Shatter follow independently. A hidden old writer is not a compatibility strategy. Existing serialized node IDs/ports and saved modifier recipes remain readable.

## 3. Engine boundary

### 3.1 What another project uses

An embedding project supplies geometry, forces, physical parameters, elapsed simulation intervals, and GPU resources when needed. It receives poses, particles, surfaces, diagnostics, and completed interval identity. It does not supply `Project`, `PresetInstance`, `EffectNodeContext`, scene cards, graph IDs, file paths, playback controllers, or renderer encoders.

The CPU public API is the existing concrete API, not a new facade:

```rust
// manifold-physics; existing signatures remain authoritative.
PhysicsWorld::new(gravity: [f32; 3]) -> Result<PhysicsWorld, PhysicsError>;
PhysicsWorld::step(&mut self, dt: Seconds, substeps: u32) -> Result<(), PhysicsError>;
PhysicsWorld::apply_impulses(&mut self, impulses: &[BodyImpulse]) -> Result<(), PhysicsError>;
PhysicsWorld::pose(&self, handle: BodyHandle) -> Result<BodyPose, PhysicsError>;

// manifold-fluids; existing signatures remain authoritative.
FluidWorld::new_seeded(config: Config, seed: u64) -> Result<FluidWorld, FluidError>;
FluidWorld::step_live_with_fields(
    &mut self, dt: Seconds, fields: &[FieldInput<'_>],
) -> Result<FrameStats, FluidError>;
FluidWorld::surface(&mut self, output: &mut Vec<SurfaceVertex>) -> Result<(), FluidError>;
```

Creation also uses the existing `BodyConfig`, mesh/hull inputs, `add_hull`/`add_hulls`/`add_triangle_mesh`, and `Config` options. Coupled clients use `RigidFluidCoupling::prepare` and `begin_frame`/`begin_live_frame`; they must not call both worlds' independent step methods for the same interval. Preserve their existing borrowing signatures at `F/coupling/owner.rs:49`, `:107`, `:130`. No adapter or public API rename is required for these CPU calls.

⚠ **VERIFY-AT-IMPL:** the GPU field/method inventories in this section are snapshots, not frozen declarations. The seam/body/sheeting/tick sessions are editing them. Before G1b/G2/G3, re-read `R/primitives/gpu_flip_step.rs`, `whitewater_step.rs`, and `R/liquid/substep_history.rs`; run `rg -n 'struct (Step|StepState|StepParams|StepFrame|StepInputs)|fn (encode|emit|tick|publish)' crates/manifold-node-engine/src/water/primitives/{gpu_flip_step,whitewater_step}.rs`, and update the reviewed mapping. Never restore an old field layout to match this document.

All new or moved fallible public engine methods return `EngineError`, one typed enum added in the new `manifold-foundation::engine_error` module in G1a. Existing CPU wrapper signatures above remain compatibility APIs; their typed PhysicsError/FluidError values convert when crossing a new stage boundary. Only the host converts EngineError to its scene diagnostic. Callers branch on variants, not detail strings.

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    InvalidInput { field: &'static str, detail: String },
    InvalidState { detail: String },
    InvalidHandle,
    Capacity { required: u64, available: u64 },
    Allocation { requested_bytes: u64, allowed_bytes: u64 },
    Kernel { operation: &'static str, detail: String },
    Backend { operation: &'static str, detail: String },
}
```

Implement Display and std::error::Error. Convert low-level GpuAllocationRefused to Allocation; invalid dimensions/layouts to InvalidInput; missing prepared resources to InvalidState; backend failures to Backend; descriptor/packing failures to Kernel. Do not collapse all failures into Backend. Private helpers may retain existing error types; new public engine methods never return `Result<_, String>`.

The future GPU embedding surface is concrete stage preparation plus encoding against `manifold_gpu::{GpuDevice, GpuEncoder, GpuBuffer}`. It exposes existing validated physical records and buffer views, not graph ports. Deferred G1b/G2/G3 relocate accepted numerical types under `crates/manifold-physics-gpu/src/`. The crate is private workspace packaging, not a released SDK. Public distribution stability is not promised for packed GPU records.

The load-bearing FLIP encode seam preserves the existing shape (pipeline provisioning is split below):

```rust
// manifold_physics_gpu::flip, moved from primitives/gpu_flip_step.rs.
// StepState -> FlipState; Step<'a> -> FlipStep<'a>; StepParams -> FlipParams.
impl FlipState {
    pub fn prepare_pipelines(&mut self, device: &GpuDevice, kernels: &FlipKernels);
    pub fn encode(
        &mut self, device: &GpuDevice, enc: &mut GpuEncoder,
        step: &FlipStep<'_>, clock_params: &GpuFlipClockParams,
    ) -> Result<(), EngineError>;
}
```

`FlipStep` retains **all** fields of `Step` at `gpu_flip_step.rs:1056`: identity, params, clock_plan, particles, out, capped, tally, count, forces, impulses, bodies, shapes, atlas, regions, reaction, dynamic, pressure, level, wall_inset, band, ghost, density, narrow_enabled, restore_narrow, sheet_rate. Types and lifetimes move unchanged; `StepParams`' fields move unchanged too. Scratch preparation remains separate from encoding and uses the existing checked capacity calculations. `StepState.history` is numerical substep-face history (`R/liquid/substep_history.rs:40`), not presentation history: move it with FLIP, but supply its generated component kernel through the kernel seam below. Keep display-frame publication in the adapter. No raw `f32` timing API is added: existing GPU POD seconds are packed from the typed host interval at the adapter boundary.

Whitewater needs a real split, not a move of the whole `Step`. At `whitewater_step.rs:1121`, `Step` owns both numerical scratch and `Outputs`; `advance` at `:1147` handles fences and publication. Keep that host orchestration, `owed`, output slots, `advance`, `advance_tick`, and string-based `tick_output` in the graph adapter. Move numerical fields and methods into `manifold_physics_gpu::whitewater::WhitewaterState`. The adapter holds that state exclusively. `StepShape`, `StepFrame`, `MotionInputs`, `FaceSource`, and `StepInputs` at `:478`–`:708` move field-for-field; `StepInputs` keeps motion, particles, solid, obstacle_source, faces, level_set, distance. Preserve packed/axis admission.

The numerical method seam keeps the existing `emit` and `tick` signatures (`:1357`, `:1585`) with `pub` visibility and manifold-gpu types. Replace publication by slot index with supplied buffers:

```rust
pub struct WhitewaterOutput<'a> {
    // Existing physical order, including non-rendered Dust.
    pub particles: [&'a GpuBuffer; 4], // foam, bubbles, spray, dust
    pub counts: &'a GpuBuffer,
}
impl WhitewaterState {
    pub fn emit(
        &mut self, enc: &mut GpuEncoder, frame: &StepFrame,
        inputs: &StepInputs<'_>, scratch: &[GpuBuffer; 5],
        offsets: &GpuBuffer, emitters: u32,
    ) -> GpuBuffer;
    pub fn tick(
        &mut self, enc: &mut GpuEncoder, device: &GpuDevice,
        frame: &StepFrame, inputs: &StepInputs<'_>, surface: &GpuBuffer,
    ) -> Result<(), EngineError>;
    pub fn publish(
        &self, enc: &mut GpuEncoder, shape: &StepShape,
        output: &WhitewaterOutput<'_>,
    );
}
```

Before: `Step::publish(enc, shape, index)` dereferences `self.outputs.slots[index]` (`:1691`). After: the adapter selects the same retired/free slot and passes its four buffers and counts to `WhitewaterState::publish`. Pool/state buffers and ping-pong state remain numerical state; output-slot selection and fence retirement remain host state. Capacity preparation, seed, and physical buffer accessors move with the numerical fields; their existing arguments remain, with the G1a budget added to allocating methods and public fallible results changed to EngineError. The host's existing `advance` and `advance_tick` retain their signatures and order. No engine method takes a Fence, string port name, or renderer encoder. Both legacy frame publication and tick capture remain supported.

**Ownership:** worlds/stage state have one mutable owner. The existing native worker owns native worlds. The render execution owner owns GPU stage state and retained buffers until completion. Content owns authored project state and sends commands; UI receives snapshots. Other hosts can choose their own scheduling around the same exclusive APIs. No new thread, channel, global scheduler, `Arc<Mutex<_>>`, or `Arc<RwLock<_>>` is introduced.

### 3.2 Dependencies and allocation

P1 adds the entries below to `[bans].deny` in deny.toml, beside the wgpu/metal precedent. Wrappers are legitimate direct parents, not exemptions for protected crates. The additional workspace targets close indirect routes and make UI's foundation-only rule enforceable.

```toml
deny = [
    { name = "manifold-app", wrappers = ["manifold-app"] },
    { name = "manifold-audio", wrappers = ["manifold-app", "manifold-recording"] },
    { name = "manifold-core", wrappers = ["manifold-app", "manifold-audio", "manifold-editing", "manifold-io", "manifold-media", "manifold-playback", "manifold-nodes"] },
    { name = "manifold-editing", wrappers = ["manifold-app", "manifold-playback", "manifold-nodes"] },
    { name = "manifold-fluids", wrappers = ["manifold-nodes"] },
    { name = "manifold-gpu", wrappers = ["manifold-app", "manifold-led", "manifold-media", "manifold-recording", "manifold-nodes", "manifold-spectral"] },
    { name = "manifold-io", wrappers = ["manifold-app", "manifold-editing", "manifold-playback", "manifold-nodes"] },
    { name = "manifold-led", wrappers = ["manifold-app"] },
    { name = "manifold-media", wrappers = ["manifold-app"] },
    { name = "manifold-native", wrappers = ["manifold-nodes"] },
    { name = "manifold-physics", wrappers = ["manifold-fluids", "manifold-nodes"] },
    { name = "manifold-playback", wrappers = ["manifold-app", "manifold-audio", "manifold-media", "manifold-nodes"] },
    { name = "manifold-profiler", wrappers = ["manifold-profiler"] },
    { name = "manifold-recording", wrappers = ["manifold-app"] },
    { name = "manifold-nodes", wrappers = ["manifold-app"] },
    { name = "manifold-spectral", wrappers = ["manifold-app", "manifold-audio"] },
    { name = "manifold-ui", wrappers = ["manifold-app", "manifold-nodes"] },
]
```

These are additional entries, not a replacement config; each gains the reason "Physics and UI dependency boundaries". App and profiler deliberately use self-wrapper sentinels: empty wrappers ban even an unreferenced workspace root. A self dependency cannot form a valid Cargo graph, so these entries admit no real consumer. The installed cargo-deny emits two `unused-wrapper` warnings; explain those sentinels in comments without globally suppressing warnings. [Cargo-deny wrapper semantics](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html#wrappers).

**Existing edges that would trip the intended boundary: none.** The exact candidate allowlists passed `cargo deny check --disable-fetch --config <temporary-config> --metadata-path <captured-metadata> --hide-inclusion-graph bans` with exit 0, existing duplicate warnings, and the two explained sentinel warnings. The initial empty-wrapper candidate failed on app/profiler roots; the sentinels fix that config failure, not a source dependency. Legitimate edges that must remain wrapped include renderer → editing (dev), editing/playback/renderer → IO (dev), and audio → playback (dev). No implementation dependency refactor is required today.

The existing landing leg runs `cargo deny check bans` (`scripts/landing_gate.py:487`–`:493`). P1 also adds `dependency_bans_cover_workspace` to its cheap preflight and tests it in `scripts/test_landing_gate.py`: parse workspace manifests and deny.toml; require every non-foundation MANIFOLD package to have a ban entry; reject protected crates in host-wrapper lists; reject UI in every non-foundation wrapper list. This is manifest/config validation, not lexical import policing. It prevents a new workspace package from silently bypassing the boundary. Test normal/dev/build/target examples with mocked manifest data, without invoking Cargo from a test. The actual cargo-deny leg validates resolved edges. G1b adds the new crate and its allowed lower dependencies to this policy.

The workspace dependencies of manifold-physics-gpu are foundation, gpu, physics, and fluids, plus the existing low-level dependencies used by the moved code. No core, editing, playback, UI, app, native Metal API, or renderer service may be imported by isolated numerical modules. `manifold-gpu` continues to own backend access. Native Metal remains the current backend; shader source names do not authorize a wgpu backend.

Host adapters keep scene-index traversal, registry lookup, parameter resolution, asset loading, collider preparation, graph extent validation, cache provenance, freeze registration, render meshes/materials, diagnostics presentation, and display interpolation policy. Engine code keeps physical data/layout arithmetic, solver state, kernels, dispatch sequencing, and physical diagnostics. A code move must follow this distinction rather than moving an entire `liquid` directory.

Allocation admission must survive the separation. Add the following backend-neutral arithmetic to `manifold-gpu` in G1a, with the current scene policy choosing `allowed_bytes` outside the engine:

```rust
pub struct GpuAllocationBudget {
    pub allocated_bytes: u64,
    pub allowed_bytes: u64,
}
pub struct GpuAllocationRefused {
    pub allocated_bytes: u64,
    pub requested_bytes: u64,
    pub allowed_bytes: u64,
}
impl GpuAllocationBudget {
    pub fn admit(&self, requested_bytes: u64) -> Result<(), GpuAllocationRefused>;
}
```

`admit` uses checked addition; overflow is refusal. Host preparation obtains the existing device snapshot and configured policy once at the allocation boundary. Missing limits remain an explicit error. Pass the budget to scratch-reservation methods that currently invoke scene-modifier admission; preserve rechecks as allocations change. The host turns refusal into its existing named scene diagnostic. This is not a new allocator and does not weaken whole-scene admission. No per-frame graph scan or new scratch allocation is allowed on a steady-state tick.

Generated kernels are another real dependency. `R/primitives/standalone_pipeline.rs:8` imports freeze codegen and `Primitive`; whitewater's `pack<P: Primitive>` at `whitewater_step.rs:745` reads descriptors. Copying their WGSL into the engine would create two numerical implementations. Keep descriptor/codegen preparation in the host and pass prepared kernels plus a graph-free uniform layout at installation:

```rust
pub enum KernelScalar { Float, Signed, Unsigned }
pub struct KernelParam {
    pub name: String,
    pub word: usize,
    pub encoding: KernelScalar,
    pub default_word: u32,
}
pub struct PhysicsKernel {
    pub pipeline: GpuComputePipeline,
    pub params: Vec<KernelParam>,
    pub count_word: usize,
    pub padded_words: usize,
}
impl PhysicsKernel {
    pub fn pack(
        &self, values: &[(&str, f32)], count: u32, output: &mut [u32; 64],
    ) -> Result<usize, EngineError>;
}
```

This is the existing bounded pack operation with descriptor facts supplied as data, not a new shader compiler. Allocate names/layouts only at installation. Validate unique names, word offsets, count placement, and the 64-word bound there. Preserve the current float/int/enum/bool conversion and padding exactly. Unknown named values reject. `FlipKernels` and `WhitewaterKernels` are prepared resource bundles: their fields mirror exactly the generated-pipeline fields of the current numerical dependency closure, replacing each pipeline with `PhysicsKernel`; fixed hand-written shader pipelines remain stage-owned. The phase inventory must list those fields before moving them. `SurfaceDistance` receives its existing generated UpwindDistance kernel; substep history receives FaceSampleComponent; whitewater receives its existing nine `standalone_pipeline` atoms at `whitewater_step.rs:914`–`:922`. Related scan/sort/identity helpers follow the same rule. Their primitive registrations remain and use the same descriptor-generated code. Byte-for-byte uniform packing and generated-versus-standalone parity tests are mandatory. Physical constants move once and descriptors reference that definition.

The kernel layout includes both declared parameters and derived scalar uniforms, with derived defaults zero, in the existing codegen order. This preserves the packing after the ordinary parameter words, not just the parameter list.

An external GPU host supplies these installation resources through manifold-gpu. Providing a packaged default kernel bundle belongs to extraction, not a claim that an external host can already link renderer without its dependencies. The workspace crate consumes kernel data and cannot import the renderer graph compiler; renderer depends on it, so a reverse dependency would form a Cargo cycle.

Consequences, stated honestly: CPU independence already exists. The GPU crate requires moving code, constants, shaders, and tests after the active solver lanes settle. Cargo checks its dependency direction; prepared kernels preserve the shared implementation. A private workspace crate does not settle public API stability, release packaging, or licensing.

### 3.3 Industry grounding

| Proven approach | Adopted here | Deliberate difference |
|---|---|---|
| Box2D v3 uses opaque world/body IDs, definition structs, explicit stepping, and post-step body events. Local Box3D exposes the same create/step/event style. [Box2D simulation API](https://box2d.org/documentation/md_simulation.html); local Box3D header above. | Owned world, runtime handles, concrete configuration, coherent results. | A graph NodeId is durable authoring identity; a body handle is not. Beat transport is translated before stepping. |
| Jolt separates `PhysicsSystem`, `BodyInterface`, temporary allocation, and job scheduling. [Jolt example](https://github.com/jrouwe/JoltPhysics/blob/master/HelloWorld/HelloWorld.cpp). | Host scheduling and resource provision stay outside scene/UI code. | Reuse existing workers; do not introduce Jolt-style scheduling abstractions without a need. |
| PhysX separates simulation submission from completion with `simulate`/`fetchResults`. [PhysX simulation](https://nvidia-omniverse.github.io/PhysX/physx/5.4.1/docs/Simulation.html). | Publish completed state, never partially advanced body/liquid pairs. | Existing completion ledger and GPU fences express this; no extra PhysicsScene service. |
| Bullet exposes world stepping with explicit timestep/substep parameters. [Bullet world header](https://github.com/bulletphysics/bullet3/blob/master/src/BulletDynamics/Dynamics/btDiscreteDynamicsWorld.h). | Engines consume physical time, not UI frames. | MANIFOLD's live/export scheduling policy remains its existing contract; this design does not copy Bullet's accumulator policy. |
| Houdini's FLIP solver has concrete geometry/data inputs. [FLIP Solver](https://www.sidefx.com/docs/houdini/nodes/sop/flipsolver.html). Blender uses simulation input/output nodes to retain state. [Blender simulation nodes](https://developer.blender.org/docs/release_notes/3.6/nodes_physics/). | Concrete stages, visible group interfaces, explicit state capture. | MANIFOLD capture/clock semantics are beat-driven and GPU-resident; regions do not nest. A group is not itself a tick region. |
| Godot exposes low-level physics through server APIs using RIDs independently of scene-node authoring. [PhysicsServer3D](https://docs.godotengine.org/en/stable/classes/class_physicsserver3d.html). | Separate engine identity and authored scene objects. | No global server singleton or second object registry; current owned worlds already provide the boundary. |

The integration of beat transport, modulation, atomic authored groups, and coherent liquid/whitewater/rigid captures goes beyond these individual API examples. It is MANIFOLD's requirement, not a claim that an industry standard guarantees it. Buffer completion, coupling, and post-reload modulation need MANIFOLD tests.

## 4. Graph boundary

Use `GroupDef` and `GroupInterface` unchanged. Every physics group declares its actual external typed ports. Scalar configuration is exposed through existing bindings. Object outputs are ordinary scene-object outputs. No physics-specific group execution mechanism or new serialized group kind is introduced.

The contract is:

1. A group owns its internal authored nodes and metadata targets. Every object output resolves to an actual scene object; duplicate/missing output ownership is rejected.
2. Inputs crossing the group boundary use declared ports or existing exposed-parameter bindings. No lookup by handle, display name, sibling order, or app state.
3. One concrete solver/coupling owner controls a simulation interval. An object may participate in a shared world outside its group; group ownership does not imply a private Box3D world.
4. Mutable tick state stays within the existing tick region. Only declared captures/results escape. `R/substeps.rs:86` (`SubstepBoundaryPorts`) is the interface: seed, capture, state, iteration scalars, results, clock. `liquid_state.rs:50` supplies the liquid instance. No region nesting.
5. Group boundaries may nest structurally. Flattening must preserve scoped identity and tick-region legality. Grouping does not change when a solver steps.
6. One accepted interval publishes a coherent result set. A visible mesh cannot read newer poses with older whitewater. Hide never changes the solver clock, seed, or reset state.

Water exposes four object outputs: Water, Foam, Spray, Bubbles. Port names come from accepted F1a; no second naming migration is imposed here. Its obstacle source stays inside. Water owns the liquid domain, tick region, whitewater stages, three child look branches, and their metadata. External source objects remain outside and route through the existing role/domain attachment path, with ports added through group boundaries by the shared graph operation. Do not add a fixed extra obstacle input to satisfy an obsolete recipe.

Rigid objects use the same structural contract with body/pose/object ports and existing shared `physics_world` wiring. They need no empty liquid ports and no synthetic tick region around every body. The shared rigid owner is the scheduler boundary. Shatter changes the authored modifier/compiled body participants, not that ownership rule. Future solvers add concrete stages and their own captures; they inherit group insertion, identities, rows, and undo without pretending to produce FLIP faces.

## 5. Authoring boundary

### 5.1 Shared edit and insertion API

`C/scene_graph_edit.rs` receives the existing pure result, renamed without field changes:

```rust
pub struct SceneGraphEdit {
    pub graph: EffectGraphDef,
    pub removed_param_ids: Vec<String>,
    pub parameter_id_remaps: Vec<(String, String)>,
}
pub struct SceneGraphEditError {
    pub at: Option<SceneNodeRef>,
    pub message: String,
}
```

Keep typed domain errors inside their builders; convert at the command boundary with the original diagnostic. `parameter_id_remaps` preserves the existing copy semantics. A simple rename of a label does not remap parameter identity.

`E/scene_transaction.rs` generalizes `StackTransaction` to crate-private `SceneGraphTransaction`. Its owner, expected graph, before graph, candidate graph, removed IDs, remaps, previous instance layer, applied state, and rejection remain. Replace `expected_generator_type` with `expected_preset_type: PresetTypeId`, obtained from the existing graph owner. Keep modifier builder admission generator-only; the transaction itself accepts existing supported `GraphTarget` owners.

```rust
impl SceneGraphTransaction {
    pub(crate) fn prepare<F>(
        project: &Project, owner: GraphTarget, owner_default: &EffectGraphDef,
        description: &'static str, edit: F,
    ) -> Result<Self, SceneGraphEditError>
    where F: FnOnce(&EffectGraphDef) -> Result<SceneGraphEdit, SceneGraphEditError>;
}
```

Execution validates the expected owner/type/graph, commits the candidate and refreshed manifest once, and records one inverse. Rejection leaves graph, instance values, mappings, and undo stack unchanged. Redo reuses the prepared IDs; it must not regenerate them. Existing modifier command structs remain domain actions backed by this transaction. It is not a public second editing service.

`C/scene_template.rs` adds the data-only template and pure insertion function:

```rust
pub struct SceneTemplate {
    pub group: EffectGraphNode,
    pub metadata: PresetMetadata,
}
pub fn insert_scene_template(
    graph: &EffectGraphDef,
    scene: &SceneNodeRef,
    template: &SceneTemplate,
    metadata: &dyn SceneExposureMetadataProvider,
) -> Result<SceneGraphEdit, SceneGraphEditError>;
```

`group` must be a complete ordinary group, with all scene-object outputs declared. Template-local stable NodeIds, document IDs, handles, numeric bindings, string bindings, and aliases are remapped once across the entire nested body. The template metadata contains only its own exposures; its preset identity/display fields do not replace the host's. Use the existing exposure stamper/provider (`C/scene_exposure.rs:44`, `:118`, `:190`), not cloned primitive descriptors.

Insertion validates the selected render scene, capacity, graph references, and all metadata before changing anything. It allocates one group identity and fresh descendant identities; routes all object outputs; increments the render-scene object count by the actual output count; merges exposures by ID; wires existing World controls through declared boundaries; and returns one candidate. No `ExposureSet`, fixed category vectors, or positional binding alignment remains. Shared World creation uses the same provider, and is part of the candidate. Domain recognition uses the core index/list.

`E/scene_template.rs` exposes:

```rust
impl InsertSceneTemplateCommand {
    pub fn new(
        project: &Project, target: GraphTarget, scene: SceneNodeRef,
        template: SceneTemplate, catalog_default: &EffectGraphDef,
        metadata: &dyn SceneExposureMetadataProvider,
    ) -> Result<Self, SceneGraphEditError>;
}
```

The constructor prepares immediately and stores no provider reference. App dispatch resolves the current scene reference from the snapshot and submits the command through `ContentCommand` to EditingService. The expected-owner check catches stale preparation. Existing async imports retain their worker result handoff, then prepare against the current owner; they do not mutate the project from the worker.

### 5.2 One scene-object model

Add `C/scene_object_model.rs`. This is derived data, never serialized and never an engine API:

```rust
pub enum SceneObjectRowKind { Object, PhysicsParent, LookChild }
pub struct SceneObjectCapabilities {
    pub rename: bool,
    pub hide: bool,
    pub delete: bool,
    pub duplicate: bool,
    pub transform: bool,
    pub enable_physics: bool,
    pub fluid_role: bool,
    pub modifiers: bool,
}
pub struct SceneObjectModel {
    pub object: SceneNodeRef,
    pub owner: SceneNodeRef,
    pub parent: Option<SceneNodeRef>,
    pub kind: SceneObjectRowKind,
    pub controls: Vec<SceneNodeRef>,
    pub capabilities: SceneObjectCapabilities,
}
pub fn scene_object_models(
    graph: &EffectGraphDef, scene: &SceneNodeRef,
) -> Result<Vec<SceneObjectModel>, SceneGraphEditError>;
```

The model resolves graph ancestry with `FlatSceneIndex` and the existing liquid/body/modifier recognition. It records actual control owners once. The nearest group containing one liquid owner and its captured display branches is Water's family owner. Independent domains in a manually grouped graph remain independent objects; grouping alone does not make them a family. Rigid ownership follows the existing body/compound relationships, not merely a common world. The same analysis supplies command capabilities and panel rows, so a shortcut cannot bypass a hidden button.

Water's real object is the parent row; its owner is the Water group. The three whitewater object branches become children only when their simulation ancestry resolves to that owner. An unrecognized authored topology stays an ordinary object with explicit unsupported-operation reasons; it is not silently relabeled Water. Malformed references fail validation. Names are display text only.

**SceneVm consumes `scene_object_models` for every row's parent, kind, and controls.** It keeps geometry, bounds, and material enrichment only. Delete its independent family/parent assignment and the app's ownership walk. App then projects SceneVm into UI-owned snapshot types using foundation IDs; UI does not import core. Preserve scope when converting SceneRowAddr and action targets. The F1b `look_mesh` target becomes part of `model.controls`; it is not a second ownership source.

`physics_boundary_rows_share_owner` asserts `parent_group_id == model.parent` on every row after resolving the stable scoped parent reference to the row's document-ID representation. It also checks kind and ordered controls. Include Water's real parent and three children, imported compounds, ordinary objects, and repeated local IDs in different scopes. SceneVm must not synthesize another parent or sort by an independently inferred group after this projection.

Only manifest-backed exposures selected by `controls` feed ParamSurface. Water parent selects simulation/emission/Amount/World sections plus its own look. Child selection includes only its material, Size, and visibility. It excludes body, transform, skin, role, and modifier cards. Role attach/retarget/remove and Enable Physics remain shared action rows driven by capabilities. All parameter writes, including existing-body enabled/visible toggles, use the manifest-backed scene parameter write path when a binding exists. There is no private physics modulation storage.

Recompute models on structural revision changes. Reuse cached rows and buffers for value-only updates. Do not scan the graph or allocate a new ownership vector every frame.

### 5.3 Shipped Water family lifecycle

F1b and F2 shipped under [WATER_FAMILY_DESIGN.md](WATER_FAMILY_DESIGN.md), independently of P1–P3. The implementation supplies `LiquidTemplate.object_outputs`, `ExposureSet::Whitewater`, recipe-owned Size bindings, shared visibility targets, and `look_mesh`, plus hide, rename, delete, and duplicate rejection. The obstacle source lives inside the family group.

P2 later migrates the landed four-output family template and its metadata compiler-driven to the common insertion path. P3 migrates its parent/kind/control projection without changing Water D1–D10. These are convergence refactors with the Water flows as regression cases, not replacement Water build phases.

Water lifecycle-to-transaction migration is outside this contract: its current writers were not audited in section 1.2. F2 keeps its own approved implementation and gates. No new deletion of an unused World is specified; there is no existing ownership/reachability check to cite. Any later World reclamation needs an audited design and its own phase.

Saved projects remain graph+metadata+instance state. No family registry or project version change. After P2/P3, save/reload and modulation must retain the landed family's behavior. Old liquid graphs remain structurally unchanged; unknown data stays inert-but-present or reports a diagnostic.

### 5.4 Convergence and cost

| Scope | Change | Cost and limit |
|---|---|---|
| Add Fluid | Replace liquid-specific insertion with the common template, remapper, and transaction. | Medium: eight constructor sites at this HEAD, nested metadata, multi-output count, and round-trip coverage. No solver change. |
| Scene panel | Replace two ownership walks with one derived model; keep ParamSurface and card hosts. | Medium: stable scoped targeting matters more than drawing rows. Value updates must remain cheap. |
| Box3D enable/add/split/duplicate | Pure graph builders produce `SceneGraphEdit`; shared transaction and exposure provider own commit. | Medium: group and loose forms, compound children, existing enabled toggles, paused duplication, free body slots, and shared-world lifetime all need fixtures. Do not create a new world per row. |
| Imported colliders | Import candidate uses the common transaction; existing asset preparation remains outside engine. | Medium: async stale-owner rejection, string bindings, held-out glTF, and collected asset reload. Not an importer rewrite. |
| Fluid roles | Assign/retarget/remove return the common candidate; model supplies action rows and recipient identity. | Medium: external ownership and full rollback are essential. Existing role limits and unsupported-geometry guards remain. |
| Shatter | Keep modifier recipe/preparation/compiler; use common transaction/model capabilities. | Small after P1/P3, but requires a real imported-body acceptance case. No solver abstraction or fragment rows invented. |
| GPU numerical separation | Isolate budgets/data, then FLIP and whitewater cores. | Highest cost; G1a adds types/checks without moves. G1b/G2/G3 remain deferred with source conformance before moves. Generated atom pipelines are supplied as prepared data; they cannot be replaced with copied shaders. Preserve numerical kernels, fusion, clock, and capture ABI. This work does not block F1b/F2. |

## 6. Invariants and enforcement

Tests below are required deliverables, not claims of tests already passing. Prefix new focused tests with `physics_boundary_` so phase scope is explicit.

| Invariant | Required machine enforcement |
|---|---|
| Engine/UI dependencies are enforced now | P1's deny.toml entries and existing `cargo deny check bans` landing leg; new `dependency_bans_cover_workspace` preflight/test prevents an unlisted workspace crate from bypassing the UI boundary. G1b adds the new crate to this policy; Cargo checks cycles. |
| One row structure and control owner | P3 delivers `physics_boundary_rows_share_owner` (parent equality on every row), `physics_boundary_scoped_duplicate_doc_ids`, and removal of fluid_controls/look_mesh ownership shortcuts. |
| No bespoke manifest-backed controls | P3 reuses `crates/manifold-ui/tests/no_bespoke_row_infra.rs`, which F1b extended to the scene panel and mapped in `scripts/cpu_scope.py`. The gate rejects physics-specific slider/drawer construction outside ParamSurface and exercises the shared gesture route. |
| Insertion is atomic | `physics_boundary_insert_rejects_without_mutation`, `physics_boundary_redo_keeps_ids`, `physics_boundary_nested_exposure_remap`; compare graph and instance-layer state before/after failure. |
| One exposure source and working modulation | `physics_boundary_save_reload_modulate`; numeric/string fan-out, aliases, mappings, and nested targets survive actual IO save/load and a subsequent modulation evaluation. |
| One step/capture owner | Existing liquid conformance plus `physics_boundary_group_preserves_tick_owner`; reject direct tick-state escape, duplicate shared-world advancement, mixed completion stamps, nested regions. |
| Water D1–D10 | F1b/F2 own their named Water tests and flows. P2/P3 rerun them; P6a retains family-duplicate rejection while migrating rigid duplication. No new Water lifecycle phase here. |
| Labels are not identity | Preserve F2's `water_family_visibility_rename_round_trip` through P2/P3, including role target, undo/redo, reload, and modulation after reload. |
| External objects survive lifecycle | `physics_boundary_role_source_survives_delete`, with a second domain/world user and undo. |
| Preserve GPU resource policy | `physics_boundary_budget_overflow`, `physics_boundary_budget_limit`, `physics_boundary_missing_limits_rejected`; existing whole-scene admission still runs. |
| Typed public stage failures | G1a defines EngineError and exhaustively matches it in tests; deferred G2/G3 use it for every new public fallible engine method and convert errors to scene diagnostics only in the host. |
| No steady-state new graph work | Rebuild counter test on value-only updates; if periodic/content-thread work is introduced, trace gate below. |
| Compatibility is not silent fallback | Round-trip old ungrouped liquid, grouped imports, and legacy whitewater axis inputs; unresolved metadata retained with a diagnostic. No Water migration/version bump. |

## 7. Phasing

Active phases are P1, P2, P3, P6a, P6b, P7, P8, and G1a. P1 precedes P2/P3; P2/P3 consume the landed F1b family. P6a/P7/P8 require P1/P3; P6b also requires P2's merge helper. G1a can land anytime and makes no numerical moves. Water F1b/F2 remain independent under their own design. Former P4/P5 are removed, not renumbered. Deferred G1b/G2/G3 are listed in section 9 and are not active work.

### 7.1 Common execution and seam rules

Before code, restate the binding decisions, forbidden moves, and anchor results for that phase. Run `git rev-parse HEAD`, `git status --short`, and its inventory searches. A changed count or missing symbol requires an updated reviewed seam brief before edits. Rename/delete old Rust symbols first, then let compile errors enumerate remaining callers. Serialized type IDs and ports are not renamed. No compatibility wrapper may preserve an obsolete authoring path.

For implementation phases, use `scripts/codex_checks.py` for the changed paths and `scripts/landing_gate.py` before landing. One cargo command at a time, `CARGO_BUILD_JOBS=4`, focused package checks/clippy and named test filters. The positive test command pattern is `CARGO_BUILD_JOBS=4 cargo test -p <package> physics_boundary_<filter>`; selected tests run under `scripts/gpu_queue.py` where required by the landing gate. GPU builds precede the GPU lock. GPU-path changes use `scripts/gpu_proofs_gate.py` with the touched-path mapping, not an all-proofs run. No workspace suite, unfiltered crate test, optional render exploration, new locks, or numerical tuning.

For UI phases, deliver the named flow and its manifest entry, then run `CARGO_BUILD_JOBS=4 scripts/run_ui_flows.py <flow-name>`. This runner already owns the GPU execution lock. Its assertions and exit code are the agent gate; retain its PNG for Peter. Target L3 through actual input, including visibly actionable row chrome. A PNG alone does not establish behavior. Give Peter the worktree binary's exact launch command at delivery. Save/reload gates must use the real IO path and modulate after reload. If a phase adds periodic/content work, run its same bounded flow with `MANIFOLD_RENDER_TRACE=1`; any frame over 20 ms fails. Otherwise no trace run is added. No background work is authorized by these docs-only edits.

API inventory at this HEAD (later P6–G3 inventories are deliberately re-derived at entry under DESIGN_DOC_STANDARD.md section 8.3 (execution pre-flight); their upstream source is changing):

- `rg -n 'StackTransaction::prepare' crates` gives **8** callers, all `E/modifier_stack.rs:277,332,386,440,493,547,604,662`. Mechanical: `StackTransaction::prepare(..., |g| domain_edit(g))` → `SceneGraphTransaction::prepare(..., |g| domain_edit(g).map_err(Into::into))`. Keep domain validation in the closure.
- `rg -n 'SceneModifierGraphEdit \{' crates --glob '*.rs'` gives **12** matches: declaration at `C/scene_modifier_edit.rs:20`; result literals at `:181,249,256,389,521,552,595,677,684,911,954`. Mechanical name/import changes; all three fields are unchanged. Re-run a full symbol search for references too.
- `rg -n 'AddSceneFluidCommand::new' crates --glob '*.rs'` gives **8** calls: `A/fluid_domain_edit.rs:797`; `A/ui_bridge/project.rs:659,2277`; `E/scene/fluid/tests.rs:87`; renderer `tests/gpu_proofs/water_basin/explicit_authoring.rs:144`, `water_basin.rs:478,683`; renderer `src/preset_runtime/physics_impulses/coupled_playback_tests.rs:31`. Each uses the new prepared constructor, complete template metadata, and a resolved `SceneNodeRef`. Do not preserve test-only old constructors.
- `rg -n 'LiquidTemplate \{' crates --glob '*.rs'` gives **8** matches including declaration, impl, return signatures, and literals: `E/scene/fluid/template.rs:46,56,83,184`; `E/scene/fluid/tests.rs:501,539`; `A/ui_bridge/project.rs:22,36`. Three recipe bodies need the new pattern: CPU compatibility template, test GPU template, production GPU recipe. Replace body vectors/output ID/category exposures with a complete group plus existing-format metadata. Remove `group_id_slot`; update brittle document-ID fixtures to stable references, preserving saved ID semantics rather than arbitrary creation order.

⚠ **VERIFY-AT-IMPL — family inventory delta:** the preceding counts are the reviewed baseline, not the concurrent family implementation. P2 reruns `rg -n 'LiquidTemplate|object_outputs|ExposureSet::(Whitewater|Look)|with_(whitewater|look)_metadata' crates`; P3 reruns `rg -n 'look_mesh|parent_group_id|fluid_controls|water_family' crates/manifold-nodes-scene/src/node_graph/scene_vm.rs crates/manifold-app/src/ui_bridge crates/manifold-ui/src`. Record every literal, reader, and count before renaming. P2 maps object_outputs to the group interface, Whitewater/Look/setters to complete metadata, and shared targets through the fresh-ID map. P3 maps look_mesh to controls and the family parent/kind to the single model. Delete old symbols first and follow compiler errors; no family-only insertion wrapper or second row projection survives. The four-output family is P2's real insertion/undo/reload case.

P3 baseline field inventory: `rg -n '\.fluid_controls\b|pub fluid_controls:' crates` returns **21** matches. Production declaration/use: `R/scene_vm.rs:261`, `A/ui_bridge/projection/scene.rs:107`, `R/viewport_gizmo.rs:199,230`. Tests: `A/ui_bridge/project.rs:2172,2173,2178,2191,2312`; `R/scene_vm.rs:1817,1837,1854,1886,3091,3094`; `R/scene_exposure/fluid_objects.rs:724`; `R/viewport_gizmo.rs:563`; `R/primitives/surface_mesh_normals.rs:140`; renderer `tests/gpu_proofs/water_basin/explicit_authoring.rs:178`, `water_basin.rs:697,795`. Initializers and the local builder at `scene_vm.rs:1344`/`:1453`, `viewport_gizmo.rs:553`/`:790` also change. App projection consumes scoped model controls. Gizmo eligibility uses the model's transform capability, not an empty controls vector. Ownership tests use scoped references; tests merely identifying a liquid use the existing liquid-domain identity. Unrelated helper/test names containing `fluid_controls` are not migration targets.

⚠ **VERIFY-AT-IMPL:** F1a can change the template inventory before P2. It is an expected source change, not permission to guess. Read its landed recipe and update the inventory and field mapping before P2 starts. The same rule applies to deferred G1b/G2/G3 while the liquid seam session owns those sources.

### P1 — Enforce dependencies and reuse the graph transaction

- **Entry/read-back:** baseline audit above; read `E/modifier_stack.rs`, `C/scene_modifier_edit.rs`, deny.toml, and the landing deny leg. Re-run the 8/12 inventories and `CARGO_BUILD_JOBS=4 cargo metadata --offline --format-version 1`; compare with section 3.2. Restate D1/D4/D6 and stale-owner rules.
- **Deliverables:** section 3.2's deny.toml entries, `dependency_bans_cover_workspace` landing preflight, and negative-fixture tests in `scripts/test_landing_gate.py`; `C/scene_graph_edit.rs`, `E/scene_transaction.rs`; old result/transaction renamed and moved; eight modifier actions use it. Preserve public domain command APIs. Add `physics_boundary_transaction_atomic`, `physics_boundary_transaction_stale_owner`, `physics_boundary_transaction_undo_instance`.
- **Seam:** before `StackTransaction::prepare(...) -> Result<Self, SceneModifierStackError>` with a modifier-only closure/result; after the exact signature in 5.1. Existing graph/result fields map unchanged; generator admission stays in modifier builders. Unsupported GraphTargets reject explicitly.
- **Gate/scope:** `cargo deny check bans` exits 0; `python3 -m unittest discover -s scripts -p test_landing_gate.py -k dependency_bans` passes with nonzero tests. Fixtures reject all protected→host edges and UI→non-foundation edges, including a newly added workspace package; no Cargo subprocess inside tests. Then focused core/editing/app check, clippy, `physics_boundary_transaction` and existing modifier sibling filters. `rg -n 'StackTransaction|SceneModifierGraphEdit' crates` returns zero. No renderer/UI behavior changes; update renamed result imports directly.
- **Demo:** none — L1. Add an app-level `physics_boundary_transaction_reload` test: save/reload an existing modifier candidate through actual project IO, modulate after reload, and restore instance state on undo. Include that focused app test in P1's gate. No new periodic work. Forbidden: weakening stale-owner checks, adding a second snapshot implementation, widening modifier owners.

### P2 — General template insertion

- **Entry/read-back:** P1 and landed F1b; read the four-output family recipe, fluid command/template, exposure provider, group flattening, and 7.1 inventories. Coordinate with F2 rather than editing its active files. Restate D3/D4/D8.
- **Deliverables:** exact template/function/command in 5.1; re-inventory and migrate every Add Fluid call and recipe body, including family literals and shared parent-visible targets. Reuse `gpu_flip_preset`, no new core recipe module. Add nested remap, fan-out, capacity rejection, atomic rejection, redo-ID, and save/reload/modulation tests named in section 6. The four-output Water family is the real primary test case.
- **Seam:** before the eight-argument `AddSceneFluidCommand::new(target, render_scene_node_id, fluid_metadata, source_metadata, material_metadata, object_metadata, template, catalog_default)` plus role/world and F1b whitewater/look setters, object_outputs, and shared-binding targets; after `InsertSceneTemplateCommand::new` in 5.1. Metadata comes from the template/provider; scene identity is scoped; errors surface before submission. Delete the old symbols first and migrate every family literal from compiler errors. Candidate publication is the P1 transaction.
- **Gate/scope:** focused core/editing/app and changed renderer recipe tests; filters `physics_boundary_insert`, `physics_boundary_nested`, `physics_boundary_redo`, `physics_boundary_save`. Negative `rg -n 'AddSceneFluidCommand|LiquidTemplate|TemplateExposure|ExposureSet' crates` zero. Whole-world controls and every object output resolve after flattening.
- **Demo:** L3 `scene-physics-template` and the landed F1b Water flow: +Water inserts four outputs, undo removes the family, redo keeps IDs, reload preserves exposures. Include two families following an imported compound; assert physical slot counts and child visibility. Preserve the landed label and automation name. Gesture: modulate Amount and child Size after reload. Forbidden: replacement Water UI, metadata vectors, separate child commands, old-project upgrading. No numerical changes.

### P3 — Shared object ownership and cards

- **Entry/read-back:** P1 and landed F1b; read `scene_vm`, app scene projection, ParamSurface, scene action routing, and section 5.2. Re-run `rg -n 'fluid_controls|object_controls|PhysicsVm|look_mesh|parent_group_id'` over R/A/U and record all readers before the rename.
- **Deliverables:** exact SceneObjectModel API; SceneVm consumes its parent/kind/controls on every row; migrate app ownership consumers and family look_mesh; app-to-UI conversion; cached structural rebuild; `physics_boundary_rows_share_owner`, `physics_boundary_scoped_duplicate_doc_ids`, `physics_boundary_value_update_no_rebuild`. Deliver `no_bespoke_row_infra` in `scripts/test_scene_param_surface.py` if F1b has not already supplied it; otherwise extend that single existing check. It is a required deliverable, not an assumed existing check. Existing cards remain accessible.
- **Seam:** before independently assigned SceneVm parent/kind, fluid_controls, F1b `look_mesh: Option<NodeId>`, and app additions; after one parent/kind/controls model. Delete old fields first; move the look mesh target into model controls, then update compiler-identified readers. Delete topology walkers, do not wrap them. SceneVm keeps only geometry/bounds/material enrichment; material rendering does not change.
- **Gate/scope:** focused core/renderer/app/UI checks and model/card tests; `rg -n '\.fluid_controls\b|pub fluid_controls:|\.look_mesh\b|pub look_mesh:' crates` zero. Run no_bespoke_row_infra with nonzero tests. P1's actual deny/preflight gate enforces UI dependencies. On every row assert parent_group_id equals model.parent after scoped identity conversion, and compare kind/controls. Cover shared worlds, Water, imported compounds, independent grouped domains, and duplicate local IDs.
- **Demo:** L3 `scene-physics-shared-cards`: select existing rigid and fluid objects and a role source; scrub one manifest control, open its modulation drawer, undo. Save/reload then modulate. Forbidden: another ownership registry, label matching, per-frame graph walks, bespoke sliders. Trace only if periodic work is added.

### P6a — Rigid add, enable, split, and duplicate

- **Entry/read-back:** P1/P3; read `E/scene/physics.rs`, add-object path, split caller, duplicate.rs, physics_match.rs, and `A/ui_bridge/project.rs` enable branch. Re-run `rg -n 'append_physics_scene_object|add_group_physics|EnableSceneObjectPhysicsCommand|DisableSceneObjectPhysicsCommand|DuplicateSceneObjectCommand|first_free_physics_body_slot' crates`; classify every constructor/helper caller.
- **Deliverables:** pure rigid graph edits and DuplicateSceneObjectCommand committed through SceneGraphTransaction; shared capability/model/provider use; enabled writes through bound parameter editing. Tests `physics_boundary_rigid_enable`, `physics_boundary_rigid_shared_world`, `physics_boundary_rigid_split_undo`, `physics_boundary_rigid_duplicate_paused`. Preserve free body-slot allocation, cloned numeric/string bindings, fluid-role routes, +0.5 placement offset, and stable redo IDs. Preserve nested-physics/copies/malformed/full-world rejection and Water's duplicate rejection.
- **Seam:** command constructors remain public entry points; their execute bodies stop owning before/after graph snapshots and instead execute the prepared transaction. Helper graph construction remains pure and returns nodes/edits; no renderer object is stored in a command. The before/after transaction seam is P1, not a new rigid API.
- **Gate/scope:** focused editing/core/app rigid tests, existing split/physics sibling filters. Negative gate: the migrated commands contain no `prev_graph`, `after_instance`, or direct `project` graph assignment outside the transaction. Existing saved loose and grouped rigid fixtures round-trip and modulate enabled/body controls after reload.
- **Demo:** L3 extend `scene-physics-controls` and rerun `scene-physics-duplicate-paused`: enable, adjust bounce, disable/re-enable, split/undo, duplicate a paused rigid object, undo/redo, and resume in the shared world. Gesture: duplicate while paused then resume; exactly one new body and no new world. Forbidden: per-object worlds, deletion on disable, new locks, coupling changes.

### P6b — Imported graph commit and collider preparation

- **Entry/read-back:** P1/P2/P3; read import worker, merge plan, `ImportModelIntoSceneCommand`, `physics_mesh`, and imported-physics flow. Inventory `rg -n 'ImportModelIntoSceneCommand::new|MergePlan' crates`; preserve current merge return data and worker protocol.
- **Deliverables:** import graph/metadata assembled as one candidate and committed through P1; shared exposure merge/remap helper from P2, without forcing imported multi-root graphs into Water's template shape; retain engine-ready immutable collider preparation. Tests `physics_boundary_import_atomic`, `physics_boundary_import_stale_owner`, `physics_boundary_import_reload`.
- **Seam:** current `MergePlan` fields map to a complete `SceneGraphEdit` before publication: nodes/wires/object count plus card params/numeric/string bindings. Report lines remain a host result, not engine data. Existing public import command signature may remain; its internal commit changes. No new asset API.
- **Gate/scope:** focused import/editing/IO tests and mapped `glb_conformance` if import code changes. Use a held-out glTF with external buffers, nested transforms, and more than one mesh, not the development fixture. Reject missing resources/stale owner without partial metadata. Save, relocate/collect as supported today, reload, enable physics, then modulate body controls.
- **Demo:** L3 extend `scene-imported-physics`; import held-out model, enable physics, undo/redo, reload. Gesture: enable physics on one imported object without affecting its sibling. Forbidden: automatic physics on import, asset I/O in the engine, changing unsupported dynamic mesh policy, promising arbitrary-scene take playback.

### P7 — Fluid role edits and rows

- **Entry/read-back:** P1/P3; read role assignment/routing/lifecycle and core liquid recognition. Run `rg -n 'AssignSceneFluidRoleCommand|RemoveSceneFluidRoleCommand|RetargetSceneFluidRoleCommand' crates`; re-resolve actual command spellings in `roles/lifecycle.rs` before classifying callers.
- **Deliverables:** assignment/retarget/removal prepare one candidate through P1; model owns role action targets; existing metadata provider and group-port traversal reused. Tests `physics_boundary_role_atomic`, `physics_boundary_role_retarget`, `physics_boundary_role_source_survives_delete`.
- **Seam:** existing role command public semantics stay; replace independent snapshot/commit internals with the P1 transaction. Preserve role limits, geometry validation, and external source identity. Do not make roles child objects owned by Water.
- **Gate/scope:** focused core/editing/app role filters; negative scan for direct project graph commits in migrated role commands. Tests cover full slots, rejected geometry, cross-domain retarget, nested groups, and rollback. Reload then modulate source/role controls.
- **Demo:** L3 extend `scene-fluid-role-lifecycle`: assign, retarget to a second Water, remove, undo, delete recipient, undo. Gesture: change inflow while keeping the source object's own transform controls usable. Forbidden: new domain type lists, automatic source deletion, bypassing recipient validation.

### P8 — Shatter presentation and ownership

- **Entry/read-back:** P1/P3; read `scene_modifier_edit`, `scene_modifier_authoring`, shatter compiler admission, and modifier card host. Re-run `rg -n 'prepare_new_scene_modifier|shatter_targets' crates/manifold-app/src crates/manifold-nodes/src` and confirm P1 already owns commit.
- **Deliverables:** Shatter action/selection uses common object references and capabilities; modifier parameters remain existing ParamSurface cards; rejected targets preserve owner state. Tests `physics_boundary_shatter_targets`, `physics_boundary_shatter_undo_reload`. No new simulation type or insertion command.
- **Seam:** existing renderer preparation signature stays; app converts shared model selection to its existing scoped target list. Remove any duplicated eligibility computation superseded by the model, while renderer compiler validation remains authoritative for prepared geometry.
- **Gate/scope:** focused modifier/renderer/app tests and mapped proof gate. Negative test rejects unsupported animated/deformed/second-active-shatter cases. Saved recipe and controls round-trip and modulate after reload. Use a held-out supported imported mesh.
- **Demo:** L3 extend `scene-modifier-preset`: select imported rigid object, add Shatter, modulate one exposed control, undo/redo/reload. Gesture: change the existing shatter control without losing the body's physics card. Forbidden: moving asset/compiler work into the engine, fragment sliders, relaxing admission to make a demo pass.

### G1a — Engine types and checks, no moves

- **Entry/read-back:** no solver-lane prerequisite. Read sections 3.1–3.2 and current foundation time/error vocabulary, GPU allocation arithmetic, and kernel packing. Do not edit the active numerical files. Restate the no-moves scope and the deferred trigger.
- **Deliverables:** new shared EngineError in foundation's new engine_error module; allocation budget/refusal types and prepared-kernel layout/packing types in manifold-gpu. No new workspace crate yet. Add `physics_boundary_budget_overflow`, `physics_boundary_budget_limit`, `physics_boundary_engine_error`, and `physics_boundary_kernel_pack_layout` using hand-built layouts, including derived words. The host missing-limits regression belongs to G1b when admission is connected. No public stage method is moved or changed here.
- **Seam:** additive types only; existing solver calls and scene admission remain unchanged. G1b will use the budget at allocation boundaries and G2/G3 will install kernels. No compatibility wrapper or alternate active solver implementation.
- **Gate/scope:** focused foundation/gpu check/clippy and the named CPU tests; `cargo deny check bans` remains green. Assert the diff contains no existing numerical Rust/shader file moves or edits. No GPU execution or renderer build is needed for these pure type/arithmetic checks.
- **Demo:** none — L1. No persistent state, periodic work, or performer surface. Forbidden: touching active body/sheeting/tick lanes, moving POD layouts, changing caps/defaults, a lexical dependency script, or a cargo-from-test compile probe.

## 8. Decided — do not reopen

1. Existing CPU worlds and concrete GPU stages are the engine; scene graphs, assets, caches, and UI remain host adapters.
2. No universal solver trait, new server singleton, second clock, new shared lock, or public release in these phases. Deferred numerical moves use the private manifold-physics-gpu workspace crate.
3. Ordinary groups and existing tick captures define the graph interface. A group does not imply a private world or a nested tick region.
4. Reuse the modifier transaction, ordinary template metadata, exposure provider, and manifest-backed ParamSurface.
5. Derive one object/ownership model and preserve scoped NodeIds across UI actions, undo, save, and reload.
6. Water D1–D10 stand. F1b/F2 shipped independently; P2/P3 later converge their insertion and rows. Lifecycle migration is out of scope.
7. Rigid/import/role/Shatter convergence has its own phases. GPU separation does not delay Water authoring.
8. Numerical relocation preserves algorithms, stage composition, ports, time policy, admission, and completion. It cannot claim to finish the liquid seam's open acceptance.

## 9. Deferred and Peter's calls

**Peter's calls:** whether an external consumer justifies external extraction sooner; the public engine name if it is released; whether and under what license/distribution terms to release it. Default: enforce boundaries now and use manifold-physics-gpu only when deferred numerical moves start; no public extraction or release. None blocks Water. Licensing requires a separate dependency/native-asset audit; no license conclusion is made here.

### G1b, G2, G3 — deferred numerical moves

**Trigger:** "seam P7a audited, P8 L3 paid, P10 landed or declined, the body/sheeting/tick lanes merged — or Peter names an external consumer". Neither branch silently discharges another session's acceptance debt. Before work, pin the accepted source tip, hand off file ownership, refresh section 3.1's marked inventories, and review a one-session move brief. No active authoring phase waits for these moves.

- **G1b — Move shared numerical data into manifold-physics-gpu.** Entry: G1a plus the trigger. Create `crates/manifold-physics-gpu/Cargo.toml` and lib/data modules as a private workspace member; its MANIFOLD dependencies are foundation, gpu, physics, fluids. Renderer depends on it. Move accepted physical records/layout helpers, not graph validators; connect G1a's budget and typed errors at existing allocation boundaries. Update deny wrappers for renderer→physics-gpu and physics-gpu→the four lower crates, never a host crate. Gate: focused crate/renderer check/clippy, cargo deny, unchanged byte-layout/overflow/refusal tests, and only mapped GPU proofs if shader/layout consumers move. Cargo's dependency graph is the compile check. Demo: none — L1. No serialization, clock, coupling, or policy changes.
- **G2 — Move FLIP numerical stages into manifold-physics-gpu.** Entry: G1b and refreshed StepState/Step/StepParams/caller inventories. Read accepted step/pressure/body/clock/sheeting contracts. Move the numerical dependency closure to `crates/manifold-physics-gpu/src/flip/`; keep graph decode, registration, capture ownership, and display publication in renderer. The encode seam in 3.1 uses EngineError; preparation installs existing generated kernels. Gate: focused checks/clippy, small CPU references, mapped GPU proofs, cargo deny, and deletion of old numerical definitions. Preserve dispatch order, POD bytes, ports, defaults, body-pressure coupling, and completion stamps. Demo: none — L1 computed parity; no optional render sweep.
- **G3 — Move whitewater numerical stages into manifold-physics-gpu.** Entry: G1b/G2 and refreshed whitewater/capture inventories. Move WhitewaterState/WhitewaterOutput and numerical records to `crates/manifold-physics-gpu/src/whitewater/`; leave Outputs, fences, advance/advance_tick/tick_output routing, and legacy publication in renderer. Use explicit-buffer publish and EngineError from 3.1. Gate: focused checks/clippy, CPU pool/type/lifetime references, mapped whitewater/capture proofs, cargo deny, old-axis-input round-trip, and descriptor packing parity. Preserve dust in state, IDs/counters, coherent liquid stamps, reset/held behavior, and buffer lifetimes. Demo: none — L1 computed parity. No new master node or cache UI.

### Other deferred work

- **External extraction and stable public API:** revisit when Peter selects an external consumer or requests release. Package the private engine crate for that consumer, verify a host-only sample without renderer/app/UI, and define semver/support. Workspace packaging alone does not complete this work.
- **Additional solver families, cross-domain coupling, and a new coupling algorithm:** require their own approved physical contract and proof set. Current common authoring interfaces do not imply numerical interoperability.
- **Bake/cache UI and arbitrary-scene recorded playback:** only after the integration plan's provenance/collected-take acceptance and the seam's required outputs are complete. Existing guards remain.
- **Old Water project upgrades, family duplication, Dust display:** excluded by Water D4/D7/D10; reopen only on Peter's explicit change of direction.
- **Broad physics graph regrouping:** only when a specific consumer needs it. Existing saved loose/grouped graphs remain supported; no mass migration for visual tidiness.

Verification limits: dependency metadata and the proposed deny configuration were checked; no app, GPU proof, render, benchmark, or Rust test suite was run for this document. Concurrent Water and liquid-seam results are unverified here. The audit proves source relationships at the named HEAD, not runtime correctness, solver quality, licensing readiness, or release packaging. Implementation gates supply that evidence phase by phase.
