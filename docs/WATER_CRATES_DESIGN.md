# Water Crates — one crate per water subsystem, the compiler holds the lines
**Status:** ACCEPTED · 2026-10-10 · Peter approved D1–D13 with the GPU MPM crate named manifold-water-gpu-mpm · five stages, none started · Section 9 (Phasing).
**Status:** PROPOSED · 2026-10-10 · Fable · five stages, none started · Section 9 (Phasing).
**Prerequisites:** BUG-hkbdp.6.6 (CPU FLIP removal), BUG-hkbdp.6.2 (coupled-scene preparation out of the engine), BUG-hkbdp.6.3 (scene types out of the engine) and BUG-hkbdp.6.4 (seam review) landed on main; BUG-hkbdp.6.8 (explicit step context) landed before stage 2.
**Work items:** BUG-hkbdp.6.12 (water subsystem boundaries the compiler enforces), under the epic BUG-hkbdp (renderer crate split epic). Stage beads are drafted beside this doc and created by the lead.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before any stage. Lead: Opus. Lanes make one commit then stop; the lead lands with `scripts/land_branch.py`.

<!-- index: Split manifold-nodes-water into rigid, liquid, gpu-flip, gpu-mpm, whitewater and surface crates under a thin registration crate, so a water edit runs its own tests plus the contract suite at its seam. -->

**The governing insight: the water crate already has a layering, it just is not enforced.** Every solver (GPU FLIP, Matter, whitewater, the mesher) reaches down into one shared contract, the liquid seam (`crates/manifold-nodes-water/src/liquid/`, `docs/LIQUID_SOLVER_SEAM_DESIGN.md`), and the seam reaches down into the Box3D rigid adapter (`physics.rs`). The cycles the lead's census found are not architecture; they are eleven misplaced items listed in section 1.2 (the coupling census), plus test harnesses and migrations that know every solver sitting inside the shared module. Move those and the layering is a tree. Cargo then refuses the next upward reach at compile time, and the landing gate's crate-to-test mapping scopes a water landing for free.

Peter, 2026-10-10 (the bead for this design): *"a water change must not need unrelated tests, so boundaries and interfaces must be rock solid and the compiler must stop small changes rippling into unexpected areas."* And the stage-2 call: *"crate split approved (stage 2 decided: each water subsystem becomes its own crate)."*

Stage translation: nothing here changes a pixel or a millisecond of water on the screen. It changes what happens when water breaks before a gig: a fix to the whitewater step builds and tests the whitewater crate and the seam proofs it implements, not the pressure solver and not Matter. A lane working in GPU FLIP cannot break Matter's build, because its crate cannot see Matter.

Binding constraints (DESIGN_AUTHORING.md section 1 (The intake)): *Hot path* — the per-tick CPU orchestration (clock advance, body rows, coupling settle, dispatch argument math) now crosses crate boundaries; section 7 (Hot paths) names the calls and the gate. *Persistence* — none: no node type id, port name, serialized field or preset JSON changes; crate and Rust module names are not serialized (INV-W5 in section 8 (Invariants and enforcement)). *Thread residency* — untouched. *Time model* — untouched; `TICK` moves house but keeps its value.

Companion docs: `docs/RENDERER_CRATE_SPLIT_DESIGN.md` (the precedent: D4 no facades, D6 linking proven, D7 one test binary per crate, D8 visibility on compiler demand, D12 layering as a test; this design extends its table), `docs/LIQUID_SOLVER_SEAM_DESIGN.md` (the liquid contract this design turns into a crate boundary), `docs/PHYSICS_ENGINE_BOUNDARY_DESIGN.md` section 4 (Graph boundary) (the rigid adapter's contract), `docs/GPU_FLUID_SURFACE_DESIGN.md` (the mesher and the particle-frame seam), `docs/GPU_WHITEWATER_DESIGN.md`, `docs/GPU_MPM_SOLVER_DESIGN.md`, `docs/GPU_FLIP_PRESSURE_SOLVE.md`.

---

## 1. Audit — what exists (verified 2026-10-10 at `8b7b4019e`, worktree slot-4)

### 1.1 The crate today

| Piece | Where | State |
|---|---|---|
| Crate size | `crates/manifold-nodes-water`: 297 `.rs` files before the CPU FLIP cut; 253 after dropping `fluid.rs`, `fluid/`, `fluid_cache.rs`, `fluid_mesh_upload.rs`, `primitives/fluid_surface*` (BUG-hkbdp.6.6 (CPU FLIP removal) scope). 110 WGSL files under `src/primitives/shaders/` | SPLIT |
| Dependencies | `Cargo.toml`: engine, core, foundation, gpu, physics, fluids; `inventory`, `naga`, `bytemuck`, `half`, `arrayvec`, `image` (one proof-only file), `zstd`+`sha2` (CPU FLIP takes, go with 6.6) | Each new crate takes only what its files import (BUG-hkbdp.6.5 (Cargo hygiene) rule) |
| Features | `testkit`, `gpu-proofs`, `fluid-perf-proofs`, `matter-perf-proofs`, `water-race-probes`, `whitewater-oracle` (`Cargo.toml:41-46`) | BUG-hkbdp.6.9 (proofs consolidation) folds the four extras; this design gives every crate exactly `testkit` and `gpu-proofs` (D7) |
| The liquid seam | `src/liquid.rs` + `liquid/{bodies,body_buffers,clock,coupling,fields,display_cursor,frame_history,frame_ring,grid,lattice,tick_samples,substep_history}.rs`; header: "What every GPU liquid domain shares" | EXISTS — becomes `manifold-water-liquid` |
| The rigid adapter | `src/physics.rs` (4793 lines, production to 2611) + `physics/{worker,impulses,serialization,targeted_fields}.rs`, `physics_mesh.rs`, `physics_events.rs`, `physics_metrics.rs`, `vector_field.rs`, `primitives/{physics_world,rigid_body,vector_fields}.rs` | EXISTS — becomes `manifold-water-rigid` |
| The native pair contract | `src/node.rs` (170 lines): `PhysicsNode` trait, `PhysicsNodeRegistration`, `LazyLock<AHashMap<TypeId, …>>` registry, `get`/`get_mut`; implemented by `GpuFlipDomain` (`primitives/gpu_flip_domain.rs:730`), `MatterDomain` (`primitives/matter_domain.rs:755`), `PhysicsWorldNode` (`primitives/physics_world.rs:763`); used by `physics_scene.rs`, `runtime/state.rs`, `primitives/physics_world.rs:935-1357` | EXISTS — every type it names is rigid vocabulary or lower (D2) |
| Registration sites | `primitive!` macro per node (inventory); `wire_values.rs:5-7` three `CpuWireRegistration`s; `graph_install.rs:87` `GraphInstantiationHook`; `liquid/migration.rs:302-338` five `GraphMigration`s; `runtime/gpu_flip_surface.rs:167` one; `primitives/blob_bounds.rs:234` one; `runtime/state.rs:169` `RuntimeRegistration`; 60+ `primitives/*/extent.rs` `ExtentRule`s; three `PhysicsNodeRegistration`s | EXISTS — inventory needs no engine change; linking is the risk (D5) |
| Shared atoms | `sort_particles_into_cells`, `prefix_scan`, `face_sample_component`, `liquid_cells`, `liquid_stats`, `particle_identity`, `particle_publication`, `liquid_bricks`, `smooth_lattice`, `redistance_lattice`, `offset_lattice`, `upwind_distance`, `whitewater_distance` (`SurfaceDistance`), `running_total`, `dot_products` — each used by two or more solvers (section 1.2 census) | Move to the liquid crate (D3) |
| Whitewater grid vocabulary | `src/whitewater.rs` (records, `grid_cells`, `refinement`, `face_offset`, `WHITEWATER_COMMON`); used by `liquid/lattice.rs:254`, `primitives/liquid_state/extent.rs:21`, `sort_particles_into_cells.rs`, `liquid_cells.rs`, `jitter_particles.rs` | Liquid crate (D3); the whitewater *step* stays a leaf |
| Cross-solver harnesses inside the seam | `liquid/conformance.rs` (723, cfg testkit/gpu-proofs; names `matter`, `liquid_stats`), `liquid/extent.rs:22-101` (`check_preset_extents`, `LiquidPreset`; names `matter_domain::admit_lattice`, `liquid_bricks::schedule_words`, `whitewater`), `liquid/scene_contract.rs` (cfg test), `primitives/gpu_flip_preset.rs` (1149, builds the whole Dam Break graph incl. surface and whitewater; used by `manifold-app/src/ui_bridge/project.rs:24,51`), `primitives/face_grid_scenes.rs`, `primitives/testkit.rs` (constructors for whitewater, surface and gpu-flip nodes) | Move up to the registration crate (D4) |
| Migrations that name several solvers | `liquid/migration.rs` (`node.gpu_flip_step`, `node.matter_state`, `node.liquid_frame` …), `runtime/gpu_flip_surface.rs` (`WaterDamBreakGpuFlip`) | Registration crate (D4); `blob_bounds::wire_blob_bounds` stays with its node |
| Runtime extension | `runtime/{state,access,physics_sampling,physics_impulses,physics_carry,physics_source_*,scene_impulses}.rs`; `rg 'crate::(primitives::(gpu_flip|matter|whitewater|liquid_)|liquid::|matter|whitewater)' src/runtime/*.rs` → 0 production hits | Already solver-neutral: registration crate |
| External production callers | `manifold-nodes/src/registry.rs:74` `PhysicsWorldNode::prewarm_pipeline`; `manifold-app/src/content_pipeline.rs:2154-2160` `physics::LiveLoad`, `PhysicsStepScope::for_frame`; `content_pipeline.rs:2975`, `content_thread.rs:93,787,799`, `frame_time.rs:41-43` `physics_metrics::*`; `ui_bridge/project.rs:24,51` `gpu_flip_preset::{gpu_flip_liquid_body, LIQUID_BODY_OUTPUT}` | Section 4.8 assigns each a home; the app and catalog keep depending on `manifold-nodes-water` only |
| Layering test | `crates/manifold-app/tests/crate_layering.rs` `LAYERS` table over `cargo metadata` (RENDERER_CRATE_SPLIT D12) | EXISTS — grows six rows (section 8) |
| Gate mapping | `scripts/gate_policy.py`: `WATER_SRC` (one prefix), `PRIMITIVE_PATHS`, `GPU_DEFAULT_CPU_ONLY`, `NARROW_ROWS`, `EXPLICIT_ROWS`, `LIB_PROOF_ROWS`, `BROAD_PATHS`, `PREFIX_ROWS`, `CATALOG_TEST_ROWS`, `INTEGRATION_ROWS`; `scripts/gpu_scope.py::is_gpu_path` discovers GPU crates by their `gpu-proofs` feature; `scripts/gpu_proofs_gate.py:920` runs every `feature_packages("gpu-proofs")`; `scripts/cpu_scope.py` selects module filtersets from the owning package via `gate_workspace.Workspace` | EXISTS — rows re-keyed per crate (section 6.4), discovery is automatic |
| WGSL and ABI roots | `crates/manifold-nodes/src/testkit/source_roots.rs` `PRIMITIVE_SOURCE_ROOTS`, `WGSL_SRC_ROOTS` ("a crate move must update the proof") | One line per new crate |
| Water tests outside the crate | `manifold-nodes/tests/contracts/water/*` (10 files), `contracts/node_graph/catalog_tests/{gpu_flip_*,liquid_*,physics_*,whitewater_*,particle_*}`, `gpu_proofs/{matter_*,physics_*,fluid_array_growth,liquid_indexed,gpu_flip_frame_perf}`, `manifold-app/tests/renderer_contracts/gpu_proofs/{liquid_conformance,water_basin*,physics_solids}` | Stay where the registry they need lives (D8); paths into water modules are re-pointed per stage |
| Precedents | RENDERER_CRATE_SPLIT P2 (three leaves carved in parallel, landing order fixed, shared-file conflicts named), P5 (the water seam as built); `manifold-physics` / `manifold-fluids` below the water crate | Shape every new crate like `manifold-nodes-water` today |

### 1.2 The coupling census, re-run with the proposed assignment

Script: the lead's `area_coupling.py` refined to the file-to-crate table of section 3, resolving `crate::` and `super::` paths, dropping CPU FLIP files and `#[cfg(test)]` bodies. Committed at stage 1 as `scripts/water_crate_edges.py` (retired at stage 5 when Cargo takes over). Production edges with the files placed as section 3 places them:

| From → to | Count | Verdict |
|---|---|---|
| whitewater → liquid 53 · gpu-flip → liquid 45 · matter → liquid 36 · surface → liquid 19 | 153 | Downward, allowed |
| gpu-flip → rigid 14 · matter → rigid 9 · liquid → rigid 9 · whitewater → rigid 3 · surface → rigid 1 | 36 | Downward, allowed. All but `RigidBody`/`pose_from_transform`/`ResolvedNodeImpulse`/`physics_mesh` are the ambient switches and metrics BUG-hkbdp.6.8 (explicit step context) replaces |
| registration → rigid 14 · → liquid 5 · → surface 1 | 20 | Downward, allowed |
| **liquid → gpu-flip 2** | `liquid/bodies.rs` → `gpu_flip_clock::GpuFlipBodyVertex`; `face_sample_component.rs` → `gpu_flip_step::face_bytes` | CUT: move the type and the function down (section 3.9) |
| **gpu-flip → whitewater 2** | `gpu_flip_step.rs:732` → `whitewater_distance::SurfaceDistance`; `liquid_state/extent.rs:7-8` → `whitewater_step::{DEFAULT_CAPACITY, MAX_CAPACITY}` | CUT: `SurfaceDistance` + `UpwindDistance` are shared → liquid; the two pool constants → `whitewater.rs` vocabulary |
| **gpu-flip → matter 1** | `gpu_flip_domain.rs` → `matter_domain::closed_faces` | CUT: move to `liquid::lattice` beside `wall_distance(closed_faces)` |
| **gpu-flip → surface 1** | `clamp_liquid_to_solids.rs:11` → `liquid_bricks::{COMMON, valid_schedule}` | CUT: `liquid_bricks` is the brick schedule ABI → liquid |
| **whitewater → surface 3** | `whitewater_step.rs` → `extend_lattice`, `lattice_curvature`, `pad_distance_lattice` | NOT A CUT: their only users are whitewater files (`rg -l` oracle below) → they are whitewater atoms; section 3 places them there |
| **whitewater → gpu-flip 1** | `whitewater_step.rs` → `gpu_flip_step::face_bytes` | CUT: same move as above |
| **gpu-flip → registration 1**, **liquid → registration 1** | `gpu_flip_preset.rs:1096` → `testkit::liquid_extents::walk` (cfg testkit); `fluid_role_source/geometry.rs:224` → `testkit::fluid_role_source` (cfg test) | Resolved by moving `gpu_flip_preset` up (D4) and `testkit/fluid_role_source.rs` down into the liquid crate's testkit |
| Test-only cross-area reaches | 125 | Each test moves with the production file it tests; a test that names two leaves is a catalog or registration-crate test (D8) |

Oracle for every row: `scripts/water_crate_edges.py crates/manifold-nodes-water/src` (stage 1 deliverable; until then the scratchpad copy at the path in the stage 1 bead). Atom ownership oracle: `rg -l '<atom>' crates/manifold-nodes-water/src` minus the atom's own files. Re-run before each stage; a count that differs from this table is listed before anything moves.

Classification: **exists** — the layering (the seam is already the hub), every registration mechanism, the layering test, gate discovery by feature, the move protocol. **One wire away** — eleven item moves, six Cargo manifests, one layering row per crate, gate rows re-keyed. **Genuinely new** — nothing at runtime. Zero-new-systems test: zero new traits, registries, id schemes or caches. The one thing that goes beyond plain Rust workspace practice is the per-crate word census and feature pin in `crate_layering.rs` (section 8); it is a test over `cargo metadata` and `rg`, nothing else.

Negative claims, checked: no serialized value names a Rust module or crate (`rg -n 'module_path!|type_name' crates/manifold-nodes-water/src` → 0 outside tests); `PresetTypeId`s, node type ids and port names are string literals in `primitive!` and preset JSON. No water crate is linked by anything but a Cargo edge (`rg 'extern crate' crates/manifold-nodes-water/src` → 0). `runtime/` names no solver module (row above).

---

## 2. Decisions

**D1 — Seven crates: five production layers under one registration crate.** Arrows point at dependencies.

| Crate | Owns | Depends on (workspace) |
|---|---|---|
| `manifold-water-rigid` | The Box3D graph adapter and the native pair contract: `physics.rs` tree, `physics_mesh`, `physics_events`, `physics_metrics` (until 6.8 moves the record types), `vector_field`, `node.rs` (`PhysicsNode`), `node.physics_world`, `node.rigid_body`, the vector-field source nodes, `CoupledRigidFrame`/`CoupledRigidLayout` (re-homed from the deleted `fluid/coupled.rs`) | core, foundation, gpu, node-engine, physics |
| `manifold-water-liquid` | The liquid seam: clock, lattice, grid, bodies, body buffers, coupling, fields, frame ring and history, display cursor, tick samples, substep history, roles (`fluid_role`, `fluid_particles`, `node.fluid_role_source`), the whitewater grid vocabulary (`whitewater.rs`), the shared atoms of section 1.1, `liquid::extent` production helpers | rigid + its set, fluids (reference oracles only, behind `testkit`) |
| `manifold-water-gpu-flip` | `gpu_flip_*`, `liquid_state`, `liquid_fill`, `liquid_solid_distance`, `clamp_liquid_to_solids`, `push_out_of_solid`, `euler_step_particles(_3d)`, `apply_radial_burst(_3d)_to_particles` | liquid + its set |
| `manifold-water-gpu-mpm` | `matter.rs` tree, every `matter_*` node, `grid_to_matter` | liquid + its set |
| `manifold-water-whitewater` | `whitewater_step` and its lifecycle, type, handoff, emitters, potentials, `nearest_crossing`, `surface_crossings`, `crossing_distance`, `keep/advect/age/retype/spawn_whitewater`, `preserve_foam`, `jitter_particles`, `sample_faces_at_particles`, `extend_lattice`, `lattice_curvature`, `pad_distance_lattice` | liquid + its set |
| `manifold-water-surface` | The mesher: `lattice_bricks`, `liquid_frame`, `particle_volume`, `volume_surface_mesh`, `count_surface_edges/triangles`, `relax/smooth_surface_mesh`, `surface_mesh_normals`, `surface_mesh_parity`, `lattice_closing_tests`, `shape_particle_blobs`, `blob_bounds` | liquid + its set |
| `manifold-nodes-water` (kept) | Registration and runtime: `lib.rs` linking every crate, `graph_install`, `physics_scene`, `migration/`, `runtime/`, `presets/` (the Dam Break builders), the cross-solver testkit and conformance harness | every crate above |

Rejected: *two crates (seam + everything else)* — a leaf edit would still rebuild and retest every solver; the bead's goal is per-solver scope. Rejected: *rigid as a leaf beside the solvers* (the bead's first sketch) — `liquid/coupling.rs` owns a `RigidSimulation` and `liquid/bodies.rs` calls `pose_from_transform`; the rigid owner of a coupled liquid is part of the seam, so rigid sits below it. Inverting that through a trait would be the new framework the bead forbids. Rejected: *merging surface into liquid* — the mesher is a look surface Peter tunes (GPU_FLUID_SURFACE_DESIGN), it is 42 files with its own proofs, and nothing in the seam needs it once `liquid_bricks` moves down. Rejected: *merging rigid into liquid* — a body-in-water bug fix would then rerun the whole seam contract; separate, the compiler also proves rigid never knows liquids (INV-W2), which is the PHYSICS_ENGINE_BOUNDARY direction.

**D2 — The native pair contract lives in the rigid crate.** `PhysicsNode` names `RigidSceneObservation`, `RigidImpulseTargets` (core after 6.3), `CoupledRigidFrame`, `ResolvedNodeImpulse`, `EventStamp`/`TickStamp` (physics), `FluidDomainSnapshot` (core), `ProjectTempo` (engine). Nothing liquid. The domains implement it from above; `physics_scene.rs` drives it from the top. Consequences, stated honestly: a crate named rigid carries the trait that liquids implement, and its doc comments say "liquid". The INV-W2 word census allowlists `node.rs` for that reason. Rejected: *a `manifold-water-core` crate for the contract alone* — it would hold one trait, one frame type and `TICK`; a crate that exists to hold a trait is the speculative framework the bead forbids. Rejected: *the trait in the registration crate* — the domains below could not implement it.

**D3 — An atom used by two solvers is a seam atom and lives in `manifold-water-liquid`.** The oracle is `rg -l '<atom>' crates/manifold-nodes-water/src` minus the atom's own files: two or more leaf areas → liquid. Today that is the section 1.1 shared-atoms row. Consequences: the liquid crate owns about 15 GPU atoms, and a change to `sort_particles_into_cells` runs every solver's seam proofs (CS-liquid, section 6.3). That is correct — every solver dispatches it. Rejected: *duplicating small atoms per leaf* — two homes for one kernel is the drift this repo's fusion proofs exist to prevent.

**D4 — Anything that names two leaves lives in the registration crate: migrations, preset builders, conformance and preset-extent harnesses, cross-solver testkits.** `liquid/migration.rs` → `manifold-nodes-water/src/migration/liquid.rs`; `runtime/gpu_flip_surface.rs` → `src/migration/gpu_flip_surface.rs`; `primitives/gpu_flip_preset.rs` → `src/presets/gpu_flip.rs`; `primitives/face_grid_scenes.rs` → `src/testkit/face_grid_scenes.rs`; `liquid/conformance.rs` → `src/testkit/conformance.rs`; `liquid/extent.rs:22-101` (`check_preset_extents`, `LiquidPreset`, `ExtentReport`) → `src/testkit/preset_extents.rs` while the per-node helpers (`liquid_lattice`, `PARTICLE`, `node_extent`, `whitewater_grid`, …) stay in `manifold-water-liquid::extent`; `primitives/testkit.rs` splits into each crate's testkit (the `member`/`dense_pipeline` fixtures go with the nodes they construct). `blob_bounds::wire_blob_bounds` stays with `node.blob_bounds` in the surface crate (it names only its own node and the frame it feeds). Migration names and stages are unchanged; `migration_order_matches_table` pins the resolved sequence. Rejected: *leaving `gpu_flip_preset` in the gpu-flip crate* — it constructs surface and whitewater nodes by type id and the app depends on it; the app would need a direct edge to a solver crate.

**D5 — Linking is proven, not assumed (RENDERER_CRATE_SPLIT D6, applied again).** `manifold-nodes-water/src/lib.rs` carries `use manifold_water_gpu_flip as _;` and the same for every crate below it, so an otherwise-unreferenced leaf rlib is linked and its `inventory` submissions exist. `manifold-nodes`, the app and every test binary that calls `PrimitiveRegistry::with_builtin` depend on `manifold-nodes-water`, never on a leaf alone, except catalog tests that construct a leaf's node directly (dev-dependency on that leaf, D8). INV-W4 (the node catalog census) fails if a leaf drops out. Rejected: *`#[used]` tricks* — an unreferenced rlib is the failure; only a reference fixes it.

**D6 — Registration lives with the type it registers, except migrations (D4).** `CpuWireRegistration::<RigidBody>` and `::<FieldValue>` in the rigid crate; `::<FluidRole>` in the liquid crate; `PhysicsNodeRegistration` beside each domain node as today; `ExtentRule`s beside their nodes as today; `RuntimeRegistration` and the `GraphInstantiationHook` at the top as today. A leaf test binary then carries exactly the wire registrations its nodes need. The engine fails loudly on a duplicate `TypeId` (`exec::cpu_values`), so a registration landing in two crates is a red test, not a silent double.

**D7 — Features: every water crate declares exactly `testkit` and `gpu-proofs`; the perf and oracle flags fold in BUG-hkbdp.6.9 (proofs consolidation).** Shape, copied from `manifold-nodes-scene/Cargo.toml`: `testkit = ["manifold-node-engine/testkit", "<lower water crate>/testkit"]`, `gpu-proofs = ["testkit", "manifold-node-engine/gpu-proofs", "manifold-gpu/gpu-proofs", "<lower water crate>/gpu-proofs"]`, plus `[dev-dependencies] <self> = { path = ".", features = ["testkit"] }` so the crate's own tests see its testkit. Until 6.9 lands, the four extra flags live only on the crates whose files use them (`water-race-probes` and `whitewater-oracle` forward `manifold-fluids` features; `fluid-perf-proofs` on gpu-flip and surface; `matter-perf-proofs` on matter) and INV-W3 lists them by name with the bead id; the pin ratchets to two when 6.9 closes. `manifold-nodes` and `manifold-app` keep forwarding `manifold-nodes-water/gpu-proofs`, which forwards down. Rejected: *one `water-proofs` umbrella feature* — the gate discovers GPU crates by the `gpu-proofs` name (`gpu_scope.py::is_gpu_path`, `gpu_proofs_gate.py:920`); a second name is a second mapping to maintain.

**D8 — A test lives in the lowest crate that links every node its graph instantiates.** The fixture decides, not the author. A proof over one solver's own nodes plus engine built-ins, seam nodes and `node.physics_world` sits in that solver's crate (its lib `gpu_tests`, or its `tests/` target under `gpu-proofs`). A proof that instantiates two solvers, or the Dam Break presets without rendering, sits in `manifold-nodes-water`. A proof that renders (`node.render_scene`, the surface group in a scene) or loads a bundled preset through the catalog stays in `manifold-nodes/tests` or `manifold-app/tests` where it is today. BUG-hkbdp.6.11 (water test cut) places each surviving test by this rule. Module names inside each crate stay what they are today (`primitives::gpu_flip_step::gpu_tests::…`), so libtest filters in `gate_policy.py` and keys in `gpu_test_times.json` survive the move; 6.9's `proofs/` consolidation changes them on purpose, later, in its own commit.

**D9 — Test-only reaches are not exempt.** A `#[cfg(test)]` reach from a lower crate into a higher one does not compile either. Every such reach in section 1.2 is resolved by moving the test (to the crate whose nodes it needs) or the helper (down to the lowest crate that uses it). No `dev-dependency` points upward: a dev-dependency cycle compiles the lower crate twice (RENDERER_CRATE_SPLIT D7's rejection).

**D10 — Move order: cut inside the crate first, carve bottom-up, split god-files after.** Stage 1 cuts the section 1.2 edges and relocates the D4 items inside `manifold-nodes-water`, so stage 2 onward are pure `git mv` moves plus path rewrites. Rigid is carved before liquid, liquid before the leaves, because each crate's `cargo check` is the oracle for the one below being complete. BUG-hkbdp.6.7 (god-file split) runs after the carve, inside the final crates, so every split file is renamed once (the carve) and split in place, never moved twice. This reverses the bead's "stage 1 rides 6.7" sketch; the reason is history: a file split before a move is two renames per file for `git blame` to follow. Consequences: 6.7's inline-tests-to-sibling-files pass can run before stage 2 (it moves no production text) if the lead wants the smaller files earlier. Rejected: *carving leaves first* — their `cargo check` would fail on every seam path until liquid exists; the compiler cannot be the oracle in that order.

**D11 — BUG-hkbdp.6.8 (explicit step context) lands before stage 2, and the step values travel as types from `manifold-physics`.** Today every solver and the app reach `physics::{offline_simulation, simulation_interval, authored_sample_only, PhysicsStepScope, LiveLoad}` and `physics_metrics::*` (section 1.2, 36 edges). After 6.8 those are values the frame passes down. The data types the app names — `LiveLoad` (already `manifold_physics::clock::LiveLoad`), `PhysicsSettings` (core), the step context and the metrics records (`PhysicsMetrics`, `ClockRecord`, `ClockMetrics`, `DroppedTimeTracker`) — live in `manifold-physics` so the app and every water crate name them without an upward or sideways edge; `manifold-app` adds `manifold-physics` as a normal dependency (it is a dev-dependency today). Default if 6.8 slips past stage 2: the scope guards and metrics stay in `manifold-water-rigid` as `pub`, the app depends on `manifold-water-rigid` directly, and the layering row records that edge; the design still holds, the surface is just wider until 6.8 narrows it. Rejected: *metrics types in `manifold-foundation`* — foundation is for UI-reachable shared types; these never cross to the UI thread as values (the snapshot copies numbers).

**D12 — `TICK` moves to `manifold_physics::clock::TICK`.** `fluid::TICK` dies with CPU FLIP (6.6 names the question). Rigid uses it (`primitives/physics_world.rs`), so `liquid/clock.rs` cannot serve it; manifold-physics already owns `SimulationClock`. Value unchanged, 1/60.

**D13 — No facade, no re-export, no transitional crate (RENDERER_CRATE_SPLIT D4).** Each stage moves items and repoints every importer in the same commit. `manifold-nodes-water` never `pub use`s a lower crate's module to keep an old path alive. The app and the catalog see exactly: the registration crate's own items (section 4.7), plus direct dependencies on the crates whose types their tests construct. Consequences: the catalog's water tests gain `[dev-dependencies]` on up to six water crates; that is the honest shape of tests that construct six families of nodes.

---

## 3. The crate map, file by file

Paths relative to `crates/manifold-nodes-water/src/` before the move. Every file not listed goes with its directory. `shaders/*.wgsl` follow the Rust file that `include_str!`s them (one WGSL per atom; the shared `whitewater_common.wgsl`, `liquid_bricks_common.wgsl`, `liquid_pose.wgsl`, `liquid_collider.wgsl`, `liquid_field.wgsl`, `liquid_faces.wgsl`, `marching_cubes_common.wgsl` follow the Rust constant that includes them). Each crate keeps a `src/primitives/` module so test paths are unchanged (D8).

### 3.1 `manifold-water-rigid`

`physics.rs` + `physics/` (worker, impulses, serialization, targeted_fields, coupling_tests) → `src/physics.rs` + `src/physics/`; `physics_mesh.rs`, `physics_events.rs`, `physics_metrics.rs` (until D11 moves the record types), `vector_field.rs`, `node.rs`; `primitives/{physics_world.rs, physics_world/, rigid_body.rs, vector_fields.rs}`; new `src/coupled_frame.rs` holding `CoupledRigidFrame` and `CoupledRigidLayout` (from the deleted `fluid/coupled.rs`, re-homed by 6.6 per this design); `testkit/physics_fixtures.rs` (`RigidBody` fixtures) → `src/testkit/`; `wire_values.rs` lines registering `RigidBody` and `FieldValue` → `src/wire_values.rs`.

### 3.2 `manifold-water-liquid`

`liquid.rs` → `src/lib.rs` body (`read_roles`, `WATER_DENSITY`, `ROLE_PORTS`); `liquid/{bodies,body_buffers,clock,coupling,fields,fields/,display_cursor,frame_history,frame_ring,grid,lattice,tick_samples,substep_history}.rs` → `src/`; `liquid/extent.rs` minus lines 22-101 → `src/extent.rs` (+ `extent/testkit.rs`); `fluid_role.rs`, `fluid_particles.rs`, `whitewater.rs`; `primitives/{fluid_role_source.rs, fluid_role_source/, sort_particles_into_cells.rs, sort_particles_into_cells/, prefix_scan.rs, face_sample_component.rs, face_sample_component/, liquid_cells.rs, liquid_cells/, liquid_stats.rs, liquid_stats/, particle_identity.rs, particle_publication.rs, liquid_bricks.rs, liquid_bricks/, smooth_lattice.rs, smooth_lattice/, redistance_lattice.rs, redistance_lattice/, offset_lattice.rs, offset_lattice/, upwind_distance.rs, upwind_distance/, whitewater_distance.rs, running_total.rs, running_total/, dot_products.rs, dot_products/}`; `testkit/{fluid_role_source,liquid_extents,particle_volume,liquid_surface}.rs` → `src/testkit/` (`liquid_surface.rs` references `sort_particles_into_cells` and `fluid_particles`, both here); `wire_values.rs` line registering `FluidRole`. ⚠ VERIFY-AT-IMPL: `testkit/liquid_extents.rs` names `primitives::gpu_flip_preset` — after D4 moves the preset builder up, the walk takes an `&EffectGraphDef` and the preset-naming caller moves to the registration crate's testkit; `rg -n gpu_flip_preset crates/manifold-nodes-water/src/testkit/liquid_extents.rs`.

### 3.3 `manifold-water-gpu-flip`

`primitives/{gpu_flip_bodies, gpu_flip_clock, gpu_flip_domain (+ /), gpu_flip_extension_tests, gpu_flip_lentine, gpu_flip_narrow_band, gpu_flip_narrow_band_tests, gpu_flip_pressure, gpu_flip_pressure_tests, gpu_flip_sheeting, gpu_flip_sheeting_*_tests, gpu_flip_step (+ /), gpu_flip_step_tests, gpu_flip_atom_tests, gpu_flip_tile_tests, gpu_flip_still, gpu_flip_volume, liquid_state (+ /), liquid_fill (+ /), liquid_solid_distance (+ /), clamp_liquid_to_solids (+ /), push_out_of_solid (+ /), euler_step_particles, euler_step_particles_3d, apply_radial_burst_to_particles, apply_radial_burst_3d_to_particles, liquid_surface_tests}`; `tests/fixtures/{dambreak_pressure_problems.bin.zst, deep_pool_*.bin.zst, gpu_flip_pressure_golden.txt}` → `crates/manifold-water-gpu-flip/tests/fixtures/`. ⚠ VERIFY-AT-IMPL: `liquid_surface_tests.rs` (2059 lines) exercises the surface group from gpu-flip particles; by D8 it belongs in the lowest crate that links both, which is the registration crate, unless it instantiates only gpu-flip nodes (`rg -n 'node\.(volume_surface_mesh|lattice_bricks|liquid_frame|particle_volume)' …/liquid_surface_tests.rs`).

### 3.4 `manifold-water-gpu-mpm`

`matter.rs` + `matter/` (coupling, look, reference) → `src/`; `primitives/{matter_body_reaction, matter_common, matter_domain, matter_face_component, matter_fill, matter_frame, matter_grid_update, matter_move_bodies, matter_state, matter_stats, matter_to_grid, grid_to_matter}` with their `/extent.rs`.

### 3.5 `manifold-water-whitewater`

`whitewater_handoff.rs`; `primitives/{whitewater_step (+ /), whitewater_step_tests, whitewater_type, whitewater_lifecycle, whitewater_emitter_cpu, whitewater_emitter_dispatch, whitewater_emitter_gpu_tests, whitewater_emitter_velocity, whitewater_engine_cpu, whitewater_engine_gpu_tests, whitewater_extent_tests, whitewater_field_tests, whitewater_grid_tests, whitewater_handoff_tests, whitewater_influence, whitewater_obstacle_source, whitewater_particle_cpu, whitewater_particle_tests, whitewater_pool_cpu, whitewater_pool_tests, whitewater_cpu, dust_potential, emission_count, energy_potential, inside_turbulence_potential, turbulence_emission_count, turbulence_field, wavecrest_potential, crossing_distance, nearest_crossing, surface_crossings, keep_whitewater, advect_whitewater, age_whitewater, retype_whitewater, spawn_whitewater, preserve_foam, jitter_particles, sample_faces_at_particles, extend_lattice, lattice_curvature, pad_distance_lattice}` with their `/extent.rs`. `testkit/{whitewater_scene,whitewater_fingerprints}.rs` name `gpu_flip_preset` today, so by D4/D8 they go to the registration crate's testkit (⚠ VERIFY-AT-IMPL `rg -n 'gpu_flip' crates/manifold-nodes-water/src/testkit/whitewater_scene.rs`). `tests/fixtures/{whitewater_tick_golden.txt, whitewater_vendored_group.json}` in `manifold-nodes/tests/fixtures/` stay with the catalog tests that read them.

### 3.6 `manifold-water-surface`

`primitives/{lattice_bricks (+ /), lattice_closing_tests (+ /), liquid_frame (+ /), particle_volume (+ /), volume_surface_mesh (+ /), count_surface_edges (+ /), count_surface_triangles (+ /), relax_surface_mesh (+ /), smooth_surface_mesh (+ /), surface_mesh_normals (+ /), surface_mesh_parity, shape_particle_blobs (+ /), blob_bounds (+ /)}`; `blob_bounds::wire_blob_bounds` and its `GraphMigration` stay here (D4).

### 3.7 `manifold-nodes-water` (registration)

`lib.rs` (link lines per D5, module list in section 5), `graph_install.rs` + `graph_install/coupling.rs` (from BUG-hkbdp.6.2 (coupled-scene preparation)), `physics_scene.rs`, `runtime/` whole, `src/migration/liquid.rs` (from `liquid/migration.rs`) + `migration/gpu_flip_surface.rs` (from `runtime/gpu_flip_surface.rs`), `src/presets/gpu_flip.rs` (from `primitives/gpu_flip_preset.rs` + `gpu_flip_preset/testkit.rs`), `src/testkit/{conformance.rs, preset_extents.rs, face_grid_scenes.rs, face_grid_tests.rs, face_grid_extent_tests.rs, whitewater_scene.rs, whitewater_fingerprints.rs, physics_history.rs}`, `live_sim_clock_reference.rs` (cfg test), `liquid/scene_contract.rs` → `src/tests/scene_contract.rs`. `wire_values.rs` is empty after D6 and is deleted.

### 3.8 Not water, confirmed

`manifold-fluids` (the FLIP reference engine) stays a dependency of the liquid crate under `testkit` only (its oracles: `matter/reference.rs` compares, `whitewater-oracle` and `face-oracle` features) and of the registration crate's conformance harness. ⚠ VERIFY-AT-IMPL after 6.6: `rg -l 'manifold_fluids' crates/manifold-nodes-water/src` — every surviving use must be under `cfg(any(test, feature = "testkit"))`; a production use is an escalation.

### 3.9 The eleven cuts (stage 1), old → new

| # | Item | From | To | Shape |
|---|---|---|---|---|
| 1 | `GpuFlipBodyVertex` | `primitives/gpu_flip_clock.rs` | `liquid/bodies.rs` | move the type; `gpu_flip_clock` and `gpu_flip_step` import it |
| 2 | `face_bytes(cells)` | `primitives/gpu_flip_step.rs:102` | `liquid/grid.rs` beside `face_len` | move the fn; four importers |
| 3 | `closed_faces(&ParamValues)` | `primitives/matter_domain.rs:476` | `liquid/lattice.rs` | move the fn; both domains import |
| 4 | `SurfaceDistance`, `scratch_bytes` | `primitives/whitewater_distance.rs` | liquid crate (`primitives/whitewater_distance.rs` keeps its name, moves crate at stage 3) | with `upwind_distance`, `redistance_lattice`, `offset_lattice` (its dependencies) |
| 5 | `DEFAULT_CAPACITY`, `MAX_CAPACITY` | `primitives/whitewater_step.rs` | `whitewater.rs` (pool vocabulary) | `whitewater_step` re-imports |
| 6 | `liquid_bricks` | surface area | liquid crate | D3; `clamp_liquid_to_solids` and the mesher import |
| 7 | `check_preset_extents`, `LiquidPreset`, `ExtentReport`, `ExtentError` | `liquid/extent.rs:22-101` | `testkit/preset_extents.rs` | split the file; per-node helpers stay |
| 8 | `liquid/conformance.rs` | seam | `testkit/conformance.rs` | move; `liquid/coupling.rs`'s cfg(test) use of it moves with the test that uses it |
| 9 | `liquid/migration.rs` | seam | `migration/liquid.rs` | move; registrations unchanged |
| 10 | `primitives/gpu_flip_preset.rs` (+ `/testkit.rs`) | gpu-flip area | `presets/gpu_flip.rs` | move; `manifold-app/src/ui_bridge/project.rs:24,51` re-pointed to `manifold_nodes_water::presets::gpu_flip::…` |
| 11 | `primitives/testkit.rs`, `testkit/{face_grid_scenes,whitewater_scene,whitewater_fingerprints}` | glue | each constructor beside the crate that owns the node; scene builders to the registration testkit | split by node ownership (sections 3.1–3.6) |

Re-derivation at stage 1 entry: `scripts/water_crate_edges.py crates/manifold-nodes-water/src`; a CUT row not in this table is listed in the stage report before any edit.

---

## 4. Interfaces — what is `pub`, and why

Rule (RENDERER_CRATE_SPLIT D8): visibility widens only on compiler demand during the carve, each widening listed in the stage report, reviewed once at stage 5. What follows is the surface the design *expects* the compiler to demand; anything beyond it is an escalation line, not a silent `pub`.

### 4.1 `manifold-water-rigid`

- `physics::{RigidBody, RigidSimulation, RigidSceneInputs, RigidSceneObservation, ResolvedRigidImpulse, pose_from_transform, installed_volume, MAX_BODIES, MAX_COPIES, BODY_PORTS, POSE_PORTS, DEFAULT_DENSITY, ColliderGeometry}` — the adapter the liquid's rigid owner drives (`liquid/coupling.rs`, `liquid/bodies.rs`) and the body source nodes publish.
- `coupled_frame::{CoupledRigidFrame, CoupledRigidLayout}` — the frame handed across the pair.
- `physics_events::{ResolvedNodeImpulse, map_rigid_receipt}`; `physics_mesh::{prepare_colliders, load_compound_materials, …}` (`fluid_role_source` cooks colliders through it); `vector_field::ContinuousField`.
- `node::{PhysicsNode, PhysicsNodeRegistration, get, get_mut}` — the pair contract (D2).
- Until 6.8 (D11): `physics::{PhysicsStepScope, PhysicsAuthoredSampleScope, offline_simulation, simulation_interval, authored_sample_only, particle_frame_duration}` and `physics_metrics::*`. After 6.8 these are gone; the values arrive as arguments.
- `testkit::physics_fixtures` under `testkit`.

### 4.2 `manifold-water-liquid` — the liquid contract

What a solver gets, and all it gets, from the seam:

- `clock::{LiquidClock, ClockFrame, FIELD_RESERVE_INTERVALS}`; `tick_samples::TickSamples`; `substep_history::*` (the accepted-substep MAC-face seam).
- `lattice::{LiquidLattice, FlipSolverGrid, PADDING_NODES, SURFACE_PADDING_CELLS, SURFACE_EXTRA_NODES, MAX_LATTICE_NODES, closed_faces}` (cut 3).
- `grid::{FACE_GRID_PORTS, FACE_INPUT_PORTS, face_dims, face_len, face_bytes, interior_len, interior_bytes, face_index, face_coords, face_position, InteriorOps, PublishedFaces, stats_failed}` (cut 2).
- `bodies::{LiquidBody, LIQUID_BODY_SPECS, LiquidShape, BodySupports, pack_supports, unpack_supports, GpuFlipBodyVertex, LIQUID_POSE, LIQUID_COLLIDER}` (cut 1); `body_buffers::*`.
- `coupling::{LiquidRigidOwner, PendingTick, HandoverError, HANDOVER_BOUND, REACTION_FLOATS, takes_reaction, decode_reaction, coupled_start}`.
- `fields::{FieldLattice, FieldBinding, LIQUID_FIELD, …}`; `frame_ring`, `frame_history`, `display_cursor` as today.
- `fluid_role::{FluidRole, FluidRoleKind, PreparedFluidGeometry, DistanceState, DISTANCE_NODES_ALONG_LONGEST}`; `fluid_particles::{FaceSample, CellRange, bin_counts, …}`; `read_roles`, `ROLE_PORTS`, `WATER_DENSITY`.
- `whitewater::*` (grid records and helpers, cut 5's constants).
- `extent::{liquid_lattice, node_extent, PARTICLE, provide_frame_faces, cover_frame_faces, search_fits, searched, required_blob_bounds, brick_schedule, surface_mesh_pass, body_rows, whitewater_lattice, whitewater_grid, whitewater_faces, KNOWN_VALUE, particle_map, particle_values, whole, nodes_total, lattice_total, field_reads}` — today `pub(crate)`; every leaf's `extent.rs` needs them, so they widen. This is the one planned widening of size; the names are the existing ones.
- The shared atoms' node types and the items leaves already import from them (`sort_particles_into_cells::{int_param, …}`, `prefix_scan::PrefixScan`, `liquid_stats::{LIQUID_STATS_WORDS, NARROW_BAND_SHORTAGE_WORD, SOLVER_WORDS, with_stats_layout, LiquidTickStats}`, `liquid_bricks::{COMMON, schedule_words, valid_schedule, dispatch, WIDTH, HEADER, GRID_OFFSET}`, `whitewater_distance::{SurfaceDistance, scratch_bytes}`, `liquid_cells::LiquidCells`, `particle_publication::scratch_bytes`, `face_sample_component::{FaceSampleComponent, axis_param}`, `running_total::EXTENT_GRID_OFFSET`).
- `testkit::{fluid_role_source, liquid_extents, particle_volume, liquid_surface}` under `testkit`.

Why it is small enough: nothing solver-specific is in it. `rg -n 'gpu_flip|matter_|whitewater_step' crates/manifold-water-liquid/src --glob '!**/whitewater.rs'` → 0 is the INV-W2 companion check for liquid.

### 4.3–4.6 The four leaves

A leaf exposes its node types (`primitive!` already makes them `pub` structs), the `pub` items catalog tests already import (the external-callers row of section 1.1 and the test tree), and its `testkit` module. Nothing else. No leaf is a dependency of another, so a leaf's `pub` is for tests and the registration crate only. Expected demands: gpu-flip `gpu_flip_domain::{GpuFlipGeometry, gpu_flip_geometry}`, `gpu_flip_step::{GpuFlipStep, FACE_VALID_LAYERS, set_force_*}` (test levers, 6.9 folds them into one `Levers` struct), `gpu_flip_volume`, `gpu_flip_still` (probe helpers, feature-gated); matter `matter::*`, `look::*`, `reference::*`; whitewater `whitewater_step::{WhitewaterStep, WHITEWATER_STEP_SHADER, FaceSource}`, `whitewater_type::WhitewaterType`, `spawn_whitewater::SpawnWhitewater`, the emitter constructors; surface `blob_bounds::BlobBounds`, `volume_surface_mesh::VolumeSurfaceMesh`, `surface_mesh_normals::SurfaceMeshNormals`, `smooth_surface_mesh::SmoothSurfaceMesh`, `particle_volume::ParticleVolume`.

### 4.7 `manifold-nodes-water` (registration)

- `runtime::{WaterRuntime, WaterRuntimeRef, WaterRuntimeExt, scene_impulses, physics_sampling::execute_physics_sample_frame, physics_impulses, testkit}` — as today.
- `presets::gpu_flip::{gpu_flip_liquid_body, LIQUID_BODY_OUTPUT, STEP_NODE, render_def, WaterScene, DAM_OBSTACLE, REST_PER_CELL, with_whitewater_axes, testkit}` — the app's authoring recipe and the proofs' scene builders.
- `prewarm_pipelines(device: &Arc<GpuDevice>)` — one function calling `PhysicsWorldNode::prewarm_pipeline` (and nothing else today); `manifold-nodes/src/registry.rs:74` calls it instead of reaching a leaf. This is a registration-layer function, not a re-export: the catalog asks the water family to warm itself.
- `testkit::{conformance, preset_extents, face_grid_scenes, whitewater_scene, whitewater_fingerprints, physics_history}` under `testkit`.
- Nothing from a lower crate is re-exported (D13).

### 4.8 Consumers

| Consumer | Today | After |
|---|---|---|
| `manifold-nodes/src/registry.rs:74` | `manifold_nodes_water::primitives::physics_world::PhysicsWorldNode::prewarm_pipeline` | `manifold_nodes_water::prewarm_pipelines` |
| `manifold-app/src/content_pipeline.rs:2154-2160` | `physics::{LiveLoad, PhysicsStepScope::for_frame}` | After 6.8: values passed into the frame call; `LiveLoad` from `manifold_physics::clock` (D11) |
| `manifold-app/src/{content_thread,frame_time,content_pipeline}.rs` | `physics_metrics::*` | After 6.8: types from `manifold_physics`, values from the runtime extension (D11) |
| `manifold-app/src/ui_bridge/project.rs:24,51` | `primitives::gpu_flip_preset::…` | `manifold_nodes_water::presets::gpu_flip::…` |
| `manifold-nodes/tests`, `manifold-app/tests` water modules | `manifold_nodes_water::{liquid, matter, physics, primitives::…, testkit::…}` | The owning crate's path; `[dev-dependencies]` on that crate with `features = ["testkit"]` |

---

## 5. Registration and linking

`manifold-nodes-water/src/lib.rs` after stage 4:

```rust
//! Water registration and runtime. Links every water crate so their nodes,
//! wire payloads, extent rules and migrations register; owns the runtime
//! extension, graph installation, migrations and the cross-solver testkit.
//! Never depends on UI, editing, IO, the app, compositor, UI paint or another node family.
use manifold_water_gpu_flip as _;
use manifold_water_gpu_mpm as _;
use manifold_water_whitewater as _;
use manifold_water_surface as _;
use manifold_water_liquid as _;
use manifold_water_rigid as _;

mod graph_install;
mod migration;
mod physics_scene;
pub mod presets;
pub mod runtime;
pub fn prewarm_pipelines(device: &std::sync::Arc<manifold_gpu::GpuDevice>) { … }
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
```

Who registers what: nodes, extent rules and `PhysicsNodeRegistration`s in the crate that defines the node; wire payloads in the crate that defines the type (D6); `GraphMigration`s in `manifold-nodes-water/src/migration/` except `wire_blob_bounds` (D4); `RuntimeRegistration` and the `GraphInstantiationHook` in `manifold-nodes-water`. The engine's inventory iteration order is link order and not stable (RENDERER_CRATE_SPLIT D5), which is why migrations carry explicit `order` values; a move does not change them.

---

## 6. Tests, testkits, proofs and the gate

### 6.1 Per-crate test targets

Each crate: lib tests (`#[cfg(test)]` sibling `_tests.rs` modules as today) and lib `gpu_tests` under `#[cfg(all(test, feature = "gpu-proofs"))]`. Integration targets only where a crate owns proofs that need a `tests/` binary today (the gpu-flip pressure fixtures). No `autotests = false` folding is required for the leaves; the registration crate keeps none of the catalog's binaries. 6.9's `proofs/` tree lands per crate afterward.

### 6.2 Testkit placement

A testkit module lives in the lowest crate whose tests use it and references only that crate or lower (section 3 places each). The engine's `testkit::water_codegen` (named by BUG-hkbdp.6.3 (scene types out of the engine) for a move into water) lands in `manifold-water-liquid::testkit::codegen`, the lowest crate whose proofs build codegen members; the whitewater `member` fixtures in the whitewater crate's testkit call it.

### 6.3 Contract suites — the behaviour the compiler cannot see

The compiler stops a signature change from rippling. A behaviour change behind an unchanged signature is caught only by the tests at that seam, so each interface owner has one named suite that always runs when the owner changes. Names are today's libtest filters; BUG-hkbdp.6.11 (water test cut) may rename or delete members and must update this table in the same commit (its row is the gate for that).

| Interface owner | Contract suite (always runs when the owner's crate changes) | Why these |
|---|---|---|
| `manifold-water-rigid` (CS-rigid) | CPU: `manifold-water-liquid` `coupling::`, `manifold-nodes-water` `runtime::physics_`, catalog `catalog_tests::physics_{impulses,sources,carry,sampling}`, `physics_scene`, app `catalog_tests::physics_impulses`; GPU: `water_basin::authored_coupling`, `liquid_conformance::liquid_coupled_`, `physics_boxes::`, `physics_solids::` | The rigid owner inside the seam, the pair scheduler, authored impulses and recorded sampling are the four consumers of the adapter |
| `manifold-water-liquid` (CS-liquid) | Every leaf's seam proofs: `liquid_conformance::` (all solvers), `contracts::water::`, `face_grid_tests::`, `matter_scene::`, `matter_coupling::`, `whitewater_golden_tests::`, `liquid_surface_tests::`, `fluid_indexed_`, `liquid_indexed::`, plus CS-rigid | Every solver implements this contract; a seam change is a change to all of them. Honest cost: a liquid edit is the broad water run, by design |
| `manifold-water-gpu-flip` | Own module filters (automatic) + `gpu_flip_` rows, `liquid_conformance::gpu_flip_`, `liquid_conformance::liquid_live_flip`, `gpu_flip_scene_tests::`, `catalog_tests::gpu_flip_body::` | The solver's seam conformance |
| `manifold-water-gpu-mpm` | Own filters + `matter_`, `substeps_`, the `liquid_conformance::` members that build Matter (⚠ VERIFY-AT-IMPL: `rg -n 'matter' crates/manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs`) | Same |
| `manifold-water-whitewater` | Own filters + `whitewater_` rows, `whitewater_golden_tests::`, `catalog_tests::whitewater_scene::`, `catalog_tests::whitewater_emitters::` | Same |
| `manifold-water-surface` | Own filters + `liquid_surface_tests::`, `surface_mesh_freeze_tests::gpu_tests::`, `volume_surface_mesh::gpu_tests::`, `fluid_indexed_`, `liquid_indexed::`, `catalog_tests::{liquid_surface,liquid_bricks_gpu,particle_volume,surface_mesh_normals,blob_bounds}::` | The particle-frame seam and the mesh contract |
| `manifold-nodes-water` | `runtime::`, `migration_order_matches_table`, the LiveSchool round-trip (`manifold-io` `load_project`), `catalog_tests::gpu_flip_preset::`, `catalog_tests::liquid_prepare::`, `gpu_flip_render_smoke::`, the CS-rigid runtime rows | Registration, migration and runtime glue |

Re-derivation command for the member list before each stage: `rg -n 'fn [a-z_]+\(' crates/manifold-nodes/tests/contracts/water crates/manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs | wc -l` against the stage report's count, and `scripts/gpu_scope.py --describe <path>` for the rows a path selects.

### 6.4 Gate mapping changes

`scripts/gate_policy.py`:

- `WATER_SRC` becomes `WATER_SRCS = ("crates/manifold-water-rigid/src/", "crates/manifold-water-liquid/src/", "crates/manifold-water-gpu-flip/src/", "crates/manifold-water-gpu-mpm/src/", "crates/manifold-water-whitewater/src/", "crates/manifold-water-surface/src/", "crates/manifold-nodes-water/src/")`; every `WATER_SRC + "…"` row is re-keyed to the crate that now owns the file (mechanical: the section 3 table is the map). `gpu_scope.py::SOURCE_ROOTS` and `is_gpu_path` take the tuple.
- `PRIMITIVE_PATHS` gains each water crate's `src/primitives/`; `PREFIX_ROWS` WGSL rows (`uniform_layout_extended`, `wgsl_validation`) per crate; `GPU_DEFAULT_CPU_ONLY` one line per crate with the same reason text as today's water row.
- `BROAD_PATHS`: `manifold-water-liquid/src/lib.rs` and `manifold-water-rigid/src/lib.rs` map to a new `WATER_BROAD_FILTERS` (the CS-liquid suite), not to `BROAD_FILTERS`; each leaf's `lib.rs` maps to that leaf's rows; `manifold-nodes-water/src/lib.rs` maps to the registration rows. A crate root is never "everything".
- Contract suites as `PREFIX_ROWS` entries keyed by crate root: `(root, ".rs", package, modules, binaries)` per section 6.3 row, so `cpu_scope.py` selects the suite whenever any file under the root changes. GPU members go in `EXPLICIT_ROWS` keyed by the same roots.
- `INTEGRATION_ROWS["crates/manifold-nodes-water/src/fluid.rs"]` is deleted with 6.6.
- `LIB_PROOF_ROWS`, `NARROW_ROWS`, `CATALOG_TEST_ROWS`: re-keyed paths, same filters.

`crates/manifold-nodes/src/testkit/source_roots.rs`: one `PRIMITIVE_SOURCE_ROOTS` and one `WGSL_SRC_ROOTS` line per crate. `.claude/hooks/context-nudges/table.json:21`: add the leaf `primitives/` globs. `scripts/feature_matrix.py` derives rows from `cargo metadata` and needs no edit. `scripts/gpu_proofs_gate.py` discovers the new crates by their `gpu-proofs` feature and needs no edit; INV-W7 is its existing hard failure on an unmapped GPU path, which is how a missed row surfaces at the first landing.

What a leaf edit then runs: `cpu_scope.py` → the changed modules' filters in the leaf package plus the leaf's contract row; `gpu_scope.py` → the leaf's GPU rows plus the fixed smoke set. Not liquid's, not another leaf's. What a liquid edit runs: liquid's modules plus CS-liquid. That is the behaviour the bead asks for, and it is the existing machinery with new keys.

---

## 7. Hot paths

No function body changes in any move. What changes is that the per-tick CPU orchestration now calls across crate boundaries. Rust 1.94 (the toolchain here) inlines small non-generic functions across crates automatically in optimised builds; generic functions (`RigidSimulation::advance_with_coupling<C: StepCoupling>`) monomorphise in the caller's crate. The calls to review are the ones inside per-body or per-row CPU loops that cross a boundary:

| Call | From → to | Frequency |
|---|---|---|
| `pose_from_transform` | `liquid/bodies.rs` → rigid | per body row per tick |
| `pack_supports`/`unpack_supports`, `body_pose_at` | gpu-flip step / matter domain → liquid `bodies` | per coupled body per tick |
| `decode_reaction`, `takes_reaction`, `LiquidRigidOwner::settle_ready` | solver → liquid `coupling` | per pending tick, per body |
| `LiquidLattice::{cell_size,min,nodes,cells}`, `grid::{face_len,face_bytes,face_index}` | every leaf → liquid | per dispatch argument, tens per tick |
| `node::get`/`get_mut` | registration `physics_scene`, `runtime/state` → rigid | per pair per frame; already a hash lookup and a fn-pointer call (BUG-hkbdp.6.4 (seam review) seam 4 may cache it at plan compile; unchanged by this design) |
| `whitewater_distance::SurfaceDistance::encode`, sort/scan atom `encode` | whitewater, gpu-flip → liquid | per tick; GPU encodes, CPU cost is argument setup |

Rule: the executor adds `#[inline]` to the non-generic functions in the first three rows (they are called per body in `for` loops; `pose_from_transform` is the one named by the census) and nothing else without a measurement. Gate (INV-W6): `target/debug/examples/fluid_capture` frame timings on `WaterDamBreakGpuFlip` and `WaterDamBreakMatter` before and after each carve stage, interleaved A/B per `feedback_frame_timing_method`, tick interval not surface wait; a difference outside noise is a stop. Per-frame allocation is unchanged by construction (no body changes); `MANIFOLD_RENDER_TRACE=1` is not needed for a move stage and is run once at stage 5 on the two presets as the content-thread gate.

---

## 8. Invariants and enforcement

| Invariant | Enforcement |
|---|---|
| INV-W1 Water Cargo edges are exactly the D1 table; no leaf depends on a leaf; nothing below depends on above; dev-deps never point up | `crates/manifold-app/tests/crate_layering.rs` `LAYERS` rows for the six crates and the updated `manifold-nodes-water` row (stage 2–4 deliverables) |
| INV-W2 Rigid names no liquid | In `crate_layering.rs`: `rg -c -i 'liquid|whitewater|matter|gpu_flip' crates/manifold-water-rigid/src` → 0 outside `src/node.rs` (the pair contract's doc comments); same shape as the engine word census BUG-hkbdp.6.2 (coupled-scene preparation) adds. Companion for liquid: `rg -n 'gpu_flip|matter_|whitewater_step' crates/manifold-water-liquid/src --glob '!**/whitewater.rs'` → 0 |
| INV-W3 Every water crate declares exactly `testkit` and `gpu-proofs` | `crate_layering.rs` reads `packages[*].features` from `cargo metadata`; until 6.9 closes, the four extra flags are allowlisted per crate by name with the 6.9 bead id in the comment, and the test fails if a flag appears on a crate not in the allowlist |
| INV-W4 The registry is complete after every stage | `cargo run -p manifold-nodes --bin gen_node_catalog -- --check` prints `node catalog in sync` (re-record the node count at stage 1); `check-presets` exit 0 through `gpu_queue.py` |
| INV-W5 Saved projects and presets load byte-identically | `project_tool` round-trip of the LiveSchool fixture byte-identical in the main checkout; `WaterDamBreakGpuFlip` and `WaterDamBreakMatter` stills pixel-diff 0 against main (five stills each, `fluid_capture` via `gpu_queue.py`); `migration_order_matches_table` green |
| INV-W6 No per-frame change | section 7 timing gate per carve stage |
| INV-W7 Every GPU path is mapped | `gpu_scope.py` hard failure on an unmapped path (existing); the stage gate runs `scripts/gpu_proofs_gate.py` in scoped mode and a selection of nothing for a touched kernel is a mapping gap, stop |
| INV-W8 Linking proven | the six `use … as _;` lines in `manifold-nodes-water/src/lib.rs` + INV-W4 |
| INV-W9 No test lost in a move | `cargo nextest list --workspace -E 'package(~manifold-water) | package(=manifold-nodes-water) | package(=manifold-nodes) | package(=manifold-app)' --message-format json` test-name multiset (crate prefix stripped) equal before and after each stage; the diff printed by name in the stage report. The retired `test_census.py` did this; the one-liner replaces it for this campaign |
| INV-W10 Every move is a rename | `git diff -M --stat <commit>^ <commit>` shows every moved file as `R` at 50% or more; a file below the threshold lands as two commits, move then rewrite |

---

## 9. Phasing

Common to every stage: one lane per stage (stage 4: one lane per leaf, disjoint files) in its own slot worktree (`scripts/agent-worktree.py acquire`); the brief carries this doc's section numbers, the entry commands and the gate; the lane makes one commit and stops; the lead reviews against INV-W10 and INV-W9 first, then lands with `scripts/land_branch.py`. Rules of the move: `git mv` per file (one move per file, never a copy), path rewrites in the same commit, no body edits in a move commit, seam edits in their own commit before the move. Visibility widens on compiler demand only and is listed in the report (D8 precedent). Reports carry `Shortcuts taken:` and `Demo artifact:`.

Order against open work: stages 1–5 start after `lane/water-cpu-flip-removal` (6.6), `lane/water-engine-seam-moves` (6.2), `lane/water-scene-types` (6.3) and `lane/water-seam-review` (6.4) land; stage 2 also waits for 6.8 (D11). Any other open branch touching `crates/manifold-nodes-water` lands before stage 2 or merges main after each carve (`git merge` follows whole-file renames; a branch touching a file 6.7 later splits lands before 6.7). The lead checks `git branch --no-merged origin/main` for water branches at each stage entry. 6.7, 6.9 and 6.11 follow stage 5, per crate.

### S1 — Cuts inside the crate (one lane, one commit)

- **Entry:** 6.6, 6.2, 6.3, 6.4 on main. `scripts/water_crate_edges.py` (copied from the scratchpad path in the bead) prints the section 1.2 table; a differing row is listed before editing.
- **Read-back:** sections 1.2, 2 (D3, D4, D9, D12), 3.9. Restate the eleven cuts and the D4 relocations.
- **Deliverables:** the eleven cuts of section 3.9 and the D4 relocations (`migration/`, `presets/`, `testkit/conformance.rs`, `testkit/preset_extents.rs`, the `primitives/testkit.rs` split); `TICK` to `manifold_physics::clock::TICK` (D12) if 6.6 has not already done it; `CoupledRigidFrame`/`CoupledRigidLayout` in `src/coupled_frame.rs` if 6.6 left them elsewhere; `scripts/water_crate_edges.py` with a `dev.py` verb (`scripts/test_dev.py` enforces the table); `manifold-app/src/ui_bridge/project.rs` re-pointed to `presets::gpu_flip`.
- **Gate:** `scripts/water_crate_edges.py` → zero CUT rows; `cargo nextest run -p manifold-nodes-water`; `cargo clippy -p manifold-nodes-water -p manifold-app --tests -- -D warnings`; `scripts/gpu_proofs_gate.py` scoped (cut 2 touches `gpu_flip_step.rs`, so the `gpu_flip_` row runs — accepted once); INV-W4, INV-W5, INV-W9.
- **Demo:** L2 — the two preset stills, pixel-diff 0 (Peter looks; agents diff).
- **Forbidden:** any new trait or generic to make a cut compile (a function that needs one was cut in the wrong place — stop); moving a file into a new crate; re-exports; "while I'm here" edits.

### S2 — Carve `manifold-water-rigid` (one lane, one commit)

- **Entry:** S1 landed; 6.8 landed (or the D11 default recorded in the report); `scripts/water_crate_edges.py` → zero CUT rows.
- **Read-back:** D1 rigid row, D2, D5, D6, D7, D11, sections 3.1, 4.1, 5, 6.4, 8.
- **Deliverables:** `crates/manifold-water-rigid/{Cargo.toml,src/lib.rs}`; `git mv` of the section 3.1 files; path rewrites (`crate::physics::` → `manifold_water_rigid::physics::` in every importer, including `manifold-nodes/tests` and `manifold-app/tests`); wire registrations for `RigidBody` and `FieldValue` moved; workspace members; `manifold-nodes-water` depends on it; `crate_layering.rs` row + INV-W2 word census + INV-W3 feature pin (first landing of both); `gate_policy.py` rows re-keyed for the moved paths; `source_roots.rs` lines; `manifold-nodes/Cargo.toml` dev-dependency with `testkit` where its tests construct rigid types.
- **Gate:** INV-W1, W2, W3, W4, W5, W7, W8, W9, W10; `cargo clippy -p manifold-water-rigid -p manifold-nodes-water -p manifold-nodes -p manifold-app --tests -- -D warnings`; `scripts/gpu_proofs_gate.py` scoped (expect the CS-rigid GPU members); section 7 timing A/B on both presets.
- **Demo:** L2 — stills pixel-diff 0.
- **Forbidden:** resolving a compile error by moving a liquid item into rigid (the compiler's demand beyond section 3.1 is an escalation line); any `pub` not demanded by the compiler; editing a function body.

### S3 — Carve `manifold-water-liquid` (one lane, one commit)

- **Entry:** S2 landed.
- **Read-back:** D1 liquid row, D3, D7, D8, D9, sections 3.2, 4.2, 6.2.
- **Deliverables:** as S2 for the section 3.2 files; `FluidRole` wire registration moved; the `extent` helpers widened to `pub` (the one planned widening); `testkit::codegen` from the engine's `water_codegen.rs` (if 6.3 left it in the engine); layering row; gate rows (`WATER_BROAD_FILTERS` for `lib.rs`); `source_roots.rs`.
- **Gate:** as S2 plus the INV-W2 liquid companion check; expect the CS-liquid run once (this landing *is* a liquid change).
- **Forbidden:** as S2; additionally moving any `gpu_flip_*`, `matter_*` or `whitewater_step*` file here.

### S4 — Carve the four leaves (four lanes, disjoint files; landing order gpu-flip → matter → whitewater → surface)

- **Entry:** S3 landed. Each later lane merges main before its gate (landing protocol).
- **Read-back:** D1 row, sections 3.3–3.6, 4.3–4.6, the section 6.3 row for the leaf.
- **Deliverables (each):** new crate; `git mv`; path rewrites in the leaf, the registration crate and the catalog/app tests; layering row; gate rows for the leaf (its contract suite as the crate-root row); `source_roots.rs`; the registration crate's `use … as _;` line; `[dev-dependencies]` in `manifold-nodes` and `manifold-app` for tests that construct the leaf's nodes. The gpu-flip lane also adds `prewarm_pipelines` to `manifold-nodes-water` and re-points `manifold-nodes/src/registry.rs:74` (one function; the only production prewarm is rigid's).
- **Gate (each):** INV-W1 through W10; `cargo clippy -p <leaf> -p manifold-nodes-water -p manifold-nodes -p manifold-app --tests -- -D warnings`; `scripts/gpu_proofs_gate.py` scoped must select the leaf's rows (a selection of nothing is a mapping gap: stop); section 7 timing A/B.
- **Demo:** L2 — stills pixel-diff 0 after each landing.
- **Shared-file conflicts:** `Cargo.toml` (members), `crates/manifold-nodes-water/{Cargo.toml,src/lib.rs}`, `crates/manifold-nodes/Cargo.toml`, `crates/manifold-app/{Cargo.toml,tests/crate_layering.rs}`, `scripts/gate_policy.py`, `source_roots.rs`, `.claude/hooks/context-nudges/table.json`. Each later lane merges main before its gate.
- **Forbidden:** touching another lane's files; resolving an import by adding a leaf-to-leaf dependency (INV-W1 fails; the item belongs in liquid by D3 — stop and escalate, do not move it yourself in a leaf commit); "fixing" a test that moved.

### S5 — Review, measurement, retirement (lead + one lane)

- **Entry:** S4 landed.
- **Deliverables:** the widening review (D8) as one commit per crate narrowing what the review rejects; crate `lib.rs` doc headers in the house voice; `scripts/water_crate_edges.py` and its verb deleted (Cargo is the oracle now); `CLAUDE.md` crate table rows for the six crates; `docs/DEVELOPMENT_REFERENCE.md` module layout; this doc's status line → `SHIPPED` with the supersession sweep (`rg 'manifold-nodes-water/src/(liquid|physics|matter|whitewater|primitives)' docs/ memory` → each hit re-pathed or tombstoned; BUG-hkbdp.6.10 (water map doc) owns the water design-doc lifecycle sweep and cites this doc as the crate contract); the after-measurement: warm edit-rebuild timings for one file per crate, same method as RENDERER_CRATE_SPLIT's post-T1 baseline, written into section 1.1 (The crate today).
- **Gate:** `cargo clippy --workspace -- -D warnings` once (the only workspace sweep of the campaign — six-crate landing, justified); `scripts/landing_gate.py`; `MANIFOLD_RENDER_TRACE=1` run on both presets (content-thread gate, no frame over 20 ms beyond main's); `design_status.py --lifecycle-check`.
- **Demo:** none — L1. The measurement and the layering test are the artifact.

Phasing-completeness check: every D1 crate appears in exactly one stage's deliverables (rigid S2, liquid S3, four leaves S4, registration crate reshaped across S1–S4); D4 S1; D6 S2/S3; D7 S2–S4; D11/D12 S1–S2; INV-W1–W3 first landing S2; INV-W9/W10 every stage; the measurement S5. 6.7, 6.9 and 6.11 are not phases of this doc; they run per crate after S5 and cite this doc for crate lines.

---

## 10. Decided — do not reopen

1. Seven crates (D1); rigid below liquid, never a leaf.
2. The pair contract lives in rigid (D2); no `manifold-water-core`.
3. An atom two solvers use is a liquid-crate atom (D3).
4. Migrations, preset builders and cross-solver harnesses live in the registration crate (D4).
5. Linking proven by `use … as _;` and the catalog census (D5).
6. Registration beside the type (D6); features exactly `testkit` + `gpu-proofs` (D7).
7. Tests live in the lowest crate that links their nodes; module names unchanged (D8); no upward dev-deps (D9).
8. Cut inside, carve bottom-up, split god-files after (D10).
9. 6.8 before stage 2; step and metrics types in `manifold-physics` (D11); `TICK` in `manifold_physics::clock` (D12).
10. No facade, no re-export, no transitional crate (D13).
11. Layering, word census and feature pin are tests over `cargo metadata` and `rg` (section 8).

## 11. Deferred

- **`manifold-physics-gpu` (PHYSICS_ENGINE_BOUNDARY G1b).** Numerical kernels below the water crates. Trigger: that design's own; the liquid crate is its natural first consumer.
- **A `proofs/` tree per crate with one cfg and a `Levers` struct.** That is BUG-hkbdp.6.9 (proofs consolidation), unchanged, now one tree per crate.
- **Folding the four extra features.** 6.9; INV-W3 ratchets when it closes.
- **Caching the `PhysicsNode` lookup at plan compile.** BUG-hkbdp.6.4 (seam review) seam 4; the crate boundary neither helps nor hurts it.
- **Per-crate `gpu_queue.py` locks.** One GPU, one lock; nothing here changes that.
- **Renaming `whitewater_distance.rs` (it is the solver-neutral level-set sweep).** A rename is its own commit after the carve, if ever; names are not serialized and the test filters key on it today.
