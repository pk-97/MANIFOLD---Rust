# Physics engine boundary — one engine contract, one authoring system

**Status:** PROPOSED · 2026-10-06 · GPT-6 · not implemented.
**Prerequisites:** review of this design; Water F1a for the Water integration phases only.
**Execution contract:** read [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5–6 before starting a phase. This task authorizes documentation only.

<!-- index: Engine, graph, and authoring boundaries for physics; Water is the first consumer, with shared insertion, controls, and lifecycle. -->

The engine owns simulation, not projects, graph editors, or scene panels. Graph adapters turn authored inputs into simulation work. Authoring owns groups, exposures, and undo. These are three boundaries within the existing infrastructure. A standalone engine crate and a public release are separate decisions.

Peter's direction, verbatim, 2026-10-06:

> if this is a large build it's worthwhile to also consolidate these into sensible API boundaries and interfaces and groups in our existing infra. You might need to refactor some other areas to get this working well and simple. This is an extremely complex system we are building with our custom physics-api. The infrastructure, UI, and UX around it needs to be unified too and exist as a cohesive system that is largely independent where possible. I might want to give back to the community one day in the future and provide access to the engine for other projects.

Companions:

- [PHYSICS_DIRECTION.md](PHYSICS_DIRECTION.md): approved physics direction and coupling requirements.
- [LIQUID_SOLVER_SEAM_DESIGN.md](LIQUID_SOLVER_SEAM_DESIGN.md): concrete solver seams, captures, clock, and coupling. Its owning session controls its open phases; this design does not amend them.
- [FLUID_ENGINE_INTEGRATION_PLAN.md](FLUID_ENGINE_INTEGRATION_PLAN.md): native integration, provenance, recording, and outstanding acceptance.
- [WATER_FAMILY_DESIGN.md](WATER_FAMILY_DESIGN.md): D1–D10 remain binding. This document replaces the technical briefs for F1b and F2 after review, not Peter's decisions.
- [NODE_GROUPS_DESIGN.md](NODE_GROUPS_DESIGN.md) and [GROUPING_GRAPHS.md](GROUPING_GRAPHS.md): existing group interface and identity rules.
- [WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) section 5b: the only manifest-backed control surface.

## 1. Audit — what exists

Verified 2026-10-06 against `c67c1e9ad84a51db2dc3f433247a1d6100cd5eb7`, branch `feat/physics-boundary-design`. HEAD and clean working state were checked before reading. This is a static source audit, not a runtime or visual verification. **Extend the listed infrastructure; do not redesign it.** Line numbers are snapshot anchors and must be re-resolved before implementation.

Path abbreviations below are exact repository-relative prefixes: `P = crates/manifold-physics/src/`, `F = crates/manifold-fluids/src/`, `R = crates/manifold-renderer/src/node_graph/`, `C = crates/manifold-core/src/`, `E = crates/manifold-editing/src/commands/graph/`, `A = crates/manifold-app/src/`, `U = crates/manifold-ui/src/`.

### 1.1 Engine and host code

| Piece | Verified source | Boundary today |
|---|---|---|
| Native rigid engine | `P/lib.rs:248` (`BodyConfig`), `:322` (`BodyHandle`), `:350` (`PhysicsWorld`), `:407` (`new`), `:690` (`step`), `:1267` (`pose`) | A concrete owned world with typed inputs and handles. It is already usable without a graph. |
| Box3D | `crates/manifold-physics/native/box3d/include/box3d/box3d.h` (`b3CreateWorld`, `b3World_Step`, body/contact event APIs); `P/lib.rs:31` | Native backend behind the Rust wrapper. Existing native serialization lock is not a reason to add a new shared lock. |
| Native liquid | `F/lib.rs:43` (`Config`), `:524` (`FluidWorld`), `:552` (`new_seeded`), `:903` (`step_live_with_fields`), `:920` (`surface`) | Concrete CPU FLIP API. The world owns native state; surface reads reuse caller storage. |
| `flip_engine` | `crates/manifold-fluids/build.rs:10`, `:150` | Vendored C++ source compiled with the bridge/probes, not another Rust crate. Changes to its solver are outside this boundary refactor. |
| Shared clock and stepping | `P/clock.rs:94` (`SimulationClock`); `P/stepping.rs:29`, `:149`, `:316`, `:869` | `StepInterval`, `CompletionLedger`, `LiveStepSchedule`, coupling protocol already exist. `R/liquid/clock.rs:2` aliases the physics clock. No second clock belongs in an engine facade. |
| Native coupling | `F/coupling/owner.rs:23`, `:107`, `:159` | `RigidFluidCoupling` owns orchestration and returns completed bodies. It borrows the concrete worlds. |
| Rigid graph adapter | `R/physics.rs:2`–`:27` imports | Uses core time, engine types, renderer transforms, and generator geometry. Scene resolution and geometry conversion are host work. |
| Liquid worker/adapter | `R/fluid.rs:9`–`:29`, `:91` | Uses core, both engine crates, cache, roles, rigid events, transforms, render vertices, channels, and worker state. This whole module is not an independent engine. |
| Liquid shared seam | `R/liquid/bodies.rs:11`, `:35`, `:199`; `lattice.rs:5`, `:60`; `fields.rs:28`–`:40`; `coupling.rs:16`–`:25` | Numerical records coexist with graph contexts, prepared renderer geometry, workers, and rigid graph inputs. Separate records/math from resolution and ownership adapters. |
| Grid/display helpers | `R/liquid/grid.rs:10`–`:13`, `:92`; `frame_ring.rs:7`–`:9`; `frame_history.rs:10`–`:12` | GPU buffers are already abstracted, but grid code imports renderer `GpuEncoder`; frame code reaches into `fluid::display_blend`. These are concrete dependencies to remove from reusable compute. |
| Validation/conformance | `R/liquid/extent.rs:22`–`:32`; `conformance.rs:217`, `:256`; `C/liquid_domain.rs:21`, `:114` | Extent validation legitimately understands graph/freeze structure. `FlatSceneIndex` and `liquid_domain_of` are already the core recognition seam. Do not add a second type-ID list. |
| GPU FLIP | `R/primitives/gpu_flip_domain.rs:19`–`:43`; `gpu_flip_step.rs:39`–`:61`, `:660`, `:1056`, `:1282` | Domain and primitive entry points resolve graph state. `StepState::encode` already takes `manifold_gpu::GpuEncoder`. Its numerical core can be isolated without changing the graph ABI. |
| FLIP numerical stages | `R/primitives/gpu_flip_pressure.rs:26`, `gpu_flip_bodies.rs:30`, `gpu_flip_clock.rs:17`, `gpu_flip_lentine.rs:7`, `gpu_flip_narrow_band.rs:9`, `gpu_flip_sheeting.rs:42` | Concrete compute stages, not app/UI APIs. Body code still imports graph role capacity and liquid coupling constants. Narrow-band/sheeting allocation calls scene-modifier admission. |
| Liquid primitives | `R/primitives/liquid_state.rs:15`–`:50`, `liquid_fill.rs:13`–`:25`, `liquid_cells.rs:10`–`:17`, `liquid_solid_distance.rs:8`–`:18` | State/capture boundaries plus composable fill, indexing, and solid-distance work. Registration and `EffectNodeContext` stay in the adapter; numerical buffer layout and dispatch belong inside the engine boundary. |
| Whitewater | `R/primitives/whitewater_step.rs:11`–`:43`, `:593`, `:666`, `:1121`, `:1714`, `:1813`; `whitewater_type.rs:9`–`:18` | Concrete step engine mixed with parameter decoding, renderer encoder, history publication, and primitive registration. `WhitewaterSpawn` is a real dependency on manifold-fluids, not merely a test oracle. |
| Whitewater atoms | `R/primitives/whitewater_emitter_velocity.rs:6`–`:15`, `whitewater_influence.rs:4`–`:10`, `whitewater_obstacle_source.rs:180`, `whitewater_distance.rs:60` | Reusable operations with graph descriptors/fusion contracts. Preserve those contracts; do not collapse them into a new monolithic Water node. |
| Renderer encoder | `crates/manifold-renderer/src/gpu_encoder.rs:7`, `:18`, `:35`, `:41` | Wraps native encoding with uniform arena, audio visuals, and frame status. It must not become the public engine encoder. |
| Allocation policy leak | `R/scene_modifier_expand/buffer_budget.rs:310`, `:343`; `gpu_flip_step.rs:601`, `:609`, `:1170`; `gpu_flip_narrow_band.rs:208`; `gpu_flip_sheeting.rs:196`; `whitewater_step.rs:954`, `:1215` | Physics allocation uses a scene-modifier error/policy module. Keep admission; separate generic checked budget arithmetic from scene policy and wording. |
| Shatter | `R/scene_modifier_authoring.rs:29`; `R/scene_modifier_expand/compiler/shatter.rs:156`, `:174`, `:233`; `C/scene_modifier_preset.rs:195` | An authored modifier recipe compiled into fragments and rigid participants. It is not another solver and must not become one. |

Manifest dependency audit: `crates/manifold-physics/Cargo.toml:8`–`:9` declares foundation and serde; build dependencies are cc/sha2. `crates/manifold-fluids/Cargo.toml` `[dependencies]` declares foundation, physics, serde, bytemuck; build dependencies are cc/sha2. Both are `publish = false`. Neither declares renderer/core/app/UI. The search `rg -n 'use .*manifold_(renderer|app|ui|core)|crate::(app|ui)' crates/manifold-{physics,fluids}/src` returned no matches. This checks source imports, not native library licensing or distribution readiness.

`crates/manifold-renderer/Cargo.toml:8`–`:17` declares physics, fluids, core, foundation, gpu, native, playback, and UI. Thus exporting a module from renderer alone does **not** make it independently linkable. `crates/manifold-ui/Cargo.toml:10` declares foundation as its only MANIFOLD dependency. Keep that restriction.

Inventory commands run included `rg --files crates/manifold-renderer/src/node_graph/primitives`, filtered for `gpu_flip_`, `whitewater_`, `liquid_`, and `matter_`; `rg -n '^use '` over those production files; and symbol searches for the APIs above. `gpu_flip_preset`, `gpu_flip_still`, `gpu_flip_volume`, the `*_tests` files, and CPU whitewater reference/oracle files are recipe/proof consumers, not a public world API. MPM's `matter_*` graph stages remain concrete solver adapters; reuse shared grid/data contracts without making FLIP depend on `matter_domain::closed_faces` (`gpu_flip_domain.rs:25`).

### 1.2 All scene entry paths in scope

| Entry | Graph, metadata, exposure, and undo path | Duplication to remove |
|---|---|---|
| Add Fluid | `A/ui_bridge/project.rs:17`–`:36`, `:652`–`:659` selects the GPU recipe. `E/scene/fluid/template.rs:16`, `:28`, `:46`, `:83` defines six exposure categories and the CPU recipe. `E/scene/fluid.rs:66`, `:215`, `:322`, `:371`, `:390`, `:448` renumbers, stamps, wraps one output, wires World controls, and snapshots graph/instance state. | The command takes separate metadata vectors, has its own transaction, and assumes one object output. `exposed_type_id` searches only top-level body nodes. The template is liquid-specific despite doing ordinary group insertion. |
| Add object in an existing physics scene | `E/scene.rs:239`; `E/scene/physics.rs:5` (`append_physics_scene_object`) | Separate transform/body/mesh/material/object construction and slot/handle allocation. This path exists as well as explicit Enable Physics. |
| Enable/disable Box3D physics | `A/ui_bridge/project.rs:851`–`:884`; `E/scene/physics.rs:647`, `:1049`, `:1141`, `:1182`, `:1245`, `:1268`, `:1344` | Group and loose-object recipes plus exposure stamping. Existing-body enable uses `SetGraphNodeParamCommand`; first enable uses `EnableSceneObjectPhysicsCommand`. Disabling in the UI writes `enabled`; it is not deletion of the engine world. `DisableSceneObjectPhysicsCommand` also exists at `:1399`; do not conflate these operations. |
| Split/imported object physics | `E/scene/split.rs:379` reuses `add_group_physics`; `R/physics_mesh.rs:38`, `:75`, `:233` resolves assets and prepares colliders. | Asset loading/cooking inputs are host preparation. The numerical world should receive immutable mesh/hull data, never a project path or an import card. |
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

⚠ **VERIFY-AT-IMPL:** Water F1a is not at this HEAD. Peter's current scope is one Water group, four object outputs, and the obstacle source **inside** the group. Read `R/primitives/gpu_flip_preset.rs` and the landed F1a tests before P2/P4. Do not reinstate the older external obstacle-source input from the checked-in Water design. Confirm actual port spellings with `rg -n 'GroupInterface|GroupPortDef|group_output|obstacle' crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs`.

The liquid seam document marks P7a unaudited, P8 owing L3 acceptance, P10 open, and P5/P6 retired. Source already contains shared grid/conformance work, so this audit does not promote those phases to complete. P1–P8 below need no unlanded liquid-seam phase. G1–G3 touch numerical/grid/capture code owned by that session: they require its accepted source tip and a conflict-free ownership handoff. If they consume P10 grid outputs, **P10's accepted grid ABI and its gates are an explicit prerequisite**. P7a's live stepping audit and P8's owed L3 remain owed; moving code does not discharge them. P11/P12 future solver work is not a prerequisite. No nested regions are introduced.

The fluid integration plan still names incomplete arbitrary-scene recording/provenance and collected-take acceptance (`BUG-vglg.17`). Nothing below relaxes those guards. Water visibility follows Water D8: hide affects display, not simulation advancement; it must not inherit older broad wording that hidden scenes stop all work.

## 2. Decisions

**D1. Keep three boundaries, with no new engine crate now.** Numerical state and stepping are engine code. Group evaluation and project-time sampling are adapters. Scene editing and cards are authoring. The existing two CPU crates remain; GPU compute is isolated in a checked module inside renderer until extraction is approved. Rejected: a new umbrella crate that still depends on renderer, because it changes packaging without establishing independence.

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

The future GPU embedding surface is concrete stage preparation plus encoding against `manifold_gpu::{GpuDevice, GpuEncoder, GpuBuffer}`. It exposes existing validated physical records and buffer views, not graph ports. G1–G3 relocate existing numerical types field-for-field under `crates/manifold-renderer/src/physics_engine/`; this module is the temporary packaging boundary. It is not yet a separately consumable dependency. Public distribution stability is not promised for packed GPU records.

The load-bearing FLIP encode seam preserves the existing shape (pipeline provisioning is split below):

```rust
// physics_engine::flip, moved from primitives/gpu_flip_step.rs.
// StepState -> FlipState; Step<'a> -> FlipStep<'a>; StepParams -> FlipParams.
impl FlipState {
    pub fn prepare_pipelines(&mut self, device: &GpuDevice, kernels: &FlipKernels);
    pub fn encode(
        &mut self, device: &GpuDevice, enc: &mut GpuEncoder,
        step: &FlipStep<'_>, clock_params: &GpuFlipClockParams,
    ) -> Result<(), String>;
}
```

`FlipStep` retains **all** fields of `Step` at `gpu_flip_step.rs:1056`: identity, params, clock_plan, particles, out, capped, tally, count, forces, impulses, bodies, shapes, atlas, regions, reaction, dynamic, pressure, level, wall_inset, band, ghost, density, narrow_enabled, restore_narrow, sheet_rate. Types and lifetimes move unchanged; `StepParams`' fields move unchanged too. Scratch preparation remains separate from encoding and uses the existing checked capacity calculations. `StepState.history` is numerical substep-face history (`R/liquid/substep_history.rs:40`), not presentation history: move it with FLIP, but supply its generated component kernel through the kernel seam below. Keep display-frame publication in the adapter. No raw `f32` timing API is added: existing GPU POD seconds are packed from the typed host interval at the adapter boundary.

Whitewater needs a real split, not a move of the whole `Step`. At `whitewater_step.rs:1121`, `Step` owns both numerical scratch and `Outputs`; `advance` at `:1147` handles fences and publication. Keep that host orchestration, `owed`, output slots, `advance`, `advance_tick`, and string-based `tick_output` in the graph adapter. Move numerical fields and methods into `physics_engine::whitewater::WhitewaterState`. The adapter holds that state exclusively. `StepShape`, `StepFrame`, `MotionInputs`, `FaceSource`, and `StepInputs` at `:478`–`:708` move field-for-field; `StepInputs` keeps motion, particles, solid, obstacle_source, faces, level_set, distance. Preserve packed/axis admission.

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
    ) -> Result<(), String>;
    pub fn publish(
        &self, enc: &mut GpuEncoder, shape: &StepShape,
        output: &WhitewaterOutput<'_>,
    );
}
```

Before: `Step::publish(enc, shape, index)` dereferences `self.outputs.slots[index]` (`:1691`). After: the adapter selects the same retired/free slot and passes its four buffers and counts to `WhitewaterState::publish`. Pool/state buffers and ping-pong state remain numerical state; output-slot selection and fence retirement remain host state. Capacity preparation, seed, and physical buffer accessors move with the numerical fields; their existing arguments remain, with G1's explicit budget added to allocating methods. The host's existing `advance` and `advance_tick` retain their public signatures and order of operations. No engine method takes a `Fence`, a string port name, or a renderer encoder. This preserves both the legacy frame publication path and the tick capture path.

**Ownership:** worlds/stage state have one mutable owner. The existing native worker owns native worlds. The render execution owner owns GPU stage state and retained buffers until completion. Content owns authored project state and sends commands; UI receives snapshots. Other hosts can choose their own scheduling around the same exclusive APIs. No new thread, channel, global scheduler, `Arc<Mutex<_>>`, or `Arc<RwLock<_>>` is introduced.

### 3.2 Dependencies and allocation

Allowed engine dependencies are foundation, the existing physics/fluid crates, manifold-gpu for GPU code, and already-used low-level dependencies. No core, editing, playback, UI, app, native Metal API, or renderer service may be imported by isolated numerical modules. `manifold-gpu` continues to own backend access. Native Metal remains the current backend; shader source names do not authorize a wgpu backend.

Host adapters keep scene-index traversal, registry lookup, parameter resolution, asset loading, collider preparation, graph extent validation, cache provenance, freeze registration, render meshes/materials, diagnostics presentation, and display interpolation policy. Engine code keeps physical data/layout arithmetic, solver state, kernels, dispatch sequencing, and physical diagnostics. A code move must follow this distinction rather than moving an entire `liquid` directory.

Allocation admission must survive the separation. Add the following backend-neutral arithmetic to `manifold-gpu` in G1, with the current scene policy choosing `allowed_bytes` outside the engine:

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
    ) -> Result<usize, String>;
}
```

This is the existing bounded pack operation with descriptor facts supplied as data, not a new shader compiler. Allocate names/layouts only at installation. Validate unique names, word offsets, count placement, and the 64-word bound there. Preserve the current float/int/enum/bool conversion and padding exactly. Unknown named values reject. `FlipKernels` and `WhitewaterKernels` are prepared resource bundles: their fields mirror exactly the generated-pipeline fields of the current numerical dependency closure, replacing each pipeline with `PhysicsKernel`; fixed hand-written shader pipelines remain stage-owned. The phase inventory must list those fields before moving them. `SurfaceDistance` receives its existing generated UpwindDistance kernel; substep history receives FaceSampleComponent; whitewater receives its existing nine `standalone_pipeline` atoms at `whitewater_step.rs:914`–`:922`. Related scan/sort/identity helpers follow the same rule. Their primitive registrations remain and use the same descriptor-generated code. Byte-for-byte uniform packing and generated-versus-standalone parity tests are mandatory. Physical constants move once and descriptors reference that definition.

The kernel layout includes both declared parameters and derived scalar uniforms, with derived defaults zero, in the existing codegen order. This preserves the packing after the ordinary parameter words, not just the parameter list.

An external GPU host supplies these installation resources through manifold-gpu. Providing a packaged default kernel bundle belongs to extraction, not a claim that an external host can already link renderer without its dependencies. The compile probe supplies kernel data directly and must not import the graph compiler.

Consequences, stated honestly: CPU independence already exists. GPU independence requires moving code, constants, shaders, and tests out of graph modules. Keeping the code inside renderer avoids premature packaging but cannot remove renderer's transitive dependencies for an external project. The isolated module and compile probe establish readiness; only later extraction supplies independent linking.

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

App projects these records into the existing scene panel snapshot. UI keeps its own row types and foundation IDs; it does not import this core type. Preserve scope when converting existing `SceneRowAddr` and action targets. Remove `fluid_controls` and app-side additions that rediscover ownership. Existing renderer geometry/bounds/material information can augment a row by its stable reference, but cannot redefine who owns it.

Only manifest-backed exposures selected by `controls` feed ParamSurface. Water parent selects simulation/emission/Amount/World sections plus its own look. Child selection includes only its material, Size, and visibility. It excludes body, transform, skin, role, and modifier cards. Role attach/retarget/remove and Enable Physics remain shared action rows driven by capabilities. All parameter writes, including existing-body enabled/visible toggles, use the manifest-backed scene parameter write path when a binding exists. There is no private physics modulation storage.

Recompute models on structural revision changes. Reuse cached rows and buffers for value-only updates. Do not scan the graph or allocate a new ownership vector every frame.

### 5.3 Water F1b and F2

F1b becomes P2 plus P3 plus P4: adapt the accepted F1a recipe to `SceneTemplate`, insert through the common transaction, and project the common rows. It is not an extension of `ExposureSet` with Whitewater/Look cases. Both Add Water and grouped presets consume the same recipe/metadata. Particle View changes only the Water display branch. New scenes show exactly Water with Foam/Spray/Bubbles beneath it; no duplicate Water child, no Dust row, no Duplicate affordance.

F2 becomes P5. Rename updates labels/section display names while preserving NodeIds, binding IDs, mappings, automation, and roles. Parent visibility fans out through the existing parent-visible binding to all four render objects without overwriting child visibility. Child Hide changes only that child's visibility. Parent deletion removes owned nodes, wires, exposures, and owned modifier state; detaches/reindexes external role routes; preserves source objects and unrelated World users; and restores all of them on undo. Delete a now-unused World only if the existing ownership/reachability check proves it has no remaining participants. No child Remove operation exists.

Saved projects remain ordinary graph+metadata+instance state. No family registry or project version change. After reload, the model derives the same ownership and modulation resolves through the same bindings. Existing old liquid graphs retain their structure and controls; they do not acquire synthetic new Water children. Unknown data remains inert-but-present or produces a diagnostic, never silently disappears.

### 5.4 Convergence and cost

| Scope | Change | Cost and limit |
|---|---|---|
| Add Fluid | Replace liquid-specific insertion with the common template, remapper, and transaction. | Medium: eight constructor sites at this HEAD, nested metadata, multi-output count, and round-trip coverage. No solver change. |
| Scene panel | Replace two ownership walks with one derived model; keep ParamSurface and card hosts. | Medium: stable scoped targeting matters more than drawing rows. Value updates must remain cheap. |
| Box3D enable/add/split | Pure graph builders produce `SceneGraphEdit`; shared transaction and exposure provider own commit. | Medium: group and loose forms, compound children, existing enabled toggles, and shared-world lifetime all need fixtures. Do not create a new world per row. |
| Imported colliders | Import candidate uses the common transaction; existing asset preparation remains outside engine. | Medium: async stale-owner rejection, string bindings, held-out glTF, and collected asset reload. Not an importer rewrite. |
| Fluid roles | Assign/retarget/remove return the common candidate; model supplies action rows and recipient identity. | Medium: external ownership and full rollback are essential. Existing role limits and unsupported-geometry guards remain. |
| Shatter | Keep modifier recipe/preparation/compiler; use common transaction/model capabilities. | Small after P1/P3, but requires a real imported-body acceptance case. No solver abstraction or fragment rows invented. |
| GPU numerical separation | Isolate budgets/data, then FLIP and whitewater cores. | Highest cost, three bounded phases with source conformance before moves. Generated atom pipelines are supplied as prepared data; they cannot be replaced with copied shaders. Preserve numerical kernels, fusion, clock, and capture ABI. This work does not block F1b/F2. |

## 6. Invariants and enforcement

Tests below are required deliverables, not claims of tests already passing. Prefix new focused tests with `physics_boundary_` so phase scope is explicit.

| Invariant | Required machine enforcement |
|---|---|
| Engine has no host dependency | `scripts/check_physics_boundary.py` checks normal Cargo edges and the isolated module imports; `physics_boundary_compile` compiles that source in a temporary probe crate with only allowed dependencies. No lexical grep alone is accepted as proof. |
| No alternate UI/ownership model | `physics_boundary_rows_share_owner` and `physics_boundary_scoped_duplicate_doc_ids`; deletion search for the `fluid_controls` field and its accesses after P3 (unrelated test-helper names may remain). UI Cargo dependency check remains foundation-only. |
| Insertion is atomic | `physics_boundary_insert_rejects_without_mutation`, `physics_boundary_redo_keeps_ids`, `physics_boundary_nested_exposure_remap`; compare graph and instance-layer state before/after failure. |
| One exposure source and working modulation | `physics_boundary_save_reload_modulate`; numeric/string fan-out, aliases, mappings, and nested targets survive actual IO save/load and a subsequent modulation evaluation. |
| One step/capture owner | Existing liquid conformance plus `physics_boundary_group_preserves_tick_owner`; reject direct tick-state escape, duplicate shared-world advancement, mixed completion stamps, nested regions. |
| Water D1–D10 | `physics_boundary_water_rows`, `physics_boundary_water_visibility`, `physics_boundary_water_delete_roles`, `physics_boundary_water_duplicate_rejected`; UI input flows exercise command and shortcut paths. |
| Labels are not identity | `physics_boundary_rename_keeps_bindings`, including role target, undo/redo, reload, and modulation after reload. |
| External objects survive lifecycle | `physics_boundary_role_source_survives_delete`, with a second domain/world user and undo. |
| Preserve GPU resource policy | `physics_boundary_budget_overflow`, `physics_boundary_budget_limit`, `physics_boundary_missing_limits_rejected`; existing whole-scene admission still runs. |
| No steady-state new graph work | Rebuild counter test on value-only updates; if periodic/content-thread work is introduced, trace gate below. |
| Compatibility is not silent fallback | Round-trip old ungrouped liquid, grouped imports, and legacy whitewater axis inputs; unresolved metadata retained with a diagnostic. No Water migration/version bump. |

## 7. Phasing

Every phase is separately reviewed and landable. Dependencies are P1 → P2 → P4; P3 → P4 → P5. P6a/P6b/P7/P8 require P1 and P3, and do not gate Water. G1 → G2 → G3 is independent of authoring after source ownership handoff. Do not combine authoring and numerical moves into one change.

### 7.1 Common execution and seam rules

Before code, restate the binding decisions, forbidden moves, and anchor results for that phase. Run `git rev-parse HEAD`, `git status --short`, and its inventory searches. A changed count or missing symbol requires an updated reviewed seam brief before edits. Rename/delete old Rust symbols first, then let compile errors enumerate remaining callers. Serialized type IDs and ports are not renamed. No compatibility wrapper may preserve an obsolete authoring path.

For implementation phases, use `scripts/codex_checks.py` for the changed paths and `scripts/landing_gate.py` before landing. One cargo command at a time, `CARGO_BUILD_JOBS=4`, focused package checks/clippy and named test filters. The positive test command pattern is `CARGO_BUILD_JOBS=4 cargo test -p <package> physics_boundary_<filter>`; selected tests run under `scripts/gpu_queue.py` where required by the landing gate. GPU builds precede the GPU lock. GPU-path changes use `scripts/gpu_proofs_gate.py` with the touched-path mapping, not an all-proofs run. No workspace suite, unfiltered crate test, optional render exploration, new locks, or numerical tuning.

For UI phases, deliver the named flow and its manifest entry, then run `CARGO_BUILD_JOBS=4 scripts/run_ui_flows.py <flow-name>`. This runner already owns the GPU execution lock. Its assertions and exit code are the agent gate; retain its PNG for Peter. Target L3 through actual input, including visibly actionable row chrome. A PNG alone does not establish behavior. Give Peter the worktree binary's exact launch command at delivery. Save/reload gates must use the real IO path and modulate after reload. If a phase adds periodic/content work, run its same bounded flow with `MANIFOLD_RENDER_TRACE=1`; any frame over 20 ms fails. Otherwise no trace run is added. No background work is authorized by these docs-only edits.

API inventory at this HEAD (later P6–G3 inventories are deliberately re-derived at entry under DESIGN_DOC_STANDARD section 8.3; their upstream source is changing):

- `rg -n 'StackTransaction::prepare' crates` gives **8** callers, all `E/modifier_stack.rs:277,332,386,440,493,547,604,662`. Mechanical: `StackTransaction::prepare(..., |g| domain_edit(g))` → `SceneGraphTransaction::prepare(..., |g| domain_edit(g).map_err(Into::into))`. Keep domain validation in the closure.
- `rg -n 'SceneModifierGraphEdit \{' crates --glob '*.rs'` gives **12** matches: declaration at `C/scene_modifier_edit.rs:20`; result literals at `:181,249,256,389,521,552,595,677,684,911,954`. Mechanical name/import changes; all three fields are unchanged. Re-run a full symbol search for references too.
- `rg -n 'AddSceneFluidCommand::new' crates --glob '*.rs'` gives **8** calls: `A/fluid_domain_edit.rs:797`; `A/ui_bridge/project.rs:659,2277`; `E/scene/fluid/tests.rs:87`; renderer `tests/gpu_proofs/water_basin/explicit_authoring.rs:144`, `water_basin.rs:478,683`; renderer `src/preset_runtime/physics_impulses/coupled_playback_tests.rs:31`. Each uses the new prepared constructor, complete template metadata, and a resolved `SceneNodeRef`. Do not preserve test-only old constructors.
- `rg -n 'LiquidTemplate \{' crates --glob '*.rs'` gives **8** matches including declaration, impl, return signatures, and literals: `E/scene/fluid/template.rs:46,56,83,184`; `E/scene/fluid/tests.rs:501,539`; `A/ui_bridge/project.rs:22,36`. Three recipe bodies need the new pattern: CPU compatibility template, test GPU template, production GPU recipe. Replace body vectors/output ID/category exposures with a complete group plus existing-format metadata. Remove `group_id_slot`; update brittle document-ID fixtures to stable references, preserving saved ID semantics rather than arbitrary creation order.

P3 field inventory: `rg -n '\.fluid_controls\b|pub fluid_controls:' crates` returns **21** matches. Production declaration/use: `R/scene_vm.rs:261`, `A/ui_bridge/projection/scene.rs:107`, `R/viewport_gizmo.rs:199,230`. Tests: `A/ui_bridge/project.rs:2172,2173,2178,2191,2312`; `R/scene_vm.rs:1817,1837,1854,1886,3091,3094`; `R/scene_exposure/fluid_objects.rs:724`; `R/viewport_gizmo.rs:563`; `R/primitives/surface_mesh_normals.rs:140`; renderer `tests/gpu_proofs/water_basin/explicit_authoring.rs:178`, `water_basin.rs:697,795`. Initializers and the local builder at `scene_vm.rs:1344`/`:1453`, `viewport_gizmo.rs:553`/`:790` also change. App projection consumes scoped model controls. Gizmo eligibility uses the model's transform capability, not an empty controls vector. Ownership tests use scoped references; tests merely identifying a liquid use the existing liquid-domain identity. Unrelated helper/test names containing `fluid_controls` are not migration targets.

⚠ **VERIFY-AT-IMPL:** F1a can change the template inventory before P2. It is an expected source change, not permission to guess. Read its landed recipe and update the inventory and field mapping before P2 starts. The same rule applies to G1–G3 while the liquid seam session owns those sources.

### P1 — Reuse the graph transaction

- **Entry/read-back:** baseline audit above; read `E/modifier_stack.rs` and `C/scene_modifier_edit.rs` in full. Re-run the 8/12 inventories. Restate D4, D6, and the stale-owner rules.
- **Deliverables:** `C/scene_graph_edit.rs`, `E/scene_transaction.rs`; old result/transaction renamed and moved; eight modifier actions use it. Preserve their public domain command APIs. Add `physics_boundary_transaction_atomic`, `physics_boundary_transaction_stale_owner`, `physics_boundary_transaction_undo_instance`.
- **Seam:** before `StackTransaction::prepare(...) -> Result<Self, SceneModifierStackError>` with a modifier-only closure/result; after the exact signature in 5.1. Existing graph/result fields map unchanged; generator admission stays in modifier builders. Unsupported GraphTargets reject explicitly.
- **Gate/scope:** focused core/editing check, clippy, tests `physics_boundary_transaction`; existing modifier transaction sibling filters selected by the gate. `rg -n 'StackTransaction|SceneModifierGraphEdit' crates` must return zero. No renderer/UI behavior changes; direct imports of the renamed core result must be updated wherever the compiler finds them.
- **Demo:** none — L1. Add an app-level `physics_boundary_transaction_reload` test: save/reload an existing modifier candidate through actual project IO, modulate after reload, and restore instance state on undo. Include that focused app test in P1's gate. No new periodic work. Forbidden: weakening stale-owner checks, adding a second snapshot implementation, widening modifier owners.

### P2 — General template insertion

- **Entry/read-back:** P1; read F1a recipe, old fluid command/template, exposure provider, group flattening, and 7.1 inventories. F1a must be accepted before changing its production recipe. Restate D3/D4/D8.
- **Deliverables:** exact template/function/command in 5.1; migrate all eight Add Fluid calls and three recipe bodies. Reuse `gpu_flip_preset`, no new core recipe module. Add nested remap, fan-out, capacity rejection, atomic rejection, redo-ID, and save/reload/modulation tests named in section 6.
- **Seam:** before the eight-argument `AddSceneFluidCommand::new(target, render_scene_node_id, fluid_metadata, source_metadata, material_metadata, object_metadata, template, catalog_default)` plus role/world setters; after `InsertSceneTemplateCommand::new` in 5.1. Metadata comes from the template and existing provider; scene identity is scoped; constructor errors are surfaced before submission. Candidate publication is the P1 transaction.
- **Gate/scope:** focused core/editing/app and changed renderer recipe tests; filters `physics_boundary_insert`, `physics_boundary_nested`, `physics_boundary_redo`, `physics_boundary_save`. Negative `rg -n 'AddSceneFluidCommand|LiquidTemplate|TemplateExposure|ExposureSet' crates` zero. Whole-world controls and every object output resolve after flattening.
- **Demo:** L3 `scene-physics-template`: existing Add Fluid action inserts the grouped recipe, undo removes it, redo keeps IDs, reload preserves exposures. The renamed Add Water label and final rows arrive in P4. Gesture: change the liquid Amount through its actual bound control after reload. Forbidden: new metadata vectors, separate child commands, old saved-project upgrading. No numerical changes.

### P3 — Shared object ownership and cards

- **Entry/read-back:** P1; read `scene_vm`, app scene projection, ParamSurface, scene action routing, and section 5.2. Re-run `rg -n 'fluid_controls|object_controls|PhysicsVm'` over R/A/U and record the complete field readers before the rename.
- **Deliverables:** exact `SceneObjectModel` API; migrate renderer/app ownership consumers; app-to-UI conversion; cached structural rebuild; tests `physics_boundary_rows_share_owner`, `physics_boundary_scoped_duplicate_doc_ids`, `physics_boundary_value_update_no_rebuild`. Existing fluid/rigid/role/force/modifier cards remain accessible.
- **Seam:** before `SceneObjectKnownRow.fluid_controls: Vec<NodeId>` plus app-side node additions; after one `controls: Vec<SceneNodeRef>` from core. Mechanical readers consume projected controls; topology walkers are deleted, not wrapped. Geometry/bounds enrichment remains renderer-owned. P3 is not permitted to change material rendering.
- **Gate/scope:** focused core/renderer/app/UI checks and model/card tests; `rg -n '\.fluid_controls\b|pub fluid_controls:' crates` zero. UI Cargo still depends only on foundation among MANIFOLD crates. Model tests cover one shared world, independent manually grouped domains, and duplicate local document IDs.
- **Demo:** L3 `scene-physics-shared-cards`: select existing rigid and fluid objects and a role source; scrub one manifest control, open its modulation drawer, undo. Save/reload then modulate. Forbidden: another ownership registry, label matching, per-frame graph walks, bespoke sliders. Trace only if periodic work is added.

### P4 — Water F1b: Add Water and family rows

- **Entry/read-back:** accepted F1a, P2/P3. Read Water D1–D10 and verify four actual object outputs plus internal obstacle source. No open liquid-seam phase is required.
- **Deliverables:** Add Water label/action uses the common recipe; parent and three look-only rows; all parent/child controls through ParamSurface; Amount 1; Particle View preserves children; grouped bundled presets regenerated. Tests `physics_boundary_water_rows`, `physics_boundary_water_duplicate_rejected`, `physics_boundary_water_particle_view`, and reload/modulation coverage.
- **Gate/scope:** focused recipe/core/app/UI tests plus mapped GPU proof scope for changed presets. Negative searches in the new flow/assertions prove no Dust row, duplicate Water child, child physics/transform/modifier controls, or family Duplicate action. Command tests reject shortcut/context duplication even without UI.
- **Demo:** L3 `scene-water-family`: Add Water, select each row, modulate parent Amount then child Size after reload, switch Particle View, undo/redo insertion. Flow asserts exactly four objects and one undo step. Peter receives the panel PNG. Gesture: perform a Size modulation on Spray without changing Foam or the simulation Amount. Forbidden: another Water builder, whitewater emission on children, Dust output, changing solver defaults beyond approved Amount.

### P5 — Water F2: lifecycle

- **Entry/read-back:** P4; read existing role lifecycle, remove/rename object commands, and parent-visible binding from F1a. Resolve every affected source/world reference before editing. Restate D6/D8.
- **Deliverables:** common transaction-backed Water rename/hide/delete operations; child Hide only; shared capability rejection in all entry routes. `physics_boundary_water_visibility`, `physics_boundary_water_delete_roles`, `physics_boundary_rename_keeps_bindings`, `physics_boundary_role_source_survives_delete`.
- **Gate/scope:** focused editing/core/app tests and named flow; compare complete graph+instance restoration after undo. Save/reload renamed/hidden state then modulate. Negative tests reject child delete and family duplicate; assert zero remaining bindings to deleted family nodes and preserved external source/world users.
- **Demo:** L3 `scene-water-family-lifecycle`: hide Spray, hide/show Water, rename Water while its Amount is modulated, delete Water with attached external roles, undo, reload. Gesture: hide/show the family during playback; assert simulation stamp advances while hidden and child visibility remains unchanged. A bounded runtime observation is mandatory for that claim. Forbidden: hide-as-reset, label-derived IDs, deleting external source objects, a second lifecycle transaction.

### P6a — Rigid add, enable, and split

- **Entry/read-back:** P1/P3; read `E/scene/physics.rs`, add-object path, split caller, and `A/ui_bridge/project.rs` enable branch. Re-run `rg -n 'append_physics_scene_object|add_group_physics|EnableSceneObjectPhysicsCommand|DisableSceneObjectPhysicsCommand' crates`; classify every constructor/helper caller before editing.
- **Deliverables:** pure rigid graph edits committed by `SceneGraphTransaction`; existing recipes/provider stamp exposures; shared capability/model use; enabled writes route through bound parameter editing. Keep domain commands as semantic entry points. Tests `physics_boundary_rigid_enable`, `physics_boundary_rigid_shared_world`, `physics_boundary_rigid_split_undo`.
- **Seam:** command constructors remain public entry points; their execute bodies stop owning before/after graph snapshots and instead execute the prepared transaction. Helper graph construction remains pure and returns nodes/edits; no renderer object is stored in a command. The before/after transaction seam is P1, not a new rigid API.
- **Gate/scope:** focused editing/core/app rigid tests, existing split/physics sibling filters. Negative gate: the migrated commands contain no `prev_graph`, `after_instance`, or direct `project` graph assignment outside the transaction. Existing saved loose and grouped rigid fixtures round-trip and modulate enabled/body controls after reload.
- **Demo:** L3 extend `scene-physics-controls`: enable physics, adjust bounce, disable/re-enable, split/undo with a second object sharing the world. Gesture: toggle enabled while paused then resume; no duplicate body/world. Forbidden: per-object worlds, topology deletion on disable, new locks, coupling changes.

### P6b — Imported graph commit and collider preparation

- **Entry/read-back:** P1/P3; read import worker, merge plan, `ImportModelIntoSceneCommand`, `physics_mesh`, and imported-physics flow. Inventory `rg -n 'ImportModelIntoSceneCommand::new|MergePlan' crates`; preserve current merge return data and worker protocol.
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

- **Entry/read-back:** P1/P3; read `scene_modifier_edit`, `scene_modifier_authoring`, shatter compiler admission, and modifier card host. Re-run `rg -n 'prepare_new_scene_modifier|shatter_targets' crates/manifold-app/src crates/manifold-renderer/src` and confirm P1 already owns commit.
- **Deliverables:** Shatter action/selection uses common object references and capabilities; modifier parameters remain existing ParamSurface cards; rejected targets preserve owner state. Tests `physics_boundary_shatter_targets`, `physics_boundary_shatter_undo_reload`. No new simulation type or insertion command.
- **Seam:** existing renderer preparation signature stays; app converts shared model selection to its existing scoped target list. Remove any duplicated eligibility computation superseded by the model, while renderer compiler validation remains authoritative for prepared geometry.
- **Gate/scope:** focused modifier/renderer/app tests and mapped proof gate. Negative test rejects unsupported animated/deformed/second-active-shatter cases. Saved recipe and controls round-trip and modulate after reload. Use a held-out supported imported mesh.
- **Demo:** L3 extend `scene-modifier-preset`: select imported rigid object, add Shatter, modulate one exposed control, undo/redo/reload. Gesture: change the existing shatter control without losing the body's physics card. Forbidden: moving asset/compiler work into the engine, fragment sliders, relaxing admission to make a demo pass.

### G1 — Compute data and allocation boundary

- **Entry/read-back:** accepted handoff from liquid-seam owner; read current grid/body records, buffer admission, manifold-gpu device snapshot, and G2/G3 imports. P10 accepted ABI is required if its grid outputs are moved. Re-run `rg -n 'admit_candidate_bytes|GpuEncoder|REACTION_FLOATS|MAX_FLUID_ROLES'` over the targeted numerical files and enumerate all import/call changes in the phase review. No Water dependency.
- **Deliverables:** `physics_engine/mod.rs` and `data.rs`; exact allocation and kernel-layout types in 3.2; move shared physical POD/layout helpers field-for-field, leave graph extraction in adapters; shared constants get one physical definition with existing graph limits checked against it. Add dependency checker and compile probe plus budget and `physics_boundary_kernel_pack_matches_descriptor` tests. Move only types needed by the reviewed numerical import closure; graph validators remain in place.
- **Seam:** before `admit_candidate_bytes(snapshot, candidate) -> Result<(), SceneModifierExpandError>` from numerical code; after `GpuAllocationBudget::admit(candidate) -> Result<(), GpuAllocationRefused>`, with host snapshot/policy/error translation. Scene-modifier public admission may remain as its own host wrapper around the shared arithmetic; physics may not call it. No admission is omitted.
- **Gate/scope:** focused gpu/renderer/physics checks and `physics_boundary_budget` tests; dependency compile probe; mapped GPU proofs if layouts/shader includes move. Byte-layout assertions and existing small lattice references must stay equal. Negative imports: no `EffectNodeContext`, renderer `GpuEncoder`, app/UI/core/playback, or scene-modifier module in the new engine module.
- **Demo:** none — L1; data/layout and refusal behavior are computed. No serialized change. Forbidden: new backend, budget fallback, changing role capacity, packed layout cleanup, new solver behavior. If the numerical closure exceeds one session, stop at the reviewed data subset and revise this phase before coding the remainder; do not partially move a type's definition.

### G2 — FLIP numerical stages

- **Entry/read-back:** G1 and liquid-seam handoff; re-read current `gpu_flip_step`, pressure, bodies, clock, narrow-band, sheeting, and their selected proofs. P7a's scheduling audit remains owned by the seam; do not change its policy. Inventory every use of `StepState`, `StepParams`, `Step`, and moved numerical modules before rename; pin the field-for-field mapping from 3.1 to the accepted tip.
- **Deliverables:** `physics_engine/flip/` contains the numerical dependency closure and shaders; primitive retains parameter decode, graph buffers, tick ownership, registry/fusion, history, and diagnostics translation. Rename StepState/Step/StepParams first as specified. Prepare scratch with G1 budgets. Add `physics_boundary_flip_encode_contract` and compile-probe coverage.
- **Seam:** exact encode before/after is 3.1; only module/type names and resource-policy arguments change. Update all atom/CPU/GPU proof imports directly; no re-export under obsolete private paths. Preserve all registered node types, ports, descriptor/fusion proofs, pressure/body math, and dispatch order.
- **Gate/scope:** focused renderer check/clippy, small CPU reference cases, then touched-path GPU proof gate under the shared lock. Compare existing deterministic atom results and counters to their pre-move assertions. Negative gate: isolated module has no `Primitive`, `EffectNodeContext`, `ParamValues`, scene-modifier admission, or renderer encoder imports. Old numerical definitions absent from graph files.
- **Demo:** no new visual surface — L1 numerical proof artifacts; existing mapped render smoke may run only when required by the gate. No claim of new visual quality. Forbidden: fusing stages, changing pressure settings, retuning timesteps, dropping a capture, extra renderer sweeps. No new serialized state.

### G3 — Whitewater numerical stages

- **Entry/read-back:** G1/G2, accepted current whitewater/capture ABI; read `whitewater_step`, its atoms, handoff/lifecycle tests, and seam conformance. If using P10 packed grid outputs, P10 is a prerequisite. Inventory the types/methods in 3.1 and every caller before moving them. P8 L3 debt is still separate.
- **Deliverables:** `physics_engine/whitewater/` contains `WhitewaterState`, `WhitewaterOutput`, physical dispatch, and existing whitewater records; graph adapter retains tick/legacy frame routing, publication ring, primitive descriptors/fusion, and material consumers. Reuse manifold-fluids' physical `WhitewaterSpawn`; no new CPU FLIP integration. Add `physics_boundary_whitewater_encode_contract`, compile probe, and old-axis-input round-trip coverage.
- **Seam:** StepFrame/StepInputs and related records move field-for-field. Split Step as specified in 3.1; retain advance/advance_tick/tick_output and Outputs in the host, and move emit/tick plus explicit-buffer publish into WhitewaterState. Renderer encoder → manifold-gpu encoder only at numerical calls. Install generated kernels through 3.2; preserve byte packing. Buffer retention and fence completion stay explicit in the host adapter. Update direct callers; delete duplicate physical definitions. Existing tick output and legacy graph support remain observable.
- **Gate/scope:** focused renderer check/clippy, existing small CPU pool/type/lifetime references, mapped whitewater/capture GPU proofs. Negative engine import gate from G2. Assert IDs/pools/counters and liquid/whitewater completion stamps remain coherent, including reset and held frames.
- **Demo:** no new visual surface — L1 computed lifecycle/dispatch artifacts; required mapped render smoke only. No simulation cache UI. Forbidden: dust removal from state, new whitewater master node, discarding legacy inputs, weakening buffer lifetime/fence tests. No serialized change.

## 8. Decided — do not reopen

1. Existing CPU worlds and concrete GPU stages are the engine; scene graphs, assets, caches, and UI remain host adapters.
2. No universal solver trait, new server singleton, second clock, new shared lock, or standalone engine crate in these phases.
3. Ordinary groups and existing tick captures define the graph interface. A group does not imply a private world or a nested tick region.
4. Reuse the modifier transaction, ordinary template metadata, exposure provider, and manifest-backed ParamSurface.
5. Derive one object/ownership model and preserve scoped NodeIds across UI actions, undo, save, and reload.
6. Water D1–D10 stand. F1b is P2–P4; F2 is P5. Internal obstacle source follows current F1a scope.
7. Rigid/import/role/Shatter convergence has its own phases. GPU separation does not delay Water authoring.
8. Numerical relocation preserves algorithms, stage composition, ports, time policy, admission, and completion. It cannot claim to finish the liquid seam's open acceptance.

## 9. Deferred and Peter's calls

**Peter's calls:** whether an external consumer justifies extracting the isolated engine sooner; the public engine name if it is released; whether and under what license/distribution terms to release it. Default: keep existing crate names and private workspace packaging, draw/enforce the boundaries now. None blocks Water. Licensing requires a separate dependency/native-asset audit; no license conclusion is made here.

Deferred with explicit triggers:

- **Standalone packaging and stable external API:** revisit when Peter selects an external consumer or requests release. Then extract the isolated module, prove a host-only sample builds without renderer/app/UI, and define semver/support. This document does not claim renderer is an independently usable GPU engine package.
- **Additional solver families, cross-domain coupling, and a new coupling algorithm:** require their own approved physical contract and proof set. Current common authoring interfaces do not imply numerical interoperability.
- **Bake/cache UI and arbitrary-scene recorded playback:** only after the integration plan's provenance/collected-take acceptance and the seam's required outputs are complete. Existing guards remain.
- **Old Water project upgrades, family duplication, Dust display:** excluded by Water D4/D7/D10; reopen only on Peter's explicit change of direction.
- **Broad physics graph regrouping:** only when a specific consumer needs it. Existing saved loose/grouped graphs remain supported; no mass migration for visual tidiness.

Verification limits: no app, GPU proof, render, benchmark, or test suite was run for this document. Concurrent F1a and liquid-seam results are unverified here. The audit proves source relationships at the named HEAD, not runtime correctness, solver quality, licensing readiness, or standalone packaging. Implementation gates above supply that missing evidence phase by phase.
