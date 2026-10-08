# Liquid Solver Seam — the one contract a liquid solver meets to play in a scene

<!-- index: The contract FLIP, GPU MLS-MPM and SWASH meet to join scenes — particle frames, face-grid outputs, Box3D coupling, clock/pause/export, scene recognition, safety rails — and the phases that move MPM and SWASH behind it. -->

**Status:** PROPOSED · 2026-10-01 · P7b, P8, P9 shipped · P13–P16 landed, gates owed · P8 owes L3 flow · P5, P6, P11, P12 retired · P7a unaudited · P10 not built · owed: GPU template lookups, BUG-2xcw (solver-neutral lookups), BUG-o3kj8 (corner lift-off), BUG-u8nqr (FLIP floats high), BUG-28j99 (MPM floats low), BUG-tsdw3 (stack wobble), BUG-yq74i (touching-tick bound), BUG-l21w1 (MPM on the law) · Peter's calls: section 8 (Calls only Peter makes).

**Prerequisites:** none for P1–P6 (MPM coupling is on main). P7a needs GPU FLIP's full step (GPU_FLIP_PRESSURE_SOLVE.md section 1 (the step)). P10 needs the BUG-imy3 (GPU whitewater, solver-agnostic) design approved.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's rule (2026-09-30, relayed by the lead): "APIs, boundaries, solvers and the physics API stay modular and safe to reuse for future solvers, algorithms, interactions and sims."

Three liquid solvers exist. FLIP is the vendored CPU engine (`crates/manifold-fluids`, `node.fluid_surface`). GPU MLS-MPM is on main ([GPU_MPM_SOLVER_DESIGN.md](GPU_MPM_SOLVER_DESIGN.md), "the MPM design", `matter_*`). GPU FLIP is the GPU water solver ([GPU_FLIP_PRESSURE_SOLVE.md](GPU_FLIP_PRESSURE_SOLVE.md), "the GPU FLIP doc"; `node.gpu_flip_domain`). Until 2026-10-01 it was SWASH, with an FFT pressure solve (`docs/archive/FFT_WATER_SOLVER_DESIGN.md`, "the SWASH design"): below, SWASH means GPU FLIP, `swash_*` files are now `gpu_flip_*`, and SWASH P3b is GPU_FLIP_PRESSURE_SOLVE.md section 8 (owed). The particle-frame seam is [GPU_FLUID_SURFACE_DESIGN.md](GPU_FLUID_SURFACE_DESIGN.md) ("the surface design"). This doc is the contract all three meet, the smallest set of seams that does it, and the phases that put MPM and SWASH behind it.

Binding from outside this doc: no FLIP tuning or integration work (Peter, 2026-09-29), so FLIP conforms as built; never a GPU port of FLIP; no fallback modes and no stopgaps; before any GPU run at a new size, prove on the CPU that every buffer covers its dispatch, and step resolution up one size at a time (two forced Mac resets above res 64); SWASH stays a challenger on its branch until Peter's SWASH P4 call.

CPU FLIP retirement (2026-10-07): native scene stepping and `node.fluid_surface` registration are proof-only, with reference fixtures outside the product catalog. Shared clock, coupling, domain, and surface contracts remain. CPU playback/cache/authoring clauses now describe the reference harness. The dependency boundary and saved-project policy are in [FLUID_ENGINE_INTEGRATION_PLAN.md](FLUID_ENGINE_INTEGRATION_PLAN.md).

## What it does on stage

Today each liquid joins the scene its own way. FLIP water takes forces, has a water panel, pauses, records takes, and floats Box3D boxes. MPM water floats boxes but ignores force fields and MIDI hits, and shows no water panel (BUG-4lfm (GPU-surface water not recognised as water)). SWASH water has none of it: it keeps moving while the transport is paused, ignores Speed, and runs at half speed when the frame rate halves.

After this design, any liquid Peter drops into a scene answers his hands the same way. Pause freezes it on the frame. Speed slows it. Reset restarts it together with the boxes floating in it. A 30 fps export matches the 60 fps one tick for tick. Forces and pad hits push it. The water panel shows up. A new solver gets all of that by meeting the contract, never by editing the scene layer.

## 1. Audit — what exists (verified 2026-09-30 at `dfc884568`; SWASH at `79d477fb8`)

Paths: `R/` = `crates/manifold-nodes/src/node_graph/`, `RP/` = `crates/manifold-nodes/src/preset_runtime/`, `P/` = `crates/manifold-physics/src/`, `core/` = `crates/manifold-core/src/`, `edit/` = `crates/manifold-editing/src/`, `app/` = `crates/manifold-app/src/`. SWASH paths are on `origin/feat/fft-water`; main has no SWASH code.

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
- Substep regions: compile-time and never nested (`R/substeps.rs:11`). A boundary opts into host syncs by naming a clock port (`SubstepBoundaryPorts`, `:40-46`; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on), item 12).
- Solid distance: `signed_distance_lattice` (`P/sdf.rs:43`), derived lazily on `PreparedFluidGeometry` (`R/fluid_role.rs:37`, `:68`).
- Domain layout: `domain_layout` (`R/fluid/domain.rs:24`): cells per axis rounded up from Resolution along the longest side, box grown about its centre, no size-multiple rule.
- Liquid predicate: `is_liquid_domain` = FLIP or matter, hard-coded (`core/liquid_domain.rs:10`). The walk `liquid_domain_of` (`R/scene_modifier_expand/acceleration.rs:87`) runs over `FlatSceneIndex` (`R/scene_modifier_expand/index.rs:15`), which uses only manifold-core types; both are `pub(super)` in the renderer, so editing and the app can't call them.
- Load-time type renames: `TYPE_ID_MIGRATIONS` (`core/type_id_migration.rs:219`).
- The added-mass result: holding the body during an explicit pressure exchange gave 16.1× and 23.7× body energy at density ratio 0.1, and halving dt did not help (FLUID_ENGINE_INTEGRATION_PLAN.md P8b; `coupling_partitioned_light_body_rejects_energy_growth`, `crates/manifold-fluids/src/tests/coupling.rs:330`). MPM's light-body proof passes (`crates/manifold-nodes/tests/gpu_proofs/matter_coupling.rs:675`).

### 1.3 Where the scene layer names a solver

Historical pre-retirement inventory: CPU FLIP runtime entries below are now proof-only; `scene_exposure/fluid_quality.rs` was removed. The cache resource classification remains for saved-project asset preservation.

`rg -n '"node\.fluid_surface"' crates -g '*.rs' -g '!*tests*' -g '!**/tests/**' -g '!**/examples/**'` gives 51 lines; 32 are production code, the rest inline test modules. Both literals over every `.rs` file: `rg -c '"node\.(fluid_surface|matter_domain)"' crates -g '*.rs'` gives 105 lines in 49 files.

| Kind | Production sites | Becomes |
|---|---|---|
| A — "is this a liquid domain" (10) | `core/scene_exposure.rs:69`, `core/scene_object_migration.rs:31`, `edit/commands/graph/scene/fluid/roles.rs:24`, `RP/physics_impulses.rs:184`, `R/scene_exposure.rs:35`, `:97`, `:399`, `app/fluid_domain_edit.rs:124`, `:234`, `app/ui_bridge/projection/scene.rs:42` | `is_liquid_domain` or a dial-table lookup (P2a) |
| B — FLIP-only meaning (17) | `core/file_loader.rs:81`; `RP/physics_sampling.rs:20`, `:147`, `:198` (history replay); `RP/physics_carry.rs:70`; `R/scene_exposure/fluid_quality.rs:6`; `R/scene_exposure/fluid_objects.rs:14`; `RP/physics_sources.rs:55`, `:65`, `:78`, `:728`; `RP/physics_source_state.rs:303`, `:358`, `:435`, `:445`; `RP/physics_source_chain.rs:18`; `RP/physics_source_runtime.rs:72` | `FLIP_DOMAIN_TYPE_ID` (P2a); takes and caches join the predicate only when a shared bake exists |
| C — its own walk (1) | `R/scene_vm.rs:1234`, FLIP only: the missing MPM water panel | the core walk (P2b) |
| D — Add Fluid (2) | `edit/commands/graph/scene/fluid.rs:23`, `app/ui_bridge/project.rs:619` | `FLIP_DOMAIN_TYPE_ID` (P2a), then the template (P9) |
| E — definitions (2) | `core/liquid_domain.rs:6`, `fluid_surface.rs:41` | stay |

### 1.4 Section 2.5 primitive audit (DECOMPOSING_GENERATORS.md section 2.5 (primitive audit))

Survey: `rg 'purpose: "' crates/manifold-node-engine/src/{primitives,water/primitives}/ crates/manifold-nodes-{image,scene}/src/node_graph/primitives/ crates/manifold-nodes/src/node_graph/primitives/ -g "*.rs"`, plus the MPM water presets read end to end. No `FluidParticle` tick boundary, stats reduction or frame publisher exists; the precedents (`node.matter_state`, `node.matter_stats`, `node.matter_frame`) are typed on the 80-byte `MatterPoint`, and `node.array_feedback` on the 64-byte `Particle`. So `node.liquid_state`, `node.liquid_stats` and `node.liquid_frame` are genuinely new, each shaped like its MPM precedent. `node.matter_solid_distance` already computes walls plus bodies on the solid lattice with nothing MPM-specific: one rename away. Face resampling is genuinely new (two per-element gathers, P10).

## 2. Decisions

**D1 — Seams on existing systems, no solver trait.** The scene, Box3D and the surface already meet a solver through a node type, ports, `StepCoupling` and the particle frame. Each seam gets one shared implementation and a check. Rejected: a `dyn LiquidSolver` trait. Solvers are atom graphs plus one CPU node; a trait would either wrap the graph (a second composition system) or re-expose every seam it claims to hide.

**D2 — One domain node per solver, built from shared pieces.** `node.fluid_surface`, `node.matter_domain`, `node.swash_domain`. What they share (clock, bodies, coupling owner, frame ring, extent checker, conformance table) lives in `R/liquid.rs` and `R/liquid/`. Rejected: one `node.liquid_domain` hosting every solver. It moves solver dials off the node the scene talks to (against MPM D17), rewires every MPM preset, and drags each solver's per-tick rules (MPM's substep bound, its reaction units) into shared code. Rejected: SWASH calling `matter_*` pieces; ownership would stay with MPM and SWASH's I1 forbids it.

**D3 — FLIP conforms as built.** It keeps its worker, `HeldClock`, retained live debt, native coupling, native whitewater and caches. Its only change is literals swapped for the constant and the predicate, behaviour unchanged. Its conformance row lists named exemptions. It publishes no grid.

**D4 — The particle frame stands, with six amendments (section 3.1).** Rejected: letting a solver wire raw state into the surface (today's SWASH): pause, the non-finite gate and the A/B ring would then live per preset.

**D5 — Grid outputs are MAC faces in the FLIP engine's layout, resampled by the producer.** Faces because two of the three solvers are face-native and whitewater's potentials are face-based. The engine's layout because BUG-imy3 feeds the C++ whitewater lifecycle through shared memory, and a matching layout means no copy. The distance field comes from the surface group, not the solvers. Rejected: node velocities as the contract (SWASH would need the face→node bridge its D2 rejects); consumers that switch on a solver's native layout; per-solver distance outputs (MPM has none, SWASH has only cell flags, and the surface already builds the one the look uses).

**BUG-215v amendment (Peter approved 2026-10-03):** GPU FLIP whitewater consumes the solver's per-step cell-centred particle φ inside the liquid tick region. This supersedes D5's surface-only distance restriction for that consumer; see GPU_WHITEWATER_DESIGN.md D3/D5. Pool, counters, IDs and rendering populations cross the liquid boundary as captured `results`.

**D6 — Coupling is liquid-first lockstep at 1/60 s, one reaction per tick (section 3.3).** It is MPM's protocol (GPU_MPM_SOLVER_DESIGN.md section 5 (Coupling protocol), D25–D30) with the solver-specific parts (fixed-point words, the substep bound) left in MPM. FLIP's synchronous exchange meets it as built. Rejected: Box3D stepping inside a GPU solver's substeps (a CPU wait per substep); an owner type per solver.

**D7 — An incompressible solver solves its bodies with the pressure.** Holding the body during the solve and applying the reaction after is the scheme FLIP measured at 16.1× and 23.7× body energy (section 1.2). SWASH P3b's plan is that scheme, so it is amended: each dynamic body adds six unknowns to the Krylov solve (the Jᵀ M⁻¹ J term FLIP's mass-aware PCG uses), proved in `scripts/swash_reference.py` first. A weakly compressible solver (MPM) keeps its per-substep GPU body integrator. Rejected: smaller ticks or more substeps (halving dt did not help); a damping term (it changes the physics feel, which is Peter's call, and hides the error).

**D8 — The reaction carries everything the liquid does at a body boundary:** pressure, projection push-out and boundary friction. Gravity, fields and contacts are Box3D's. One generic proof per coupled solver (section 3.3). Rejected: per-solver coupling proofs; MPM's light-body proof folds into the generic one.

**D9 — One clock for GPU liquids: `LiquidClock`,** which is `MatterClock` moved, plus a `held` flag (section 3.4). Box3D and FLIP keep `HeldClock`. Rejected: `HeldClock` for GPU liquids (its debt batches suit a CPU worker, not a GPU tick region); SWASH's frame-count time.

**D10 — SWASH's tick loop is a substep region.** The boundary `node.liquid_state` names the clock port; its body is one step; count = ticks due × steps per tick; host syncs fall between ticks. Fusion never crosses the border. Amended 2026-10-01: the pressure solve's loops now run inside `node.gpu_flip_step` (GPU_FLIP_PRESSURE_SOLVE.md section 1.1 (stage design)), so nothing nests, and the compiler refuses a boundary inside another region's body. Rejected: mux-gated fixed step copies (the dispatches still run, and they can't run every due tick offline, go above Speed 1, or host a coupled sync); refusing exports below 60 fps; unrolling the Krylov passes (SWASH D8 rejects it); running the whole frame graph once per tick (the mesher would run per tick).

**D11 — Scene recognition: one list, one walk, one contract (section 3.5).** Rejected: a solver branch at each site; keeping the walk in the renderer, where editing and the app can't reach it (which is how `scene_vm.rs:1234` grew its own FLIP-only walk).

**D12 — Solids reach every solver through the shared distance lattice (section 3.6).** Rejected: SWASH P3b's analytic box clip. It handles boxes only; Peter's scenes carry meshes.

**D13 — Safety rails are contract clauses, each with a check (section 3.7).** Rejected: per-solver extent checkers; two exist today with different rule shapes.

**D14 — Each GPU domain refuses resolutions above its verified maximum, by name.** Matter stays at 64 until BUG-gwe4 (staged GPU check above res 64) closes; SWASH's ceiling is the highest size its P7b ladder proves. Defaulted; Peter can lift it (section 8, call 2).

### 2.1 Body handoff amendment (Peter approved 2026-10-06)

Scope: GPU FLIP and MPM, the live liquids. Measured 2026-10-06 at `f7e7888b6` (headless `frame-time`, live particle count per tick, BUG-beblk (water drains under a light moving box)): water is deleted only where a dynamic box touches the floor or a wall (a still box at density 2000 on the floor loses 429, 717, 904 particles/s at 60, 30, 15 Hz Sim Rate; a Fixed box loses none). A density-100 box in open water never settles: 0.14, 0.23, 0.54 m/s of motion above the gravity term at 60, 30, 15 Hz (BUG-a8qyy (light coupled boxes never settle in still water)). The cause in code: Box3D gets the reaction as an impulse before its gravity substeps (`liquid/coupling.rs:485-490`), while the water poses bodies at p + v·t (`liquid/bodies.rs:118`, `gpu_flip_step.wgsl:772`, `gpu_flip_bodies.wgsl:179-182`, `gpu_flip_clock.wgsl:283-284`, `keep_whitewater_body.wgsl:62`, `liquid_solid_distance_body.wgsl:93`, `whitewater_obstacle_source_body.wgsl:63`) and moves them at v + a·t + reaction (`gpu_flip_step.wgsl:839`), blind to contacts (`manifold-physics/src/lib.rs:296`). For a floating box that leaves a drift of g·dt·(N−1)/(2N) per tick, 0.06, 0.14, 0.31 m/s at 60, 30, 15 Hz, with N = `box3d_substep_count(dt)`. Inside-solid deletion (`gpu_flip_step.wgsl:2255`, `:2718`) then removes water the mispredicted box overlaps. No test caught it: GPU FLIP is exempt from all of `liquid_coupling_collision` (`liquid/conformance.rs:448`) though its reason covers only momentum, and no test covers resting contact or light floaters.

**D15 — One body motion law inside a tick, owned by `manifold-physics`.** GPU FLIP and the owner place and move a coupled dynamic body by `coupled_state_at` (section 3.8) and its WGSL twin in `liquid_pose.wgsl`. With h = dt/N, a the body's external acceleration, P(t) the reaction impulse the liquid has accumulated by time t, and Δv(t) = a·t + P(t)/m after D16: v(t) = v0 + Δv(t), x(t) = x0 + v0·t + ½·Δv(t)·(t + h). Angular: ω(t) = ω0 + α·t + I⁻¹·L(t), and the rotation vector ω0·t + ½·(ω(t) − ω0)·(t + h) turns q0 in the world frame. Under D17, with no contact, damping, motion lock or speed cap acting, x(dt) and v(dt) equal Box3D's end state exactly (symplectic Euler over N substeps of constant total acceleration gives x0 + v0·dt + ½·ā·dt·(dt + h)), however P grew; fields enter both through the same start state. Inside the tick it is a prediction, and angular is approximate (Box3D's gyroscopic step); D18 measures both. A tick Box3D splits into several steps (fast-body microsteps, impulse-event segments) ends at h_eff = Σh_k²/dt rather than h; the law keeps h = dt/N(dt) and D18 reports the gap. Prescribed bodies (a = 0, P = 0) reduce to p + v·t. MPM keeps its per-substep body move (D7), which already tracks when its reaction lands; it shares the handoff (D17) and the check (D18), and moving it onto this law waits on proofs (section 7). Rejected: p + v·t (today); a predictor per incompressible solver (how this bug class was born); timing moments in the handoff (Box3D would replay each liquid's own schedule).

**D16 — Supports shape the prediction and hold the body in the solve (Peter, 2026-10-06).** At tick start the owner reads each coupled body's touching contact points with static and kinematic shapes, and with dynamic bodies below it, from Box3D (`b3Body_GetContactData`, `PhysicsWorld::support_points`): lever arm, normal out of the support, friction as the world mixes it, the support's velocity and spin there, and the point's patch (its Box3D manifold) with the patch centre and the support's velocity at it. Up to 16 a body; every support's first point comes before any second, and overflow is logged. The law applies Box3D's contact rule with rigid contacts (`b3SolveContacts_Mesh`; Catto's sequential impulses), patch by patch: each point's one-sided normal, twist friction about the normal inside μ·Σ(arm·λn), then sliding friction at the patch centre inside the circle μ·Σλn. It runs first on the tick-start velocity, so a closing body stops at once as in Box3D's first substep, then on the known increment. It is a predictor, not Box3D: no restitution, softness, speculative points or substeps. Measured against Box3D on a floor (`coupled_motion_matches_box3d_on_a_floor`): inside D18 from the second tick of a slide; the first tick of a slide from rest slips about 1.3 cm/s further in Box3D at any rate, its soft contacts giving as the load shifts. The supports the result stays on (closing slower than 1 mm/s; a patch stuck or unturned while its friction is inside its limit) are held through each pressure solve: the body's response there is M_c = (L·P)(L·P)ᵀ, with M⁻¹ = L·Lᵀ and P the projector off the held rows in the mass metric. M_c is fixed for the whole solve, so the operator stays symmetric and positive semidefinite. Box3D still gets the full reaction and owns every real contact. Measured 2026-10-06: the solve's free mobility pushed a resting box into its floor and drained 38% / 22% of the water at 15 / 30 Hz; holding the body there drained none. Rejected: the first cut's normals-only linear projection with a free solve (that drain); a mobility clipped per product (not symmetric); contact forces solved inside the liquid, as Monolith does (Takahashi and Batty 2020: a second coupling style); last tick's measured acceleration (impacts become phantom bounces). A dynamic body counts as a support only where the contact normal into this body points against gravity, so a stack is read bottom up and each body stands on the one below, which moves with its own law (shock propagation, Guendelman, Bridson and Fedkiw 2003); side and overhead contacts between dynamic bodies stay Box3D's alone. Not yet: MPM's own move, which rebuilds displacement from reaction-history moments, so an endpoint projection alone would be wrong there (BUG-l21w1 (MPM on the law)).

**D17 — One handoff: the reaction is a steady force over the tick.** The owner queues J/dt and L/dt on each body before the tick's one Box3D step; Box3D clears forces after each step. Still one reaction per tick, applied once (D6). A new liquid only has to accumulate its reaction. Buoyancy equal to weight now nets zero on every substep. Rejected: the impulse before the step (today's drift).

**D18 — Handover agreement is checked every tick.** After Box3D settles, the owner compares its end pose and velocity with the law at dt under the retired reaction. Live: GPU FLIP's domain publishes the worst body as its `handover_position`, `handover_velocity` and `handover_rotation` outputs; the owner logs over bound, never clamped, for every row. MPM publishes none until BUG-l21w1 (MPM on the law) puts its bodies on the law. Conformance asserts the bound. The law is exact for a free body and a rigid-contact predictor on a touching one; the bound for touching ticks is Peter's call (BUG-yq74i (touching-tick handover bound)).

**D19 — Water is never deleted at a solid (Peter, 2026-10-06).** A particle inside a solid moves out along the distance gradient to free space, the standard push-out (Bridson, *Fluid Simulation for Computer Graphics*). The check runs whatever the particle's travel, and the end point is checked against every body and the walls. If no free point is found, the particle stays where it is and the existing refused push-out count (`push_refused`) records it. Rejected: deleting overlapped water (today); a tolerance band that hides overlap.

**D20 — Conformance exemptions are per assertion.** `Check::Collision` splits into `CollisionMomentum` and `CollisionEnergy`; GPU FLIP keeps only the momentum exemption. An exemption is for a check that cannot apply to a row. A logged bug that fails part of a check the row runs lists only the failing misses as known red; the check still runs, and a known red that stops failing fails it (the strict expected-failure the glTF conformance manifest uses).

## 3. The contract

### 3.1 Particle frame

The surface design's section 3 (The particle-frame contract) stands: 32-byte `FluidParticle` (`R/fluid_particles.rs:12`), ports `particles_a/b`, `count_a/b`, `identity_a/b`, `solid_a/b`, `grid_bounds`, `grid_nodes_x/y/z`, `blend`, `span`, display one tick behind (s = target − tick). Amendments:

1. A producer publishes through a frame node (`node.matter_frame`, `node.liquid_frame`), never raw solver state. The frame node owns the A/B ring (`R/liquid/frame_ring.rs`) and holds while the clock is held.
2. A tick with any non-finite position or velocity is never published. The stats node flags it; the frame keeps the last good tick and the domain shows a named error. The BUG-7qzk clock implementation reseeds live GPU FLIP particles on the next retired fault without resetting its epoch/time; offline and other solver fault policy remains unchanged. The lead must run `liquid_nonfinite_live_flip_reseeds_without_stopping_clock` before this recovery is considered verified (see LIVE_SIM_CLOCK_DESIGN.md section 9). Narrow-band reseed capacity shortage (stats word 27) keeps the last good particles, faces and interior and halts until Reset.
3. `solid_*` comes from `node.liquid_solid_distance`: walls plus every collider role and coupled body. No preset wires a constant.
4. Records past `count` have radius 0.
5. GPU FLIP publishes a compact copy with strictly increasing nonzero birth IDs, a cleared tail, and each accepted frame's count, time and identity epoch; its working state stays cell-sorted. Reset, growth or identity renumbering collapses A onto B. The all-zero ID path remains for producers without persistent identity; see [BUG-upao pass 2](GPU_FLUID_SURFACE_DESIGN.md#bug-upao--pass-2-and-sim-rate-2026-10-03).
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

The visible surface distance remains a rendering output, exported as `level_set` with its bounds and node counts. GPU FLIP also publishes its particle distance for per-tick whitewater (BUG-215v); whitewater owns resampling or re-distancing onto its lattice.

The Ferstl et al. (2016) narrow-band amendment in `GPU_FLIP_NARROW_BAND_DESIGN.md` permits optional solver interior distance because deep liquid has no particles. `gpu_flip_step.interior` holds exactly nx·ny·nz f32 distances in metres, x fastest, at m+(i+½,j+½,k+½)h. `liquid_state.interior_in` captures it beside the tick particles, and `liquid_frame.interior` publishes `interior_a/b` through the same ring indices, epoch, lattice and failed-tick gate. A disabled field is positive everywhere. The mesher accepts optional `interior`, samples the cell-centred lattice and unions the particle field with interior+h before solid/border constraints. Unwired consumers retain their existing particle path; no solver-specific branch is needed. Stats contain 28 words: the 18-word solver tail preserves separating-floor diagnostics at words 17–26 and appends narrow-band reseed shortages at word 27. Coarse solve stage 3 retains its standalone reference-proven boundary scatter and local projection; Solve Level integration remains stage 4.

Accepted numerical substeps (BUG-g75v.7): a producer may also publish
`substep_schedule` (four f32 words per row: seconds, elapsed endpoint, bitcast
one-shot impulse-index/valid-bit, reserved zero), `substep_u/v/w` (concatenated
MAC arrays in the layout above), and `substep_count`. Duration zero denotes an
inactive encoded slot. The event high bit marks a hit; remaining bits index the
domain impulse lattices. Face data is valid only for positive-duration rows;
consumers must skip inactive rows before reading their grids. GPU FLIP leaves
inactive grids untouched and dispatches their gathers with zero workgroups.
This is a solver-neutral consumer seam; a producer
adapts its private scheduler/velocity layout before publication. Whitewater
never reads a GPU FLIP private face record or scheduler structure.

### 3.3 Two-way Box3D coupling

Per accepted interval k (nominal 1/60 s in export):

1. The owner (`LiquidRigidOwner`) holds Box3D's settled state at the start of tick k and writes the body rows (`LiquidBody`, 128 bytes) into the liquid's shared buffer.
2. The liquid runs tick k on the GPU with those bodies and accumulates its reaction.
3. The reaction crosses as one `BodyImpulse` per body: linear impulse in N·s and angular impulse in N·m·s about the body's centre of mass, scene space. Each solver decodes its own words through the owner's decode closure (MPM: i32 fixed point, `REACTION_WORDS` = 16, `R/matter.rs:201`). Within the tick, GPU FLIP places and moves each coupled body by the shared motion law (D15, D16) with its own accumulated reaction; MPM by its per-substep move (D7, D16).
4. The owner queues it on Box3D tick k exactly once, as a steady force and torque over the tick's one step (D17), through `advance_with_coupling`, the only way a coupled Box3D world steps. Box3D adds gravity, fields and contacts. The owner then checks the handover (D18).
5. BUG-7qzk replaces the live tick allowance with one full accepted interval from `manifold_physics::clock`. `PendingTick` carries its endpoints; retired reaction settlement advances Box3D over that same interval. Numerical FLIP substeps use reference CFL, with the last allowed substep taking the remainder. The 2026-10-04 ruling replaces the stretched interval with at most two fixed Sim Rate steps a frame, leftover time dropped (LIVE_SIM_CLOCK_DESIGN.md section 8 (Resolved decisions)). Offline retains every nominal tick and its existing host exchanges. Runtime and verification status: [LIVE_SIM_CLOCK_DESIGN.md](LIVE_SIM_CLOCK_DESIGN.md#9-current-implementation-seam-and-outstanding-work).
6. Liquid and bodies share transport, Speed and reset. Different Speeds are refused by name (`matter_domain.rs:835`). A restart of either side restarts both with a new epoch.

Stability (D7): a weakly compressible solver moves bodies on the GPU every substep, under its own substep bound (MPM: `R/matter/coupling.rs:275`). An incompressible solver puts each dynamic body's mass and inertia inside its pressure solve. FLIP meets 3 and 4 synchronously: its native exchange applies the reaction every native substep with body mass in the PCG (`owner.rs:144`).

The proof, `liquid_coupling_collision`, run for every coupled solver: zero gravity, walls out of reach, a liquid blob at 1 m/s strikes a free box at density ratios (box ÷ liquid) 0.1, 1 and 10, over 30 ticks.
- Total momentum (liquid plus bodies) moves by at most 1% of the momentum exchanged.
- Total kinetic energy never exceeds 1.01 × its start.
- Body kinetic energy never exceeds 1.01 × the starting total; the added-mass blow-up shows here first.

With it: `liquid_floating_draft` (a box at half the liquid's density settles within one cell of its analytic draft), `liquid_hydrostatic_lift` (a fixed box under a still pool feels ρgV within 5%; a solver may state tighter), `liquid_free_flight` (a body that never touches the liquid matches uncoupled Box3D bit for bit over 60 ticks).

Body handoff proofs (section 2.1 (Body handoff amendment)), every coupled row. Each runs a tick every frame, and floating rest also asserts water in motion after the drop and the water's push (the body's velocity change beyond gravity) holding the box up, so a frozen or stalled run cannot pass. Body motion is RMS tick-to-tick motion, linear and rotational, plus drift as the centre's end-to-end displacement over the window, never a signed average. "No water removed" means the liquid's summed mass never drops in these closed tanks; each also asserts no particle is left inside a solid (D19's refused count). A miss a logged bug keeps red on a row is listed by name in the conformance table (`known_red`): the proof still runs, every other miss still fails it, and a listed miss that stops failing fails it too.
- `liquid_floating_rest`: boxes at 0.05 and 0.5 of the liquid's density, let go 5 cm above where they float, at 15, 30 and 60 Hz. Over the last 3 s of 7: RMS motion under 1 cm/s, drift under 1 cm, centre within a cell of analytic, the water's push within 30% of the box's weight, no water removed.
- `liquid_resting_contact`: a box at twice the liquid's density flat on the floor against a wall under 1 m of water, at 15 and 30 Hz. Over 10 s: RMS motion under 1 mm/s, no tick over 5 mm/s (Box3D's soft contact leaves sub-millimetre jitter; a visible twitch is cm/s), no water removed.
- `liquid_lift_off`: a box at 0.3 of the liquid's density resting on one bottom edge under 1 m of water, at 30 Hz, leaves the floor within 1 s and reaches the surface by 4 s, no water removed. On one edge because a box flat on the floor leaves no cell for water under it, so it gets no lift on any grid.
- `liquid_submerged_stack`: three boxes at 1.5 times the liquid's density stacked under water, at 30 Hz. Over 8 s no body faster than 2 m/s; over the last 2 s each body's RMS motion under 1 cm/s; no water removed. This one is the evidence for section 7's repeated swaps.
- `liquid_handover_agreement`: on floating rest and lift-off, the D18 error stays under 0.5 mm, 5 mm/s and 0.1° per tick.

### 3.4 Clock, pause, speed, reset, export

All live solvers use the physics-layer interval contract. GPU liquids retain the `LiquidClock` import as an alias for `SimulationClock`; it has no independent time-dropping policy.

| Event | What the liquid does |
|---|---|
| Play | Accept the whole transport span × Speed; numerical subdivisions do not change the accepted endpoint. |
| Pause or Speed 0 | No ticks; outputs held; `ClockFrame.held` true. A tick already on the GPU completes and publishes. Impulses fired while held are discarded with a receipt; a hit fired before the first tick is due is kept. |
| Speed change | Applies from the interval after the frame that sees it; must equal the paired Box3D world's Speed. |
| Reset, backward seek, setup change | Restart: new epoch, state reseeds, a coupled Box3D world restarts with it. |
| Forward jump, live | Cover the complete interval. `dropped_seconds` remains a compatibility output fixed at zero; cap hits are explicit HUD warnings. |
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
- Add Fluid: authors GPU FLIP only, from `gpu_flip_liquid_template` (app `ui_bridge/project.rs`), which wraps `gpu_flip_liquid_body` (`R/primitives/gpu_flip_preset.rs`) with card exposures. The body comes from the same builder the shipped GPU FLIP preset is checked against. No solver dropdown, no fallback to another solver.
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

// R/liquid/clock.rs — compatibility import; the physics layer owns timing.
pub use manifold_physics::clock::{SimulationClock as LiquidClock, ClockFrame};

// R/liquid/bodies.rs — moved from R/matter.rs:82-168 and R/matter/bodies.rs
pub struct LiquidBody;  pub const LIQUID_BODY_SPECS;  pub struct LiquidShape;  pub const LIQUID_SHAPE_SPECS;
pub struct LiquidBodies;  pub enum BodiesStatus;  pub fn pack_distance_atlas;  pub fn body_pose_at;

// R/liquid/coupling.rs
pub struct PendingTick { pub tick: u64, pub stamp: u64, pub interval: StepInterval, pub offline: bool }
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
    pub known_red: &'static [KnownRed],            // { check, miss, reason }: D20
}
pub const LIQUID_SOLVERS: &[LiquidSolverRow];

// R/liquid/grid.rs (P10)
pub const FACE_GRID_PORTS: [&str; 7] =
    ["face_u", "face_v", "face_w", "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"];

// manifold-physics/src/coupled_motion.rs (D15)
pub struct CoupledStart {
    pub position: [f32; 3],                 // centre of mass
    pub rotation: [f32; 4],
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
    pub inverse_mass: f32,
    pub inverse_inertia: [[f32; 3]; 3],     // world, row-major
    pub linear_acceleration: [f32; 3],      // Box3D's external acceleration
    pub angular_acceleration: [f32; 3],
}
pub struct SupportPoint { pub lever: [f32; 3], pub normal: [f32; 3], pub friction: f32, pub support_velocity: [f32; 3], pub support_spin: f32, pub patch: u32, pub patch_lever: [f32; 3], pub patch_velocity: [f32; 3] }
pub struct Held { pub closed: u32, pub stuck: u32, pub unturned: u32 }  // bit i: support point i; stuck and unturned per patch
pub struct CoupledState { pub position: [f32; 3], pub rotation: [f32; 4], pub linear_velocity: [f32; 3], pub angular_velocity: [f32; 3], pub held: Held }
pub type Mobility = [f32; 21];  // 6 × 6 symmetric, upper triangle by rows (mobility_index)
pub fn coupled_substep(dt: Seconds) -> f32;  // h = dt / box3d_substep_count(dt)
pub fn coupled_state_at(start: &CoupledStart, supports: &[SupportPoint], linear_push: [f32; 3], angular_push: [f32; 3], t: f32, h: f32) -> CoupledState;
pub fn constrained_mobility(start: &CoupledStart, supports: &[SupportPoint], held: Held) -> Mobility; // D16

// manifold-physics/src/lib.rs
impl PhysicsWorld {
    pub fn support_points(&self, body: BodyHandle, out: &mut [SupportPoint]) -> Result<SupportCount, PhysicsError>; // D16: touching static or kinematic contact points
    pub fn queue_reaction_over_step(&mut self, reactions: &[BodyImpulse], dt: Seconds) -> Result<(), PhysicsError>; // D17
}

// R/liquid/coupling.rs (D18): worst body this tick, published as the domain's handover_* outputs
pub struct HandoverError { pub position: f32, pub velocity: f32, pub rotation: f32 }
```

WGSL twin (D15): `liquid_pose.wgsl` gains `liquid_body_state(...) -> LiquidBodyState`, taking the row's fields one by one (the row struct is declared per shader). GPU FLIP evaluates it once per step: `pose_bodies` (`gpu_flip_step.wgsl`) poses every body of the tick at the step's end into a `posed` row array, and the solid distance, `closest_body`, `solid_face_velocity` and the body passes' lever arms (`gpu_flip_bodies.wgsl`) read those rows with no time added. The reaction it reads is what the water had put on the body when the step began, so the pose lags this step's own pressure by one step. h rides `Params.coupled_h`. The support points ride a `contacts` array of 80 vec4 per body row, five a point (`BodySupports`), tick major beside the rows (`LiquidBodies::set_contacts`, the domain's `contacts` output); the 128-byte `LiquidBody` row keeps its layout. `pose_bodies` also writes each body's packed `liquid_constrained_mobility` (six vec4 a body), which `impulse_finalize` uses in place of M⁻¹. The CFL clock keeps its own speed bound (v0 + a·t + P/m before projection, an upper bound). The published surface's solid and the whitewater obstacle source still pose raw rows at p + v·t.

`AtomExtent` carries an atom's resolved params, its input and output array lengths, and its dispatch grid. P3 derives it the way `matter_extent_tests.rs` does today.

### 3.9 Plausible wrong turns, forbidden by name

- A `dyn LiquidSolver` trait, or one `node.liquid_domain` hosting solvers.
- A new `|| type_id == "node.…"` at any site.
- SWASH importing any `matter_*` item, or MPM importing SWASH's.
- Holding a body fixed in an incompressible solve and applying the reaction afterwards.
- An analytic box clip for solids.
- Mux-gated step copies to fake pause or Speed.
- A solver publishing a distance field outside the optional narrow-band interior contract in section 3.2.
- A consumer that branches on which solver made the grid.
- `Arc<Mutex>` for a readback.
- `pub use` aliases for renamed items.
- A reaction in grid units, or torque taken about the box centre instead of the centre of mass.
- A body pose advanced from a velocity anywhere but `coupled_state_at`, its WGSL twin and MPM's body move.
- Damping, velocity clamps or sleep thresholds to hide body jitter; a smaller tick or more substeps as the fix.
- Deleting water at a solid.
- An exemption whose reason covers fewer assertions than the check it exempts.
- A second coupling style (contacts solved inside one liquid's solve) without Peter's call.

### 3.10 Block occupancy map (BUG-1z1p (shared block occupancy map))

**Retired 2026-10-02.** The map and both its uses are deleted. As surface_crossings' only input it cost more than it saved (P11 numbers below). Inside the GPU FLIP step, a block skip built from sort occupancy was bit-exact but saved 0.2 to 0.5 ms of a 30 ms frame at Resolution 64, inside frame noise. The frame is dominated by the pressure solve's dispatch count, the subject of the liquid speed phase bead. Revisit only if a profile shows per-cell work, not dispatch count, as the cost.

## 4. Invariants & enforcement

| # | Invariant | Check |
|---|---|---|
| I1 | Liquid type-id literals live in one place | `liquid_type_ids_live_in_one_place` (core test): scans every `.rs` under `crates/`; the literals of `LIQUID_DOMAIN_TYPE_IDS` may appear only in `core/liquid_domain.rs`, `core/type_id_migration.rs` and each domain's own primitive file |
| I2 | Every domain type has a conformance row | `liquid_conformance_covers_every_domain` (CPU) |
| I3 | Every domain meets the scene contract, gaps only shrink | `liquid_domain_scene_contract` with `LIQUID_SCENE_OWED` |
| I4 | A coupled Box3D world steps once per tick, only through its owner | `liquid_coupled_world_steps_once_per_tick` (conformance, coupled rows); negative: `rg -n '\.advance_worker\(' crates/manifold-nodes/src -g '!**/tests/**'` hits only `R/liquid/coupling.rs`, `R/fluid/coupled/native.rs` and the inline tests of `R/physics/worker.rs` |
| I5 | Momentum and energy hold across the boundary | `liquid_coupling_collision`, `liquid_floating_draft`, `liquid_hydrostatic_lift`, `liquid_free_flight` |
| I6 | Pause holds frames and discards impulses | `liquid_pause_holds_frames`, `liquid_pause_discards_impulses` |
| I7 | Export never drops ticks | `liquid_export_frame_rate_independent` (30 fps equals 60 fps) |
| I8 | A non-finite tick is never published | `liquid_nonfinite_tick_not_published` |
| I9 | Every buffer covers every dispatch before the GPU sees it | `liquid_presets_all_extent_checked` |
| I10 | Setup problems refuse by name | `liquid_refusals_name_their_control` (each row's `refusals`) |
| I11 | Overflow is counted and reported | `liquid_overflow_is_reported` |
| I12 | No atomics where a solver forbids them | `liquid_atomic_free_atoms` (scans each listed atom's WGSL for `atomic`) |
| I13 | Uncoupled live frames never wait; coupled live frames use bounded per-tick exchanges | `liquid_live_frames_never_wait` (uncoupled); `liquid_coupled_live_frame_rate` (24 fps and 60 fps tick states) |
| I14 | No new locks | `rg -n 'Arc<(Mutex\|RwLock)' crates/manifold-node-engine/src/water/liquid crates/manifold-nodes/src/node_graph/primitives -g '{matter,gpu_flip,liquid}_*.rs'` → zero |
| I15 | Fusion never crosses a region border; regions never nest | `substeps_freeze_never_fuses_across_border`, `substeps_region_nested_boundary_rejected` |
| I16 | Grid outputs share one layout | `liquid_face_grid_layout` (a rigid-rotation field through each solver's resample matches CPU-expected at every face) |
| I17 | The motion law matches Box3D | `coupled_motion_matches_box3d` (manifold-physics, CPU): a free body under gravity and a steady force in a real Box3D world at 15, 30 and 60 Hz; end position and velocity within 1e-5 relative; the angular case under its stated bound |
| I18 | One home for body poses | `liquid_body_state_matches_cpu`: the WGSL twin against `coupled_state_at`, value for value; review holds every site in section 2.1 to it |
| I19 | The handover agrees | `liquid_handover_agreement` |
| I20 | Bodies rest and water stays | `liquid_floating_rest`, `liquid_resting_contact`, `liquid_lift_off`, `liquid_submerged_stack` |

Rows I4–I8, I11, I13 and I16 run for every row of `LIQUID_SOLVERS` unless the row names an exemption.

## 5. Phasing

Order: P1 → P2a → P2b and P1 → P3 → P4 on main; P5 → P6 on main, in parallel with P1–P4. SWASH P3, then P7a on `feat/fft-water` once P1, P3, P4 and P6 are merged into it. SWASH P3b starts only after P7a. P7b before SWASH becomes an app instrument (BUG-l2h3 (SWASH to a live instrument), phase 6). P8 after P2b and P4. P9 after P2b. P10 after the BUG-imy3 design is approved. Clippy per phase: `cargo clippy -p <touched> -- -D warnings`.

### P1 — The shared liquid module, out of MPM (seam brief)

- **Entry state:** Peter's go on section 8, call 1. `rg -n 'pub struct MatterClock' crates/manifold-node-engine/src/water/matter.rs` and `rg -n 'pub struct RigidOwner' crates/manifold-node-engine/src/water/matter/coupling.rs` match. Record the numbers the MPM coupling, scene and bodies proofs print, before touching anything.
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
- **Call-site inventory:** 230 matching lines in 30 files at `dfc884568` (15 renderer sources, 8 shaders, 5 files under `crates/manifold-nodes/tests`, `WaterDamBreakMatter.json`, `WaterFloatingBoxMatter.json`). Re-derive, and if the count differs, list the new sites before touching anything:
  `rg -c 'MatterClock|\bClockFrame\b|MAX_LIVE_TICKS|\bMatterBody\b|MATTER_BODY_SPECS|\bMatterShape\b|MATTER_SHAPE_SPECS|\bMatterBodies\b|BodiesStatus|\bRigidOwner\b|\bReactionSlot\b|\bMatterCoupling\b|pack_distance_atlas|body_pose_at|matter_pose\.wgsl|matter_collider\.wgsl|matter_solid_distance|MatterSolidDistance|matter_rotate|matter_turn|matter_body_velocity|matter_atlas_half' crates -g '*.rs' -g '*.json' -g '*.wgsl'`
  All sites are mechanical renames except `settle`'s callers in `matter_domain.rs` (`:655-692`, `:854`), which pass the decode closure. Worked example: `owner.settle(&observation.inputs, |_| true, || reaction_words(Some(reaction)))` → `owner.settle(&observation.inputs, |_| true, |pending, rows, impulses| decode(pending, scale, rows, reaction_words(Some(reaction)), impulses))`.
- **Migration:** compiler-driven: delete the old names first. The two MPM preset JSONs take the new type id; saved projects load through the migration row. The clock tests move with the clock as `liquid_clock_*`.
- **Deliverables:** `R/liquid.rs`, `R/liquid/{clock,bodies,coupling,frame_ring}.rs`, the renamed shaders and node, the migration row; pointer lines in GPU_MPM_SOLVER_DESIGN.md section 13 (Phasing): P3a → this doc's P2a/P2b, P3c → P8, P4b → P9, P6 → BUG-imy3 plus P10, P7 → keys on the predicate, not `MATTER_DOMAIN_TYPE_ID`.
- **Gate:** positive: `cargo nextest run -p manifold-nodes liquid matter`; `cargo nextest run -p manifold-core type_id_migration`; `scripts/gpu_proofs_gate.py` green with the recorded MPM proof numbers unchanged; `cargo run -p manifold-app --bin graph-tool -- validate <preset> --kind generator` on both MPM water presets. Negative: the inventory pattern returns zero outside `core/type_id_migration.rs`; `rg -n 'pub use .*[Mm]atter' crates/manifold-node-engine/src/water/liquid.rs crates/manifold-node-engine/src/water/liquid` returns zero.
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

- **Entry state:** P2a on main. `rg -n 'pub\(super\) fn liquid_domain_of' crates/manifold-node-engine/src/load/expand/acceleration.rs` and `rg -n 'pub\(super\) struct FlatSceneIndex' crates/manifold-core/src/scene_index.rs` match.
- **Read-back:** section 3.5 (Scene recognition), section 3.8 (Committed signatures); `R/scene_vm.rs:1060` and `:1234`; `edit/commands/graph/scene/physics.rs:1025`.
- **Old → new:** `R/scene_modifier_expand/index.rs` `pub(super) struct FlatSceneIndex` and its `SceneModifierExpandError` returns → `core/scene_index.rs` with `SceneIndexError`, and `impl From<SceneIndexError> for SceneModifierExpandError` in the renderer. `acceleration.rs:87` `pub(super) fn liquid_domain_of(...) -> Result<Option<SceneNodeRef>, SceneModifierExpandError>` → `core::liquid_domain::liquid_domain_of(...) -> Result<Option<SceneNodeRef>, SceneIndexError>`. `scene_vm.rs:1234`'s own walk → the core walk. `scene_object_physics_plan` refuses when `liquid_domain_of(object)` is `Some`; the app hides the Enable Physics toggle by the same call. Inventory: `rg -c 'FlatSceneIndex' crates -g '*.rs'` (44 lines in 11 files at `dfc884568`, all under `R/scene_modifier_expand`, all mechanical import changes).
- **Deliverables:** the moves; `liquid_domain_scene_contract` and `LIQUID_SCENE_OWED` (MPM owes `acceleration_field` and the impulse hooks to P8); tests `scene_physics_refuses_enable_physics_on_water`, `scene_vm_traces_matter_domain`; flow `scripts/ui-flows/scene-liquid-recognition.json`.
- **Gate:** positive: the tests; every `scene-fluid-*` and `scene-physics-*` flow on disk (count them). Negative: `rg -n 'fn liquid_domain_of|struct FlatSceneIndex' crates/manifold-nodes` returns zero.
- **Demo:** L3: the flow opens Dam Break Matter as a scene and selects the water; the water panel shows its dials, Enable Physics is absent, the Force target list offers the water; undo, redo, save, reload, check again.
- **Gesture:** click the water in a GPU-liquid scene and turn Speed on the water panel.
- **Forbidden:** a second walk; a renderer copy of the index kept for convenience; changing FLIP behaviour.
- **Test scope:** focused core, editing, renderer, app.

### P3 — Safety rails on the CPU

- **Entry state:** P1 on main. `rg -n 'fn matter_buffers_cover_their_dispatch_at_every_resolution' crates/manifold-nodes/src/node_graph/primitives/matter_extent_tests.rs` matches.
- **Read-back:** section 3.7 (Safety rails); `matter_extent_tests.rs` whole; SWASH's `swash_extent_tests.rs` via `git show origin/feat/fft-water:crates/manifold-nodes/src/node_graph/primitives/swash_extent_tests.rs`; `admit_lattice` (`matter_domain.rs:280`).
- **Deliverables:** `R/liquid/extent.rs` with MPM's rules moved in (`matter_extent_tests.rs` deleted) and `liquid_presets_all_extent_checked`: every preset under `crates/manifold-nodes/assets/generator-presets` holding a liquid domain, at every resolution from the domain's floor to its ceiling; an atom type without a rule fails by name. `LIQUID_MAX_RESOLUTION` and the matter domain's refusal above it, naming Resolution. `R/liquid/conformance.rs` with the MPM and FLIP rows, `liquid_conformance_covers_every_domain` and `liquid_refusals_name_their_control`.
- **Gate:** positive: the tests. Negative: `rg -n 'fn matter_buffers_cover' crates` returns zero.
- **Demo:** L1: the refusal texts, printed by the test, in the phase report.
- **Gesture:** drag Resolution past 64 on Dam Break Matter; the node says which control stopped it and why.
- **Forbidden:** a silent clamp; a per-solver checker; skipping an atom because it looks safe.
- **Test scope:** focused renderer, CPU only.

### P4 — The conformance suite on the GPU

- **Entry state:** P3 on main.
- **Read-back:** section 3.3 (Two-way Box3D coupling), section 3.4 (Clock, pause, speed, reset, export), section 4 (Invariants & enforcement); the proofs being replaced: `tests/gpu_proofs/matter_coupling.rs:596` (`export_frame_rate_independent`), `:675` (`energy_light_body`), `tests/gpu_proofs/matter_scene.rs:680` (`nonfinite_tick_not_published`).
- **Deliverables:** `tests/gpu_proofs/liquid_conformance.rs` running I4–I8, I11, I13 for each GPU row, plus a speed-0.5 check (half the water time) and a reset check (new epoch); the test-build wait counter on the frame clock; the FLIP row runs its CPU checks and lists its exemptions: live debt policy (D3), synchronous coupling (D3), pause-discards-impulses (BUG-xt71 (MIDI impulse during pause lands on resume), FLIP frozen). MPM's impulse checks sit on `LIQUID_SCENE_OWED` until P8. The three MPM proofs above are deleted.
- **Gate:** positive: `scripts/gpu_proofs_gate.py` green; the report lists energy ratio and momentum error per density ratio, draft error and lift error. Negative: `rg -n 'fn (export_frame_rate_independent|energy_light_body|nonfinite_tick_not_published)' crates/manifold-nodes/tests/gpu_proofs` returns zero.
- **Demo:** L1: the proof numbers.
- **Forbidden:** loosening a threshold to pass MPM (escalate instead); per-solver copies of a generic check.
- **Test scope:** GPU proofs (`cargo test`, never nextest).

### P5 — Nested regions: the compiler

**Retired 2026-10-01, with P6.** Nesting's only user was the pressure solve's Krylov loops, which now run inside `node.gpu_flip_step`. The nesting support was deleted; a boundary inside another region's body is a compile error (`substeps_region_nested_boundary_rejected`).

- **Entry state:** `rg -n 'never nested' crates/manifold-node-engine/src/exec/substeps.rs` matches.
- **Read-back:** FREEZE_COMPILER_MAP.md section 4 (The cut rules — when fusion says no) and section 9 (Executor contracts fusion leans on), item 12; `R/substeps.rs` whole; MPM D7; SWASH D8 and D10 on the branch.
- **Deliverables:** a region's body may contain whole regions, depth at most 2. An inner region lies wholly inside one outer body; only an outer region may name a clock; an inner region's escaping outputs feed only its outer body or the outer capture. Compile errors name the NodeIds for partial overlap, depth 3 and a clock on an inner region. Tests: `nested_region_contracts_inner_whole`, `nested_region_rejects_partial_overlap`, `nested_region_rejects_depth_three`, `nested_region_rejects_inner_clock`; every existing substep test unchanged.
- **Gate:** `cargo nextest run -p manifold-nodes substep`.
- **Demo:** none — L1.
- **Forbidden:** flattening the inner region into the outer; host syncs in an inner region; depth 3; running a malformed nest as plain traversal.
- **Test scope:** focused renderer.

### P6 — Nested regions: executor and freeze

**Retired 2026-10-01** (see P5).

- **Entry state:** P5 on main.
- **Read-back:** P5's deliverables; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on), item 12; the executor's region loop.
- **Deliverables:** the executor runs an inner region its count times per outer iteration, each level with its own per-iteration scalars, through the same step evaluator; fused kernels never contain nodes across either border; freeze cache keys include nesting; FREEZE_COMPILER_MAP.md section 9 (Executor contracts fusion leans on), item 12 updated. GPU proofs on `substeps::test_nodes`: `nested_region_matches_unrolled` (a 3 × 4 nest equals the same nodes unrolled, value level), the fused-vs-unfused proof on a nested body, `nested_region_fusion_stays_inside` (I15), `nested_region_host_sync_only_between_outer_iterations`.
- **Gate:** `scripts/gpu_proofs_gate.py` green; the MPM proofs unchanged.
- **Demo:** none — L1.
- **Forbidden:** a separate executor path for nesting; fusion across a border.
- **Test scope:** GPU proofs.

### P7a — SWASH on the contract (`feat/fft-water`)

- **Entry state:** SWASH P3 built; P1, P3, P4 and P6 merged into the branch from `origin/main`. `rg -n 'fn water_step' crates/manifold-nodes/src/node_graph/primitives/swash_preset.rs` and `rg -n 'liquid_feedback' crates/manifold-nodes/src/node_graph/primitives/swash_preset.rs` match.
- **Read-back:** sections 3.1–3.7 here; the SWASH design's D7, D8, D10 and I6; `swash_preset.rs` whole; `matter_domain.rs` as the shape of a domain node (read, never import).
- **Deliverables:**
  - `node.swash_domain`: params under FLIP's names (Resolution, Speed, Reset, gravity); a `LiquidClock`; roles through `PreparedFluidGeometry`; the extent check before the first dispatch; a named refusal when Resolution gives a lattice other than the preset's baked one (lifted in P7b); refusal of inflow and drain roles by name.
  - `node.liquid_state`: the `FluidParticle` tick boundary with the clock port. Its body is one SWASH step, count = ticks × steps per tick. The two step copies and `node.liquid_feedback` are deleted.
  - `node.liquid_stats` (count, momentum, fastest particle, non-finite flag, overflow count; a barriered reduction, no atomics) and `node.liquid_frame` on `FrameRing`.
  - `node.liquid_solid_distance` feeds the surface's `solid`; `grid_bounds` and nodes come from the domain; the `DAM_MIN` box constants are deleted.
  - `SWASH_DOMAIN_TYPE_ID` in `LIQUID_DOMAIN_TYPE_IDS`; the SWASH conformance row (coupled checks owed to SWASH P3b, its atomic-free list from SWASH D7); its dial row; its extent rules in `liquid::extent` (`swash_extent_tests.rs` deleted).
  - SWASH design amendments: D8 (tick region with the Krylov regions nested, D10 here), P3b (D7 and D12 here: bodies inside the solve, faces from the shared distance lattice, the held-out mesh collider), I6 → I9 here; its Deferred row on nested regions removed.
- **Gate:** positive: the SWASH conformance rows green; SWASH's P3 race numbers (ms per tick, volume drift) within 5% of before, since only the loop moved. Negative: `rg -n 'liquid_feedback|DAM_MIN' crates/manifold-nodes/src` and SWASH's I1 pattern both return zero.
- **Demo:** L2: Dam Break SWASH, 300 frames headless at 60 fps and exported at 30 fps; a scripted diff checks frame 300 at 60 fps against frame 150 at 30 fps (the agent's gate); a paused run's frames are identical. Peter looks at the PNGs.
- **Gesture:** pause mid-wave, then play; the wave carries on from the same crest.
- **Forbidden:** mux-gated step copies; a SWASH-only clock; importing any `matter_*` item; changing the step's numerics.
- **Test scope:** focused renderer; GPU proofs.

### P7b — GPU FLIP's Resolution knob (`feat/gpu-flip-liquid-fields`)

**As built (2026-10-01, by GPU_FLIP_PRESSURE_SOLVE.md section 1.1 (stage design)).** The step and its solver read the lattice at run time, so Resolution applies on change at any side from 1 to 1024 with no graph rebuild. The V-cycle depth follows the lattice down to 4³ and solves that level exactly, which supersedes the fixed five levels and the multiple-of-16 refusal below. Proof: `gpu_flip_resolution_card_resizes_at_runtime` (64 → 32 → 100, a step and whitewater frame at each).

- **Entry state:** P7a built. `rg -n 'its lattice cannot change yet' crates/manifold-node-engine/src/water/primitives/gpu_flip_domain.rs` matches.
- **Read-back:** `gpu_flip_preset.rs` whole (`water_def`, `render_def`, `Builder::lattice`, `lattice_box`, the V-cycle builders); `gpu_flip_domain.rs` (`MULTIGRID_LEVELS`, `multigrid_refusal`); `R/fluid/domain.rs`; `R/array_growth.rs`; the GPU FLIP extent rules in `R/liquid/extent.rs`.
- **Decided (Peter, 2026-10-01; supersedes BUG-86kv (GPU FLIP Resolution knob) option (c) and closes BUG-znja (where the water graph is rebuilt)):** Resolution is a plain param that applies on change, with no graph rebuild and no new mechanism. The V-cycle has a fixed 5 levels at every lattice (`MULTIGRID_LEVELS`), so a lattice side must be a multiple of 16, refused by name otherwise. The coarsest level is 2³ at 32, 4³ at 64 and 8³ at 128; it is smoothed by 16 red-black rounds each way (`COARSE_SWEEPS`) instead of solved exactly. Measured with `scripts/mgpcg_reference.py --depth 5 --coarse-sweeps 16`, the conjugate gradient iterations to a 1e-5 residual match the exact coarse solve at every size: Dam Break 7–8 at 32, 64 and 128; deep pool 6 at 32 and 64, 7 at 128. Fewer rounds cost the deep pool one more iteration (2 rounds at 64, 4 or 8 at 128). So the fixed depth does not cap Resolution across 32–128.
- **What still fixes the graph to one lattice:** every lattice atom holds `nodes_x/y/z`, `cell_size` and `lattice_min_*` as build params, the vector atoms hold `row_length`, and array capacities are planned from those params; `array_growth` regrows only CPU-origin arrays. The face extension band is lattice-dependent too: `band_layers` is 3 at 32, 4 at 64 and 6 at 128 `node.extend_faces` copies. Until these are lifted, `built_resolution` and its refusal stay.
- **Deliverables (BUG-o65k (GPU FLIP lattice wiring)):** the lattice wired from `node.gpu_flip_domain`'s outputs into every lattice atom and `row_length`; GPU-origin array capacities re-derived from the wired lattice when it changes; the extension band fixed at the ceiling's count; `built_resolution` and its refusal deleted; an end-to-end water panel test on the GPU FLIP Dam Break (the panel walk is already solver-neutral through `is_liquid_domain` and the GPU FLIP dial row).
- **Gate:** the CPU extent check at every multiple of 16 from 32 to the ceiling; GPU runs one size at a time upward from 32, each after its CPU proof, each with the conformance rows; the highest green size becomes GPU FLIP's refusal ceiling (D14).
- **Demo:** L2: GPU FLIP Dam Break at each size, PNGs.
- **Gesture:** raise Resolution on the water panel between songs; the water restarts at the new detail.
- **Forbidden:** skipping a size on the GPU; a preset copy per resolution; a graph rebuild on a Resolution change.
- **Test scope:** focused renderer; GPU proofs.

### P8 — Forces and impulses for GPU liquids (supersedes MPM P3c)

- **Entry state:** P2b and P4 on main. `rg -n 'impulses on the live liquid itself are not supported yet' crates/manifold-node-engine/src/water/primitives/matter_domain.rs` matches.
- **Read-back:** MPM P3c; FLUID_ENGINE_INTEGRATION_PLAN.md section 5 (Timing, events and lifecycle); `acceleration.rs:37`; the impulse hooks at `R/primitive.rs:400-470`.
- **Deliverables:** `R/liquid/fields.rs`: per-tick force and impulse lattices from the scene's field and the shared event queue, used by every GPU domain. `acceleration_field` and the impulse hooks on `node.matter_domain` (and `node.swash_domain` on the branch). `ClockFrame.held`. An impulse lands on the first tick due after it fires, once, across substeps; a held clock discards it with a receipt. The `matter_domain.rs:501` refusal and MPM's owed entries are deleted. Tests: `liquid_impulse_once_per_tick_across_substeps`, `liquid_force_lattice_matches_field` (CPU-expected), `liquid_pause_discards_impulses` un-owed for MPM.
- **Gate:** the tests; `scene-forces-controls` passes; new flow `scripts/ui-flows/scene-liquid-forces.json`: bind a radial impulse to a clip edge on the Dam Break Matter scene, rebind it to a MIDI-mapped Fire, save, reload, fire (one receipt per fire), pause and fire (a discard receipt, no splash on resume). L3.
- **Gesture:** map a pad to Fire on a radial impulse; the pool splashes on every hit and ignores hits while paused.
- **Forbidden:** per-node CPU field evaluation; a liquid-only force system or trigger router; replaying a paused hit on resume.
- **Test scope:** focused renderer, app; GPU proofs.
- **As built (the API a second domain calls):** `LiquidImpulses` owns the queue on the liquid's clock: `observe_frame(transport, &ClockFrame)` after the clock advances, then the four impulse hooks map to `stamp`, `enqueue` (a held clock discards at once, with a receipt), `drain_applied` and `drain_discarded`. A hit reads "applied" only after the domain calls `commit_frame` once its fields are on the GPU; a frame that fails or holds calls `abandon_frame` (drains do it as a backstop), so its hits come back discarded, never applied. Forces are evaluated per tick, never per display frame (BUG-8tl0 (liquid forces per tick)). Before each frame the physics history replay asks the domain for the transport times its coming ticks start at (`request_physics_samples` → `LiquidFields::request_samples`, from `LiquidClock::tick_start`, which maps tick k through Speed) and runs the field's authored ancestry at exactly those times; the domain records each tick's field (`observe_sample`). `LiquidFields::prepare(lattice, field, &LiquidClock, &ClockFrame, &impulses)` lays those out on a coarse lattice (`FieldLattice::of`, one node per 4 cells, covering the solver lattice): one lattice while the ticks agree, one per tick otherwise. A tick nobody sampled is an error, never a guess. Tick k reads the same forces at 24, 30 and 60 fps, live and exported, at any Speed (`liquid_forces_per_tick_match_across_frame_rates`, `liquid_forces_per_tick_follow_simulation_speed`). When a live frame drops time the owed tick's start moves into the past; it reads that frame's evaluation, one tick late. A drop re-anchors the tick-to-transport map at that frame, so the skipped time never becomes lasting lag: every tick reads a time less than two ticks before the previous frame, however long the stall (`liquid_forces_per_tick_survive_late_frames`). Graph-authored modulation (LFOs, curves) is exact per tick; host-fed audio modulation is per hop (below). `upload` writes the impulse lattice and the force lattices to the GPU; the domain publishes `forces`, `impulses` and the `FieldFrame` scalars (`field_nodes_x/y/z`, `field_spacing`, `force_lattices`, `impulse_tick`), and atoms pick their tick's lattice from `tick_index` and the domain's `first_tick` (`liquid_field_force_base`). Atoms read them with `LIQUID_FIELD` (`liquid_field.wgsl`, CPU twin `FieldLattice::sample`) and `FieldBinding::read`; MPM adds the force to gravity in `node.matter_grid_update` and the impulse on the impulse tick's first substep, and `node.matter_body_reaction` repeats both. GPU FLIP does the same (BUG-70l4 (wire GPU FLIP to liquid fields)): `node.gpu_flip_domain` owns a `LiquidImpulses` and a `LiquidFields` whose lattice covers the face grid from the box min (`GpuFlipGeometry::field_lattice`), and the step's `face_gravity` pass adds the force to gravity on each face and the impulse on the impulse tick's first step. GPU FLIP has no body owner yet, so it refuses an impulse aimed at a rigid body; the solids phase plugs its owner into the same hooks.
- **Host-fed modulation per tick (BUG-2jx6 (host-fed modulation sampled per liquid tick)).** An audio kick driving a liquid force lands on the tick it happened in, not the display frame that delivered it.
  - *One thread, no new channel.* Modulation (`advance_audio_hops`, `compose_retained_controls`) and the generator's `PresetRuntime` both run on the content thread. The per-hop values ride on the generator's `PresetInstance` the host already hands the runtime each frame (`set_physics_source_instance`).
  - *Record.* The hop walk already advances every stateful shaper (attack/release, Step/Random fires, trigger counters) once per hop and keeps each hop's stage output in `audio_observations`. Just before the frame's composition, each generator audio mod turns those into `hop_timeline.values`: (transport time, effective value), where the effective value is the same per-parameter composition (`compose_param`) with that hop's audio state. Nothing is advanced or conditioned twice. The buffer is cleared, never reallocated, per frame.
  - *Hop time.* Offline export stamps every hop with its timeline time; that is the hop's time. Live hops carry only the audio sample clock, so each mod anchors that clock to transport time at the lowest latency it has seen: the anchor moves only when a hop would land after the frame that delivered it, or more than 100 ms behind it (a seek or stall). Anchored, hop times are a function of the audio alone, whatever the display rate.
  - *Consume.* `sample_physics_history` routes each mod whose card binding writes a node in the sampled physics ancestry (for a GPU liquid, the `acceleration_field` ancestry only) and, at every sample time, writes the latest hop value at or before that time through the binding. Before the frame's first hop the interval keeps the prior frame's final value, which is the last hop's. Values reaching nothing in the ancestry are never routed. The per-frame hold for those parameters is gone; there is no fallback to it.
  - *Proof.* `host_kick_forces_per_tick_match_across_frame_rates` (a kick through an audio mod onto a uniform force gives the same per-tick force at 24, 30 and 60 fps, live and offline) and `attack_release_hop_values_match_frame_values` (an attack/release shaper's per-hop values equal its per-frame values at the frame times).
  - *Not covered.* Drivers, envelopes and automation lanes compose per frame; graph-side LFOs already sample per tick. Owed in BUG-ywal (per-tick sampling for drivers, envelopes and automation).
  - *Scene-modifier cards.* A card's params are host params on the owner generator, and expansion composes them into ordinary node bindings on the owner graph, so they take the same route with no extra path. `modifier_card_kick_forces_per_tick_match_across_frame_rates` proves it on a Uniform Force card.
### P9 — Add Fluid authors the default liquid template (seam brief; supersedes MPM P4b)

- **Entry state:** P2b on main. `rg -n 'const FLUID_TYPE_ID' crates/manifold-editing/src/commands/graph/scene/fluid.rs` matches.
- **Read-back:** `edit/commands/graph/scene/fluid.rs` whole; `app/ui_bridge/project.rs:619`; GROUPING_GRAPHS.md.
- **Old → new:** `AddSceneFluidCommand` builds `node.fluid_surface` from `FLUID_TYPE_ID` (`fluid.rs:23`) with metadata the app looks up for that type (`project.rs:619`) → the command inserts the template graph the app hands it, and the app resolves it from one constant, `DEFAULT_LIQUID_TEMPLATE`, which names `gpu_flip_liquid_template`. ⚠ VERIFY-AT-IMPL: the command's `catalog_default` may already carry the graph; if so, the change is deleting `FLUID_TYPE_ID` and building from it.
- **Deliverables:** tests `scene_physics_add_fluid_template_undo_reload` for the FLIP template and for a GPU template (Dam Break Matter's Live Matter and Liquid Surface groups) passed in by the test; flow `scripts/ui-flows/scene-fluid-template.json`. Another solver's template is its own builder plus its own Add Fluid tests, never a one-line swap.
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

### P11 — Block occupancy map (retired, deleted 2026-10-02)

Built, measured, then deleted with the rest of section 3.10 (block occupancy map). GPU ms per dispatch on the shipped Dam Break at Surface Detail 0, frames 30, 90 and 150:

| Resolution | Blocks with surface | Map | Crossings, map off → on | Saved, net of the map |
|---|---|---|---|---|
| 64 (70³ cells, 18³ blocks) | 17–29% | 0.10 | 0.31–0.50 → 0.29–0.50 | −0.08 to −0.10 |
| 128 (134³ cells, 34³ blocks) | 10–27% | 0.64–0.79 | 1.18–3.06 → 0.89–2.89 | −0.41 to −0.48 |

With surface_crossings as its only consumer, the map costs more than it saves at both sizes. surface_crossings already skips footprints with no sign change, so the map only spares it the level-set reads. Building the map reads the same level set, one thread per block.

### P12 — Solver tile skipping (retired)

Measured as a bit-exact in-step block skip and dropped; see section 3.10 (block occupancy map).

### P13 — Body handoff tests first (`feat/liquid-body-handoff`)

- **Entry state:** section 2.1 (Body handoff amendment) approved. `rg -n 'GPU_FLIP_WALLS_IN_SOLVE\)' crates/manifold-node-engine/src/water/liquid/conformance.rs` matches a whole-check `Check::Collision` exemption.
- **Read-back:** section 2.1; section 3.3 (Two-way Box3D coupling); `tests/gpu_proofs/liquid_conformance.rs` (`liquid_floating_draft`, `liquid_coupling_collision`).
- **Deliverables:** the body handoff proofs in section 3.3 on the GPU FLIP and MPM rows, with a several-box scene for the stack; the D20 split.
- **Gate:** one recorded run, expected red: floating rest fails at 15 and 30 Hz on GPU FLIP with rest motion near g·dt·(N−1)/(2N); resting contact loses water. Red never lands; each proof lands with the phase that turns it green.
- **Forbidden:** loosening a bound to reach green; a proof without the validity guards.
- **Test scope:** focused renderer; GPU proofs (`liquid_conformance`).

### P14 — Water is never deleted at a solid

- **Entry state:** P13's red run recorded on the branch.
- **Read-back:** D19; `gpu_flip_step.wgsl` `resolve_solid` and its two deletion sites (`:2255`, `:2718`).
- **Deliverables:** D19 at both sites; an audit of whether MPM removes particles at solids, and the same change there if it does. Audit result: MPM removes a point only when its stencil leaves the lattice (`grid_to_matter_body.wgsl`); at a body it projects the point onto the surface (D29), so MPM needs no change.
- **Recorded:** push-out stopped the collision proof losing water, but a still box on the floor kept losing 52%/31% at 15/30 Hz with nothing left inside a solid. The drain was the boundary velocity (v0 + a·t + P/m) of a body posed still (p + v0·t); P15/P16 remove it.
- **Gate:** resting contact and the stack remove no water; the refused count is zero on floating rest, so push-out is not what makes it pass. Every rest proof asserts it.
- **Test scope:** focused renderer; GPU proofs.

### P15 — One motion law, one handoff

- **Entry state:** P14 on the branch.
- **Read-back:** D15, D17, D18; `liquid/coupling.rs` (`capture_rows`, `exchange`); `liquid/bodies.rs:118`; `liquid_pose.wgsl`; `box3d/src/solver.c:65-220`.
- **Deliverables:** `coupled_motion.rs` and its WGSL twin; `queue_reaction_over_step` in place of `apply_impulses` in the liquid exchange, waking a sleeping body; every GPU FLIP pose site in section 2.1 on the law; the handover check; I17, I18, I19.
- **Gate:** floating rest and handover agreement green on GPU FLIP at all three rates; MPM's conformance numbers equal or better; `liquid_free_flight` still bit for bit. Look at it: Peter's waterLossTest file at 15 Hz with the box at density 100 sits still and keeps its water.
- **Gesture:** drop a light box into the Dam Break pool; it bobs and settles.
- **Recorded (2026-10-06):** free and floating bodies end every tick on the law (1e-7 m, 3e-8 m/s) once a reaction keeps them awake. MPM's floating heights no longer depend on the rate (half-density centre 1.067/1.042/1.017 m at 15/30/60 Hz before, 0.969/0.965/0.965 after), which uncovered BUG-28j99 (MPM floats low): its draft passed on main only through the old impulse's lift. GPU FLIP floating rest improved (density 0.05 RMS 0.45 to 0.17 m/s at 15 Hz) but stays red, BUG-u8nqr (GPU FLIP floats high). The failing misses land known red by bug name (Peter, 2026-10-06). MPM's draft is worse than main's number at 60 Hz, so the gate's "equal or better" is not met there; Peter accepted it (section 8 (Calls only Peter makes), item 5).
- **Forbidden:** section 3.9's body handoff entries.
- **Test scope:** manifold-physics; focused renderer; GPU proofs.

### P16 — Supports in the prediction and the solve

- **Entry state:** P15 on the branch.
- **Deliverables:** `support_points` over Box3D's touching contact points; the per-body support buffer; D16's projection in the law and its held mobility in GPU FLIP's pressure solve, with a proof that the solve's body operator stays symmetric.
- **Gate:** resting contact and lift-off green on both rows; the stack's result reported to Peter, which decides section 7's repeated swaps.
- **Recorded (2026-10-06):** resting contact green on both rows (GPU FLIP 0% water lost at 15 and 30 Hz). The law's friction is Box3D's per-patch rule; against Box3D on a floor it is inside D18 except the first tick of a slide from rest. Lift-off red on GPU FLIP, BUG-o3kj8 (GPU FLIP corner lift-off), a liquid-side failure that predates the branch. Stack: 10% water lost to none once a box stands on the box below (D16); the top box still rests at about 1.07 cm/s RMS against the 1.0 bound, BUG-tsdw3 (GPU FLIP stack: the top box wobbles just over the rest bound). The failing misses land known red by bug name (Peter, 2026-10-06).
- **Test scope:** manifold-physics; focused renderer; GPU proofs.

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
13. Add Fluid authors GPU FLIP, with no solver picker and no fallback to CPU FLIP or Matter (Peter, 2026-10-02).
14. One handoff for every liquid, a steady force over the tick (D17); one in-tick motion law for GPU FLIP and the owner (D15).
15. Water is never deleted at a solid (D19; Peter, 2026-10-06).
16. One coupling style: liquids take turns with Box3D, which owns every contact. A liquid never solves contact forces; it only keeps a body on the supports Box3D reports (D16; Peter, 2026-10-06).

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
| Repeated swaps within a tick (hand off until both sides agree) | `liquid_submerged_stack` or `liquid_lift_off` fails after P14 |
| Dynamic-body contacts in D16's prediction | the stack's handover error flags them |
| MPM's body move on the D15 law | an early-versus-late impulse proof and a light-body impact proof pass on MPM |

## 8. Calls only Peter makes

1. **P1 edits MPM-owned files before SWASH's P4.** The SWASH design's decided item 1 keeps MPM untouched until P4. The move changes no behaviour and is gated on unchanged proof numbers; without it SWASH P3b must copy MPM's coupling owner or import it. Recommendation: yes, before SWASH P3b.
2. **The resolution ceiling (D14).** Recommendation: yes. It turns a machine lockup into a named refusal; each ceiling lifts as its staged GPU check passes.
3. **Which liquid Add Fluid authors (P9).** Decided: GPU FLIP, no picker, no fallback (section 6 (Decided), item 13).
4. **The bake workflow for GPU liquids** (BUG-vglg.18). Recommendation: GPU liquids offer no cache until that talk.
5. **MPM's floating height moves with P15.** The old handoff lifted floating bodies by a rate-dependent amount, which hid BUG-28j99 (MPM floats low). A half-density box's centre against the analytic 1.000 m: 1.067/1.042/1.017 m at 15/30/60 Hz on main, 0.969/0.965/0.965 m on the law. Closer at 15 and 30 Hz, further at 60 Hz, and the same at every rate. Decided (Peter, 2026-10-06): land it with the draft known red; BUG-28j99 is fixed on MPM's own side.
