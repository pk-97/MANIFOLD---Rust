# Whitewater Stage Fusion — fuse the stage's internal atom chains, drop its grid copies, retire the face adapters

**Status:** IN PROGRESS · 2026-10-06 · P0 (golden fingerprints) and P1 (copies) on main · owed: P2–P5.
**Prerequisites:** none. The display-history landing (`85226c917`) is on main; this design touches nothing it owns.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

**The governing insight: `node.whitewater_step` already is the stage node section 1.2 asks for, but inside it still runs the atom catalog one dispatch at a time.** Its `atom()` helper (whitewater_step.rs:511-520) dispatches a `standalone_for_spec` pipeline per registered atom and barriers after each, so a tick of the shipped GPU FLIP Dam Break issues 86 compute dispatches and 3 blits (section 1.3 census). Twelve of those are per-particle atoms with no cross-particle dependency between them, two are zero-width padding copies of lattices the stage could read in place, and one is a full-lattice copy that a buffer-index swap replaces. None of this is visible to freeze; all of it is stage-internal under DECOMPOSING_GENERATORS.md section 1.2 and ADDING_PRIMITIVES.md exclusion 6, so the fix is hand kernels inside the stage, proven at the boundary, not new atoms and not the region compiler.

Peter, 2026-10-06: "I don't want a huge mess of nodes that aren't fully optimised ... Users won't touch these GPU FLIP nodes"; on the audit: "Yes, fantastic findings, those look like more free optimisation wins"; "The Sim and physics API is supposed to use one unified grid for all of these things ... one map for all grid based solves and renders."

Peter, 2026-10-01 (DECOMPOSING_GENERATORS.md section 1.2): "GPU FLIP is one step node plus one whitewater node, not five stages." and "No internal pass is used twice outside the solver. If it is, it stays a catalog atom that the stage wires to or calls."

Equivalence bar, from the brief and carried as invariants I1–I3: bit-identical whitewater particle state per tick (pool, state words, counts, four population arrays) on the shipped Dam Break and on a fixture with every emitter type and dust on; `gpu_flip_frame_perf` whole-frame hashes unchanged; the existing whitewater parity proofs green. `MTLMathMode::Fast` (crates/manifold-gpu/src/metal/device.rs:760) means two differently written kernels agreeing bitwise is a test result, never a construction, so the old atom pipelines stay in the tree as in-test oracles (D4).

Companion docs: `GPU_WHITEWATER_DESIGN.md` (the stage's own design; section 3.9 "As built: one stage"; section 6 decided items stand), `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` section 11 (the migration-rung precedent and the node-measure method), `LIQUID_SOLVER_SEAM_DESIGN.md` section 3.2 (the face-array seam contract the adapters implement), `GPU_FLIP_PRESSURE_CAP_DESIGN.md` C0 (the golden-fingerprint precedent).

---

## 1. Audit — what exists (verified 2026-10-06 at `5a1218c89`)

Every claim below is anchored. Extend, don't redesign: the stage, its hand kernels, its scan/sort helpers, and the atoms' WGSL bodies are the material; nothing here is rewritten from memory.

### 1.1 The stage and its internal chains

| Piece | Where | State |
|---|---|---|
| Stage node, inputs `face_u/v/w` **required**, `distance`/`obstacle_source`/`pool`/`pool_state` optional | `crates/manifold-renderer/src/node_graph/primitives/whitewater_step.rs:84-218` (inputs :88-119) | tick mode when `distance` is wired (:1498); legacy level-set mode otherwise (:1547-1568) |
| `atom::<P>()` / `atom_then` / `dispatch` — one standalone pipeline per atom, barrier after each, **16-binding cap** (uniform + 15 buffers) | :488-533 (`[GpuBinding; 16]` at :500) | the fusion boundary the audit found |
| `Pipelines` — 24 codegen atom pipelines + 9 hand entries from `whitewater_step.wgsl` | :535-647 | prepared at install (:612-642) |
| `Fields` — crossings×2, distance, surface, cells, curvature×2, turbulence, influence×2, empty_source, **spawns**, typed, pools×2, order, state, scan | :670-708 | `spawns` (:701) exists only as the spawn→type intermediate |
| `ParticleScratch` — 5 buffers per liquid-particle slot: jittered, sampled, energy, wavecrest, inside | :711-728 | wavecrest/inside only feed the count |
| `advance_tick` — copies pool + state in (2 blits), `emit`, `tick`, `publish` | :928-970 (blits :956-957) | boundary-owned pool, stage-owned scratch |
| `emit`: pad distance → surface distance → pad surface | :1056-1063 | both pads at `padding = face_offset(...)[0]` |
| `emit`: influence atom into `influence[1]` then **full-lattice copy back to `influence[0]`** | :1095-1101 | 68³×4 B = 1.26 MB blit per tick |
| `emit`: liquid_cells, curvature, extend×3, turbulence_field (grid passes) | :1104-1125 | stay |
| `emit`: **jitter → sample faces → emitter velocity → energy → wavecrest → inside → emission count** (7 dispatches over `emitters`) | :1131-1193 | the 7→1 target; `unscaled = sampled` kept at :1161 for dust |
| `emit`: emission scan, **spawn → type** (2 dispatches over `capacity`), live_flags, append scan, append, append_state | :1194-1219 | the 2→1 target |
| `emit`: dust path — dust_potential (writes into `inside`), energy over `unscaled` (overwrites `energy`), count with `rate 0`, `seed + 104729`, scan, spawn over `unscaled`, type with `dust 1`, live_flags/append/append_state again | :1220-1250 | sequential reuse of `energy`, `inside`, `offsets`, `spawns`, `typed` |
| `tick`: **advect → retype → age** (3 dispatches over `capacity`, a→b→a→b) | :1305-1316 | the 3→1 target; sort reads `b` (:1319) |
| `tick`: sort (9 dispatches), preserve_foam (optional), keep, keep scan, compact, compact_state | :1318-1383 | stay (real boundaries) |
| `publish`: split_flags, split scan, split, publish_counts | :1387-1398 | stay |
| Hand kernels: seed, live_flags, append, append_state, compact, compact_state, split_flags, split, publish_counts | `shaders/whitewater_step.wgsl` | atomic-free scatter by scan; stay |
| Zero-padding copy kernel | `pad_distance_lattice.rs:60-105`; `padded_extent` :31-38; GPU proof :197-226 | "deliberately a stage helper rather than a catalog primitive" (:4-6) |
| Padding rule: `pad = (cells − face_cells)/2`, integer and equal on every axis, else a named refusal | `node_graph/whitewater.rs:170-183` | at 64: cells = face_cells = 67 → **pad 0** (section 1.4) |
| Surface distance (engine reinit) | `whitewater_distance.rs:95-100` returns its `current` buffer; passes :160-230 | 4 + 6×5 = **34 dispatches per tick**, fixed iteration count, convergence via indirect args |
| Prefix scan: BLOCK 256, TAIL 16384, ≤4 levels; n ≤ 4.19 M → 2 levels → 3 dispatches | `prefix_scan.rs:16-51`, :224-238; `encode_into(src, dst)` :169-178 | stays |
| Sort: clear, count, scan(3), ranges, tail, scatter, stabilise = 9 | `sort_particles_into_cells.rs:228-242` | stays; the stage calls its code (rule 3 of section 1.2) |
| Shared WGSL: `ww_random(slot, seed, epoch, stream)`, grid helpers | `shaders/whitewater_common.wgsl:5-63` | the RNG every atom draws from |
| Shared WGSL: `lf_stencil`, `lf_face_index`, `lf_pad` (axis-array face indexing) | `shaders/liquid_faces.wgsl:9-35` | the axis-array read the packed read replaces |
| Shader-composition precedent: shared WGSL concatenated ahead of a hand shader, one-line `.replace` variant | `gpu_flip_step.rs:398-403` | the shape for the stage's fused shader |
| Test-lever precedent: a `cfg(all(test, feature = "gpu-proofs"))` atomic that switches an internal path | `gpu_flip_step.rs:233-246` (`set_gate_off`, `gating`) | the shape for the reference lever (D4) |
| Golden-fingerprint precedent | `GPU_FLIP_PRESSURE_CAP_DESIGN.md` C0 (`pressure_module_matches_main_golden`, `MANIFOLD_RECORD_GOLDEN=1`) | the shape for I1 |

### 1.2 The atom bodies the fusions will copy (operation order, RNG streams, dead-slot rules)

All under `crates/manifold-renderer/src/node_graph/primitives/shaders/`. The fused kernels copy this text phase by phase (D3); nothing here is re-derived from the engine.

| Body | Reads | Writes | Dead-slot / early-out rule | RNG stream |
|---|---|---|---|---|
| `jitter_particles_body.wgsl` | particles | particles | radius ≤ 0 passes whole | 0, 1, 2 |
| `sample_faces_at_particles_body.wgsl` | particles, face_u/v/w | particles (velocity) | radius ≤ 0 passes whole; outside grid → velocity 0 | — |
| `whitewater_emitter_velocity_body.wgsl` | particles, surface, cells | particles (velocity scaled) | scales only when \|d\|<1.5h ∧ borders air ∧ d > −0.75h | 9 |
| `energy_potential_body.wgsl` | particles | f32 | 0 for radius ≤ 0 or empty range | — |
| `wavecrest_potential_body.wgsl` | particles, surface, curvature, cells | f32 | 0 unless \|d\|<1.5h ∧ borders air; still/flat/sharpness gates | — |
| `inside_turbulence_potential_body.wgsl` | particles, surface, turbulence, cells | f32 | 0 **when** \|d\|<1.5h ∧ borders air (the complement) | — |
| `turbulence_emission_count_body.wgsl` | particles, energy, wavecrest, inside, influence (by emitter cell) | u32 | 0 past `live_count`, radius ≤ 0, \|v\|<1e-3, Ie<1e-6, coin ≥ generation_rate; **rounding `u32(floor(per_tick + 0.5)) × ticks`** | 10 |
| `dust_potential_body.wgsl` | unscaled particles, solid, turbulence, source | f32 | source kind 0 / strength ≤ 0 / domain rule / clearance ∉ [0, 2.5h] → 0 | — |
| `spawn_whitewater_body.wgsl` | offsets, particles, energy, face_u/v/w, solid | `WhitewaterSpawn` (lifetime 0 = empty) | slot ≥ min(total, capacity) → empty; thinning by `sw_mul_div(idx, total, slots)`; outside grid / solid < 0.25h / lifetime ≤ 0 → empty | 4 (radius), 5 (angle), 6 (height), 7 (variance) |
| `whitewater_type_body.wgsl` | spawns, surface, cells | spawns (kind) | lifetime ≤ 0 passes whole; `dust > 0.5` → kind 4, **no speed draw** | 11 (fresh spray speed) |
| `advect_whitewater_body.wgsl` | pool, face_u/v/w, solid, substep schedule/history, forces, impulses | pool | the outer loop applies impulses to every non-foam kind **before** `aw_step` returns kind 3 or > 4 unchanged (advect_whitewater_body.wgsl:322) — no early return on kind; non-finite travel → lifetime −1e6 | — |
| `retype_whitewater_body.wgsl` | pool, surface, cells, face_u/v/w | pool (kind, velocity on bubble→foam/spray) | kind > 2 passes; dead particles still retyped | — |
| `age_whitewater_body.wgsl` | pool | pool (lifetime) | kind 3 or > 4 passes; dust ages at 1/s | — |

Dust seeds: `frame.seed + 104729.0` for the dust count coin and dust spawn draws (whitewater_step.rs:1233, :1237); the normal seed in tick mode is `tick_index × TICK` (:1445).

Three bodies (emitter velocity, wavecrest, inside) each recompute the same `wc_borders_air(floor(q))` over `cells` and the same eight-corner trilinear of `surface` at `s = q − 0.5`; type and retype recompute the same classification. The engine computes each once per emitter (`diffuseparticlesimulation.cpp:1571-1606` sorts markers into surface/inside once; `:1688-1716` and `:1778-1800` then evaluate potentials on the sorted sets).

### 1.3 Dispatch census per whitewater tick (shipped Dam Break: dust off, preserve foam off, motion wired, pad 0)

| Group | Dispatches | Blits | Source |
|---|---|---|---|
| copy pool + state in | 0 | 2 | :956-957 |
| pad distance, surface distance, pad surface | 1 + 34 + 1 | 0 | :1056-1063, whitewater_distance.rs:160-230 |
| influence + copy-back | 1 | 1 | :1095-1101 |
| liquid_cells, curvature, extend×3, turbulence | 6 | 0 | :1104-1125 |
| emitter chain | 7 | 0 | :1131-1193 |
| emission scan, spawn, type, live_flags, append scan, append, append_state | 3+1+1+1+3+1+1 = 11 | 0 | :1194-1219 |
| advect, retype, age | 3 | 0 | :1305-1316 |
| sort, keep, keep scan, compact, compact_state | 9+1+3+1+1 = 15 | 0 | :1318-1378 |
| split_flags, split scan, split, publish_counts | 6 | 0 | :1387-1398 |
| **Total** | **86** | **3** | |

This design removes 11 dispatches and 1 blit per tick (2 pads, 1 influence blit, 6 from the emitter chain, 1 from spawn+type, 2 from the lifecycle): **13% of the dispatch count**. Said plainly: the count is dominated by the surface-distance reinit (34, fixed iterations) and the scan/sort/compaction boundaries (≈29), which the brief keeps. The win that scales with particles is the traffic, not the count: per emitter the chain today writes jittered (32 B), sampled (32), scaled (32), energy (4), wavecrest (4), inside (4), count (4) = 112 B and re-reads the record four times; fused it writes sampled (32), energy (4), count (4) = 40 B. Per spawn slot: 32 B spawn round trip gone. Per pool slot: two 48 B pool round trips gone (192 B/slot, 19.2 MB at capacity 100,000, as the audit computed). All unmeasured until section 7's A/B.

### 1.4 The grids this preset runs on (why the pads are zero)

| Piece | Where | Value at res 64 |
|---|---|---|
| Solver cells / corners / surface nodes | `node_graph/liquid/lattice.rs:37-55` (`FlipSolverGrid::cells = surface.nodes − 1`) | 67³ cells, 68³ nodes |
| `gpu_flip_step` outputs `grid_nodes_x/y/z = solver.nodes()`, `face_cells_x/y/z = cells` | `gpu_flip_step.rs:2319-2320`; outputs :45-61 | 68 / 67 |
| Stage shape: `cells = grid_cells(nodes)`, pad = `face_offset(nodes, face_cells)` | whitewater_step.rs:251-255; whitewater.rs:170-183 | cells 67 = face_cells 67 → **pad 0** |
| `require_inputs`: distance must hold `cell_total(face_cells)·4` bytes | whitewater_step.rs:391 | 67³×4 = 1.20 MB |
| `gpu_flip_domain` `mesh_min/mesh_nodes/mesh_wall_inset = surface.min()/nodes()/solver.wall_inset()` | `gpu_flip_domain.rs:92-98` | node 8 and node 9 sample the **same 68³ lattice** the solver's corners use |
| Solver corners: `encode_solid_distance` with `closed_faces: 63`, `step.wall_inset`, `p.box_min`, per substep | `gpu_flip_step.rs:1325-1350` | same kernel as node 8 (`liquid_solid_distance.rs:206`) |
| Node 8's pose time: `adaptive_tick_seconds` = `clock_plan.elapsed` in live mode, else `tick_seconds` | `shaders/liquid_solid_distance_body.wgsl:89-95`; node 8's own plan is zero-filled (`liquid_solid_distance.rs:151-163`) | node 8 = end-of-interval pose; solver = substep pose |
| Node 9's wall arithmetic: `high = min + (n − (1+inset))·h` vs node 8's `high = low + (n − (1+2·inset))·h` | `whitewater_obstacle_source_body.wgsl:27-28`; `liquid_solid_distance_body.wgsl:62-63` | algebraically equal, differently ordered (the audit's "wall arithmetic differs") |
| Existing padded fixture: nodes 13, face_cells 8 → pad 2 | `whitewater_step_tests.rs:579-600` | keeps the padded path exercised (I3) |

### 1.5 The face adapters

| Piece | Where | State |
|---|---|---|
| `node.face_sample_component`: one axis of the packed `FaceSample` lattice to the seam's f32 array; weight rule `select(0, v[a], w[a] > 0)` | `face_sample_component.rs:46-126`; `shaders/face_sample_component_body.wgsl` (last line) | codegen Pointwise, BufferGather |
| `FaceSample { velocity: [f32;4], weight: [f32;4] }` = 32 B; lattice (cells+1)³ | `node_graph/fluid_particles.rs:95-103` | 68³×32 B = 10.06 MB |
| Preset nodes 11/12/13: `state.faces` → adapters → `frame.face_u_in/v_in/w_in` | `WaterDamBreakGpuFlip.json:263-330` (nodes), :4195-4281 (wires) | built by `gpu_flip_preset.rs:92-94, :495-506` under `scene.faces` |
| `liquid_frame` copies every wired field into its history slot per publication | `liquid_frame.rs:330-345`; sizing :239-246; `faces_published` :360 | 3 blits of 1.22 MB each per publication |
| **Nothing reads `frame.face_u/v/w`, `face_cells_*` or `face_valid_layers`** in either shipped preset | GPU FLIP: wires from node 10 carry only `solid_a/solid_b` (:4904-4912); Particles: whitewater's `face_cells_*`/`face_valid_layers` come from node 6 (`WaterDamBreakParticles.json:2825-2850`) | the audit's finding, confirmed in both presets |
| `liquid_state` allocates its held face grid and blits the tick's faces into it **only when its `faces` output is consumed** | `liquid_state.rs:508-516` (`.filter(... outputs.array("faces").is_some())`), :556-575, :770-773 | the three frame adapters are its only consumers → dropping them also drops a **10.06 MB blit per frame** and the 10 MB held buffer (⚠ VERIFY-AT-IMPL: `ctx.outputs.array("faces")` is `None` when no wire leaves `state.faces` — read `effect_node.rs` `outputs.array` and run the P4 gate's dump) |
| Preset nodes 496/497/498: `step.faces` → adapters → `whitewater.face_u/v/w`, inside the tick region | `WaterDamBreakGpuFlip.json:2702-2724`; wires :4651-4738; Particles :1159-1181, :2895-2958 | 3 dispatches per tick, 3.66 MB of axis writes per tick |
| The builder regenerates both presets from the bundled seed; whitewater wires are added by explicit retain-then-push | `gpu_flip_preset.rs:764-854` (seed :534; obstacle_source/dust rewire :836-840); `particle_view_def` :879; snapshot tests :1609-1639 | **presets are never hand-edited**; `UPDATE_GPU_FLIP_PRESET=1` |
| The vendored (CPU engine) whitewater comparison removes the whitewater's adapters and reads the frame's face outputs | `whitewater_scene_tests.rs:278-300` (:298) | `scene.faces` must stay a builder option for tests |

### 1.6 Catalog status of the whitewater atoms (the brief's question)

Method: `grep -rl '"node.<id>"' crates/manifold-renderer/assets/` per atom, plus `picker:` / `examples:` in each `primitive!`.

**Catalog atoms other presets wire (stay catalog, untouched by this design):**

| Atom | Presets |
|---|---|
| `node.sort_particles_into_cells` | WaterDamBreakGpu, WaterDamBreakGpuFlip, WaterStillPoolMatter, WaterFloatingBoxMatter, WaterDamBreakMatter (the stage calls its code, `ParticleSorter`, whitewater_step.rs:37, :1330) |
| `node.liquid_solid_distance` | WaterDamBreakParticles, WaterDamBreakGpuFlip, WaterDamBreakMatter, WaterFloatingBoxMatter |
| `node.whitewater_obstacle_source` | WaterDamBreakParticles, WaterDamBreakGpuFlip |
| `node.face_sample_component` | WaterDamBreakParticles, WaterDamBreakGpuFlip (six instances each; all retired from both by P4; the node stays catalog for the seam's P10 contract) |

**Stage internals registered as catalog atoms with `picker: { category: Atom }`, `examples: []`, and zero preset consumers (24):** `surface_crossings`, `nearest_crossing`, `crossing_distance`, `liquid_cells`, `lattice_curvature`, `extend_lattice`, `turbulence_field`, `whitewater_influence`, `dust_potential`, `jitter_particles`, `sample_faces_at_particles`, `whitewater_emitter_velocity`, `energy_potential`, `wavecrest_potential`, `inside_turbulence_potential`, `turbulence_emission_count`, `spawn_whitewater`, `whitewater_type`, `advect_whitewater`, `retype_whitewater`, `age_whitewater`, `preserve_foam`, `keep_whitewater`, `upwind_distance`. (⚠ VERIFY-AT-IMPL: `node.emission_count` — `rg 'picker:' crates/manifold-renderer/src/node_graph/primitives/emission_count.rs`; if it carries a picker, it joins the list.) These can stop being catalog entries (D9). WaterDamBreakParticles uses the same `node.whitewater_step` stage (`WaterDamBreakParticles.json:911`), not these atoms.

Mechanics: `picker:` is optional in `primitive!` (`primitive.rs:1300`); omitted → `None` → absent from `palette_atoms()` (:1405-1413, `__primitive_picker` :1495-1505) and from the catalog's picker-labelled strata (`catalog_gen.rs:57-58, :199-200`; `gen_node_catalog --check` is the CI gate, `bin/gen_node_catalog.rs:13-14, :74`). The type stays registered (inventory submit :1407-1412), so a saved graph holding one still loads; `UnknownTypeId` (`graph_loader.rs:1328`) is never reached.

### 1.7 Oracles, proofs and measurement tooling

| Piece | Where |
|---|---|
| Shipped-def harness that runs a preset frame by frame and dumps any node's provided array (`Show::new`, `dumped::<T>`, `provided`) | `whitewater_scene_tests.rs:407-610` |
| Per-tick GPU rows proof on a pad-2 fixture; cross-frame CPU-model proofs (tolerance, not bitwise) | `whitewater_step_tests.rs:563-700` |
| Atom value + fused-vs-unfused proofs (turbulence, inside/dust counts, influence, emitter speed, obstacle source, dust lifecycle, fresh spray speed, dust population) | `whitewater_emitter_gpu_tests.rs:164-848` |
| Engine-surface-distance and substep/outflow proofs | `whitewater_engine_gpu_tests.rs:14-600` |
| `gpu_flip_frame_perf` — per-tick FNV frame hashes printed (`output hash over the timestamped frames`) | `tests/gpu_proofs/gpu_flip_frame_perf.rs:361-376, :467-516` |
| `liquid_conformance` (coupling, export schedule, pause) | `tests/gpu_proofs/liquid_conformance.rs` |
| Scoped proofs gate | `scripts/gpu_proofs_gate.py` (scoped by `gpu_scope.py`). `--test NAME` selects a test **binary**, `--filter NAME` a test function; every acceptance command in this doc uses `--filter <fully qualified name>` and the report quotes a nonzero executed-test count |
| House measurement: `manifold frame-time <project> --frames N --stamp-every K [--stamp-granularity node]` under `scripts/gpu_queue.py` | `crates/manifold-app/src/frame_time.rs:1-33` |
| Pipeline occupancy query precedent (`maxTotalThreadsPerThreadgroup`) | `crates/manifold-gpu/src/metal/raytrace/tracer.rs:137-138`; `GpuComputePipeline` has no accessor (`metal/types.rs:340-352`) |
| Migration ladder: last rung 1.17.0 → 1.18.0 (`solve_level_card_v1180.rs`), ladder-top test | `crates/manifold-io/src/migrate.rs:162-166, :853`; `migrations/mod.rs` |

---

## 2. Decisions

**D1 — Fusion is stage-internal hand kernels, never the region compiler.** The stage already satisfies section 1.2's three conditions (seam ports; one method, proven against the vendored engine; the only passes it calls that exist outside it are the scan and the sort, called as code). Its internal per-particle passes become four hand kernels plus the turbulence grid variant in a new `shaders/whitewater_fused.wgsl`, composed with the shared WGSL exactly as `gpu_flip_step.rs:398-403` composes its shader, built at install in `Pipelines::prepare`. Rejected: fusing through freeze regions, because the stage's dispatches never enter a region and the partitioner refuses a buffer region with more than one escaping output (GPU_WHITEWATER_DESIGN.md section 3.3, BUG-imy3.5) — spawn needs both `sampled` and `energy`. Rejected: new catalog atoms for the fused passes, because section 1.2 forbids it and Peter: "Users won't touch these GPU FLIP nodes."

**D2 — Exactly four fused kernels; everything else stays.** (a) `ww_emit` over `emitters`: jitter, sample, emitter speed, energy, wavecrest, inside, count → writes `sampled` (32 B), `energy`, `counts`, and, when `dust_enabled`, `unscaled` (the pre-speed-scale record; written for every slot, dead slots passed whole, exactly as `SampleFacesAtParticles` writes it today) and `wavecrest_bits` (the wavecrest operand dust reads once). (b) `ww_dust` over `emitters`, dust on only: dust potential, energy over `unscaled`, dust count → writes `dust_energy` and its counts straight into `offsets`, which the shared emission scan then scans in place. (c) `ww_spawn` over `capacity`: spawn + type → writes `typed` directly; `Fields.spawns` is deleted. (d) `ww_lifecycle` over `capacity`: advect + retype + age, `a → b` in one pass. Scans, sort, keep, preserve_foam, compaction, split and the nine hand passes are untouched. Rejected: folding dust into (a), because with axis-array faces the binding count reaches 17 against the 16-slot `dispatch` cap (whitewater_step.rs:500) and dust is a separate emitter type with its own scan/spawn/append sequence anyway. Rejected: fusing `turbulence_field`/`liquid_cells`/`curvature` into the emitter kernel, because they are grid passes the particle gathers read after a barrier.

None of the four cuts needs another invocation's newly computed particle output: spawn gathers immutable emitter records and completed offsets, classification consumes its own spawn, lifecycle gathers immutable grids and history and transforms its own pool record (Astra, 2026-10-06). The preservation contract each fused kernel copies, as complete functions including wrapper logic:
- emitter **slot** indices for RNG streams 0–2, 9 and 10; spawn **slot** indices for streams 4–7 and 11, including capacity thinning — never particle id, never emitter index where the atom used a spawn index;
- seed bitcasts, epoch rounding, and the floating-point `seed + 104729.0`;
- count multiplication **after** rounding, exactly as turbulence_emission_count_body.wgsl:36;
- dust's pre-speed-scale record for every slot including dead ones, and dust typing's early return before stream 11;
- every default `pack` supplies today (whitewater_step.rs:440), not only the explicitly overridden params — the fused uniform structs are filled from the same defaults;
- lifecycle: advect's impulse loop runs for every non-foam kind before any kind-based pass-through (section 1.2 table); no early `kind == 3 || kind > 4` return at the top of `ww_lifecycle`.

**D3 — Fused kernel text is the atoms' body text, phases in atom order, with no hoisting in the first fuse.** Each phase is the atom's `body` copied verbatim into a named function (`fn ww_phase_jitter(...)` etc.), the `wc_*` helpers deduplicated by name only. Pinned: operand order, parenthesisation, constants, explicit `fma` sites, accumulation order, branch predicates, and the former record/scalar boundaries (each phase reads its input record and writes its output record as a local, exactly as the atom read and wrote the buffer). Keep `length(dt * v)` (spawn_whitewater_body.wgsl:195), never `dt * length(v)`. No manual common-expression hoisting: `MTLMathMode::Fast` (device.rs:760) can contract or reassociate after inlining across former dispatch boundaries, and if two independent atom kernels produce different bits for a nominally identical value, one shared value cannot reproduce both. Textual alignment is an implementation constraint, not a proof; I2 is the proof. The three hoists (`q`/`floor(q)`; `wc_borders_air`; the eight `surface` corner reads and their weighted sum `d`) are a separate, separately reviewed and separately proven change (P3b), taken only after the unhoisted fuse is bitwise. Rejected: `include_str!` of the body files into the fused shader at build time with string renames, because the bodies collide on `body`/`wc_*` names and a build-time text rewrite of 13 files is a second shader compiler nobody wants to debug; copy once, prove equality by test, and name the duplication as the honest cost (section 6).

**D4 — Bitwise equality is a test result; the atom chain stays as the in-test oracle and the first-difference diagnostic.** The current `emit`/`tick` dispatch sequences move verbatim into `whitewater_reference.rs` under `#[cfg(all(test, feature = "gpu-proofs"))]`, selected per `Step` instance (an instance-local field set by the test harness, never a process-global atomic: a global lets a concurrent test flip another test's oracle path). Production never dispatches an atom pipeline for the fused passes (I14). Only the 14 replaced pipelines move (jitter, sample faces, emitter velocity, energy, wavecrest, inside, emission count, dust potential, spawn, type, advect, retype, age, turbulence field); the grid atoms, preserve and keep stay in production.

The comparison contract (I2), executable:
- run the **production entry points** (`Step::emit`/`tick`) and the reference chain from independently initialised, identical inputs and state; assert both paths executed (a dispatch-label count on each);
- compare intermediates, not only final populations: emitter records, unscaled records, energy and pre-scan counts; typed spawns; lifecycle output **before sort and compaction** (final populations hide drift in discarded slots);
- synthetic cases beside the two scenes: emission counts at half-integers, classification boundaries, zero emitters, capacity overflow and thinning, dead slots, impulses, several substeps, preserve-foam on and off;
- the reference keeps its own scratch, defaults and orchestration, independent of the fused implementation.

Never refresh a golden or weaken equality to make a lane pass. On a mismatch the executor reports the first differing element (tick, buffer, index, both bit patterns), tries exactly one repair — reordering the fused expression to the atom's textual order at the identified site — and escalates to Peter if it persists, with these options named: keep that sub-chain unfused; or accept a documented last-ulp delta with a bounded proof. The executor does not pick between them.

**D5 — Zero-padding alias with the padded path kept.** In `emit`, when `padding == 0`: bind `inputs.distance` wherever `f.distance` was bound, and bind the `SurfaceDistance::encode` return buffer wherever `f.surface` was bound; no pad dispatches. When `padding > 0`: today's two pads. `padded_extent(cells, 0) == cells` (pad_distance_lattice.rs:31-38) and the pad kernel's interior copy is the identity, so the alias is bit-identical by construction; the padded fixture (nodes 13 / face_cells 8, pad 2, whitewater_step_tests.rs:579) keeps the other branch proven (I3). `Fields.distance`/`Fields.surface` stay allocated when **legacy level-set mode OR pad > 0** (`Option<GpuBuffer>`): the legacy graph at pad 0 still writes `f.distance` and copies it into `f.surface` (whitewater_step.rs:1064). Mode is part of the reservation and `held_bytes` key, so a mode change re-reserves exactly as a shape change does. On the shipped grid (tick mode, pad 0) `held_bytes` drops by 2×cells×4. The distance and surface views are resolved once for the **whole emit-plus-lifecycle interval** (retype reads surface after `emit` returns). The surface view is `SurfaceDistance`'s own reusable `current` storage (whitewater_distance.rs:213), which its next encode overwrites: it is never retained as history, and the existing barriers before its reuse stay. Rejected: dropping the padded path because the shipped grid has pad 0 — `face_offset` admits any equal integer pad and the extent fixture uses 2.

**D6 — Influence ping-pong by index.** `Step` gains `influence_current: usize`; the atom reads `influence[cur]`, writes `influence[1 − cur]`, the count kernel reads `influence[1 − cur]`, then `cur = 1 − cur`. The copy at :1101 is deleted (negative gate in I4). `reset_influence` already ignores the input on epoch change (`whitewater_influence_body.wgsl:4`) and the kernel overwrites every element, so the swap needs no seeding. Pinned: normal **and** dust counts read the same newly written buffer; exactly one swap per influence update; the `influence_epoch` invalidations at whitewater_step.rs:913, :965 and :988 stay. Covered by several ticks in one encoder, an epoch reset, disable then re-enable, and shape replacement (I4). The legacy level-set path's `copy_buffer_to_buffer(&f.distance, &f.surface)` at :1093 is **not** touched (Deferred, section 9).

**D7 — Whitewater reads the packed face grid; the axis arrays stay for the legacy path.** New optional input `faces: Array(FaceSample)`; `face_u/v/w` become optional. Tick mode requires exactly one source: `faces`, or all three axis arrays; both wired or neither is a named refusal (I5). Legacy level-set mode accepts axis arrays only (its frame publishes arrays). Every face-reading fused kernel and the stage's `turbulence_field` pass (which is a grid atom today; it becomes a fourth variant-bearing stage kernel, body copied, oracle kept) is built twice from one source with a one-line prefix `const LF_PACKED: bool = …;` (the `gpu_flip_step.rs:398` shape), both buffer sets declared, the unused set bound to `f.state` as advect already binds absent optionals (whitewater_step.rs:1294-1307). The packed read: lattice `m = face_cells + 1` per axis; face `f` of axis `a` (after `g = f − pad`, out-of-range → 0 as `lf_face_index` returns `LF_NONE`) reads record `g.x + m.x·(g.y + m.y·g.z)` and returns `select(0.0, s.face_velocity[a], s.face_weight[a] > 0.0)` — the adapter body's own rule, so the value is bit-identical by construction (no arithmetic). Two traps the packed variant must not fall into (Astra): advect gates its history reads on `aw_face_len(axis) = arrayLength(&buf_face_u/v/w)` (advect_whitewater_body.wgsl:38, :77); with those arrays bound to the `f.state` stand-in the length is tiny and most samples vanish even where `aw_face` would read valid substep history. In packed mode `aw_face_len` returns the **logical** axis length (`cell_total` of that axis's face dims), and the substep history keeps its own separate indexing. And the invalid-corner branch stays a skipped contribution (`if i != LF_NONE && i < len { sum = sum + … }`); replacing it with an unconditional `weight * 0.0` changes the expression shape. The extent rule is required work, not conditional: liquid/extent.rs:1831 unconditionally `covers` all three axis inputs; runtime and extent validation implement one truth table (packed only; three axes only; packed plus any axis → refusal; one or two axes only → refusal). Rejected: a runtime uniform selecting the path, because it leaves a dead binding set on the hot path; rejected: making `faces` required, because the frame-stage interface "remains available for older saved graphs" (GPU_WHITEWATER_DESIGN.md section 3.9).

**D8 — The frame's face publication leaves both shipped presets; the ports stay.** `WaterScene::faces` stays a builder option (the vendored comparison needs it, whitewater_scene_tests.rs:298). `dam_break()` already sets `faces: false`; the forced publication comes from `render_def(...with_faces())` and `particle_view_def(...with_faces())` (gpu_flip_preset.rs:766, :880). Those two overrides are removed, the vendored comparison keeps its explicit `with_faces()`, so nodes 11/12/13 and their six wires are no longer generated. Both presets are regenerated and validated. The held-state saving (liquid_state.rs:516 allocation filter) stays a claim until P4's dump shows it. `liquid_frame.face_*_in` stay optional inputs; `liquid_state.faces` stays an output; a user or another preset wiring them gets today's behaviour. `liquid_frame.face_valid_layers` then reports 0 (liquid_frame.rs:360), which nothing in either preset reads (section 1.5). Rejected: deleting the ports — the seam's P10 contract (LIQUID_SOLVER_SEAM_DESIGN section 3.2) and saved graphs.

**D9 — The 24 stage-internal atoms lose their `picker:`; they stay registered.** Palette and catalog no longer show them; saved graphs still load; `standalone_for_spec::<P>()` still compiles them for the oracle tests. Rejected: deregistering, because a saved graph holding one would hit `UnknownTypeId` and "silently dropping unresolvable data on a load path is the forbidden move" — and there is no census of saved projects to prove none exist. Deregistration is Deferred with its trigger.

**D10 — One migration rung, v1.19.0 → v1.20.0, `migrations/face_adapters_v1200.rs`, shaped like `solve_level_card_v1180.rs`.** For every stored graph (`for_each_preset_instance` + `embeddedPresets`, saved `WaterDamBreakParticles` graphs and authored graphs alike), the rung matches and validates the **complete destination face-source group before any mutation**. Whitewater group: the destination is a tick-mode `node.whitewater_step`; all three of its `face_u/v/w` are fed by `node.face_sample_component` nodes with the correct axis-to-port mapping (U→face_u, V→face_v, W→face_w; an intentionally swapped mapping is not "corrected"); all three adapters read the **same** solver node's `faces` output; the lattice provenance is compatible (that solver's face cells match the whitewater's `face_cells_*` sources); no `faces` wire already exists on the destination; each adapter's `out` has no consumer other than its whitewater port; no binding or interface param targets an adapter. Only then: delete the three adapters with their wires and add one `step.faces → whitewater.faces`. Frame group: the same complete-group rule for a `node.liquid_frame` whose three `face_*_in` come from adapters on one `node.liquid_state.faces`, when nothing reads that frame's `face_u/v/w`, `face_cells_*` or `face_valid_layers`. Anything short of a complete match leaves the whole group unchanged and is reported via `note_migration`. Adapters are deleted only after every consumer is accounted for. Nested groups are walked and handled by the same rule, or reported if the walk cannot resolve a wire across the group boundary (the cited precedent deliberately skips them, solve_level_card_v1180.rs:13; this rung does not). Idempotent; a migrated graph is a passthrough. Needed because "a saved generator layer carries its own graph snapshot and that snapshot is the manifest authority" (solve_level_card_v1180.rs:1-5): without the rung Peter's show projects keep the adapters and never see P4's saving. Correctness never depends on the rung (the old wiring stays bit-identical).

**D11 — Shared solver inputs, what is built: whitewater's φ and faces become literal reads of the solver's buffers.** This is shared solver inputs, not finished universal grid unification: padded mode still copies, the reinitialised surface and the diffuse classification remain derived fields, and histories remain distinct versions. Direct same-tick reads are structurally supported (the solver publishes `faces` and `l.phi`, gpu_flip_step.rs:2291; the executor installs provided buffers before downstream reads, execution.rs:2492), so the packed graph must stay inside the tick region, tested with successive ticks sharing one encoder. After D5 and D7: the solver's `distance` (`gpu_flip_step.rs:2280`, `l.phi`) is read in place, the solver's packed `faces` are read in place, the stage's `liquid_cells` classification keeps its own inputs-shared derivation (the engine's own diffuse material grid, `diffuseparticlesimulation.cpp:1609-1686`; replacing it with the solver's water mask changes classification and is forbidden by name, section 6). The solid lattice is the remaining duplicate and is Deferred (D12) — honestly, not quietly.

**D12 — Solid distance + nearest-object metadata together, and shared with the solver's corners: deferred, with the design sketched.** Facts: node 8, node 9 and the solver's corners sample the same 68³ lattice; node 8 and the solver's corners share a kernel body, node 9 does **not** (whitewater_obstacle_source_body.wgsl:32 vs liquid_solid_distance_body.wgsl:62); for a static body the pose terms vanish exactly (`v·t = 0`, `liquid_turn(q, 0, t) = q`), so with `closed_faces == 63` node 8's field and the solver's corners are bit-identical regardless of pose time; for moving bodies they are not (substep pose vs end-of-interval pose), and node 9's metadata is additionally one frame stale relative to the tick (node 9 runs after the region). The shape that would honour "produced together": nearest-object metadata computed inside the stage's influence pass (the stage gains `bodies`, `body_count`, `rows`, `closed_faces`, `wall_inset` ports; `obstacle_source` stays as an override input), materialised to a stage-owned lattice only when dust is on; node 9 then leaves the shipped presets. The shape that would honour "shared with the solver": the step publishes `solid` (= `l.corners` after its last substep) and the domain publishes `solid_static` (closed 63 ∧ every body row with zero linear and angular velocity ∧ `dynamic_bodies == 0`); node 8 publishes the step's buffer in place when the predicate holds. Why deferred: a static predicate avoids the moving-body alias rather than changing moving-body semantics, but its real prerequisites are matching geometry, walls, body version and a valid initialised solver snapshot — zero velocity alone does not prove freshness after an edit or on a frame with no tick; moving-body sharing would move toward the engine (last-substep `_solidSDF`, `fluidsimulation.cpp:6980-7000`), a fidelity call only Peter makes; the saving is one 314k-thread dispatch and a 5 MB lattice per display frame, unmeasured; the metadata relocation has its own timing question; the two-output atom would leave the codegen mandate; and the predicate adds four ports across three nodes. Revive trigger in section 9.

---

## 3. Design body

### 3.1 The fused shader and its variants

`crates/manifold-renderer/src/node_graph/primitives/shaders/whitewater_fused.wgsl`, one file, four entry points (`ww_emit`, `ww_dust`, `ww_spawn`, `ww_lifecycle`) plus `ww_turbulence` (the grid pass's packed/axis variant), each `@workgroup_size(256)` over `count` as `dispatch` already sizes groups (whitewater_step.rs:505). Built by:

```rust
// whitewater_step.rs, beside WHITEWATER_STEP_SHADER
fn fused_source(packed: bool) -> String {
    format!("const LF_PACKED: bool = {packed};\n{WHITEWATER_COMMON}\n{LIQUID_FACES}\n{LIQUID_FIELD}\n{WHITEWATER_FUSED_SHADER}")
}
```

`Pipelines` gains `fused: [Vec<GpuComputePipeline>; 2]` indexed by `packed`, five entries each (`FUSED_ENTRIES: [(&str, &str); 5]` in `Hand` style, labels `node.whitewater_step.emit`, `.dust`, `.spawn`, `.lifecycle`, `.turbulence`, so the profiler still splits the stage as section 1.2 requires). The 14 replaced atom pipeline fields (D4 list) and their `prepare` lines move to `whitewater_reference.rs` under the test cfg; the grid atoms, preserve and keep stay. Uniform layout per fused kernel: a `#[repr(C)]` struct per kernel (`EmitParams`, `SpawnParams`, `LifecycleParams`), replacing `pack::<P>` for those passes; `pack` stays for the grid atoms that remain (`LiquidCells`, `LatticeCurvature`, `ExtendLattice`, `WhitewaterInfluence`, `SurfaceCrossings`, `NearestCrossing`, `CrossingDistance`, `PreserveFoam`, `KeepWhitewater`).

Face access inside the fused shader:

```wgsl
@group(0) @binding(N)   var<storage, read> buf_face_u: array<f32>;   // axis set
@group(0) @binding(N+1) var<storage, read> buf_face_v: array<f32>;
@group(0) @binding(N+2) var<storage, read> buf_face_w: array<f32>;
@group(0) @binding(N+3) var<storage, read> buf_faces: array<FaceSample>; // packed set
fn ww_face(f: vec3<i32>, axis: u32, pad: vec3<i32>, face_cells: vec3<u32>) -> f32 {
    if LF_PACKED {
        let g = f - pad;                       // same range test as lf_face_index
        var dims = face_cells; dims[axis] += 1u;
        if any(g < vec3<i32>(0)) || any(g >= vec3<i32>(dims)) { return 0.0; }
        let m = face_cells + vec3<u32>(1u);
        let u = vec3<u32>(g);
        let i = u.x + m.x * (u.y + m.y * u.z);
        if i >= arrayLength(&buf_faces) { return 0.0; }
        let s = buf_faces[i];
        return select(0.0, s.face_velocity[axis], s.face_weight[axis] > 0.0);
    }
    let i = lf_face_index(f, axis, pad, face_cells);
    if i == LF_NONE || i >= ww_face_len(axis) { return 0.0; }
    return ww_face_axis(axis, i);
}
```

The unused set is bound to `f.state` (the existing stand-in for absent optionals). `substep_u/v/w` histories (advect's motion path) stay axis arrays: they are the step's own per-substep publication, not an adapter, and out of scope.

### 3.2 Buffers after fusion

| Buffer | Today | After |
|---|---|---|
| `ParticleScratch` | jittered, sampled, energy, wavecrest, inside | `sampled`, `energy`, `unscaled`, `dust_energy`, `wavecrest_bits` (same byte total: 2×32 + 3×4 per slot) |
| `Fields.spawns` | spawn → type intermediate | deleted; `ww_spawn` writes `typed` |
| `Fields.distance`, `Fields.surface` | always | `Option`, allocated only when pad > 0 |
| `Fields.influence[2]` + copy | copy-back each tick | index swap, no copy |
| `held_bytes` | whitewater_step.rs:310-321 | updated to match; `whitewater_extents_at_64` (I9 of the whitewater design) re-derived |

Dust sequencing with one emission-scan storage: `ww_emit` writes normal counts into `offsets` (the scan's level-0 storage, as today); the normal scan/spawn/append run; then `ww_dust` writes dust counts directly into `offsets`, which are scanned in place with `emission_scan.encode_labelled` under `EMISSION_SCAN` before dust spawn/append. Never use `encode_into` with the scan's own storage as its destination: its parent totals would overlap level 0. Order of every state-word update is unchanged.

`wavecrest_bits` is a pure side channel: `ww_emit` writes it only when dust is enabled; `ww_dust` reads it once for the same `idx` in that emission, and it is dead afterwards. It is never read across ticks, and nothing between the two dispatches binds it. Dust counts never occupy this buffer.

### 3.3 Port changes on `node.whitewater_step` (seam brief)

Old → new, inputs block (whitewater_step.rs:96):
- `face_u: Array(f32) required, face_v: Array(f32) required, face_w: Array(f32) required` → `faces: Array(FaceSample) optional, face_u: Array(f32) optional, face_v: Array(f32) optional, face_w: Array(f32) optional`.
- `run` (:1513-1523): the `let (Some(particles), …, Some(face_u), …)` destructure becomes: tick mode → `FaceSource::Packed(buf)` xor `FaceSource::Axes([u, v, w])`, else `ctx.error("Whitewater Step: wire faces, or face_u, face_v and face_w, not both")` / `"… neither"`; legacy mode → axes only (packed wired → refusal naming the legacy interface).
- `StepInputs.faces: [&GpuBuffer; 3]` → `faces: FaceSource<'_>`; `require_inputs` (:392-394) checks the packed grid holds `face_bytes(face_cells)` (`gpu_flip_step::face_bytes`) or the three axis lengths as today.

Call-site inventory (re-derive at execution: `rg -n '"face_u"|face_u:|faces\[' crates/manifold-renderer/src/node_graph/primitives/whitewater_step.rs whitewater_step_tests.rs whitewater_scene_tests.rs whitewater_emitter_gpu_tests.rs whitewater_engine_gpu_tests.rs gpu_flip_preset.rs liquid/conformance.rs liquid/extent.rs` — count and list before touching; if the count differs from the P4 brief's list, stop and list the new sites). Mechanical rewrites: test fixtures that build `StepInputs { faces: [u, v, w], … }` → `faces: FaceSource::Axes([u, v, w])`. Needs the new pattern: `gpu_flip_preset.rs:836-840` gains a retain-then-push for `step.faces → whitewater.faces` and a removal of the three `whitewater_face_*` adapter nodes from the seed; `liquid/extent.rs` if it names the adapters (⚠ VERIFY-AT-IMPL: `rg face_sample_component crates/manifold-renderer/src/node_graph/liquid/`).

Compiler-driven: rename `StepInputs.faces` first; every site goes red; no parallel field.

### 3.4 Reference path and the lever

`whitewater_reference.rs` (test cfg only) holds: the 14 replaced atom pipeline slots and their `prepare`; `emit_reference(&mut Step, …)` and `tick_reference(…)` — the current :1025-1252 and :1305-1316 bodies moved, not rewritten — operating on the same `Fields`/`ParticleScratch`, with the five-buffer scratch they expect allocated by the test. `Step::emit`/`tick` branch on the instance's own `reference` field (test cfg only, default false; the non-test build has no field and no branch). The `Fields.spawns` buffer the reference needs is allocated by the reference's own `reserve`.

### 3.5 Dispatch legality check

`manifold-gpu` (crate boundary crossed on purpose, one method): `impl GpuComputePipeline { pub fn max_threads_per_threadgroup(&self) -> Option<u32> }` — Metal: `Some(state.maxTotalThreadsPerThreadgroup())` (precedent `tracer.rs:137-138`); Vulkan: `None`. The gate (I10) is legality only: each fused pipeline's value is ≥ 256, the workgroup size the stage dispatches. The values for fused and replaced pipelines are printed as diagnostics. They do not prove occupancy or the absence of spills (Apple documents this value as a legal per-group maximum), so no claim about occupancy rests on them; the A/B is the performance evidence. Honest expectation: the emitter kernel's live state across phases is the particle record, `phi[8]`, three potentials and the hoisted flags (under ~48 scalars); the lifecycle kernel's longest straight line is advect's collision march, which already exists; neither phase keeps more live than the largest unfused kernel plus the carried record. The risk is real but bounded, and the gate is mechanical.

### 3.6 Measurement (section 7 of the brief, applied)

House method, stated once: two **release** binaries, A = the pinned base SHA, B = branch SHA, each running the project with its own bundled presets (B's load migrates the snapshot through D10 where P4 applies). Pinned per run and quoted in the report: base SHA, project snapshot (a copy of the file, hashed), clock mode, resolution, sim rate, warmup (`--splash-frames`). Runs: `scripts/gpu_queue.py --label <phase> -- <binary> frame-time <project> --frames 300` interleaved A, B, A, B, A, B, plain samples; a separate stamped set with `--stamp-every 1` (never paced `--stamp-every 10`: on a coupled project the stamped frames skip the step, frame_time.rs:22-28). Reported: accepted ticks per run; tick-frame wall-interval p50/p95 from the plain runs; `node.whitewater_step.*` labels from the stamped runs **as ratios only**. Scenes: the shipped Dam Break, plus the all-emitters fixture with dust on (the dust chain is otherwise never measured). P4 adds a display-only table for the publication saving, which a tick-only headline cannot show. Systematic regression is defined before the runs: B's tick p50 above A's in all three interleaved pairs, or B's median p50 more than 2% above A's. That is an escalation, not noise.

---

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| I1 | Whitewater tick state (pool_out, state_out, counts_out, foam/bubble/spray/dust arrays) is bit-identical per tick to the recorded golden on the Dam Break and the all-emitters fixture | `whitewater_tick_state_matches_golden` (gpu proof, P0): per-tick FNV fingerprints in `tests/fixtures/whitewater_tick_golden.txt`, recorded on the base SHA with `MANIFOLD_RECORD_GOLDEN=1`, shape of `pressure_module_matches_main_golden` |
| I2 | Each fused kernel equals its atom chain bitwise on the same inputs, intermediates included | `whitewater_fused_emit_matches_reference`, `_dust_`, `_spawn_`, `_lifecycle_`, `_turbulence_` (P2/P3/P4): two `Step` instances (fused, reference) from identical independent init, both paths asserted executed, the D4 comparison contract (intermediates before sort/compaction, the synthetic cases), first differing element reported |
| I3 | The alias is taken only in tick mode at pad 0; legacy mode or pad > 0 allocates and pads | `whitewater_per_tick_gpu_rows_match_at_every_frame_rate` (pad 2, existing) stays green; `whitewater_alias_follows_live_padding`: one live instance driven pad 0 → pad 2 → pad 0 with changed face dimensions, output equal to fresh instances at each step; legacy level-set mode at pad 0 keeps `Fields.distance/surface` allocated and its output unchanged |
| I4 | No influence copy-back; one swap per update; normal and dust counts read the same buffer | negative gate: `rg -n 'copy_buffer_to_buffer\(&f\.influence' crates/manifold-renderer/src/node_graph/primitives/whitewater_step.rs` → 0; `whitewater_influence_swap_lifecycle`: several ticks in one encoder, epoch reset, disable/re-enable, shape replacement, counts equal to the reference |
| I5 | Exactly one face source in tick mode; runtime and extent share the truth table | `whitewater_refuses_two_face_sources`, `whitewater_refuses_no_face_source`, `whitewater_refuses_partial_axes` (packed plus one axis; one or two axes only), each checked through both `run` and the extent rule (CPU, extent harness) |
| I6 | Packed read equals axis read | `whitewater_packed_faces_match_axis_arrays`: the shipped def wired both ways, I1's fingerprints equal; plus a fixture with nonzero padding, mixed valid weights, and substep history in use |
| I7 | Whole-frame hashes unchanged | `gpu_flip_frame_perf*` printed hash lines identical on base and branch (landing report quotes both) |
| I8 | Shipped presets equal the builder | existing `gpu_flip_preset` snapshot tests (:1609-1639) |
| I9 | Hidden atoms still load; catalog in sync | `hidden_whitewater_atoms_still_register` (registry has every type id; `palette_atoms()` has none); `cargo run -p manifold-renderer --bin gen_node_catalog -- --check` |
| I10 | Fused pipelines are legal at the dispatched workgroup size | `whitewater_fused_pipelines_are_dispatch_legal` (section 3.5; values printed as diagnostics) |
| I11 | Migration is all-or-nothing per face group, idempotent, and round-trips | `face_adapters_v1200_rewires_once`, `_leaves_foreign_consumers`, `_is_passthrough_second_time`, `_leaves_mixed_solver_sources`, `_leaves_swapped_axes`, `_leaves_partial_groups` (one adapter with a foreign consumer leaves all three), `_leaves_legacy_consumer`, `_walks_nested_groups`, `_handles_particles_preset`; round-trip: save a migrated project → reload → I1 fingerprints equal |
| I12 | No new lock or thread | `git diff -U0 origin/main -- crates/manifold-renderer crates/manifold-gpu ':!*tests.rs'` added lines: `rg -e 'Arc<Mutex' -e 'Arc<RwLock' -e 'thread::'` → 0 |
| I13 | No per-frame allocation on the stage's run path | added lines in `whitewater_step.rs` outside `#[cfg(test)]`: `rg -e 'Vec::new\(\)' -e 'to_string\(\)' -e 'format!\('` → 0 beyond the existing error strings (list them in the brief) |
| I14 | Production never dispatches the replaced atoms | `rg -n 'atom::<(JitterParticles|SampleFacesAtParticles|WhitewaterEmitterVelocity|EnergyPotential|WavecrestPotential|InsideTurbulencePotential|TurbulenceEmissionCount|DustPotential|SpawnWhitewater|WhitewaterType|AdvectWhitewater|RetypeWhitewater|AgeWhitewater|TurbulenceField)>' crates/manifold-renderer/src/node_graph/primitives/whitewater_step.rs` → 0 (all live in `whitewater_reference.rs`). Phase-scoped: after P2 the regex covers only the eight emitter/dust atoms; after P3 add spawn, type, advect, retype, age; `TurbulenceField` joins at P4 |
| I15 | RNG streams, seeds and draw indices unchanged | the constant-set check alone cannot catch particle id used for slot, or emitter index used for spawn index; so `whitewater_rng_calls_are_the_atoms` compares each `ww_random(...)` call's full argument text (index expression, seed expression, stream) per phase against the atom body, and I2's synthetic thinning and capacity-overflow cases catch the rest dynamically |

---

## 5. Phasing

Order P0 → P1 → P2 → P3 → P3b → P4 → P5; each lands on its own. P1, P2, P3 are independent of P4; P3b is optional (the hoists); P5 depends on nothing but is last so the catalog changes after the oracles exist. GPU proofs run through `scripts/gpu_proofs_gate.py --filter <fully qualified test name>` (explicit; `--test` names a binary, not a function); every gate report quotes a nonzero executed-test count. Never nextest for GPU tests.

### P0 — Golden fingerprints and the all-emitters fixture (no behaviour change)
- **Entry:** the base SHA is pinned in the brief (current main) and the golden is recorded there; `scripts/gpu_proofs_gate.py --filter <fully qualified whitewater_per_tick_gpu_rows test>` green with a nonzero count; anchors re-checked: whitewater_step.rs:928-970, whitewater_scene_tests.rs:407-610, gpu_flip_frame_perf.rs:467-516.
- **Read-back:** this doc section 1.3, section 1.7, D4; restate the two fixtures and why fingerprints are recorded on the base SHA.
- **Deliverables:** `whitewater_golden_tests.rs` (gpu-proofs cfg): `Show`-based run of the shipped Dam Break def, capturing **every simulation tick** (not only each display frame's last result), dumping `whitewater` `pool_out`, `state_out`, `counts_out` and the four populations per tick via `dumped::<T>`, FNV fingerprints per buffer per tick. Tick count and enabled flags do not prove coverage: the test asserts that spawns, deaths, compaction, capacity overflow, the id wrap at 256 and dust spawns each occurred in the recorded run, and constructed cases supply any the scenes do not reach; the all-emitters fixture = the same def with `dust_emission 1, boundary_dust 1, inside_emission 1, preserve_foam 1, generation_rate 0.5, spray_speed 2.0` on the whitewater node; `tests/fixtures/whitewater_tick_golden.txt` recorded with `MANIFOLD_RECORD_GOLDEN=1`; `whitewater_tick_state_matches_golden` (I1).
- **Gate:** the test passes twice in a row (determinism); `scripts/gpu_proofs_gate.py --filter <fully qualified whitewater_tick_state_matches_golden>` with a nonzero count; `cargo clippy -p manifold-renderer -- -D warnings`.
- **Forbidden:** recording the golden on a branch with any stage change; hashing only counts (hash the full buffers); a tolerance anywhere.
- **Test scope:** focused (the new test, `whitewater_step_tests`); no workspace run. Demo: none — L1.

### P1 — Copies: zero-padding alias and influence index swap
- **Entry:** P0 landed; anchors: whitewater_step.rs:1056-1063, :1095-1101, :670-708; whitewater.rs:170-183; pad_distance_lattice.rs:31-38.
- **Read-back:** D5, D6, I3, I4; restate that the padded path stays and the legacy copy at :1093 is untouched.
- **Deliverables:** `Fields.distance/surface: Option<GpuBuffer>` allocated for legacy mode or pad > 0, mode in the reservation key; views resolved for the whole emit-plus-lifecycle interval; alias branch in `emit`; `influence_current`; `held_bytes` updated; `whitewater_alias_follows_live_padding`, `whitewater_influence_swap_lifecycle`.
- **Gate:** I1 golden match; I3 (live pad 0 → 2 → 0 transition and legacy pad 0); I4 rg → 0 and the swap lifecycle test; `whitewater_extents_at_64` updated and green; `whitewater_per_tick_gpu_rows_match_at_every_frame_rate`, `whitewater_step_matches_cpu_across_frames*` green; clippy `-p manifold-renderer`.
- **Forbidden:** deleting `pad_distance_lattice.rs`; touching the legacy level-set copy; "while here" edits to the surface-distance passes.
- **Test scope:** focused. Demo: none — L1.

### P2 — Emitter fusion (`ww_emit`, `ww_dust`) with the reference lever
- **Entry:** P1 landed; anchors: whitewater_step.rs:1131-1250, :488-533; the 8 emitter bodies in section 1.2; gpu_flip_step.rs:233-246, :398-403.
- **Read-back:** D1–D4, section 3.1, section 3.2, section 3.4, section 3.5; restate the RNG stream table and the dead-slot rules verbatim.
- **Deliverables:** `shaders/whitewater_fused.wgsl` with `ww_emit`, `ww_dust` (axis variant only in this phase; `LF_PACKED` constant present, packed branch compiled but unreachable until P4); `whitewater_reference.rs` with the moved emitter chain and the instance-local reference selector, its own scratch and defaults; `ParticleScratch` reshaped; `Fields.spawns` kept until P3; `GpuComputePipeline::max_threads_per_threadgroup` in manifold-gpu; tests `whitewater_fused_emit_matches_reference`, `whitewater_fused_dust_matches_reference` (all-emitters fixture plus the D4 synthetic emitter cases: half-integer counts, zero emitters, dead slots), `whitewater_fused_pipelines_are_dispatch_legal`, `whitewater_rng_calls_are_the_atoms`. No hoisting (D3).
- **Gate:** I1 on both fixtures; I2 for emit/dust with intermediates (emitter records, unscaled, energy, pre-scan counts); I10 legality, values printed; I14 phase-scoped; I12–I14 rg gates; `whitewater_emitter_gpu_tests` and `whitewater_engine_gpu_tests` green (atoms untouched); `gpu_flip_frame_perf` hash lines identical to base (I7); clippy `-p manifold-renderer -p manifold-gpu`.
- **Forbidden:** fuse-for-parity shortcuts (changing an atom body to make the fused kernel match — the atoms are the oracle); any edit under `flip_engine/`; a runtime flag choosing fused vs reference; editing a `wgsl_body` file; dropping `unscaled` for dead slots.
- **Test scope:** focused GPU proofs through the gate script; no workspace run. Demo: `gpu_flip_frame_perf` output quoted (hashes + whitewater counts) — L1 (the PNG is not an agent gate).

### P3 — Spawn+type and lifecycle fusion (`ww_spawn`, `ww_lifecycle`), first A/B
- **Entry:** P2 landed; anchors: whitewater_step.rs:1194-1219, :1305-1316, :1338-1383; spawn/type/advect/retype/age bodies.
- **Read-back:** D2(c)(d), section 3.2 (the `a → b` parity with `current`), section 3.6.
- **Deliverables:** `ww_spawn`, `ww_lifecycle` (advect's impulse loop before any kind pass-through, D2); `Fields.spawns` deleted (reference allocates its own); tests `whitewater_fused_spawn_matches_reference` (typed spawns compared before append; thinning, capacity overflow, classification thresholds), `whitewater_fused_lifecycle_matches_reference` (lifecycle output before sort/compaction; impulses on dead and foam slots, several substeps with history, `preserve_foam` on and off); A/B report per section 3.6.
- **Gate:** I1, I2, I7, I10, I12, I13, I14 phase-scoped (turbulence still an atom until P4); `liquid_conformance` and `whitewater_scene_tests` green; A/B table per section 3.6 (pinned inputs, plain and stamped sets, dust-on scene) — a systematic regression as defined there is an escalation.
- **Forbidden:** changing the pool ping-pong bookkeeping; fusing `keep` or `preserve_foam` into the lifecycle (they read the sort); measuring with a debug binary or without `gpu_queue.py`.
- **Test scope:** focused GPU proofs + the two named suites. Demo: the A/B table — measure level.

### P3b — The three hoists (optional, separately reviewed)
- **Entry:** P3 landed with I2 bitwise; the P3 A/B shows the emitter kernel is a measurable share of the whitewater stamps.
- **Deliverables:** hoist `q`/`floor(q)`, `wc_borders_air`, and the eight `surface` corner reads with their weighted sum `d` into one evaluation per emitter in `ww_emit`, in the atoms' loop order; Astra or Fable reviews the diff before any run.
- **Gate:** I1 and I2 bitwise against the unchanged reference. A mismatch here means the hoist is not free under Fast math: revert the hoist that caused it and report the first differing element; no tolerance, no golden refresh.
- **Forbidden:** any other change in the same commit.

### P4 — Face adapters: frame publication off, whitewater packed faces, migration rung
- **Entry:** P3 landed; anchors: gpu_flip_preset.rs:92-94, :495-506, :764-854, :879, :1609-1639; whitewater_scene_tests.rs:278-300; liquid_state.rs:508-516, :556-575, :770-773; liquid_frame.rs:330-345; migrate.rs:162-166, :853; solve_level_card_v1180.rs; `CURRENT_PROJECT_VERSION` in manifold-core.
- **Read-back:** D7, D8, D10, section 3.1 face access, section 3.3 seam brief; re-run the section 3.3 inventory command and list the sites.
- **Deliverables:** `faces` input + optional `face_u/v/w` on the stage; `FaceSource`; packed variants of `ww_emit`/`ww_spawn`/`ww_lifecycle`/`ww_turbulence` (the turbulence grid pass moves into the fused shader with its own reference oracle `whitewater_fused_turbulence_matches_reference`); packed-mode `aw_face_len` returning logical axis lengths with substep history indexed separately, the invalid-corner skip kept (D7); the whitewater extent rule (liquid/extent.rs:1831) rewritten to the same truth table as `run`; builder: the forced `with_faces()` overrides in `render_def` and `particle_view_def` (gpu_flip_preset.rs:766, :880) removed, the vendored comparison's explicit `with_faces()` kept, whitewater adapters removed from the seed and `step.faces → whitewater.faces` pushed (retain-then-push shape, :836-840); both presets regenerated with `UPDATE_GPU_FLIP_PRESET=1`; `migrations/face_adapters_v1200.rs` + rung in `migrate.rs` + version bump; tests I5, I6, I11; dump proving `liquid_state`'s held faces are unallocated (`provided_bytes("state", "faces") == 0` or the field `None`) — closes the ⚠ in section 1.5; second A/B.
- **Gate:** I1 (the golden still matches: the packed read is value-identical), I5, I6, I7, I8, I11 (including round-trip: save → reload → fingerprints), migration ladder test (:853), `check-presets` sub-second validator, `whitewater_per_tick_preset_closes_the_liquid_region`, the vendored comparison in `whitewater_scene_tests` (needs `faces: true` in its own scene — ⚠ VERIFY-AT-IMPL `rg 'faces:' crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs whitewater_scene_tests.rs`); A/B table; clippy `-p manifold-renderer -p manifold-io -p manifold-core`.
- **Forbidden:** hand-editing either preset JSON; deleting `node.face_sample_component`, `liquid_frame.face_*_in` or `liquid_state.faces`; a migration that deletes an adapter with any consumer outside the two named patterns; touching `substep_u/v/w`.
- **Test scope:** focused + `manifold-io` migration tests. Demo: the A/B table and the `note_migration` line from loading Peter's project — measure level; **round-trip gate mandatory**.

### P5 — Catalog demotion and docs
- **Entry:** P2–P4 landed; anchors: primitive.rs:1300, :1405-1413; catalog_gen.rs:57-58, :199-200; gen_node_catalog.rs:13-14.
- **Read-back:** D9, section 1.6 list; resolve the `emission_count` ⚠.
- **Deliverables:** remove `picker:` from the 24 (or 25) atoms; `hidden_whitewater_atoms_still_register`; regenerated `docs/NODE_CATALOG.md`; `GPU_WHITEWATER_DESIGN.md` section 3.9 gains an "as fused" paragraph pointing here; this doc's Status line updated by the landing.
- **Gate:** I9; `gen_node_catalog --check`; `cargo test -p manifold-renderer palette` (whatever names the palette tests carry — list them in the brief after `rg palette_atoms`); clippy.
- **Forbidden:** deregistering or deleting atom files; renaming type ids.
- **Test scope:** focused CPU tests. Demo: none — L1.

Phasing-completeness check: D5/D6 → P1; D1–D4 → P2/P3 (hoists → P3b); D7/D8/D10 → P4; D9 → P5; D11 is satisfied by P1+P4; D12 → Deferred; measurement → P3 and P4 gates.

---

## 6. Honest costs and the plausible-wrong turns

**Consequences, stated honestly:**
- The fused kernels duplicate the atoms' math as text. Two sources of truth, held equal only by I2. The atoms are hidden, not deleted, so the duplication is permanent until Deferred item 1 fires.
- Two shader variants per face-reading kernel (ten fused pipelines at install instead of five). Install cost is one extra MSL compile each, cached.
- The legacy level-set path keeps its own copy at :1093 and its axis-array-only interface; it is slower than the tick path and stays that way.
- Dispatch count falls 13%; the brief's larger wins are bandwidth and are unmeasured until P3/P4. If the A/B shows nothing, that is the result to report, not a reason to widen scope.

**Forbidden by name:** fusing through freeze regions (D1); replacing `liquid_cells` with the solver's `water` mask (the engine keeps its own diffuse material grid, `diffuseparticlesimulation.cpp:1609`); substituting raw φ for the reinitialised surface field in typing/curvature; aliasing the solver's `l.corners` into whitewater "because the buffer exists" (D12); dropping the padded path (D5); hand-editing the presets (section 1.5); editing a `wgsl_body` to make a fused kernel match (D4); any edit under `flip_engine/`; a runtime switch between fused and unfused paths outside the test cfg.

---

## 7. Decided — do not reopen

1. Stage-internal hand kernels; no new atoms; no region compiler (D1).
2. Four fused kernels plus the turbulence variant; scans, sort, keep, preserve, compaction, split stay (D2).
3. Body text copied phase by phase with no hoisting; the three hoists are a separate proven step (P3b); atoms are the oracle, compared on intermediates (D3, D4).
4. Bitwise is tested, never asserted; mismatch → one named repair, then Peter (D4).
5. Zero-pad alias only at pad 0; padded path kept and tested (D5).
6. Influence ping-pong by index (D6).
7. `faces` packed input added; axis arrays optional; exactly one source in tick mode (D7).
8. Frame face publication off in both shipped presets; ports stay (D8).
9. The 24 internals lose their picker; they stay registered (D9).
10. Migration rung v1.20.0 rewires saved GPU FLIP graphs only on a complete, validated face-group match, else leaves the group and reports; correctness never depends on it (D10).
11. Solid-lattice sharing and metadata-with-distance are deferred, with the shape written down (D12).
12. Measurement is the house A/B at P3 and P4; node stamps are ratios only.

## 8. Deferred

| Item | Revives when |
|---|---|
| Deregister the hidden atoms and delete `whitewater_reference.rs` | Peter accepts losing loadability of any saved graph holding them, after one release with them hidden |
| D12: nearest-object metadata inside the stage's influence pass; `step.solid` + `domain.solid_static` sharing into node 8 | a node-stamp table shows nodes 8+9 above 2% of a tick frame, or Peter rules on moving-body whitewater fidelity (engine: last-substep SDF) |
| Legacy level-set path's distance→surface copy (:1093) and its axis-array-only faces | a saved graph on that path is measured on stage |
| Surface-distance reinit: 6 fixed iterations × 5 dispatches (34 per tick) gated only through indirect args | `whitewater.distance.*` labels exceed 10% of the stamped tick; the engine's convergence test would have to become an early exit of encoded work, a replay-layer question |
| `substep_u/v/w` axis histories inside the step | the lifecycle's motion path is measured against a packed per-substep snapshot |
| Fusing `face_sample_component` into the seam for other consumers | a preset other than these two wires the frame's face arrays |

---

