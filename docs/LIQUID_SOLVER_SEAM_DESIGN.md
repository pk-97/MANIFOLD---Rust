# Liquid Solver Seam — the one contract a liquid solver meets to play in a scene

<!-- index: The contract FLIP, GPU MLS-MPM and SWASH meet to join scenes — particle frames, face-grid outputs, Box3D coupling, clock/pause/export, scene recognition, safety rails — and the phases that move MPM and SWASH behind it. -->

**Status:** PROPOSED · 2026-09-30 · P1–P10 not built · owed: Peter's calls in section 8 (Calls only Peter makes) · amends GPU FLIP's tick loop and solids phase (D7, D10, D12).

**Prerequisites:** none for P1–P6 (MPM coupling is on main). P7a needs GPU FLIP's full step (GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)). P10 needs the BUG-imy3 (GPU whitewater, solver-agnostic) design approved.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's rule (2026-09-30, relayed by the lead): "APIs, boundaries, solvers and the physics API stay modular and safe to reuse for future solvers, algorithms, interactions and sims."

Three liquid solvers exist. FLIP is the vendored CPU engine (`crates/manifold-fluids`, `node.fluid_surface`). GPU MLS-MPM is on main ([GPU_MPM_SOLVER_DESIGN.md](GPU_MPM_SOLVER_DESIGN.md), "the MPM design", `matter_*`). GPU FLIP is the GPU water solver ([GPU_FLIP_PRESSURE_SOLVE.md](GPU_FLIP_PRESSURE_SOLVE.md), "the GPU FLIP doc"; `node.gpu_flip_domain`). Until 2026-10-01 it was SWASH, with an FFT pressure solve (`docs/archive/FFT_WATER_SOLVER_DESIGN.md`, "the SWASH design"): below, SWASH means GPU FLIP, `swash_*` files are now `gpu_flip_*`, and SWASH P3b is GPU_FLIP_PRESSURE_SOLVE.md section 8 (owed). The particle-frame seam is [GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md) ("the surface design"). This doc is the contract all three meet, the smallest set of seams that does it, and the phases that put MPM and SWASH behind it.

Binding from outside this doc: no FLIP tuning or integration work (Peter, 2026-09-29), so FLIP conforms as built; never a GPU port of FLIP; no fallback modes and no stopgaps; before any GPU run at a new size, prove on the CPU that every buffer covers its dispatch, and step resolution up one size at a time (two forced Mac resets above res 64); SWASH stays a challenger on its branch until Peter's SWASH P4 call.

## What it does on stage

Today each liquid joins the scene its own way. FLIP water takes forces, has a water panel, pauses, records takes, and floats Box3D boxes. MPM water floats boxes but ignores force fields and MIDI hits, and shows no water panel (BUG-4lfm (GPU-surface water not recognised as water)). SWASH water has none of it: it keeps moving while the transport is paused, ignores Speed, and runs at half speed when the frame rate halves.

After this design, any liquid Peter drops into a scene answers his hands the same way. Pause freezes it on the frame. Speed slows it. Reset restarts it together with the boxes floating in it. A 30 fps export matches the 60 fps one tick for tick. Forces and pad hits push it. The water panel shows up. A new solver gets all of that by meeting the contract, never by editing the scene layer.

## 1. Audit — what exists (verified 2026-09-30 at `dfc884568`; SWASH at `79d477fb8`)

Paths: `R/` = `crates/manifold-renderer/src/node_graph/`, `RP/` = `crates/manifold-renderer/src/preset_runtime/`, `P/` = `crates/manifold-physics/src/`, `core/` = `crates/manifold-core/src/`, `edit/` = `crates/manifold-editing/src/`, `app/` = `crates/manifold-app/src/`. SWASH paths are on `origin/feat/fft-water`; main has no SWASH code.

### 1.1 The three solvers side by side

| | FLIP (CPU engine) | GPU MLS-MPM | SWASH |
|---|---|---|---|
| Domain node | `node.fluid_surface` (`R/primitives/fluid_surface.rs:41`) | `node.matter_domain` (`R/primitives/matter_domain.rs:75`) | none: the preset chains `liquid_fill` → `liquid_feedback` → two step copies → back (`R/primitives/swash_preset.rs:297`) |
| Velocity lives on | MAC faces inside the C++ engine | grid nodes: 32-byte `MatterGridNode` (`R/matter.rs:59`), node i at min + i·dx, 3 padding nodes (`:397`, `:402`) | faces: 32-byte `FaceSample` over (n+1)³, index i + (n+1)(j + (n+1)k) (`R/fluid_particles.rs:126`) |
| Particle frame | `particles_a/b`, `count_a/b`, `solid_a/b`, `grid_bounds`, `grid_nodes_*` | `node.matter_frame`: A/B ring, non-finite gate (`R/primitives/matter_frame.rs:68`) | raw state straight into the Liquid Surface group; fill count; a zeroed `test.value_source` as the solid; hard-coded bounds (`swash_preset.rs:355`); advects the bin-sorted copy, so ids are out of order |
| Clock | `HeldClock` (`R/physics.rs:40`) on a worker; live debt kept in batches of 4 (`R/fluid.rs:64`, `:285`) | `MatterClock` (`R/matter.rs:509`): fixed 60 Hz, live cap 3 ticks, drops the rest and reports it (`:502`, `:534`) | none: two steps of 1/120 s every frame (`swash_preset.rs:104`), paused or not; no Speed; water time = frames × 1/60 s |
| Box3D coupling | `RigidFluidCoupling` (`crates/manifold-fluids/src/coupling/owner.rs:23`): every native substep, body mass inside the PCG (`:144`) | `RigidOwner` + `MatterCoupling` (`R/matter/coupling.rs:43`, `:364`): liquid-first lockstep, fenced reaction readback, GPU body integrator every substep | planned in SWASH P3b: analytic box clip, body held during the solve, reaction applied after |
| Solids | engine face weights from its solid distance field (`crates/manifold-fluids/native/flip_engine/fluidsimulation.cpp:6480`) | distance atlas per shape (`R/matter.rs:120`, `:156`) and body poses (`:168`) | none yet |
| Forces, impulses | `acceleration_field` input (`fluid_surface.rs:108`), impulse hooks | no `acceleration_field`; impulses refused (`matter_domain.rs:501`) | none |
| Whitewater | native foam, bubbles, spray outputs (`fluid_surface.rs:123-128`) | none | none |
| Record / playback | take journal (`R/fluid/take.rs`), `cache_mode` (`fluid_surface.rs:168`) | none | none |
| Extent proof | not needed (CPU) | `matter_buffers_cover_their_dispatch_at_every_resolution` (`R/primitives/matter_extent_tests.rs:42`) | `fft_water_*_cover_every_dispatch` (`R/primitives/swash_extent_tests.rs:361`, `:377`), a different rule shape |

### 1.2 Shared pieces that already exist

- One coupling trait: `StepCoupling`, `SubstepExchange`, `Uncoupled` (`P/stepping.rs:14`, `:30`, `:53`), implemented by FLIP (`R/fluid/coupled/native.rs:314`, `:352`) and MPM. `BodyImpulse` (`P/lib.rs:302`), `apply_impulses` (`:1054`), `TickStamp` (`P/interaction.rs:21`). A coupled Box3D world steps through `advance_worker` from exactly two owners: FLIP's (`R/fluid/coupled/native.rs:115`, `:211`) and MPM's (`R/matter/coupling.rs:69`, `:264`), over `RigidSimulation::advance_with_coupling` (`R/physics.rs:623`, `R/physics/worker.rs:166`).
- Offline mode: `offline_simulation()` (`R/physics.rs:59`), set only by export through `PhysicsStepScope::with_preview_budget` (`app/content_pipeline.rs:2034`). Live recording runs the live policy.
- Substep regions: compile-time and never nested (`R/substeps.rs:11`). A boundary opts into offline host syncs by naming a clock port (`SubstepBoundaryPorts`, `:40-46`; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on), item 12).
- Solid distance: `signed_distance_lattice` (`P/sdf.rs:43`), derived lazily on `PreparedFluidGeometry` (`R/fluid_role.rs:37`, `:68`).
- Domain layout: `domain_layout` (`R/fluid/domain.rs:24`): cells per axis rounded up from Resolution along the longest side, box grown about its centre, no size-multiple rule.
- Liquid predicate: `is_liquid_domain` = FLIP or matter, hard-coded (`core/liquid_domain.rs:10`). The walk `liquid_domain_of` (`R/scene_modifier_expand/acceleration.rs:87`) runs over `FlatSceneIndex` (`R/scene_modifier_expand/index.rs:15`), which uses only manifold-core types; both are `pub(super)` in the renderer, so editing and the app can't call them.
- Load-time type renames: `TYPE_ID_MIGRATIONS` (`core/type_id_migration.rs:219`).
- The added-mass result: holding the body during an explicit pressure exchange gave 16.1× and 23.7× body energy at density ratio 0.1, and halving dt did not help (FLUID_ENGINE_INTEGRATION_PLAN.md P8b; `coupling_partitioned_light_body_rejects_energy_growth`, `crates/manifold-fluids/src/tests/coupling.rs:330`). MPM's light-body proof passes (`crates/manifold-renderer/tests/gpu_proofs/matter_coupling.rs:675`).

### 1.3 Where the scene layer names a solver

`rg -n '"node\.fluid_surface"' crates -g '*.rs' -g '!*tests*' -g '!**/tests/**' -g '!**/examples/**'` gives 51 lines; 32 are production code, the rest inline test modules. Both literals over every `.rs` file: `rg -c '"node\.(fluid_surface|matter_domain)"' crates -g '*.rs'` gives 105 lines in 49 files.

| Kind | Production sites | Becomes |
|---|---|---|
| A — "is this a liquid domain" (10) | `core/scene_exposure.rs:69`, `core/scene_object_migration.rs:31`, `edit/commands/graph/scene/fluid/roles.rs:24`, `RP/physics_impulses.rs:184`, `R/scene_exposure.rs:35`, `:97`, `:399`, `app/fluid_domain_edit.rs:124`, `:234`, `app/ui_bridge/projection/scene.rs:42` | `is_liquid_domain` or a dial-table lookup (P2a) |
| B — FLIP-only meaning (17) | `core/file_loader.rs:81`; `RP/physics_sampling.rs:20`, `:147`, `:198` (history replay); `RP/physics_carry.rs:70`; `R/scene_exposure/fluid_quality.rs:6`; `R/scene_exposure/fluid_objects.rs:14`; `RP/physics_sources.rs:55`, `:65`, `:78`, `:728`; `RP/physics_source_state.rs:303`, `:358`, `:435`, `:445`; `RP/physics_source_chain.rs:18`; `RP/physics_source_runtime.rs:72` | `FLIP_DOMAIN_TYPE_ID` (P2a); takes and caches join the predicate only when a shared bake exists |
| C — its own walk (1) | `R/scene_vm.rs:1234`, FLIP only: the missing MPM water panel | the core walk (P2b) |
| D — Add Fluid (2) | `edit/commands/graph/scene/fluid.rs:23`, `app/ui_bridge/project.rs:619` | `FLIP_DOMAIN_TYPE_ID` (P2a), then the template (P9) |
| E — definitions (2) | `core/liquid_domain.rs:6`, `fluid_surface.rs:41` | stay |

### 1.4 Section 2.5 primitive audit (DECOMPOSING_GENERATORS.md section 2.5 (primitive audit))

Survey: `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g "*.rs"`, plus the MPM water presets read end to end. No `FluidParticle` tick boundary, stats reduction or frame publisher exists; the precedents (`node.matter_state`, `node.matter_stats`, `node.matter_frame`) are typed on the 80-byte `MatterPoint`, and `node.array_feedback` on the 64-byte `Particle`. So `node.liquid_state`, `node.liquid_stats` and `node.liquid_frame` are genuinely new, each shaped like its MPM precedent. `node.matter_solid_distance` already computes walls plus bodies on the solid lattice with nothing MPM-specific: one rename away. Face resampling is genuinely new (two per-element gathers, P10).

## 2. Decisions

**D1 — Seams on existing systems, no solver trait.** The scene, Box3D and the surface already meet a solver through a node type, ports, `StepCoupling` and the particle frame. Each seam gets one shared implementation and a check. Rejected: a `dyn LiquidSolver` trait. Solvers are atom graphs plus one CPU node; a trait would either wrap the graph (a second composition system) or re-expose every seam it claims to hide.

**D2 — One domain node per solver, built from shared pieces.** `node.fluid_surface`, `node.matter_domain`, `node.swash_domain`. What they share (clock, bodies, coupling owner, frame ring, extent checker, conformance table) lives in `R/liquid.rs` and `R/liquid/`. Rejected: one `node.liquid_domain` hosting every solver. It moves solver dials off the node the scene talks to (against MPM D17), rewires every MPM preset, and drags each solver's per-tick rules (MPM's substep bound, its reaction units) into shared code. Rejected: SWASH calling `matter_*` pieces; ownership would stay with MPM and SWASH's I1 forbids it.

**D3 — FLIP conforms as built.** It keeps its worker, `HeldClock`, retained live debt, native coupling, native whitewater and caches. Its only change is literals swapped for the constant and the predicate, behaviour unchanged. Its conformance row lists named exemptions. It publishes no grid.

**D4 — The particle frame stands, with six amendments (section 3.1).** Rejected: letting a solver wire raw state into the surface (today's SWASH): pause, the non-finite gate and the A/B ring would then live per preset.

**D5 — Grid outputs are MAC faces in the FLIP engine's layout, resampled by the producer.** Faces because two of the three solvers are face-native and whitewater's potentials are face-based. The engine's layout because BUG-imy3 feeds the C++ whitewater lifecycle through shared memory, and a matching layout means no copy. The distance field comes from the surface group, not the solvers. Rejected: node velocities as the contract (SWASH would need the face→node bridge its D2 rejects); consumers that switch on a solver's native layout; per-solver distance outputs (MPM has none, SWASH has only cell flags, and the surface already builds the one the look uses).

**D6 — Coupling is liquid-first lockstep at 1/60 s, one reaction per tick (section 3.3).** It is MPM's protocol (GPU_MPM_SOLVER_DESIGN.md section 5 (Coupling protocol), D25–D30) with the solver-specific parts (fixed-point words, the substep bound) left in MPM. FLIP's synchronous exchange meets it as built. Rejected: Box3D stepping inside a GPU solver's substeps (a CPU wait per substep); an owner type per solver.

**D7 — An incompressible solver solves its bodies with the pressure.** Holding the body during the solve and applying the reaction after is the scheme FLIP measured at 16.1× and 23.7× body energy (section 1.2). SWASH P3b's plan is that scheme, so it is amended: each dynamic body adds six unknowns to the Krylov solve (the Jᵀ M⁻¹ J term FLIP's mass-aware PCG uses), proved in `scripts/swash_reference.py` first. A weakly compressible solver (MPM) keeps its per-substep GPU body integrator. Rejected: smaller ticks or more substeps (halving dt did not help); a damping term (it changes the physics feel, which is Peter's call, and hides the error).

**D8 — The reaction carries everything the liquid does at a body boundary:** pressure, projection push-out and boundary friction. Gravity, fields and contacts are Box3D's. One generic proof per coupled solver (section 3.3). Rejected: per-solver coupling proofs; MPM's light-body proof folds into the generic one.

**D9 — One clock for GPU liquids: `LiquidClock`,** which is `MatterClock` moved, plus a `held` flag (section 3.4). Box3D and FLIP keep `HeldClock`. Rejected: `HeldClock` for GPU liquids (its debt batches suit a CPU worker, not a GPU tick region); SWASH's frame-count time.

**D10 — SWASH's tick loop is a substep region, and its Krylov regions nest inside it, depth at most 2.** The outer boundary `node.liquid_state` names the clock port; its body is one SWASH step; count = ticks due × steps per tick; offline host syncs fall between ticks. The Krylov regions stay inside the step with clock `None` (SWASH D10). Fusion never crosses either border. This reopens SWASH D8, which rejected nesting because the compiler forbade it. Rejected: mux-gated fixed step copies (the dispatches still run, and they can't run every due tick offline, go above Speed 1, or host a coupled sync); refusing exports below 60 fps; unrolling the Krylov passes (SWASH D8 rejects it); running the whole frame graph once per tick (the mesher would run per tick).

**D11 — Scene recognition: one list, one walk, one contract (section 3.5).** Rejected: a solver branch at each site; keeping the walk in the renderer, where editing and the app can't reach it (which is how `scene_vm.rs:1234` grew its own FLIP-only walk).

**D12 — Solids reach every solver through the shared distance lattice (section 3.6).** Rejected: SWASH P3b's analytic box clip. It handles boxes only; Peter's scenes carry meshes.

**D13 — Safety rails are contract clauses, each with a check (section 3.7).** Rejected: per-solver extent checkers; two exist today with different rule shapes.

**D14 — Each GPU domain refuses resolutions above its verified maximum, by name.** Matter stays at 64 until BUG-gwe4 (staged GPU check above res 64) closes; SWASH's ceiling is the highest size its P7b ladder proves. Defaulted; Peter can lift it (section 8, call 2).

## 3. The contract

### 3.1 Particle frame

The surface design's section 3 (The particle-frame contract) stands: 32-byte `FluidParticle` (`R/fluid_particles.rs:12`), ports `particles_a/b`, `count_a/b`, `identity_a/b`, `solid_a/b`, `grid_bounds`, `grid_nodes_x/y/z`, `blend`, `span`, display one tick behind (s = target − tick). Amendments:

1. A producer publishes through a frame node (`node.matter_frame`, `node.liquid_frame`), never raw solver state. The frame node owns the A/B ring (`R/liquid/frame_ring.rs`) and holds while the clock is held.
2. A tick with any non-finite position or velocity is never published. The stats node flags it; the frame keeps the last good tick and the domain shows a named error.
3. `solid_*` comes from `node.liquid_solid_distance`: walls plus every collider role and coupled body. No preset wires a constant.
4. Records past `count` have radius 0.
5. Ids are sorted ascending or all 0. A solver that reorders its state each tick (SWASH's bin sort) publishes 0.
6. `grid_bounds` and `grid_nodes_*` come from `domain_layout` over the domain's own bounds and Resolution. No hard-coded box.

### 3.2 Grid outputs

One layout for every solver: MAC faces in the FLIP engine's layout over the `domain_layout` cells (nx, ny, nz), cell size h, box minimum m.

| Array | Length | Index of face (i, j, k) | Sits at |
|---|---|---|---|
| `face_u` | (nx+1)·ny·nz | i + (nx+1)·(j + ny·k) | m + (i, j+½, k+½)·h |
| `face_v` | nx·(ny+1)·nz | i + nx·(j + (ny+1)·k) | m + (i+½, j, k+½)·h |
| `face_w` | nx·ny·(nz+1) | i + nx·(j + ny·k) | m + (i+½, j+½, k)·h |

f32 in m/s, scene space, shared storage. Scalars: `face_cells_x/y/z` and `face_valid_layers` (how many face layers past the liquid carry extrapolated velocity). Published from the frame's last tick, only when wired, held while paused. For MPM the grid escapes its region as a boundary result (`SubstepBoundaryPorts::results`).

The producer resamples; no consumer sees a native layout:
- SWASH: `node.face_sample_component` copies one axis of its `FaceSample` lattice into the array, skipping the padding entries.
- MPM: `node.matter_face_component` averages the four grid nodes around each face centre, after the lattice padding (`R/matter.rs:287`, `:300`).
- FLIP: no grid (D3).

The liquid distance field is not a solver output. The Liquid Surface group already builds it (`R/primitives/particle_volume.rs:54`: distance to the nearest blob, negative inside, capped at a tenth of a bin outside). The group exports it as `level_set` with `level_set_bounds` and `level_set_nodes_x/y/z`. Whitewater owns the one atom that resamples or re-distances it onto the lattice it needs.

### 3.3 Two-way Box3D coupling

Per 1/60 s tick k:

1. The owner (`LiquidRigidOwner`) holds Box3D's settled state at the start of tick k and writes the body rows (`LiquidBody`, 128 bytes) into the liquid's shared buffer.
2. The liquid runs tick k on the GPU with those bodies and accumulates its reaction.
3. The reaction crosses as one `BodyImpulse` per body: linear impulse in N·s and angular impulse in N·m·s about the body's centre of mass, scene space. Each solver decodes its own words through the owner's decode closure (MPM: i32 fixed point, `REACTION_WORDS` = 16, `R/matter.rs:201`).
4. The owner applies it to Box3D tick k exactly once, through `advance_with_coupling`, the only way a coupled Box3D world steps. Box3D adds gravity, fields and contacts.
5. Live never waits. While tick k's reaction is in flight the pair holds: the liquid runs no tick (clock cap 0) and Box3D does not step. Offline, the tick region's host sync waits for the fence between ticks, so every due tick runs.
6. Liquid and bodies share transport, Speed and reset. Different Speeds are refused by name (`matter_domain.rs:835`). A restart of either side restarts both with a new epoch.

Stability (D7): a weakly compressible solver moves bodies on the GPU every substep, under its own substep bound (MPM: `R/matter/coupling.rs:275`). An incompressible solver puts each dynamic body's mass and inertia inside its pressure solve. FLIP meets 3 and 4 synchronously: its native exchange applies the reaction every native substep with body mass in the PCG (`owner.rs:144`).

The proof, `liquid_coupling_collision`, run for every coupled solver: zero gravity, walls out of reach, a liquid blob at 1 m/s strikes a free box at density ratios (box ÷ liquid) 0.1, 1 and 10, over 30 ticks.
- Total momentum (liquid plus bodies) moves by at most 1% of the momentum exchanged.
- Total kinetic energy never exceeds 1.01 × its start.
- Body kinetic energy never exceeds 1.01 × the starting total; the added-mass blow-up shows here first.

With it: `liquid_floating_draft` (a box at half the liquid's density settles within one cell of its analytic draft), `liquid_hydrostatic_lift` (a fixed box under a still pool feels ρgV within 5%; a solver may state tighter), `liquid_free_flight` (a body that never touches the liquid matches uncoupled Box3D bit for bit over 60 ticks).

### 3.4 Clock, pause, speed, reset, export

GPU liquids run on `LiquidClock` (today's `MatterClock`, `R/matter.rs:509`).

| Event | What the liquid does |
|---|---|
| Play | Fixed 60 Hz ticks; target += transport delta × Speed; display one tick behind. |
| Pause or Speed 0 | No ticks; outputs held; `ClockFrame.held` true. A tick already on the GPU completes and publishes. Impulses fired while held are discarded with a receipt; a hit fired before the first tick is due is kept. |
| Speed change | Next frame; must equal the paired Box3D world's Speed. |
| Reset, backward seek, setup change | Restart: new epoch, state reseeds, a coupled Box3D world restarts with it. |
| Forward jump, live | At most 3 ticks per frame (`MAX_LIVE_TICKS`), one tick of jitter debt kept, the rest dropped and reported in `dropped_seconds`. |
| Export (offline) | Every due tick, host syncs between coupled ticks. 30 fps export equals 60 fps at the same transport time. |
| Live recording | Live policy. |
| Cache Record / Playback | FLIP only. GPU liquids have no cache row until a particle-frame bake exists (section 7). |

`held` is true when the target did not advance this frame (pause or Speed 0). The impulse queue needs it to tell a pause (discard) from a coupled hold or a jitter frame with no tick due (keep).

### 3.5 Scene recognition

- One list: `LIQUID_DOMAIN_TYPE_IDS` in `core/liquid_domain.rs`; `is_liquid_domain` reads it. No other file writes a domain type-id literal (I1).
- One walk: `liquid_domain_of` and `FlatSceneIndex` move into manifold-core, so the renderer (forces, pairing, `scene_vm`), editing (roles, Enable Physics) and the app (panels, gizmo) ask the same question.
- One contract: a liquid domain exposes the ports and params the scene uses, under FLIP's names (MPM D17): `role_*`, `acceleration_field`, the impulse hooks, `speed`, `reset`, and its row in `LIQUID_DIAL_PARAMS`. `liquid_domain_scene_contract` checks every listed type. Gaps sit in `LIQUID_SCENE_OWED`, each naming the phase that closes it; the list only shrinks, and the test fails if an owed item is already met.
- Water panel: exposure keys on the predicate plus the type's dial row, replacing the FLIP-only filters (`R/scene_exposure.rs:97`, `core/scene_exposure.rs:69`).
- Enable Physics: refused and hidden on any object whose surface walks to a liquid domain. Today `scene_object_physics_plan` refuses only objects with a fluid role (`edit/commands/graph/scene/physics.rs:1025`).
- Forces: a domain is a force target by having `acceleration_field` (`R/scene_modifier_expand/acceleration.rs:37`).
- Add Fluid: authors the one template `DEFAULT_LIQUID_TEMPLATE` names. No solver dropdown.
- Pairing: one liquid domain and one rigid world per coupled scene (`R/scene_modifier_expand/coupling.rs:85`).

### 3.6 Solids

Every solver reads collider roles and coupled bodies through the same two things: the shared distance lattice (`signed_distance_lattice` via `PreparedFluidGeometry`), packed into one atlas by `pack_distance_atlas`, and the body rows posed by `body_pose_at` / `liquid_body_velocity`. SWASH's face open fractions come from that distance sampled at each face's four corners, the way the FLIP engine gets its face weights from its solid distance field. Solid face velocity is the body's rigid velocity at the face centre. Held-out input for SWASH P3b's gate: an L-shaped mesh collider turned 30°.

### 3.7 Safety rails

1. Extents are proven on the CPU before any GPU dispatch. One checker (`R/liquid/extent.rs`) holds a rule per GPU atom type; a GPU atom without a rule in a liquid preset fails. Every liquid preset is checked at every resolution its domain admits. On the GPU, resolution goes up one size at a time, each after its CPU proof.
2. Each GPU domain refuses resolutions above its verified maximum (D14).
3. Setup problems are refused by name: the error names the control to change. Never a silent clamp.
4. Run-time overflow (particles past capacity, bins, collar lists) is counted on the GPU and surfaced one frame later as a named error with the count. Never silent truncation.
5. A solver that forbids atomics lists its atoms, and the check scans their WGSL.
6. Live frames never wait on the GPU; GPU results are read after a completed fence, or offline.
7. No new `Arc<Mutex>` or `Arc<RwLock>`; readbacks cross through shared storage and fences.
8. Every domain type has a conformance row (`LIQUID_SOLVERS`, test builds only).

### 3.8 Committed signatures

```rust
// core/liquid_domain.rs
pub const FLIP_DOMAIN_TYPE_ID: &str = "node.fluid_surface";
pub const MATTER_DOMAIN_TYPE_ID: &str = "node.matter_domain";
pub const GPU_FLIP_DOMAIN_TYPE_ID: &str = "node.gpu_flip_domain";      // P7a
pub const LIQUID_DOMAIN_TYPE_IDS: &[&str] = &[FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID]; // P7a adds GPU FLIP
pub fn is_liquid_domain(type_id: &str) -> bool;                          // LIQUID_DOMAIN_TYPE_IDS.contains
pub fn liquid_domain_of(index: &FlatSceneIndex, object: &SceneNodeRef)
    -> Result<Option<SceneNodeRef>, SceneIndexError>;                    // body moved unchanged
/// Water-panel params per domain type, FLIP names (MPM D17).
pub const LIQUID_DIAL_PARAMS: &[(&str, &[&str])];

// core/scene_index.rs — moved from R/scene_modifier_expand/index.rs, `pub(super)` → `pub`
pub struct FlatSceneIndex { pub flat: EffectGraphDef,
    pub by_ref: BTreeMap<SceneNodeRef, u32>, pub by_id: BTreeMap<u32, SceneNodeRef> }
impl FlatSceneIndex {
    pub fn build(owner: &EffectGraphDef) -> Result<Self, SceneIndexError>;
    pub fn node(&self, reference: &SceneNodeRef) -> Result<&EffectGraphNode, SceneIndexError>;
    pub fn input(&self, reference: &SceneNodeRef, port: &str)
        -> Result<Option<&EffectGraphWire>, SceneIndexError>;
    // scene_objects: signature kept, error type swapped
}
pub enum SceneIndexError {                     // the renderer maps it variant for variant
    Duplicate { path: String, detail: String }, MissingTarget { path: String, detail: String },
    Invalid { path: String, detail: String }, Capacity { path: String, detail: String },
    Unsupported { path: String, detail: String },
}

// R/liquid/clock.rs — MatterClock moved; advance/set_tick_cap signatures unchanged
pub struct LiquidClock;  pub struct ClockFrame { /* today's fields */ pub held: bool /* P8 */ }
pub const MAX_LIVE_TICKS: u32 = 3;

// R/liquid/bodies.rs — moved from R/matter.rs:82-168 and R/matter/bodies.rs
pub struct LiquidBody;  pub const LIQUID_BODY_SPECS;  pub struct LiquidShape;  pub const LIQUID_SHAPE_SPECS;
pub struct LiquidBodies;  pub enum BodiesStatus;  pub fn pack_distance_atlas;  pub fn body_pose_at;

// R/liquid/coupling.rs
pub struct PendingTick { pub tick: u64, pub stamp: u64 }
pub struct LiquidCoupling;                     // MatterCoupling moved: the one-exchange StepCoupling
impl LiquidRigidOwner {                        // RigidOwner moved; new/matches/geometries/rows kept
    pub fn set_pending(&mut self, pending: PendingTick);
    /// Settle the pending tick if the GPU finished it: decode its reaction, step Box3D
    /// once, and return how many liquid ticks may run this frame (0 or 1).
    pub fn settle(
        &mut self,
        inputs: &RigidSceneInputs,
        complete: impl FnOnce(u64) -> bool,
        decode: impl FnOnce(PendingTick, &[LiquidBody], &mut [BodyImpulse]) -> Result<(), String>,
    ) -> Result<u32, String>;
}
// Stays in MPM: decode, body_limit, body_substep, and ReactionScale { unit, cell_size, offset }.

// R/liquid/frame_ring.rs — the A/B ring of matter_frame.rs:68, used by matter_frame and liquid_frame
pub struct FrameRing;

// R/liquid/extent.rs
pub struct ExtentRule { pub type_id: &'static str, pub check: fn(&AtomExtent) -> Result<(), String> }
pub fn check_preset_extents(def: &EffectGraphDef, resolution: u32) -> Result<(), ExtentError>;
pub const LIQUID_MAX_RESOLUTION: &[(&str, u32)];   // (MATTER_DOMAIN_TYPE_ID, 64)

// R/liquid/conformance.rs — #[cfg(any(test, feature = "gpu-proofs"))] #[doc(hidden)] pub
pub enum Fixture { StillPool, DamBreak, Collision { density_ratio: f32 }, FloatingBox, SubmergedBox }
pub struct LiquidSolverRow {
    pub type_id: &'static str,
    pub fixture: fn(Fixture) -> EffectGraphDef,
    pub gpu: bool,
    pub coupled: bool,
    pub atomic_free: &'static [&'static str],
    pub refusals: &'static [RefusalCase],          // input change → control the error must name
    pub exempt: &'static [(Check, &'static str)],  // closed list, each with its reason
}
pub const LIQUID_SOLVERS: &[LiquidSolverRow];

// R/liquid/grid.rs (P10)
pub const FACE_GRID_PORTS: [&str; 7] =
    ["face_u", "face_v", "face_w", "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"];
```

`AtomExtent` carries an atom's resolved params, its input and output array lengths, and its dispatch grid. P3 derives it the way `matter_extent_tests.rs` does today.

### 3.9 Plausible wrong turns, forbidden by name

- A `dyn LiquidSolver` trait, or one `node.liquid_domain` hosting solvers.
- A new `|| type_id == "node.…"` at any site.
- SWASH importing any `matter_*` item, or MPM importing SWASH's.
- Holding a body fixed in an incompressible solve and applying the reaction afterwards.
- An analytic box clip for solids.
- Mux-gated step copies to fake pause or Speed.
- A solver publishing its own distance field.
- A consumer that branches on which solver made the grid.
- `Arc<Mutex>` for a readback.
- `pub use` aliases for renamed items.
- A reaction in grid units, or torque taken about the box centre instead of the centre of mass.

## 4. Invariants & enforcement

| # | Invariant | Check |
|---|---|---|
| I1 | Liquid type-id literals live in one place | `liquid_type_ids_live_in_one_place` (core test): scans every `.rs` under `crates/`; the literals of `LIQUID_DOMAIN_TYPE_IDS` may appear only in `core/liquid_domain.rs`, `core/type_id_migration.rs` and each domain's own primitive file |
| I2 | Every domain type has a conformance row | `liquid_conformance_covers_every_domain` (CPU) |
| I3 | Every domain meets the scene contract, gaps only shrink | `liquid_domain_scene_contract` with `LIQUID_SCENE_OWED` |
| I4 | A coupled Box3D world steps once per tick, only through its owner | `liquid_coupled_world_steps_once_per_tick` (conformance, coupled rows); negative: `rg -n '\.advance_worker\(' crates/manifold-renderer/src -g '!**/tests/**'` hits only `R/liquid/coupling.rs`, `R/fluid/coupled/native.rs` and the inline tests of `R/physics/worker.rs` |
| I5 | Momentum and energy hold across the boundary | `liquid_coupling_collision`, `liquid_floating_draft`, `liquid_hydrostatic_lift`, `liquid_free_flight` |
| I6 | Pause holds frames and discards impulses | `liquid_pause_holds_frames`, `liquid_pause_discards_impulses` |
| I7 | Export never drops ticks | `liquid_export_frame_rate_independent` (30 fps equals 60 fps) |
| I8 | A non-finite tick is never published | `liquid_nonfinite_tick_not_published` |
| I9 | Every buffer covers every dispatch before the GPU sees it | `liquid_presets_all_extent_checked` |
| I10 | Setup problems refuse by name | `liquid_refusals_name_their_control` (each row's `refusals`) |
| I11 | Overflow is counted and reported | `liquid_overflow_is_reported` |
| I12 | No atomics where a solver forbids them | `liquid_atomic_free_atoms` (scans each listed atom's WGSL for `atomic`) |
| I13 | Live frames never wait on the GPU | `liquid_live_frames_never_wait` (a test-build wait counter on the frame clock stays 0 over 120 live frames) |
| I14 | No new locks | `rg -n 'Arc<(Mutex\|RwLock)' crates/manifold-renderer/src/node_graph/liquid crates/manifold-renderer/src/node_graph/primitives -g '{matter,gpu_flip,liquid}_*.rs'` → zero |
| I15 | Fusion never crosses a region border, nested or not | `nested_region_fusion_stays_inside` (freeze tests) |
| I16 | Grid outputs share one layout | `liquid_face_grid_layout` (a rigid-rotation field through each solver's resample matches CPU-expected at every face) |

Rows I4–I8, I11, I13 and I16 run for every row of `LIQUID_SOLVERS` unless the row names an exemption.

## 5. Phasing

Order: P1 → P2a → P2b and P1 → P3 → P4 on main; P5 → P6 on main, in parallel with P1–P4. SWASH P3, then P7a on `feat/fft-water` once P1, P3, P4 and P6 are merged into it. SWASH P3b starts only after P7a. P7b before SWASH becomes an app instrument (BUG-l2h3 (SWASH to a live instrument), phase 6). P8 after P2b and P4. P9 after P2b. P10 after the BUG-imy3 design is approved. Clippy per phase: `cargo clippy -p <touched> -- -D warnings`.

### P1 — The shared liquid module, out of MPM (seam brief)

- **Entry state:** Peter's go on section 8, call 1. `rg -n 'pub struct MatterClock' crates/manifold-renderer/src/node_graph/matter.rs` and `rg -n 'pub struct RigidOwner' crates/manifold-renderer/src/node_graph/matter/coupling.rs` match. Record the numbers the MPM coupling, scene and bodies proofs print, before touching anything.
- **Read-back:** section 3.3 (Two-way Box3D coupling), section 3.4 (Clock, pause, speed, reset, export), section 3.8 (Committed signatures); GPU_MPM_SOLVER_DESIGN.md section 5 (Coupling protocol) and section 10 (Reuse contract and forbidden moves); `R/matter.rs:397-600`, `R/matter/bodies.rs`, `R/matter/coupling.rs`, `R/primitives/matter_frame.rs` whole.
- **Old → new:**

  | Old | New |
  |---|---|
  | `matter::{MatterClock, ClockFrame, MAX_LIVE_TICKS}` (`R/matter.rs:483-534`) | `liquid::clock::{LiquidClock, ClockFrame, MAX_LIVE_TICKS}` |
  | `matter::{MatterBody, MATTER_BODY_SPECS, MatterShape, MATTER_SHAPE_SPECS, pack_distance_atlas, body_pose_at}` (`R/matter.rs:82-168`) | `liquid::bodies::{LiquidBody, LIQUID_BODY_SPECS, LiquidShape, LIQUID_SHAPE_SPECS, pack_distance_atlas, body_pose_at}` |
  | `matter::bodies::{MatterBodies, BodiesStatus}` (`R/matter/bodies.rs:81`) | `liquid::bodies::{LiquidBodies, BodiesStatus}` |
  | `matter::coupling::{RigidOwner, MatterCoupling}` (`:43`, `:364`) | `liquid::coupling::{LiquidRigidOwner, LiquidCoupling}` |
  | `ReactionSlot { tick, stamp, unit, cell_size, offset }` (`:31`) | `PendingTick { tick, stamp }` in the owner; `matter::coupling::ReactionScale { unit, cell_size, offset }` in MPM |
  | `RigidOwner::settle(inputs, complete, words)` (`:190`), decoding inside | `LiquidRigidOwner::settle(inputs, complete, decode)`; MPM passes a closure over its `decode` (`:214`) |
  | A/B ring inside `matter_frame.rs:68` | `liquid::frame_ring::FrameRing`, used by `matter_frame` |
  | `matter_pose.wgsl`, `matter_collider.wgsl`; `matter_rotate`, `matter_turn`, `matter_body_velocity`, `matter_atlas_half` | `liquid_pose.wgsl`, `liquid_collider.wgsl`; `liquid_rotate`, `liquid_turn`, `liquid_body_velocity`, `liquid_atlas_half` |
  | `node.matter_solid_distance` (`R/primitives/matter_solid_distance.rs`, `shaders/matter_solid_distance_body.wgsl`) | `node.liquid_solid_distance` (`liquid_solid_distance.rs`, `liquid_solid_distance_body.wgsl`) plus a `TYPE_ID_MIGRATIONS` row |

  Stays in MPM: `MatterPoint`, `MatterGridNode`, `MatterLattice`, `PADDING_NODES`, `REACTION_WORDS`, `decode`, `body_limit`, `body_substep`, `ReactionScale`, and everything else in `matter_domain.rs`.
- **Call-site inventory:** 230 matching lines in 30 files at `dfc884568` (15 renderer sources, 8 shaders, 5 files under `crates/manifold-renderer/tests`, `WaterDamBreakMatter.json`, `WaterFloatingBoxMatter.json`). Re-derive, and if the count differs, list the new sites before touching anything:
  `rg -c 'MatterClock|\bClockFrame\b|MAX_LIVE_TICKS|\bMatterBody\b|MATTER_BODY_SPECS|\bMatterShape\b|MATTER_SHAPE_SPECS|\bMatterBodies\b|BodiesStatus|\bRigidOwner\b|\bReactionSlot\b|\bMatterCoupling\b|pack_distance_atlas|body_pose_at|matter_pose\.wgsl|matter_collider\.wgsl|matter_solid_distance|MatterSolidDistance|matter_rotate|matter_turn|matter_body_velocity|matter_atlas_half' crates -g '*.rs' -g '*.json' -g '*.wgsl'`
  All sites are mechanical renames except `settle`'s callers in `matter_domain.rs` (`:655-692`, `:854`), which pass the decode closure. Worked example: `owner.settle(&observation.inputs, |_| true, || reaction_words(Some(reaction)))` → `owner.settle(&observation.inputs, |_| true, |pending, rows, impulses| decode(pending, scale, rows, reaction_words(Some(reaction)), impulses))`.
- **Migration:** compiler-driven: delete the old names first. The two MPM preset JSONs take the new type id; saved projects load through the migration row. The clock tests move with the clock as `liquid_clock_*`.
- **Deliverables:** `R/liquid.rs`, `R/liquid/{clock,bodies,coupling,frame_ring}.rs`, the renamed shaders and node, the migration row; pointer lines in GPU_MPM_SOLVER_DESIGN.md section 13 (Phasing): P3a → this doc's P2a/P2b, P3c → P8, P4b → P9, P6 → BUG-imy3 plus P10, P7 → keys on the predicate, not `MATTER_DOMAIN_TYPE_ID`.
- **Gate:** positive: `cargo nextest run -p manifold-renderer liquid matter`; `cargo nextest run -p manifold-core type_id_migration`; `scripts/gpu_proofs_gate.py` green with the recorded MPM proof numbers unchanged; `cargo run -p manifold-renderer --bin graph-tool -- validate <preset> --kind generator` on both MPM water presets. Negative: the inventory pattern returns zero outside `core/type_id_migration.rs`; `rg -n 'pub use .*[Mm]atter' crates/manifold-renderer/src/node_graph/liquid.rs crates/manifold-renderer/src/node_graph/liquid` returns zero.
- **Demo:** none — L1. Nothing changes on stage.
- **Forbidden:** `pub use` aliases; any changed number (a move that changes behaviour is a broken move); moving MPM's decode, `body_limit` or `body_substep` into the shared module; touching `feat/fft-water`.
- **Test scope:** focused renderer and core; GPU proofs (shaders moved).

### P2a — One list (seam brief)

- **Entry state:** P1 on main. Re-derive both inventories of section 1.3 (Where the scene layer names a solver); if the counts differ from 32 production sites and 105 lines in 49 files, list the new sites first.
- **Read-back:** section 1.3, section 3.5 (Scene recognition); MPM D17.
- **Old → new:** `is_liquid_domain`'s hard-coded `||` → `LIQUID_DOMAIN_TYPE_IDS.contains(&type_id)`. Kind A sites → `is_liquid_domain`, or a `LIQUID_DIAL_PARAMS` lookup where the site filters FLIP params (`core/scene_exposure.rs:69`, `R/scene_exposure.rs:97`); kind B and D sites and every test literal → `FLIP_DOMAIN_TYPE_ID` or `MATTER_DOMAIN_TYPE_ID`. Worked example: `RP/physics_sources.rs:65` `.any(|node| node.type_id == "node.fluid_surface")` → `.any(|node| node.type_id == FLIP_DOMAIN_TYPE_ID)` (kind B, take provenance); `RP/physics_impulses.rs:184` `ImpulseTarget::Fluid => "node.fluid_surface"` → resolve the target node by `is_liquid_domain` (kind A). A JSON fixture embedded in Rust builds its type id from the constant.
- **Deliverables:** the rewrites; `LIQUID_DIAL_PARAMS` with FLIP's current filter as its FLIP row and MPM's dials as its matter row; test `liquid_type_ids_live_in_one_place` (I1).
- **Gate:** positive: I1's test; every `scene_physics_`, `fluid_` and `scene_exposure` test; every `scene-fluid-*` flow on disk passes (count them at entry). Negative: I1.
- **Demo:** none — L1; FLIP behaviour is unchanged by construction, and P2b carries the visible part.
- **Forbidden:** a `||` branch at a site; changing FLIP behaviour; a second list.
- **Test scope:** focused core, editing, renderer, app.

### P2b — One walk and the scene contract (seam brief; supersedes MPM P3a)

- **Entry state:** P2a on main. `rg -n 'pub\(super\) fn liquid_domain_of' crates/manifold-renderer/src/node_graph/scene_modifier_expand/acceleration.rs` and `rg -n 'pub\(super\) struct FlatSceneIndex' crates/manifold-renderer/src/node_graph/scene_modifier_expand/index.rs` match.
- **Read-back:** section 3.5 (Scene recognition), section 3.8 (Committed signatures); `R/scene_vm.rs:1060` and `:1234`; `edit/commands/graph/scene/physics.rs:1025`.
- **Old → new:** `R/scene_modifier_expand/index.rs` `pub(super) struct FlatSceneIndex` and its `SceneModifierExpandError` returns → `core/scene_index.rs` with `SceneIndexError`, and `impl From<SceneIndexError> for SceneModifierExpandError` in the renderer. `acceleration.rs:87` `pub(super) fn liquid_domain_of(...) -> Result<Option<SceneNodeRef>, SceneModifierExpandError>` → `core::liquid_domain::liquid_domain_of(...) -> Result<Option<SceneNodeRef>, SceneIndexError>`. `scene_vm.rs:1234`'s own walk → the core walk. `scene_object_physics_plan` refuses when `liquid_domain_of(object)` is `Some`; the app hides the Enable Physics toggle by the same call. Inventory: `rg -c 'FlatSceneIndex' crates -g '*.rs'` (44 lines in 11 files at `dfc884568`, all under `R/scene_modifier_expand`, all mechanical import changes).
- **Deliverables:** the moves; `liquid_domain_scene_contract` and `LIQUID_SCENE_OWED` (MPM owes `acceleration_field` and the impulse hooks to P8); tests `scene_physics_refuses_enable_physics_on_water`, `scene_vm_traces_matter_domain`; flow `scripts/ui-flows/scene-liquid-recognition.json`.
- **Gate:** positive: the tests; every `scene-fluid-*` and `scene-physics-*` flow on disk (count them). Negative: `rg -n 'fn liquid_domain_of|struct FlatSceneIndex' crates/manifold-renderer` returns zero.
- **Demo:** L3: the flow opens Dam Break Matter as a scene and selects the water; the water panel shows its dials, Enable Physics is absent, the Force target list offers the water; undo, redo, save, reload, check again.
- **Gesture:** click the water in a GPU-liquid scene and turn Speed on the water panel.
- **Forbidden:** a second walk; a renderer copy of the index kept for convenience; changing FLIP behaviour.
- **Test scope:** focused core, editing, renderer, app.

### P3 — Safety rails on the CPU

- **Entry state:** P1 on main. `rg -n 'fn matter_buffers_cover_their_dispatch_at_every_resolution' crates/manifold-renderer/src/node_graph/primitives/matter_extent_tests.rs` matches.
- **Read-back:** section 3.7 (Safety rails); `matter_extent_tests.rs` whole; SWASH's `swash_extent_tests.rs` via `git show origin/feat/fft-water:crates/manifold-renderer/src/node_graph/primitives/swash_extent_tests.rs`; `admit_lattice` (`matter_domain.rs:280`).
- **Deliverables:** `R/liquid/extent.rs` with MPM's rules moved in (`matter_extent_tests.rs` deleted) and `liquid_presets_all_extent_checked`: every preset under `crates/manifold-renderer/assets/generator-presets` holding a liquid domain, at every resolution from the domain's floor to its ceiling; an atom type without a rule fails by name. `LIQUID_MAX_RESOLUTION` and the matter domain's refusal above it, naming Resolution. `R/liquid/conformance.rs` with the MPM and FLIP rows, `liquid_conformance_covers_every_domain` and `liquid_refusals_name_their_control`.
- **Gate:** positive: the tests. Negative: `rg -n 'fn matter_buffers_cover' crates` returns zero.
- **Demo:** L1: the refusal texts, printed by the test, in the phase report.
- **Gesture:** drag Resolution past 64 on Dam Break Matter; the node says which control stopped it and why.
- **Forbidden:** a silent clamp; a per-solver checker; skipping an atom because it looks safe.
- **Test scope:** focused renderer, CPU only.

### P4 — The conformance suite on the GPU

- **Entry state:** P3 on main.
- **Read-back:** section 3.3 (Two-way Box3D coupling), section 3.4 (Clock, pause, speed, reset, export), section 4 (Invariants & enforcement); the proofs being replaced: `tests/gpu_proofs/matter_coupling.rs:596` (`export_frame_rate_independent`), `:675` (`energy_light_body`), `tests/gpu_proofs/matter_scene.rs:680` (`nonfinite_tick_not_published`).
- **Deliverables:** `tests/gpu_proofs/liquid_conformance.rs` running I4–I8, I11, I13 for each GPU row, plus a speed-0.5 check (half the water time) and a reset check (new epoch); the test-build wait counter on the frame clock; the FLIP row runs its CPU checks and lists its exemptions: live debt policy (D3), synchronous coupling (D3), pause-discards-impulses (BUG-xt71 (MIDI impulse during pause lands on resume), FLIP frozen). MPM's impulse checks sit on `LIQUID_SCENE_OWED` until P8. The three MPM proofs above are deleted.
- **Gate:** positive: `scripts/gpu_proofs_gate.py` green; the report lists energy ratio and momentum error per density ratio, draft error and lift error. Negative: `rg -n 'fn (export_frame_rate_independent|energy_light_body|nonfinite_tick_not_published)' crates/manifold-renderer/tests/gpu_proofs` returns zero.
- **Demo:** L1: the proof numbers.
- **Forbidden:** loosening a threshold to pass MPM (escalate instead); per-solver copies of a generic check.
- **Test scope:** GPU proofs (`cargo test`, never nextest).

### P5 — Nested regions: the compiler

- **Entry state:** `rg -n 'never nested' crates/manifold-renderer/src/node_graph/substeps.rs` matches.
- **Read-back:** FREEZE_COMPILER_MAP.md section 4 (The cut rules — when fusion says no) and section 9 (Executor contracts fusion leans on), item 12; `R/substeps.rs` whole; MPM D7; SWASH D8 and D10 on the branch.
- **Deliverables:** a region's body may contain whole regions, depth at most 2. An inner region lies wholly inside one outer body; only an outer region may name a clock; an inner region's escaping outputs feed only its outer body or the outer capture. Compile errors name the NodeIds for partial overlap, depth 3 and a clock on an inner region. Tests: `nested_region_contracts_inner_whole`, `nested_region_rejects_partial_overlap`, `nested_region_rejects_depth_three`, `nested_region_rejects_inner_clock`; every existing substep test unchanged.
- **Gate:** `cargo nextest run -p manifold-renderer substep`.
- **Demo:** none — L1.
- **Forbidden:** flattening the inner region into the outer; host syncs in an inner region; depth 3; running a malformed nest as plain traversal.
- **Test scope:** focused renderer.

### P6 — Nested regions: executor and freeze

- **Entry state:** P5 on main.
- **Read-back:** P5's deliverables; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on), item 12; the executor's region loop.
- **Deliverables:** the executor runs an inner region its count times per outer iteration, each level with its own per-iteration scalars, through the same step evaluator; fused kernels never contain nodes across either border; freeze cache keys include nesting; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on), item 12 updated. GPU proofs on `substeps::test_nodes`: `nested_region_matches_unrolled` (a 3 × 4 nest equals the same nodes unrolled, value level), the fused-vs-unfused proof on a nested body, `nested_region_fusion_stays_inside` (I15), `nested_region_host_sync_only_between_outer_iterations`.
- **Gate:** `scripts/gpu_proofs_gate.py` green; the MPM proofs unchanged.
- **Demo:** none — L1.
- **Forbidden:** a separate executor path for nesting; fusion across a border.
- **Test scope:** GPU proofs.

### P7a — SWASH on the contract (`feat/fft-water`)

- **Entry state:** SWASH P3 built; P1, P3, P4 and P6 merged into the branch from `origin/main`. `rg -n 'fn water_step' crates/manifold-renderer/src/node_graph/primitives/swash_preset.rs` and `rg -n 'liquid_feedback' crates/manifold-renderer/src/node_graph/primitives/swash_preset.rs` match.
- **Read-back:** sections 3.1–3.7 here; the SWASH design's D7, D8, D10 and I6; `swash_preset.rs` whole; `matter_domain.rs` as the shape of a domain node (read, never import).
- **Deliverables:**
  - `node.swash_domain`: params under FLIP's names (Resolution, Speed, Reset, gravity); a `LiquidClock`; roles through `PreparedFluidGeometry`; the extent check before the first dispatch; a named refusal when Resolution gives a lattice other than the preset's baked one (lifted in P7b); refusal of inflow and drain roles by name.
  - `node.liquid_state`: the `FluidParticle` tick boundary with the clock port. Its body is one SWASH step, count = ticks × steps per tick. The two step copies and `node.liquid_feedback` are deleted.
  - `node.liquid_stats` (count, momentum, fastest particle, non-finite flag, overflow count; a barriered reduction, no atomics) and `node.liquid_frame` on `FrameRing`.
  - `node.liquid_solid_distance` feeds the surface's `solid`; `grid_bounds` and nodes come from the domain; the `DAM_MIN` box constants are deleted.
  - `SWASH_DOMAIN_TYPE_ID` in `LIQUID_DOMAIN_TYPE_IDS`; the SWASH conformance row (coupled checks owed to SWASH P3b, its atomic-free list from SWASH D7); its dial row; its extent rules in `liquid::extent` (`swash_extent_tests.rs` deleted).
  - SWASH design amendments: D8 (tick region with the Krylov regions nested, D10 here), P3b (D7 and D12 here: bodies inside the solve, faces from the shared distance lattice, the held-out mesh collider), I6 → I9 here; its Deferred row on nested regions removed.
- **Gate:** positive: the SWASH conformance rows green; SWASH's P3 race numbers (ms per tick, volume drift) within 5% of before, since only the loop moved. Negative: `rg -n 'liquid_feedback|DAM_MIN' crates/manifold-renderer/src` and SWASH's I1 pattern both return zero.
- **Demo:** L2: Dam Break SWASH, 300 frames headless at 60 fps and exported at 30 fps; a scripted diff checks frame 300 at 60 fps against frame 150 at 30 fps (the agent's gate); a paused run's frames are identical. Peter looks at the PNGs.
- **Gesture:** pause mid-wave, then play; the wave carries on from the same crest.
- **Forbidden:** mux-gated step copies; a SWASH-only clock; importing any `matter_*` item; changing the step's numerics.
- **Test scope:** focused renderer; GPU proofs.

### P7b — SWASH's wired lattice (`feat/fft-water`)

- **Entry state:** P7a on the branch.
- **Read-back:** `R/fluid/domain.rs`; `R/execution/array_growth.rs`; `krylov_basis.rs`; the SWASH extent rules.
- **Deliverables:** `domain_layout_snapped(bounds, resolution, multiple)` beside `domain_layout`; SWASH uses the multiple its FFT plans accept (8 expected; ⚠ VERIFY-AT-IMPL in the `fft_3d` plan limits). The lattice is wired from `node.swash_domain` into every SWASH atom; capacities derive from one provided lattice-sized array, re-derived by `array_growth.rs` when Resolution changes; P7a's lattice refusal is deleted. ⚠ VERIFY-AT-IMPL: if array growth can't re-derive through the Krylov region's boundary, stop and escalate; no special path.
- **Gate:** the CPU extent check at every resolution from 32 to the ceiling in steps of the multiple; GPU runs one size at a time (32, 40, 48, 56, 64 and up), each after its CPU proof, each with the conformance rows; the highest green size becomes SWASH's `LIQUID_MAX_RESOLUTION`.
- **Demo:** L2: Dam Break SWASH at each size, PNGs.
- **Gesture:** raise Resolution on the water panel between songs; the water restarts at the new detail.
- **Forbidden:** skipping a size on the GPU; a preset copy per resolution.
- **Test scope:** focused renderer; GPU proofs.

### P8 — Forces and impulses for GPU liquids (supersedes MPM P3c)

- **Entry state:** P2b and P4 on main. `rg -n 'impulses on the live liquid itself are not supported yet' crates/manifold-renderer/src/node_graph/primitives/matter_domain.rs` matches.
- **Read-back:** MPM P3c; FLUID_ENGINE_INTEGRATION_PLAN.md section 5 (Timing, events and lifecycle); `acceleration.rs:37`; the impulse hooks at `R/primitive.rs:400-470`.
- **Deliverables:** `R/liquid/fields.rs`: per-tick force and impulse lattices from the scene's field and the shared event queue, used by every GPU domain. `acceleration_field` and the impulse hooks on `node.matter_domain` (and `node.swash_domain` on the branch). `ClockFrame.held`. An impulse lands on the first tick due after it fires, once, across substeps; a held clock discards it with a receipt. The `matter_domain.rs:501` refusal and MPM's owed entries are deleted. Tests: `liquid_impulse_once_per_tick_across_substeps`, `liquid_force_lattice_matches_field` (CPU-expected), `liquid_pause_discards_impulses` un-owed for MPM.
- **Gate:** the tests; `scene-forces-controls` passes; new flow `scripts/ui-flows/scene-liquid-forces.json`: bind a radial impulse to a clip edge on the Dam Break Matter scene, rebind it to a MIDI-mapped Fire, save, reload, fire (one receipt per fire), pause and fire (a discard receipt, no splash on resume). L3.
- **Gesture:** map a pad to Fire on a radial impulse; the pool splashes on every hit and ignores hits while paused.
- **Forbidden:** per-node CPU field evaluation; a liquid-only force system or trigger router; replaying a paused hit on resume.
- **Test scope:** focused renderer, app; GPU proofs.

### P9 — Add Fluid authors the default liquid template (seam brief; supersedes MPM P4b)

- **Entry state:** P2b on main. `rg -n 'const FLUID_TYPE_ID' crates/manifold-editing/src/commands/graph/scene/fluid.rs` matches.
- **Read-back:** `edit/commands/graph/scene/fluid.rs` whole; `app/ui_bridge/project.rs:619`; GROUPING_GRAPHS.md.
- **Old → new:** `AddSceneFluidCommand` builds `node.fluid_surface` from `FLUID_TYPE_ID` (`fluid.rs:23`) with metadata the app looks up for that type (`project.rs:619`) → the command inserts the template graph the app hands it, and the app resolves it from one constant, `DEFAULT_LIQUID_TEMPLATE` (today's FLIP scene fluid). ⚠ VERIFY-AT-IMPL: the command's `catalog_default` may already carry the graph; if so, the change is deleting `FLUID_TYPE_ID` and building from it.
- **Deliverables:** tests `scene_physics_add_fluid_template_undo_reload` for the FLIP template and for a GPU template (Dam Break Matter's Live Matter and Liquid Surface groups) passed in by the test; flow `scripts/ui-flows/scene-fluid-template.json`. Switching the constant to SWASH is a one-line change on Peter's go (section 8, call 3), not this phase.
- **Gate:** positive: the tests and every `scene-fluid-*` flow. Negative: `rg -n 'FLUID_TYPE_ID' crates/manifold-editing/src` returns zero.
- **Demo:** L3: the flow adds a fluid, plays, moves the source, undoes, redoes, saves, reloads and plays.
- **Gesture:** Add Fluid into a scene and drag the source while it pours.
- **Forbidden:** a solver dropdown; migrating existing FLIP scenes; a second Add command.
- **Test scope:** focused editing, app.

### P10 — Grid outputs (entry: the BUG-imy3 design approved)

- **Entry state:** BUG-imy3's design names the ports it reads: GPU_WHITEWATER_DESIGN.md section 3.2 (What the whitewater reads), which builds this phase as its P1 (Grid outputs). P7a merged to main or the SWASH half waits on the branch.
- **Read-back:** section 3.2 (Grid outputs); docs/ADDING_PRIMITIVES.md (codegen path).
- **Deliverables:** `FACE_GRID_PORTS` on `node.matter_frame` and `node.liquid_frame`; atoms `node.matter_face_component` and `node.face_sample_component` (per-element gathers on the freeze codegen path, value-level `gpu_tests` against CPU-expected, fused-vs-unfused proofs); the Liquid Surface group's `level_set` outputs; `liquid_face_grid_layout` (I16); extent rules for the new arrays.
- **Gate:** `scripts/gpu_proofs_gate.py` green; I9 covers the new arrays.
- **Demo:** L2: a face-speed slice of SWASH and MPM Dam Break at the same tick, side by side.
- **Forbidden:** a consumer that switches on solver; node velocities as the contract; per-solver distance outputs; publishing every tick.
- **Test scope:** focused renderer; GPU proofs.

## 6. Decided — do not reopen

1. Seams on existing systems; no solver trait (D1).
2. One domain node per solver; shared pieces in `R/liquid` (D2).
3. FLIP conforms as built and publishes no grid (D3; Peter, 2026-09-29).
4. The particle frame is the surface design's plus six amendments (D4).
5. Grid outputs are engine-layout MAC faces, resampled by the producer; the distance field comes from the surface group (D5).
6. Liquid-first lockstep, one reaction per tick, Box3D steps only through the owner (D6).
7. No held-body exchange for incompressible solvers (D7).
8. `LiquidClock` for GPU liquids; `HeldClock` for Box3D and FLIP (D9).
9. Nested regions, depth 2, fusion never crosses (D10).
10. One list, one walk, one scene contract (D11).
11. Solids through the shared distance lattice (D12).
12. Named refusals and counted overflow, never clamps or truncation (D13).

## 7. Deferred

| Item | Revives when |
|---|---|
| Particle-frame bake and takes for GPU liquids | the bake talk with Peter settles, BUG-vglg.18 under BUG-vglg (CPU FLIP liquids in scene physics); MPM P7 then keys on the predicate |
| FLIP grid outputs | Peter lifts the FLIP freeze |
| SWASH ids and interpolation | an id-matching consumer, or the surface design's P3 revives |
| Several liquid domains in one coupled scene | a scene needs two liquids around the same bodies |
| Cloth or XPBD with liquid | a cloth solver joins Box3D scenes |
| Nesting deeper than 2 | a solver needs a loop inside its Krylov loop |
| GPU liquid state kept across graph recompiles | a live edit restarts water Peter wanted kept |
| More than one coupled tick per live frame | coupled live scenes drop ticks at 60 fps |
| SWASH inflow and drain roles (refused by name until then) | SWASH P3c, or a scene with a source |
| Vulkan | the Vulkan backend lands (`docs/VULKAN_BACKEND_DESIGN.md`) |

## 8. Calls only Peter makes

1. **P1 edits MPM-owned files before SWASH's P4.** The SWASH design's decided item 1 keeps MPM untouched until P4. The move changes no behaviour and is gated on unchanged proof numbers; without it SWASH P3b must copy MPM's coupling owner or import it. Recommendation: yes, before SWASH P3b.
2. **The resolution ceiling (D14).** Recommendation: yes. It turns a machine lockup into a named refusal; each ceiling lifts as its staged GPU check passes.
3. **Which liquid Add Fluid authors (P9).** FLIP until his SWASH P4 go.
4. **The bake workflow for GPU liquids** (BUG-vglg.18). Recommendation: GPU liquids offer no cache until that talk.
