# Fluid engine integration — creative scene physics in Manifold

<!-- index: FLIP integration through shared manifold-physics forces, scene authoring, timed controls, rigid-body interaction, baking and export. -->

**Status:** IN PROGRESS · 2026-09-26 · Codex. Full implementation authorised; integration preflight underway. P1–P11 are not yet complete.
**Prerequisites:** existing CPU FLIP work at `3683a086d66bd5edf68328a7fdb427515258292b` on `codex/flip-fluids-engine`; reuse the concurrent mesh-collision implementation before the mesh-authoring phases.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5–6. Keep work in the existing worktree; Peter has deferred landing. No public push is authorised by this plan.

Make fluid an ordinary, editable part of a 3D scene. `manifold-physics` owns the shared vocabulary for gravity, spatial fields, timed impulses and collision inputs. Box3D and FLIP implement that vocabulary with their respective solvers. Manifold owns composition, timing, user controls and rendering; upstream FLIP continues to own liquid integration and surface reconstruction.

Peter: “nothing is hard coded to specific actions”; “These forces and interactions should generalise to the 3D scene so we can use box3D with them and other physics interactions”. Also: “there's more important stuff to get it into the app for the user to play with first”. These set the boundary and delivery order.

The first hands-on milestone is **P2: add and edit a fluid in an existing scene**. Complete the later phases for arbitrary scene geometry, shared forces, musical control and repeatable high-quality export. Do not postpone that first milestone until every advanced feature is finished.

Companions: [WATER_SIMULATION_DESIGN.md](WATER_SIMULATION_DESIGN.md) records the current prototype contract; [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) governs graph composition; [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) governs GPU access; [WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) governs the existing parameter UI.

## 1. Audit — what exists

Verified 2026-09-26 against the worktree baseline above. These are source findings unless an observation is explicitly identified. Re-find symbols at phase entry. **Extend, do not redesign.**

| Piece | Source anchor | State and integration consequence |
|---|---|---|
| Owned Box3D API | `crates/manifold-physics/src/lib.rs` (`PhysicsWorld`, `BodyHandle`, `set_gravity`) | Exists. This is the shared physics foundation, currently a concrete rigid-body wrapper; a solver-neutral field/event contract still needs adding. |
| Native rigid forces | `crates/manifold-physics/native/box3d/include/box3d/box3d.h` (`b3Body_ApplyForce`, `b3Body_ApplyLinearImpulse`) | Exists upstream. Rust adapters are new work; no need to implement rigid dynamics. |
| Mesh collision work | Concurrent `codex/flower-mesh-drop`, observed in slot-0: `crates/manifold-physics/src/lib.rs` (`cook_hull`, `add_hulls`, `add_triangle_mesh`, `velocity_at_local_point`) | Dependency outside this baseline. Reuse its cooking, proxies and mesh asset path. Do not duplicate it or assume it has landed. |
| Owned CPU FLIP engine | `crates/manifold-fluids/src/lib.rs` (`FluidWorld`); `native/PROVENANCE.md` | Exists: pinned upstream solver, one documented numerical fix, private native serialization. No GPU solver. |
| Prototype geometry inputs | `crates/manifold-fluids/native/bridge.cpp` (`NativeWorld`); `src/lib.rs` (`set_emitter`, `set_obstacle`) | One box source and one box obstacle. Multiple roles, arbitrary meshes and native removal/lifetime handling are new. |
| Upstream extensibility | `crates/manifold-fluids/native/flip_engine/fluidsimulation.h` (`addMeshFluidSource`, `addMeshObstacle`, `addMeshFluid`); `forcefield.h` (`ForceField`) | Mesh and field extension points exist. Bridge them; do not fork the numerical solver to add scene behaviours. |
| Worker and presentation | `crates/manifold-renderer/src/node_graph/fluid.rs` (`FluidRuntime`, `Request`, `advance`); `primitives/fluid_surface.rs` | Exists: fixed 60 Hz integration, bounded channels, retained time debt, immutable accepted output. Renderer controls still assume the demonstration's box arrangement. |
| Input sampling | `crates/manifold-renderer/src/preset_runtime/physics_sampling.rs` (`sample_physics_history`) | Exists for stateless CPU ancestry. Sampling the current audio/trigger context at historical times does **not** reconstruct past live inputs. A timestamped shared input path is new. |
| Geometry cache | `crates/manifold-renderer/src/node_graph/fluid_cache.rs` (`CacheWriter`, `CacheReader`) | Exists, manifest v5 with v3/v4 readers; geometry/whitewater/obstacle snapshots, not solver checkpoints. Arbitrary input recording and dependency identity are new. |
| Scene authoring | `crates/manifold-ui/src/panels/scene_setup_panel.rs` (`SceneSetupVm`, `build_filtered_properties_owned`); `scene_setup_actions.rs`; `crates/manifold-core/src/scene_exposure.rs` (`stamp_scene_node_exposures`) | Reuse scene selection, action dispatch and ordinary exposed graph parameters. Adding a fluid/field action and projecting its properties are new. |
| World/body projection | `crates/manifold-renderer/src/node_graph/scene_vm.rs` (`physics_world_doc_ids`, `physics_body_doc_id`); `crates/manifold-app/src/ui_bridge/projection/inspector.rs` | World physics already participates in scene projection. Extend this path, not a second fluid inspector database. |
| Trigger and modulation routes | `crates/manifold-core/src/audio_trigger.rs` (`TriggerFireMode`); renderer `primitives/trigger_gate.rs`, `primitives/trigger_ease_to.rs`; `crates/manifold-playback/src/modulation.rs` | Reuse clip-edge/transient choices, envelopes and continuous modulation. Preserve every event between simulation ticks; a display-frame boolean is insufficient. |
| Offline audio | `crates/manifold-app/src/offline_audio_mod.rs` (`OfflineAudioModDriver`); `content_export.rs` | Existing analysis can be reused. Feeding simulation controls independently of output FPS still needs implementation. |
| Existing force nodes | Renderer `primitives/curl_slope_force_3d.rs`, `radial_burst_force_field.rs`, `apply_radial_burst_3d_to_particles.rs`, `field_combine.rs` | Not a shared CPU 3D field API: they include texture/particle-specific payloads, 2D fields and a fixed four-zone burst. Reuse suitable mathematics, not those restrictions. |
| Rendering and examples | Renderer `assets/generator-presets/WaterBasin.json`, `WaterDamBreak.json`, `HoneyDamBreak.json`; `primitives/fluid_surface.rs` | Ordinary meshes, PBR materials and whitewater already render. Preserve them as editable examples; they are not the product's only scene configurations. |

Observed baseline: the cinematic captures established useful water/honey output. Extremely coarse 12³ probes kept approximately real time at 30/60 display FPS on this busy machine, but lost substantial shape detail. Neither observation establishes arbitrary-scene performance or production app acceptance. The existing rendering includes screen-space approximations; this plan does not promise physically complete underwater optics or caustics.

## 2. Decisions

**D1 — Shared interaction API, specialised backends.** Put field evaluation, impulse semantics and tick identity in `manifold-physics`; FLIP consumes them. Retain concrete `PhysicsWorld` and `FluidWorld` ownership. Rejected: a fluid-only force system, because it duplicates exactly the scene interaction boundary Peter wants shared. Also rejected: replacing both solvers with a universal engine framework before users can try fluid.

**D2 — Scene authoring over a parallel editor.** Add Fluid and Force to the Scene Panel, using ordinary graph nodes, stable node references, parameter surfaces and undoable edits. Existing objects can acquire Source, Drain and Collider roles. A Scene modifier may compose a behaviour using these inputs; it is not required to create fluid. Rejected: a hardcoded demo selector or a separate fluid timeline.

**D3 — Compose shape, strength, targets and timing.** Uniform, radial and vortex fields are useful starting blocks. Sum, scale, spatial masks and user-supplied sampled vector fields provide composition. An explosion is a radial field with a triggered impulse/envelope preset. It has no privileged engine action or fixed relationship to an audio band.

**D4 — One authored input stream, fixed simulation time.** Keep the current 60 Hz outer tick initially, independent of display/export FPS; native substeps remain internal. Beat-addressed automation, events and recorded external inputs feed that clock. Rejected: increasing force per displayed frame or rerunning stateful trigger nodes during historical sampling.

**D5 — Same solver across quality levels.** Draft and final use FLIP with the same authored controls and recorded inputs. Different spatial resolution can change motion and event outcomes; promise consistent intent and timing, not identical trajectories. Rejected: silently switching solvers or resolution during a take.

**D6 — Explicit interaction scope.** Shared fields affect liquid and rigid bodies. Static/animated/Box3D-driven colliders displace liquid. Liquid pushing bodies back is a later two-way coupling project, not an implication of sharing the API. Rejected: claiming buoyancy or fluid drag exists merely because the solvers exchange poses.

**D7 — Reuse collision assets, keep visible detail independent.** Photoscans use the existing proxy/cooking path. A detailed render mesh need not be its collision mesh. Preserve concavity when the source representation permits it; never silently replace a bowl with its solid convex hull.

**D8 — Separate simulation, meshing and appearance.** Viscosity/surface tension alter simulation; surface reconstruction alters mesh output; absorption/refraction/roughness alter rendering. Water and honey presets set these deliberately but leave them editable. Runtime-mutability is capability metadata, not a promise that every native setting can change safely mid-step.

## 3. Architecture and ownership

```text
Scene graph + ordinary parameters + existing event/modulation sources
                         |
       content-owned preparation and timestamped input capture
                         |
        manifold-physics shared fields, impulses and tick contract
                   /                            \
       PhysicsWorld / Box3D             FluidWorld / CPU FLIP
                   \                            /
           accepted poses, fluid mesh and whitewater
                         |
               existing scene renderer / manifold-gpu
```

Dependency direction: `manifold-physics -> manifold-foundation`; add `manifold-fluids -> manifold-physics`. Neither crate depends on the renderer, UI or project model. `manifold-physics` must not depend on `manifold-fluids`. It defines the common language, not knowledge of every backend.

The content thread owns authored state and prepares immutable input snapshots. UI sends existing `ContentCommand`/`EditingService` edits and receives snapshots. Native worlds have exclusive owners. Reuse the fluid worker's bounded request/reply channels, cancellation epochs and recycled buffers. No new shared mutable locks or independent event bus.

For a connected rigid/fluid scene, one worker owns both native worlds and advances their shared tick. Independent rigid-only scenes retain their existing path. The shared scheduling module lives in `manifold-physics`; renderer graph preparation maps scene references to its runtime handles and supplies backend operations. Extract the common scheduling policy rather than retain two divergent debt/event implementations. Native process-global locks remain private to their existing crates.

Per connected tick: apply queued edits/events; sample continuous controls; apply rigid forces; step Box3D; provide previous/current rigid poses as the FLIP collision interval; step FLIP; publish both results with one tick stamp. Animated collider poses come from the authored interval. If FLIP needs a next pose, extrapolate from the accepted end velocity and label this approximation; do not advance Box3D an extra visible tick. No fluid reaction force returns to Box3D in this version.

This costs throughput: a coupled scene presents at its slowest required solver's accepted time. Publishing a new rigid pose over stale water would break contact alignment. Preview retains the previous coherent result and reports lag; offline waits. Other uncoupled scenes must not be enlisted in that wait.

### 3.1 First committed shared API

Add `crates/manifold-physics/src/interaction.rs`, re-exported by `lib.rs`. These are runtime types, not a second project serialization format. Arrays follow the existing native-wrapper convention.

```rust
pub trait VectorField: Send + Sync {
    /// Dimensionless world-space vector at a world-space position.
    fn sample(&self, position: [f32; 3]) -> [f32; 3];
}

pub struct FieldInput<'a> {
    pub field: &'a dyn VectorField,
    pub acceleration: f32,   // metres / second²
    pub delta_velocity: f32, // metres / second, consumed once this tick
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickStamp {
    pub epoch: u64,
    pub tick: u64,
}

impl PhysicsWorld {
    pub fn apply_fields(
        &mut self,
        bodies: &[BodyHandle],
        fields: &[FieldInput<'_>],
        dt: Seconds,
    ) -> Result<(), PhysicsError>;
}
```

`apply_fields` samples each dynamic body's centre of mass; the adapter converts acceleration to force using body mass and velocity change to a mass-scaled impulse. Fixed/animated bodies retain their authored behaviour. Spatial torque, explicit force-at-point in newtons and impulses in newton-seconds remain Box3D-specific capabilities; do not pretend the centre-sampled field produces torque.

`VectorField` is the extension seam, not a list of prescribed actions. Implement immutable prepared uniform/radial/vortex fields, arithmetic composition and trilinearly sampled grids. Masks and transforms are part of the prepared evaluator; validate finite outputs and non-singular transforms before a tick. No allocation inside `sample`. Target selection resolves to recipient lists during preparation, not string searches per cell/body.

Add to `manifold-fluids/src/lib.rs` in P6:

```rust
impl FluidWorld {
    pub fn step_with_fields(
        &mut self,
        dt: Seconds,
        fields: &[manifold_physics::FieldInput<'_>],
    ) -> Result<FrameStats, FluidError>;
}
```

Keep `step(dt)` as the compatibility entry with no additional fields. The bridge uses the existing native `ForceField` extension to supply a prepared field, using reusable grid storage; it must not retain the borrowed Rust inputs after the call. Combine acceleration with `delta_velocity / dt` for that outer interval so the impulse integrates once across adaptive native substeps. Verify this against the native integrator before exposing the control: pressure projection and collisions can change the resulting liquid velocity, but impulse strength must not multiply with substep count. Failure of that conformance test blocks this adapter, not permission to patch arbitrary particle velocities.

Common semantics are world metres, seconds and vectors. Default World gravity is `[0, -9.81, 0]`; a fluid inherits it unless an explicit domain override is enabled. The override is visible. Existing scale-dependent viscosity/tension coefficients retain their honest artistic labels.

### 3.2 Geometry and solver capabilities

Replace singleton native source/obstacle ownership with per-world handle tables. Stable graph references map to opaque, provenance-checked runtime handles, following `BodyHandle`; handles are never project IDs. Support Initial Fill, Inflow, Outflow and Collider independently, including multiple instances and transformed meshes. Initial Fill runs at epoch creation/reset; inflow/outflow remain controllable during playback.

Use one prepared collision asset for all consumers where suitable: cooked hull sets can become FLIP boundary triangles; static triangle proxies preserve concavity. Geometry buffers and revisions are immutable; poses update without recooking. Resolve scaling at preparation, accept translation/rotation per tick, and reject invalid/non-manifold volume input with an actionable object name. Collider thin-shell handling must follow verified upstream capability rather than guessing that every open scan encloses liquid.

**⚠ VERIFY-AT-IMPL:** mesh descriptor ownership follows the concurrent cooking work. Before P3/P4, run `rg -n 'cook_hull|add_hulls|add_triangle_mesh|struct.*Collider|enum.*Collider' crates/manifold-physics/src crates/manifold-renderer/src/node_graph` in the selected base. Read those definitions and pin the mesh bridge signatures in this document before delegating. If absent, integrate the verified dependency; do not create a replacement cooking pipeline. The snapshot observed in slot-0 is not a base revision guarantee.

Rectangular domains use upstream cell counts per axis and one uniform cell size. V1 domain bounds are axis-aligned in scene space, with editable size/position and closed/open faces through `fluidsimulation.h` (`setFluidBoundaryCollisions`). A source outside the domain is visibly flagged; moving the domain requires a new simulation. Separate domains do not exchange liquid or collide with each other's liquid in this version.

Expose backend capability metadata to preparation: supported roles, moving geometry, fields, setup-only versus live settings. An unsupported connection is rejected before playback with its reason. Never accept a wire and silently ignore it.

## 4. Scene Panel and creative workflow

From an existing 3D scene, **Add → Fluid** inserts a domain, a small initial fill and ordinary surface material through one undoable graph edit. Select Fluid to see Simulation, Sources & Colliders, Surface and Material sections using the existing parameter surface. World contains shared Gravity and Physics status. **Add → Force** inserts a selectable field with a position/direction/radius gizmo and a Targets chooser. Source/drain/collider roles can be assigned to existing scene objects; one visible mesh may have more than one explicit role.

A new force targets the scene's dynamic bodies and fluid domains by default. The user can restrict it to named bodies/domains; spatial masks then control influence within each domain. An ordinary render-only object has no physical response until it has a physics role. Target references survive renaming and grouping.

| Selection | Controls | Timing contract |
|---|---|---|
| World | Gravity XYZ, shared playback/reset status | Live vector control; Reset is an event. |
| Fluid / Simulation | Domain bounds, initial fill, liquid preset, viscosity, surface tension, preview quality, export quality | Bounds/quality/fill and initially liquid coefficients are setup edits; one restart on committed gesture. Preset changes are normal undoable parameter edits. |
| Source | Enabled, geometry/transform, emission velocity XYZ, flow control, inherit motion | Live controls where native support is verified. Source topology/geometry revision prepares a new epoch. |
| Drain | Enabled, shape/transform, removal strength where supported | Live enable/pose; do not expose a fabricated removal-rate control when upstream semantics differ. |
| Collider | Existing collision proxy, role, friction/slip capability, motion binding | Existing physics mesh workflow; live pose, prepared geometry. |
| Force | Field shape/graph, transform, extent/falloff, continuous strength, impulse strength, targets | Continuous parameters use standard drivers. The impulse input accepts existing event sources. |
| Surface | Mesh detail, smoothing and particle reconstruction settings | Reconstruct/rebake according to cache capabilities; no hidden physics restart labelled “material edit”. |
| Material / whitewater | Existing PBR controls; foam/bubble/spray appearance and generation controls | Materials live; native population-generation changes are setup edits until proven live-safe. |

All exposed live scalar/vector controls use the usual parameter rows, keyframes, LFOs, beat drivers, audio modulation and MIDI/OSC mappings. Reuse current event selection for Clip Edge, Transient and Both; allow explicit manual triggers and timeline events through the same action binding. Never add a dropdown naming special behaviours such as “kick splash”.

Examples are editable compositions: a radial impulse on a clip edge; a vortex whose strength follows any audio send; a moving emitter with velocity modulation; a gravity flip at a timeline marker. A user can replace any trigger or field without changing the solver. Sum modulation with the authored base value using existing arithmetic/envelope nodes; do not overwrite a user's fader with a hardcoded trigger envelope.

Save these as ordinary graph/preset structures with stable `NodeId`/scene references. Group, duplicate, rename, undo/redo and save/reload preserve role assignments and bindings. Runtime worlds, handles, pending events and buffers are skipped in project serialization; new persistent fields use camelCase and backward-compatible defaults. Existing water/honey presets remain loadable and migrate through the normal graph compatibility path.

For the authoring workflow, use Houdini's separation of container, source, collision and surface as a reference, while keeping Manifold's existing scene selection and parameter gestures. Its [minimal FLIP setup](https://www.sidefx.com/docs/houdini/fluid/sopminimalsetup.html) and [tank controls](https://www.sidefx.com/docs/houdini/fluid/sopconfigtank.html) illustrate those roles. This is a UI reference, not a claim of equivalent solver capabilities. Physical controls, surface reconstruction and material/style controls must remain distinguishable in the inspector, with setup edits labelled as requiring restart/rebake.

## 5. Timing, events and lifecycle

Continuous inputs and discrete events share a capture clock but remain distinct. Capture events at their existing producer boundary with beat/time and a stable sequence number; resolve authored beats using the project tempo rules. At preparation, map references to runtime recipients. Capture resolved strength, transform and targets for an impulse so later edits cannot rewrite an already queued hit.

The fixed interval is `[t_n, t_(n+1))`. Events in that interval apply once at tick n; an event exactly on the upper boundary belongs to the next tick. Order equal-time events by producer sequence; reset ends the old epoch before applying following events to the new one. Never use a display-frame pulse as the queue. Late input enters the next unstarted tick with lateness recorded; it does not mutate an already published tick. Offline playback uses the recorded applied tick to reproduce a live take, including this quantisation.

Retain continuous input history until its consuming tick completes. Sample stateless authored curves at physics time. For live audio/external data, retain actual timestamped feature/control samples and use hold/interpolation rules appropriate to each value. Do not synthesise historical audio by reusing today's frame context. Offline source audio passes through the existing analyzer at audio/control cadence and produces the same timestamped stream independently of output FPS.

| Action | Behaviour |
|---|---|
| Pause / resume | Hold the accepted state; discard paused wall time. Resume at the next simulation tick. |
| Normal clip edge | Preserve the scene's simulation. A reset occurs only when the user binds it. |
| Explicit Reset | Cancel old work by epoch, restore initial fill, clear old events; keep authored bindings. |
| Seek / backward loop in Live | Start a new epoch and reset; display the reset state honestly. No false reconstruction of uncomputed history. |
| Seek with a valid bake | Address the cached timeline. No native simulation and no newly applied live force. |
| Need an exact live-state seek | Replay from bake start/pre-roll using recorded inputs; indicate preparing until the requested time is reached. Geometry caches are not checkpoints. |
| Topology/setup edit | Prepare resources off the content hot path, cancel old epoch and restart once. Undo restores authoring, then follows the same rebuild rule. |
| Hidden/removed scene | Stop scheduling hidden simulation; reactivation resumes without hidden elapsed time. Removal cancels and releases owned resources. |
| Lag / capacity / native error | Retain coherent output, expose the affected scene and measured lag/error. Do not silently drop impulses, lower resolution or claim current-time output. |

The shared input queue is bounded and reuses storage. Overflow pauses the affected simulation with a visible diagnostic and retains the recording prefix; it cannot silently overwrite unread history. The UI offers Restart or Bake, rather than asking for CFL/substep tuning.

## 6. Baking, quality and rendering

Record a **take** as timestamped resolved controls/events plus geometry revisions, initial conditions, seed and tempo mapping. This makes an audio/MIDI-driven draft replayable at higher quality even when the original live source is gone. Keep the authored graph as the editable source; a take is a deliberate performance recording, not a second graph.

The Bake action selects project-relative start/end and pre-roll, uses export quality, and presents progress/cancel/status in the existing app job presentation. Reuse the existing worker and atomic cache writer. Bake identity includes solver revision **and local numerical fix revision**, adapter/schema version, quality, geometry/content hashes, source/collider/field graph, initial conditions and input-take hash. Material/camera/light changes do not invalidate physics; native meshing/whitewater changes invalidate the relevant stored outputs.

Extend cache schema beyond v5 with explicit legacy readers. Existing v3/v4/v5 demonstration caches keep their historical playback semantics; they are not relabelled as arbitrary-scene takes. Missing/corrupt/incompatible assets are visible errors. Cancellation preserves a clearly marked playable completed range and never registers an unfinished range as complete. Saving/collecting a project includes references to its take, proxies and cache assets through the existing asset mechanism; no absolute workstation-only cache paths in new project data.

Reuse the RT quality presentation pattern, with independent Fluid preview/export settings. Provide six named levels: Ultra Low, Low, Medium, High, Extra High, Ultra. The internal policy derives cell size from domain extent, with explicit resource caps and visible achieved detail. Do not put native resolution, CFL or substeps in the primary UI. Derive tier budgets from bounded representative probes at P10; the current 12³ box measurement is not a universal recipe. A setting change restarts/rebakes, and preview quality is fixed for a take.

Keep surface detail and rendering style independent of solver quality. Offer water/honey starting materials alongside ordinary opaque/stylised PBR appearances and composable whitewater meshes. Preserve the existing optical limitations honestly. Export drains all required fixed ticks, then samples accepted/cached states at the requested output cadence. A 24/30/60 FPS choice must not change the input stream or simulation clock. Deforming-fluid temporal sampling uses the existing cinematic capture approach as the reference, integrated into normal export rather than requiring a special demo binary.

CPU field/geometry inputs are the first path. Arbitrary GPU-generated vector fields require an explicit, timestamped `manifold-gpu` readback bridge, reusing existing readback patterns such as `primitives/color_sample.rs`. A requested simulation tick waits for its matching field revision; it never consumes an unlabelled stale texture. Implement this advanced bridge in P11 after the basic workflow. No Metal pointers in the shared physics API and no wgpu dependency. Vulkan runtime support remains unverified until that backend executes the same proofs.

## 7. Invariants and enforcement

Test names below are **required new tests**, not checks already run. Prefix them `scene_physics_` so the focused phase command selects them.

| Invariant | Required enforcement |
|---|---|
| Shared acceleration/impulse meaning | `scene_physics_mass_independent_field_response`, `scene_physics_impulse_once_across_substeps`; verify free-body numerical expectations and bounded liquid momentum response. |
| Events survive display stalls and run once | `scene_physics_multiple_events_between_frames`, `scene_physics_boundary_event_once`, `scene_physics_no_stale_epoch_input`. |
| Output FPS does not alter physics inputs | `scene_physics_input_stream_24_30_60`, including recorded audio and several events within one display interval. |
| Arbitrary roles are not demo indices | `scene_physics_two_sources_and_drain`, `scene_physics_rotated_mesh_collider`, with a held-out concave proxy asset. |
| Shared pose time | `scene_physics_rigid_fluid_same_tick`, including delayed fluid replies, reset and cancellation. |
| Ownership and persistence | `scene_physics_roles_undo_reload`, `scene_physics_binding_after_reload`, handle-provenance rejection; no new `Arc<Mutex`/`Arc<RwLock` in changed code. |
| Bake identity and failure honesty | `scene_physics_cache_dependency_invalidation`, `scene_physics_cancelled_bake_range`, legacy cache-reader tests, missing-input error assertions. |
| Bounded hot path | Allocation/queue-capacity assertions after preparation; `MANIFOLD_RENDER_TRACE=1` on named UI flows, content work >20 ms fails acceptance and is diagnosed. Report concurrent machine load. |
| GPU portability boundary | New GPU work only through `manifold-gpu`; focused GPU proof selection includes the new field bridge/render paths. No Vulkan runtime claim from Metal results. |

## 8. Phasing

Each phase ends at a committable state; it is not a delivery-time estimate. The lead owns design/read-back and pins any changed public seam before a mechanical worker receives it. Follow current AGENTS.md worker preparation and diff-based check selection. No phase authorises landing.

Shared entry check for every phase: inspect the worktree diff, verify the recorded base/dependency commits, re-run `rg -n` for that phase's named symbols below and read their definitions. Restate its binding decisions and forbidden moves before editing. A moved API is a conformance update to this plan, not permission for a parallel implementation.

All commands run from the owned worktree, using its absolute `Cargo.toml` via a task-specific `FLUID_MANIFEST` variable. Use the repository build lock and guard for executable checks. For a listed crate, focused gate means `RUSTC_WRAPPER= cargo test --manifest-path "$FLUID_MANIFEST" -p <crate> scene_physics_` plus focused clippy for that changed crate. GPU work instead uses `python3 scripts/gpu_proofs_gate.py --manifest-path "$FLUID_MANIFEST" --filter scene_physics_`, adding existing `fluid_`/`water_` filters when touched. Re-run no passed checks without a relevant change. UI acceptance flows below are deliverables registered in `scripts/ui-flows/manifest.json`, then run with `python3 scripts/run_ui_flows.py <flow-name>`; success requires the assertions, not merely flow completion. These commands describe future validation, not work performed for this plan.

### P1 — Shared interaction contract and Box3D adapter

- **Entry/read-back:** D1/D3/D4; `PhysicsWorld`, `BodyHandle` and native force declarations. No fluid dependency changes yet.
- **Deliver:** `interaction.rs`, API in §3.1, prepared field evaluators and Box3D force/impulse adapter. Add mass, static-body, foreign-handle, falloff/transform and finite-value tests.
- **Gate/scope:** focused `manifold-physics` tests/clippy; numerical acceleration/impulse expectations pass. Inspect diff for zero new shared locks and no FLIP dependency in `manifold-physics/Cargo.toml`.
- **Demo/gesture:** none — L1 backend contract. No new UI or renderer sweep. **Forbidden:** a universal backend registry or renamed/replaced Box3D API.

### P2 — First usable Add Fluid workflow

- **Entry/read-back:** D2/D8; `SceneSetupVm`, `stamp_scene_node_exposures`, `fluid_surface`, normal graph edit commands and existing water preset bindings.
- **Deliver:** Add Fluid graph insertion, selection, domain/reset/source controls and ordinary material sections. Keep prototype geometry limitations explicit until P4. Add `scene_physics_add_fluid_undo_reload` and `fluid-authoring` UI flow.
- **Gate/scope:** focused core/editing/UI/app tests and changed-crate clippy; `fluid-authoring` L3 drives add → change source position → play → pause → reset → undo/redo → save/reload → change again. Existing water GPU proof once if its path changes; inspect render-trace costs. Provide Peter the exact worktree launch command.
- **Gesture:** move the pouring source while previewing fluid in an existing scene. **Forbidden:** replacing the user's scene with Water Basin, direct UI model writes, exposing CFL, claiming arbitrary mesh support yet.

### P3 — Native multiple-source and collision bridge

- **Entry/read-back:** §3.2 dependency verification; `NativeWorld`, `MeshFluidSource`, `MeshObject`, source/obstacle add/remove lifetimes. Pin bridge mesh/handle signatures against the verified cooking API.
- **Deliver:** per-world role handles, reusable mesh storage, initial fills/inflows/outflows/colliders, rectangular domain configuration and validation. Preserve old box convenience calls through the same implementation. Add held-out mesh and multi-source tests.
- **Gate/scope:** focused `manifold-fluids` tests/clippy; finite nonempty output, independent removal/enabling, rotated obstacle displacement and foreign-handle rejection. No GPU work. **Demo:** none — L1 native output assertions.
- **Forbidden:** a new convex-decomposition pipeline, retained dangling native field/source pointers, box-only fallbacks for rejected meshes.

### P4 — Arbitrary scene role authoring

- **Entry/read-back:** P3; §4; current scene-reference/mesh asset/proxy APIs from the collision work. Verify old preset migration seam.
- **Deliver:** select existing mesh → assign role/domain, multiple sources and drains, visible bounds/proxies, capability validation, stable serialization and undo. Add `fluid-scene-roles` flow and role round-trip tests.
- **Gate/scope:** focused changed core/editing/renderer/UI/app crates; flow L3 assigns two differently transformed sources plus a drain and concave container, reloads and changes one source independently. Computed particle/surface bounds confirm the held-out container is not replaced by a solid hull. Focused GPU proof only for affected geometry presentation.
- **Gesture:** turn an imported mesh into a pouring source and rotate it. **Forbidden:** new scene-object IDs, copied photoscan cooking, silently accepted unsupported roles. Dynamic Box3D collision binding waits for P8.

### P5 — Shared timestamped input and event scheduler

- **Entry/read-back:** D4; `physics_sampling.rs`, `TriggerFireMode`, trigger/envelope primitives, playback modulation and offline audio driver. Inventory existing producer timestamps before extending their payloads.
- **Deliver:** shared tick/event retention in `manifold-physics`, content-owned capture, ordered impulses/reset, continuous sample history, recorded applied ticks, epoch/lifecycle handling. Preserve stateless history sampling without replaying stateful nodes. Add all timing tests in §7.
- **Gate/scope:** focused physics/playback/renderer/app tests/clippy; identical resolved streams at 24/30/60 FPS with a display stall and audio fixture, boundary/reset event assertions and queue exhaustion diagnostic. Negative source check: no second audio analyzer or UI trigger router.
- **Demo:** none — L1 deterministic input traces. **Forbidden:** one event per display-frame storage, current-audio substitution for history, silently dropped debt.

### P6 — FLIP consumes the shared fields

- **Entry/read-back:** P1/P5; `ForceField`, `ForceFieldGrid`, `FluidWorld::step`; verify upstream integration and registration lifetimes.
- **Deliver:** `step_with_fields`, reusable native field-grid adapter, common uniform/radial/vortex/masked inputs and explicit validation. Field changes must not reconstruct the world. Add fluid impulse/substep conformance and gravity override tests.
- **Gate/scope:** focused physics/fluids tests/clippy. Compare force-off versus force-on momentum/centre-of-mass at fixed configuration, verify zero field compatibility, one-shot impulse budget across substeps and no retained borrowed data. No visual quality claim from these numerical checks.
- **Demo:** none — L1 adapters. **Forbidden:** hardcoded explosions, direct particle kicks that bypass the solver, numerical solver rewrites to compensate for a failed bridge.

### P7 — Shared Force UI and musical bindings

- **Entry/read-back:** P4/P5/P6; normal parameter surface, event chooser, stable binding references and graph arithmetic/envelopes.
- **Deliver:** Add Force, field composition/targeting, gizmos, continuous/impulse inputs; shared bindings for Box3D and fluid. Add editable radial-hit/vortex examples, not engine actions. Add `scene-forces` UI flow and binding-after-reload test.
- **Gate/scope:** focused changed crates, `scene-forces` L3: set field strength, bind a clip edge, rebind it to a different audio send/event, save/reload and trigger again; numerical recipient velocities/field samples confirm both backends receive the intended input. GPU proofs cover gizmos only if their GPU path changes.
- **Gesture:** one radial hit affects selected rigid bodies and liquid; unselected objects remain unaffected. **Forbidden:** FLIP-only Force inspector, fixed audio-band semantics, replacing manual strength with an envelope.

### P8 — Coherent Box3D-to-fluid collision playback

- **Entry/read-back:** §3 ownership/order; existing rigid stepping, fluid `Request`/reply and accepted obstacle pose; verified mesh-collision dependency.
- **Deliver:** connected-scene worker ownership, shared tick scheduling, Box3D pose/velocity collision intervals, matching render poses, cancellation and slow-worker diagnostics. Add same-tick/epoch tests and `scene-fluid-rigid` flow.
- **Gate/scope:** focused physics/renderer/app checks and affected GPU proof. L3 flow drops a rigid proxy into fluid, delays fluid replies, pauses/resets and verifies matching stamps/poses at every accepted snapshot. Record lag and content-thread trace; no requirement that dense FLIP reaches real time.
- **Gesture:** drop a photoscan object using its existing collision proxy into a filled domain. **Forbidden:** second native owner, async stale collider pose, describing this as buoyancy or two-way coupling.

### P9 — Recordable takes and complete cache identity

- **Entry/read-back:** §5/§6; `CacheWriter`, `CacheReader`, current asset serialization and offline analyzer inputs. Pin new manifest/take structs using existing asset references and camelCase conventions.
- **Deliver:** timestamped input-take writer/reader, content/dependency hashes, new cache manifest, legacy readers, partial-range metadata and explicit missing-input errors. No UI bake job yet.
- **Gate/scope:** focused renderer/app/storage-owner tests and clippy; record → reload → replay matches controls/events exactly and fixed-config solver output within declared tolerance. Dependency mutations invalidate the expected cache; material edits do not. Test interrupted/held-out legacy inputs.
- **Demo:** none — L1 persistence assertions. **Forbidden:** treating a geometry cache as a restart checkpoint, recomputing a live performance from unavailable audio, silently loading mismatched caches.

### P10 — Bake, quality tiers and normal export

- **Entry/read-back:** P9; existing app export/job/asset paths and RT quality settings. Pin fluid tier resource budgets from bounded representative probes, not the 12³ demo alone.
- **Deliver:** independent preview/export tiers, Bake start/end/pre-roll/cancel, cache status/range, collected-project references and normal export integration. Keep numerical tuning internal. Add `fluid-bake-export` flow and quality/cache tests.
- **Gate/scope:** L3 flow bakes a short recorded take, cancels another, reloads the project and exports at 30/60 FPS; assert complete ranges, identical tick/event histories, zero native stepping during cached playback and valid decoded frame counts. Produce one bounded full-speed review clip. Focused GPU proofs for actual changed export/render paths; report playback FPS separately from bake throughput.
- **Gesture:** record a force performance in Draft, select higher export quality, bake and render with a different material. **Forbidden:** silent resolution adaptation, relabelling partial output complete, promising identical trajectories across tiers.

### P11 — Custom spatial fields and final acceptance

- **Entry/read-back:** P7/P10; `VectorField` grid evaluator, existing GPU readback abstraction and primitive decomposition/fusion rules.
- **Deliver:** explicit graph-to-CPU sampled 3D field bridge with tick/revision stamps and bounded reusable buffers; support user-composed field volumes without new solver actions. Register focused proof selection. Complete lifecycle/migration/round-trip matrix across the phases above.
- **Gate/scope:** compare sampled GPU field vectors against a CPU analytic fixture at named positions/times; prove stale readback rejection and queue bounds. Add `scene-custom-field` L3 flow driving the same field into both backends. Run the necessary Metal proofs; Vulkan remains a named verification gap until available. Final scene starts blank, uses held-out mesh proxies, two sources, a drain, shared fields, event/audio bindings and a recorded/exported take.
- **Gesture:** replace a stock radial field with a user-composed masked field without changing targets or trigger bindings. **Forbidden:** synchronous Metal-specific pointer access, a field bridge that only works in the demo binary, claiming arbitrary GPU-deformed mesh collision from vector-field support.

Coverage: shared API P1; first app use P2; general engine geometry P3; scene roles/persistence P4; timing/lifecycle P5; FLIP fields P6; creative force/event UI P7; rigid collision interaction P8; replay identity P9; user baking/quality/rendering/export P10; custom fields and cross-feature acceptance P11. Remaining exclusions are explicit below.

## 9. Decided — do not reopen during implementation

1. `manifold-physics` owns shared interaction semantics; Box3D and FLIP consume them.
2. FLIP stays upstream CPU; this is integration, not a new numerical solver.
3. Scene/graph composition and existing parameter/event routes are the product API.
4. Fixed physics time and captured input, independent of output FPS; native stability controls stay internal.
5. Show the first editable fluid in the app at P2, before advanced draft optimisation.
6. Reuse existing collision proxies and cooking; visible scans keep their detail.
7. Shared forces and rigid-to-fluid collision are included; fluid-to-rigid feedback is not yet implemented.
8. Quality changes can change detailed motion and require restart/rebake.

## 10. Deferred, with revival triggers

| Exclusion | Revisit when |
|---|---|
| Two-way buoyancy, fluid pressure/drag feedback and angular coupling | P8 is accepted and Peter wants floating/reactive bodies. Design momentum exchange and stability tests before calling it supported. |
| GPU FLIP, sparse/adaptive solver research | CPU profiling demonstrates a product blocker after the app workflow exists. Evaluate an upstream-compatible backend through Metal/Vulkan-neutral boundaries; no promise of an easy port. |
| Automatic adaptive preview tiers and elaborate sketch prediction | Users can author scenes and tier measurements expose a concrete usability problem. Never change a running take silently. |
| Granular sand/rocks, dust/smoke, elastic goo | A separate material model is requested. FLIP liquid and whitewater particles do not provide these behaviours. Honey remains a viscous-liquid preset. |
| Multi-liquid mixing, interacting liquid domains and calibrated rheology | A concrete scene requires them and upstream capability/units are verified. |
| GPU-deformed topology-changing collision meshes | Fixed-topology/proxy scenes are usable and such a mesh is required. Needs its own timestamped geometry readback/cooking budget; P11 field support does not imply it. |
| Checkpoint-based instant seeking, recursive refraction, caustics and underwater rendering | Cache replay and normal export are accepted and an observed scene requires one of these features. |
