# Liquid Surface Stage — fold the surface chain into two optimised stage nodes

**Status:** PROPOSED · 2026-10-06 · Fable (design, audited at 5a1218c89), Claude lead · owed: Astra review.
**Prerequisites:** none to start P0/P1. P2 needs P1 landed and measured. The particle_volume brick gather (docs/PARTICLE_VOLUME_BRICK_GATHER_DESIGN.md, branch `feat/volume-brick-gather`) runs in parallel and owns `particle_volume.rs`; this design never touches it.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

**The governing insight: on the shipped Dam Break every lattice pass in the Liquid Surface group is an identity, yet it still costs 8–10 dispatches, and the mesh chain writes the 80-byte renderer vertex four times.** Folding the group into two stage nodes lets the stage skip identity passes from the CPU each frame, keep every live control, and later rewrite the mesh internals over a packed 16-byte position buffer. Covers items 2, 4 and 5 of the over-noding audit, BUG-cu6l1 (GPU FLIP over-noding), and BUG-twnl (Replace the Liquid Surface group with one node).

Peter, 2026-10-06: "I don't want a huge mess of nodes that aren't fully optimised ... Users won't touch these GPU FLIP nodes"; on the audit: "Yes, fantastic findings, those look like more free optimisation wins".

House rules applied: docs/DECOMPOSING_GENERATORS.md section 1.2 (Specialised solvers are stage nodes), docs/ADDING_PRIMITIVES.md exclusion 6 (stage internals exempt from the codegen path and per-atom proofs) and exclusion 1 (barriered reductions and scans), docs/GPU_FLIP_PRESSURE_SOLVE.md section 1.1 (Stage design, the precedent).

**Supersedes:** docs/GPU_FLUID_SURFACE_DESIGN.md section 4 (The atom chain): its D16/D17 and the "One gpu_fluid_mesher node — Forbidden" entry. That ban predates the stage-node rule and the gpu_flip_step/whitewater_step precedent; BUG-twnl and Peter's instruction reopen it. P1 adds a dated supersession note there.

**Fast-math context.** Kernels compile with `MTLMathMode::Fast` (`crates/manifold-gpu/src/metal/device.rs:760`). The brick gather lane found a cooperative rewrite 1–3 ulp off the generated kernel. So bitwise across differently written kernels is a test result, not a construction. Every phase below says which kernels are moved verbatim (bitwise by construction, checked by string equality) and which are rewritten (bitwise is a gate with a stated fallback, D12).

---

## 0. Scope answers

**One shared group body.** The surface group is identical in `WaterDamBreakGpuFlip`, `WaterDamBreakGpu`, `WaterStillPoolMatter`, `WaterDamBreakMatter`, `WaterFloatingBoxMatter` (only the group's own nodeId differs: `surface` vs `liquid_surface`). Enforced by `gpu_flip_surface_group_is_shared_with_all_water_presets` (`gpu_flip_preset.rs:1462`); the runtime builder copies it (`surface_group()`, `gpu_flip_preset.rs:539`). Groups flatten at load (docs/GROUPING_GRAPHS.md section 2 (The one invariant that makes grouping safe)); bindings target inner nodes by stable nodeId. One body in five files plus the builder; one migration rung covers every instance.

**Saved projects.** Peter's saved layer fixture `crates/manifold-io/tests/fixtures/water_layer_graph_v1160.json` is an older 20-node shape: no `lattice_bricks`, no `blob_bounds`, no edge count/scan, no fill-pits nodes, two `node.relax_surface_mesh` instead of `smooth_surface_mesh` + `surface_mesh_normals`, no `indices` output, group params `mesh_relaxation 0.0, particle_scale 2.2, smoothing_passes 2.0`. Today the loader grafts the missing pieces at runtime (`wire_blob_bounds`, `graph_loader.rs:973`; `preset_runtime/gpu_flip_surface.rs:78-144` grafts `node.lattice_bricks`). This design adds two rungs to `crates/manifold-io/src/migrations/`: `liquid_surface_mesh` (P1) and `liquid_blobs` (P3). Version numbers are taken from the ladder top at landing (main is at 1.19.0; the whitewater stage fusion design also wants the next rung, so whichever lands second renumbers). The rungs are total for the surface group (D10).

**Cards and bindings survive.** Bindings target inner nodeIds (`WaterDamBreakGpuFlip.json:2893-3165`): Surface Detail ×3 → `liquid_volume`/`liquid_mesh`/`liquid_bricks`.`resolution_scale`; `surface_particle_scale` → `liquid_blobs.particle_scale`; `mesh_relaxation` → `liquid_mesh_relaxation.value`; `surface_smoothing_iterations` → `liquid_smooth_mesh.iterations`; `surface_stretch`, `surface_centre_smoothing` → `liquid_blobs.stretch`/`.smoothing`; `surface_fill_pits` → `liquid_fill_pits.value`. The stages reuse those nodeIds and param names: the mesh stage lives at `liquid_mesh` (`resolution_scale`, `max_capacity`, `iterations`), the blob stage at `liquid_blobs` (`particle_scale`, `stretch`, `smoothing`, `isolated_scale`, `min_neighbours`). The scalar nodes (`liquid_fill_pits`, `liquid_fill_distance`, `liquid_fill_negative`, `liquid_fill_band`, `liquid_redistance_band`, `liquid_brick_band`, `liquid_smoothing_passes`, `liquid_mesh_relaxation`, Lattice Box, the three bin maths) are free CPU scalars and stay, feeding stage scalar ports. The only binding that moves is `liquid_smooth_mesh.iterations` → `liquid_mesh.iterations` (rung step 5). Live modulation keeps working: the stage reads every control as a per-frame scalar (`ctx.scalar_or_param`, as `lattice_bricks.rs:464`) and evaluates its bypass predicates every frame (D3).

---

## 1. Audit

Oracle scene: GPU FLIP Dam Break, res 64, engine surface defaults → lattice 68³ = 314,432 nodes, 67³ = 300,763 cells, ~343k blobs. Dispatch counts are read from code; P0 confirms them from the per-dispatch stamp table.

| # | Claim | Anchor | Change |
|---|---|---|---|
| A1 | With Fill Pits 0 and Smoothing Passes 0 every lattice pass is an identity. grow: `if offset == 0.0 { return e_levelset; }` | `primitives/shaders/offset_lattice_body.wgsl:2` | bypassed (D3) |
| A2 | redistance: `if enabled == 0.0 { return original; }`, `enabled` ← `liquid_fill_distance` | `redistance_lattice_body.wgsl:31` | bypassed |
| A3 | shrink is `offset_lattice` with `offset ← fill_distance` | as A1 | bypassed |
| A4 | smooth x/y/z: `p = i32(clamp(round(passes), 0, 3)); if p == 0 \|\| min(nodes) < 2 { identity }` | `smooth_lattice_element.wgsl:22-23` | bypassed |
| A5 | A scheduled atom runs 2 passes when `bricks` is wired (dense exterior + indirect over active bricks) | `smooth_lattice.rs:144`; `liquid_bricks.rs:10,39` | — |
| A6 | Freeze fuses redistance→shrink and smooth_z→clamp; nothing else in the chain fuses. So the lattice chain is 8–10 real dispatches, all identities on the oracle | `lattice_closing_gpu_tests.rs:86`; `liquid_bricks_gpu_tests.rs:518`; `freeze/region.rs:331-347` | P1: 0 |
| A7 | Final clamp: identity outside bricks; inside, `max(e_levelset, 0.0)` where trilinear solid < 0 | `clamp_liquid_to_solids_element.wgsl:4-15, 50, 54` | removed when every lattice pass bypasses, gated (D4) |
| A8 | particle_volume already applies the same clamp with the same trilinear text (`pv_solid`) | `particle_volume_body.wgsl:9, 25, 146-156` | the claim D4 tests |
| A9 | Mesh chain = 23 dispatches: triangle count 2, edge count 1, two prefix-scan hierarchies (5 each) + two `read_total`, mesh 2 (tail clear + emit), relax iterations 2 → 4, normals 2 | `count_surface_triangles.rs:126`; `count_surface_edges.rs:96-109`; `prefix_scan.rs:37-71`; `running_total.rs:72-118`; `volume_surface_mesh.rs:447-452`; `relax_surface_mesh.rs:238-292`; `surface_mesh_normals.rs:53-77` | P1 keeps 23 (verbatim); P2 → 19 |
| A10 | The 80-byte `MeshVertex` is written four times (emit, relax ×2, normals); each Jacobi step reads ~7 × 80 B per vertex; emit computes a gradient normal that normals later replaces | `volume_surface_mesh_body.wgsl:20, 95-96`; `surface_mesh_normals_body.wgsl:2-4`; `surface_mesh_adjacency.wgsl:28` | P2 |
| A11 | Blob fast path `smoothing == 0 && stretch <= 1 && isolated_scale == 1` writes a fixed sphere per sorted particle; the sort's `stabilise` already writes the sorted record at `place` | `shape_particle_blobs_body.wgsl:93`; `sort_particles_into_cells.wgsl:160-186` | P3: blob written in `stabilise` |
| A12 | blob_bounds = 2 dispatches | `blob_bounds.rs:47-50` | stage-internal, still 2 |
| A13 | Sort 11 + blobs 1 + bounds 2 = 14 | `sort_particles_into_cells.rs:79-97` | P3 → 13, minus 64 B/particle |
| A14 | Group output `level_set` is the pre-clamp smoothed lattice; no bundled preset consumes it; Peter's v1160 layer does (whitewater legacy interface) | preset group wires; `water_layer_graph_v1160.json` | written only when consumed (D7) |
| A15 | Unconsumed outputs get no slot | `execution_plan.rs:590, 858` | — |
| A16 | Mesh buffers are provided and grown by the atom; LiveExtent on `indices` and `vertices` | `volume_surface_mesh.rs:69-97, 194, 250, 395-414`; `liquid/extent.rs:1167-1193` | stage owns both with the same sizing functions |
| A17 | Tests asserting the chain shape must be rewritten | `liquid/lattice.rs:485`; `liquid_bricks_gpu_tests.rs:88, 508, 513, 518`; `liquid_bricks_tests.rs:5, 272`; `liquid_surface_tests.rs:2782`; `surface_mesh_freeze_tests.rs:73, 159`; `surface_mesh_normals.rs:93`; `gpu_flip_preset.rs:1238, 1292`; `liquid/conformance.rs:311-314, 435-438`; `scene_modifier_expand/coupling.rs:372`; `tests/gpu_proofs/gpu_flip_frame_perf.rs:346` | section 6 |
| A18 | Frame oracle: per-frame FNV hashes are literals; `LABELLED` splits per-label stamps | `tests/gpu_proofs/gpu_flip_frame_perf.rs:26-27, 59, 222-240` | hashes unchanged P1–P3; stage ids added to `LABELLED` |

Recorded scale (from the sparse blocks design's node measure): at 64 the surface chain minus particle_volume is 1.6 ms of a 22.06 ms frame; at 128 it is 12.5 of 98.04 (`smooth_lattice` alone 3.56).

---

## 2. Decisions

**D1 — Two stage nodes.** `node.liquid_surface_mesh` (nodeId `liquid_mesh`: lattice conditioning, clamp, counts, scans, mesh, relaxation, normals) and `node.liquid_blobs` (nodeId `liquid_blobs`: sort, blobs, bounds). Rejected: one node absorbing particle_volume (collides with the live gather lane; saves nothing today, D2); a separate lattice stage (two rungs and two boundary proofs for one chain); bypass inside the existing atoms (keeps 23 mesh dispatches, and an atom cannot skip a dispatch it is fused into).

**D2 — particle_volume, lattice_bricks and the scalar nodes stay their own nodes.** Folding particle_volume in saves no dispatch and no copy (the stage binds its output buffer directly), and would cost the gather lane its codegen oracle. Trigger to fold: gather lane landed and measured; then it is a verbatim move.

**D3 — Bypass predicates are the shaders' own identity branches, evaluated on the CPU each frame in f32 with the shaders' semantics.** grow `grow_offset == 0.0`; redistance and shrink `fill_distance == 0.0`; smooth `(round(passes).clamp(0, 3) as i32) == 0 || min(nodes) < 2`. NaN compares false, so a NaN control runs the pass as the shader would. A bypassed pass contributes no dispatch and passes its input buffer handle through. A non-bypassed pass runs the moved kernel with today's dispatch shape. No stage-level "disabled" param.

**D4 — The final clamp is removed only when every lattice pass bypasses, and only if the idempotence test is green.** With all passes bypassed the clamp's input is particle_volume's output, which already applied `max(phi, 0)` where `pv_solid(p) < 0` with the same trilinear text. It is a test, not a construction: the two predicates compile in different kernels under Fast math and can disagree within an ulp of zero. Gate I4. Fallback if red: keep the clamp as the stage's last lattice pass into a stage-owned buffer (2 dispatches); the bypass still removes the other 6–8. Never clamp in place on particle_volume's buffer.

**D5 — The stage validates every input and extent before its first dispatch and dispatches nothing on error.** Today a broken frame may run some atoms and not others; this changes behaviour only on frames already broken.

**D6 — P2 mesh internals: packed relaxation state, one assembly, gradient normals dropped.** Emit writes `positions: array<vec4<f32>>` (w = 0) and `uvs: array<vec2<f32>>` (from the original position, as `volume_surface_mesh_body.wgsl:95`), plus indices and edge tables as today. Relaxation ping-pongs the 16-B positions with the same neighbour order (`surface_mesh_adjacency.wgsl:28`) and the same per-vertex expression text. The final pass computes normals with the `rsm_normal_vertex` text, reads uv once, writes the 80-B `MeshVertex` once, and zeroes the capacity tails, replacing the per-iteration clears. 23 → 19 dispatches, vertex-loop traffic ~5× less, 80-B writes 4 → 1. Rejected: fusing the last Jacobi step into normals or emit into relaxation (both need a barrier).

**D7 — `level_set` keeps today's meaning (pre-clamp smoothed lattice) and costs nothing unless consumed.** When smoothing runs, the smoothed buffer exists and is exposed. When every pass bypasses, alias the input if the slot system allows an output to alias an input; otherwise one dense copy, emitted only when `level_set` is consumed.

**D8 — Shared code stays shared and is called by the stage:** `ParticleSorter` (`sort_particles_into_cells.rs:122-185`), `PrefixScan`, the `read_total` kernel, `liquid_bricks::dispatch`, `brick_layout`/`refined_nodes`, and `start_capacity`/`emit_slots`/`grown_capacity` (moved with the stage).

**D9 — The stage is always indexed.** The five presets already wire `indices`. The rung adds the output and outer wire for older snapshots. The `Unindexed` perf variant (`gpu_flip_frame_perf.rs:346`) is dropped. No `indexed` toggle.

**D10 — Rungs are total for the surface group, matched by type id, and the atoms are deleted in the same phase.** A group containing a deleted type is rewritten by table; a shape the table cannot match is replaced by the shipped body with a migration note. A group with none of the deleted types is never touched. No hidden legacy types kept around.

**D11 — `running_total` and the scalar nodes stay catalog atoms.** `running_total` is used by `tests/fixtures/whitewater_vendored_group.json` and `whitewater_extent_tests.rs:291`; the stage calls `PrefixScan` directly.

**D12 — Fast-math rule for rewritten kernels (P2, P3).** Primary gate: bitwise against the old kernel text run in-test on the same inputs. A last-place mismatch does not kill the phase, but it does not land until (1) the f64 references pass at their existing tolerances (`surface_mesh_parity.rs:301`, `surface_mesh_normals.rs:197`, `shape_particle_blobs.rs:198`), (2) the frame-hash literals are updated in a commit naming the first differing word and ulp distance, and (3) Peter approves the hash change. Beyond 4 ulp, or any count or index word, is a bug.

**D13 — Buffers resize on parameter change, never on graph rebuild:** lattice scratch on `nodes`; mesh capacity on `max_capacity`/`resolution_scale` and `grown_capacity` growth as `volume_surface_mesh.rs:97`; relaxation scratch on vertex capacity.

---

## 3. Stage port surface

### `node.liquid_surface_mesh` (nodeId `liquid_mesh`, handle "Liquid Surface Mesh", boundary reason `BarrieredReduction`, declared like `whitewater_step.rs:84-240`)

Inputs: `levelset` (Array f32) ← `liquid_volume.levelset`; `nodes_x/y/z`; `solid`, `solid_nodes_x/y/z`; `center_x/y/z`, `size_x/y/z` ← Lattice Box; `cell_size`; `bricks` (optional, Array u32); `grow_offset` ← `liquid_fill_negative`; `fill_distance` ← `liquid_fill_distance`; `redistance_band` ← `liquid_redistance_band`; `smoothing_passes` ← `liquid_smoothing_passes`; `strength` ← `liquid_mesh_relaxation`.

Params: `resolution_scale` (Surface Detail target), `max_capacity` (Mesh Capacity), `iterations` (Smoothing Iterations).

Outputs: `vertices` (provided, grown, LiveExtent from the edge scan), `indices` (provided, grown, LiveExtent per item 3), `level_set` (D7).

Internal passes, labelled `liquid_mesh.<pass>`:
1. `grow` [bypass] → scratch L1
2. `redistance_shrink` [bypass] — the fused region text from A6 → L2
3. `smooth_x`, `smooth_y`, `smooth_z` [bypass] → ping-pong
4. `clamp` [removed under D4 when 1–3 all bypass; else the fused smooth_z+clamp text]
5. `count_triangles`, 6. `count_edges`
7. `scan_triangles` + `read_total_triangles`, 8. `scan_edges` + `read_total_edges`
9. P1: `mesh`, `relax` × iterations, `normals` — moved verbatim. P2: `emit`, `relax` × iterations over `positions`, `assemble` (D6).
10. `level_set_copy` only when consumed, all of 1–3 bypassed, and aliasing unavailable.

Oracle dispatch count: today 31–33; P1 23; P2 19.

### `node.liquid_blobs` (nodeId `liquid_blobs`, handle "Liquid Blobs", `BarrieredReduction`)

Inputs: `particles`, `count`, `center/size`, the bin scalars the sort takes today, `cell_size`. Params: `particle_scale`, `stretch`, `smoothing`, `isolated_scale`, `min_neighbours`. Outputs: exactly what `liquid_volume`, `liquid_bricks` and `group_output.level_set_bounds` consume from today's sort/blobs/bounds (P3 entry fixes names). The sorted particle buffer becomes internal.

Passes: `sort.*` via `ParticleSorter` (11) with an additive `SortJob { blobs: Option<&GpuBuffer>, blob_radius }`. When the fast path holds (CPU-evaluated in f32), `stabilise` writes the 48-B blob at `place` with the same expressions in the same order and skips the sorted write. Otherwise `stabilise` writes `sorted` as today and the moved blob kernel runs. Then `bounds.partial` + `bounds.finish`. 14 → 13 on the oracle.

---

## 4. Internals vs catalog atoms

| Type id | Used in | Fate |
|---|---|---|
| `offset_lattice`, `redistance_lattice`, `smooth_lattice`, `clamp_liquid_to_solids`, `count_surface_triangles`, `count_surface_edges`, `volume_surface_mesh`, `smooth_surface_mesh`, `relax_surface_mesh`, `surface_mesh_normals` | only the surface group, the v1160 fixture, and their own tests/registrations | internals of `liquid_surface_mesh`; deleted in P1 |
| `shape_particle_blobs`, `blob_bounds`, `sort_particles_into_cells` | same, plus the `graph_loader.rs:973` graft | internals of `liquid_blobs`; deleted in P3 (`ParticleSorter` stays shared with `gpu_flip_step`) |
| `particle_volume`, `lattice_bricks` | same, plus the `gpu_flip_surface.rs` graft | stay atoms (D2) |
| `running_total` | surface group, `whitewater_vendored_group.json`, `whitewater_extent_tests.rs:291` | stays (D11) |
| `value`, `math`, `scale_offset_value`, `transform_components` | everywhere | stay |

The node catalog lines for the deleted atoms go with them; two stage lines are added.

---

## 5. Invariants

| | Invariant | Check |
|---|---|---|
| I1 | The surface group body is identical in all five presets and holds exactly one of each stage | `gpu_flip_surface_group_is_shared_with_all_water_presets` extended with a type count |
| I2 | No deleted type id survives in presets, fixtures (except rung `before` fixtures), registry or palette | `liquid_surface_stage_leaves_no_legacy_atoms` + the negative rg per phase |
| I3 | A bypassed pass is a passthrough; a run pass uses the moved kernel with today's dispatch shape | `liquid_surface_stage_plan_bypasses_identities` (a pure `fn plan(&Controls, bricks: bool) -> Vec<Pass>`: defaults → no lattice passes; fill 2 + passes 2 → full list; NaN → runs) |
| I4 | Clamp removal is idempotent on this device | `liquid_surface_stage_clamp_is_idempotent_on_particle_volume`: bitwise lattice, clamp on vs off, `fixture(64)`/`(128)` at ticks 30/60/90/120, an adversarial plane solid whose zero crossing lies exactly on lattice nodes, and the Floating Box scene |
| I5 | Boundary equivalence on the oracle: vertices, indices, both totals bitwise against the atom chain | `liquid_surface_stage_matches_atom_chain_bitwise_64`/`_128` (atoms present in commit A; captured kernels after deletion) and `gpu_flip_frame_perf` hashes unchanged |
| I6 | Non-zero controls equivalence | `liquid_surface_stage_controls_nonzero_matches_oracle_bitwise` (Fill Pits 2, Smoothing Passes 2, relaxation 0.5, iterations 3, bricks on and off) |
| I7 | Changing a control between frames changes the plan with no lattice reallocation and no rebuild | `liquid_surface_stage_live_controls_take_effect_without_rebuild` |
| I8 | One extent rule per stage, same sizing functions, every pass covered | stage rules in `liquid/extent.rs`; `gpu_flip_memory_at_every_lattice` |
| I9 | Every internal pass is labelled and exercised by a scene | `gpu_flip_scenes_cover_every_dispatch` extended |
| I10 | Stage shaders are atomic-free (sorter excluded) | `liquid/conformance.rs:396` gains both stages |
| I11 | Migration: idempotent, total, binding-preserving | `liquid_surface_mesh_rung_rewrites_peter_layer`, `_is_idempotent`, `_bundled_presets_pass_through_unchanged`, `_retargets_iterations_binding`, `_leaves_groups_without_legacy_atoms`, `_replaces_unmatched_shape_with_shipped_body_and_notes`; same set for the blobs rung |
| I12 | `level_set` costs nothing unless consumed | plan test: no copy label when unconsumed |
| I13 | Dense vs bricks stays bitwise for the stage | `fluid_bricks_lattice_and_mesh_bit_identical_dense_64/128` rewritten against the stage |
| I14 | Blob fast path equals the general path on fixed spheres | `fluid_shape_fixed_spheres_match_neighbour_gather` rewritten against the stage, u32 words |

---

## 6. Phases

Measurement for every timing gate: two bundled release binaries (base SHA and branch SHA, each with its own presets), `scripts/gpu_queue.py` lock, `manifold frame-time <project> --frames 300 --splash-frames 100` for tick interval p50/p95 and `--stamp-every 1 --stamp-granularity node` for node stamps, interleaved A/B ×3, node stamps as ratios only. Scenes: Dam Break GPU FLIP at 64 and 128, Floating Box Matter for a moving solid. Tick p50 must not regress at either resolution; p95 not by more than 2%.

### P0 — Census (no code)
The per-dispatch stamp table on the oracle confirming A6/A9/A13; per-node stamps at 64 and 128 for the chain (ratio bases); the exact group wire and param tables from the five JSONs and the v1160 fixture; whether grow/redistance dispatch 1 or 2 passes under bricks; `level_set` consumers per preset. Paste corrections into section 1.

### P1 — `node.liquid_surface_mesh` with lattice bypass; presets, rung, atom deletion
Read-back: `whitewater_step.rs:84-240, 440-560`, `volume_surface_mesh.rs` whole, `relax_surface_mesh.rs:116-292`, `liquid/extent.rs:574-626`, `freeze/install.rs:1871-2002`.

Commit A (atoms present): the stage node and its shader files holding the moved texts (generated standalone texts for grow and the smooths, the two fused region texts, count/edge/mesh/relax/normals verbatim); `liquid_surface_stage_shaders_equal_installed_region_sources` asserting string equality with what freeze installs today, which makes "verbatim" a checked fact; I3–I8, I12. Captured texts go to `tests/fixtures/liquid_surface_oracle/`.

Commit B: five presets and the builder switched (I1); the migration rung and I11; the ten atoms, shaders, registrations, extent rules and catalog lines deleted; dependent tests rewritten (A17); `LABELLED` gains the stage; `Unindexed` dropped; I2, I9, I10, I13.
Docs in commit B: a supersession note in docs/GPU_FLUID_SURFACE_DESIGN.md; the stage named in docs/DECOMPOSING_GENERATORS.md section 1.2 (Specialised solvers are stage nodes) and in the docs/ADDING_PRIMITIVES.md exclusion 6 list.

Gate: I1–I13 green; frame hashes unchanged (D4 fallback if I4 is red, hashes still unchanged); landing gate green; negative rg `rg -n "node\.(offset_lattice|redistance_lattice|smooth_lattice|clamp_liquid_to_solids|count_surface_triangles|count_surface_edges|volume_surface_mesh|smooth_surface_mesh|relax_surface_mesh|surface_mesh_normals)"` hits only in the rung and `crates/manifold-io/tests/fixtures/`. Timing: at 128 stage stamp / P0 sum ≤ 0.75; at 64 ≤ 0.9; tick p50 not worse. Above 1.0 means the stage is wrong (same kernels minus eight).

Forbidden: changing a moved kernel's text; touching `particle_volume.rs`; a codegen path for the stage; an `indexed` toggle; dropping `level_set`.

### P2 — Mesh internals rewrite (D6)
Entry: P1 landed and measured, and the mesh passes are ≥ 10% of the stage stamp at 128; under 10%, P2 is dropped.
Deliverables: `emit`, `relax`, `assemble` kernels and packed buffers; plan test at 19; `liquid_surface_stage_mesh_rewrite_matches_captured_kernels_bitwise` (ticks 30/60/90/120, iterations 0–3, strength 0/0.5, bricks on/off: vertices, indices, totals, tails); f64 references green.
Gate: bitwise, or D12 with Peter's sign-off. Timing: stage stamp P2/P1 ≤ 0.85 at 128, < 1.0 at 64; tick p50 not worse.
Forbidden: changing neighbour order, uv source, index/edge ownership, tail semantics; fusing a Jacobi step into another pass; touching the lattice passes.

### P3 — `node.liquid_blobs`
Entry: P1 landed; independent of P2.
Deliverables: the stage + moved blob/bounds shaders; the additive `SortJob` extension (`blobs: None` for every existing caller); plan test (13 on the oracle, 14 with stretch 1.5); I14; `liquid_blobs_stage_matches_atom_chain_bitwise` (blobs, ranges, bounds as u32 words); the blobs rung + I11 set; three atoms deleted, `wire_blob_bounds` graft and its tests deleted (the rung inserts bounds); presets, builder, conformance, labels, extent as P1.
Gate: bitwise or D12 (the fast path has no add/mul pairs to contract, so a mismatch is likely a bug). Timing: sort+blobs+bounds stamp ratio < 1.0 at 64 and 128, p95 not worse.
Forbidden: changing `ParticleSorter` pass order, any existing `SortJob` caller, or `gpu_flip_step` sort labels.

### P4 — Cleanup
Entry: P1 and P3 landed. Delete the `gpu_flip_surface.rs:78-144` graft and its tests once the rung demonstrably inserts `lattice_bricks` for the v1160 shape; close BUG-twnl; catalog and doc sweep; `gpu_scope` mapping for the new files.

---

## 7. Migration rung `liquid_surface_mesh` (spec)

Walk every preset instance including `embeddedPresets[].def` (as `solve_level_card_v1180.rs:31`). For each group whose body contains a deleted type:

1. Match by type id: `particle_volume` (keep); `volume_surface_mesh` becomes the stage (keep id, nodeId, handle, position; copy `resolution_scale`, `max_capacity`); `smooth_surface_mesh` → `iterations` (absent and n relax nodes → `iterations = n`); `clamp_liquid_to_solids`; `smooth_lattice` ×3 by axis; `offset_lattice` ×2 (grow feeds redistance, shrink is fed by it); `redistance_lattice`; both counts; `running_total` ×1–2 (only those fed by the counts); `surface_mesh_normals`; `relax_surface_mesh` ×n. Any other multiplicity → step 6.
2. Wire the stage: `levelset` ← what fed grow (or smooth_x, or clamp in the oldest shape); solid, nodes, box, cell size, bricks ← what the clamp/mesh/smooth nodes had; `grow_offset`, `fill_distance`, `redistance_band` ← their scalar sources if present, else unwired (0 = bypass = the old shape's behaviour); `smoothing_passes` ← smooth_x.passes source; `strength` ← the relax/smooth strength source.
3. Delete the matched nodes (except particle_volume and the stage) and their wires.
4. Consumers of vertices/indices/level_set now read the stage. Missing `indices` → add the interface output and the outer wire to the node consuming `vertices`; if it has no `indices` port, note and skip.
5. Bindings and interface params on `(smooth_surface_mesh nodeId, iterations)` → `(stage nodeId, iterations)`; its `targetHandle` → the stage handle.
6. Unmatched shape: replace the body with the shipped body, keep interface params by name, note the replacement.
7. Idempotent; bundled presets pass through byte-identical.

Tests on `water_layer_graph_v1160.json`: the 20-node body becomes particle_volume + stage + surviving scalars; `iterations = 2`; `strength` wired to the old relaxation source; `indices` added; whitewater's `level_set` wires preserved; the loader then does no graft, and the plan runs the three smooths (passes 2.0). Peter's own layer only gets the lattice win once its Smoothing Passes card is 0.

The blobs rung follows the same shape for sort + blobs + bounds → stage at the blobs node's id, inserting bounds when absent.

---

## 8. Decided — do not reopen
- Two stages; particle_volume and lattice_bricks stay atoms in v1.
- Bypass from the shaders' own branches, CPU-evaluated per frame; no control removed.
- Clamp removal gated by I4 with the 2-dispatch fallback; never in place on an upstream buffer.
- Moved kernels verified by string equality; rewritten kernels gated by captured texts in-test under D12.
- Always indexed; `Unindexed` perf variant dropped.
- Rungs total by type id; atoms deleted in the same phase.
- `running_total` and scalar nodes stay catalog atoms.
- Measurement is the house method, ratios only.

## 9. Deferred (with triggers)
- Fold particle_volume + lattice_bricks into the mesh stage — gather lane landed and measured.
- Fold the blob stage into the mesh stage — both landed and a measured cost at their boundary.
- Two-lane triangle+edge scan — scans + read_totals > 10% of the stage stamp at 128.
- `bounds.partial` folded into `stabilise` — bounds > 5% of the blob stage stamp.
- Emit dispatched indirectly over the triangle extent — measured emit cost with mostly-empty bricks.
- Delete `running_total` from the catalog — `whitewater_vendored_group.json` retired.
- `level_set` aliasing instead of a copy — slot-system passthrough support confirmed.
- Shrinking the renderer's 80-B `MeshVertex` — out of scope.
