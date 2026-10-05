# Cooperative brick gather for `node.particle_volume`

**Status:** APPROVED in direction (Peter, D1) · 2026-10-06 · Fable (design), Claude lead · owed: Astra review, then one build session (P0 folded into P1).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase. Anchors are at origin/main 5a1218c89.

`node.particle_volume` builds the water's level set: every lattice node gathers the fluid blobs near it and keeps the minimum distance. On the GPU FLIP Dam Break oracle (res 64, 30 fps project) it costs about 7 ms on every frame, half of a frame without a solver tick. A frame with a tick costs about 35 ms against a 33.3 ms budget.

The cooperative gather is not mainly a bandwidth trick. Today every node recomputes, for every blob it visits, the blob's support box, `floor((c ± support − lmin)/h)` (six divisions, six floors, twelve subtractions), before it knows whether the blob can touch it (`particle_volume_body.wgsl:120-124`). A brick that stages each blob once can compute that box once for 256 nodes. Pure load sharing is predicted to break even at best, and the house already tried a naive tile and lost 15× (`docs/GPU_FLUID_SURFACE_DESIGN.md:1199`: 4³ tile, 31 KB of threadgroup memory, 30.4 ms vs 1.95 ms). This design differs in three ways: streamed chunks of at most 9 KB, per-lane exact range iteration instead of a per-slot predicate, and the box hoist. The build first gathers evidence of where the 7 ms goes, and a timing gate decides whether the kernel stays.

On stage: if the timing gate passes, particle_volume drops from about 7 to at most 4.5 ms on every frame and tick frames move toward the 33.3 ms budget of a 30 fps project; whether that is enough for a steady 30 fps is measured, not promised. The surface is identical pixel for pixel (hash-equal); Peter stops feeling the tick.

## 1. Audit — what exists

| Piece | Where | State |
|---|---|---|
| `particle_volume` declaration | `primitives/particle_volume.rs:114-119` | `fusion_kind: Pointwise`, `wgsl_body`, `input_access: [BufferGather ×6]`, `derived_uniforms`, `wgsl_includes: [liquid_bricks::COMMON]`, `buffer_index: "liquid_brick_index"`. Pipeline from `standalone_pipeline::<Self>` (`:195`, codegen). |
| Does it ever fuse? | `freeze/region.rs:1210-1211`, `:1569-1570` | No. A `buffer_index` atom without `dense_buffer_fusion` is `NodeClass::Boundary`; `particle_volume` declares none. `GPU_FLUID_SURFACE_DESIGN.md:668` expects it standalone. Machine check: `cargo run -p manifold-renderer --bin graph-tool -- fusion crates/manifold-renderer/assets/generator-presets/WaterDamBreakGpuFlip.json` shows it in no region. Fusion is not at stake; only the codegen-path rule is. |
| Dispatch shape | `particle_volume.rs:270-324`, `liquid_bricks.rs:25-45` | With `bricks` wired: pass 2 (exterior, dense `count.div_ceil(256)` groups), then pass 1 (indirect at `GRID_OFFSET`). Header `bricks[1] = 2·active` (`shaders/lattice_bricks.wgsl:211-212`): two 256-thread groups per brick. `liquid_brick_map` (`shaders/liquid_bricks_common.wgsl:11-23`) gives group `g` the half-brick z ∈ [4·(g%2), 4·(g%2)+3], sentinel `0xffffffff` past `dims`. |
| Per-node search | `shaders/particle_volume_body.wgsl:95-108` | Window `[first_bin, last_bin]` = `home ± reach_bins` ∩ `[query_lo, query_hi]` ∩ lattice; loops z, y, x ascending, then `k` ascending (`:109-114`). Per blob: box test `:120-124`, term `:127-134`, `phi = min(phi, term)`. After the gather: interior union `:148-154`, solid clamp `:155-157`. |
| Preset wiring | `WaterDamBreakGpuFlip.json:535-540, 669-674` | Scale 1 on volume and bricks; `interior` wired from `system.group_input.interior` (sentinel values while Narrow Band is 0, `gpu_flip_step.wgsl:2740-2742`); `band_extra` ← `liquid_fill_band` = 0; bricks wired. |
| Metal math mode | `crates/manifold-gpu/src/metal/device.rs:759` `setMathMode(MTLMathMode::Fast)` | Contraction is on and context-dependent: `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md:161` records two bodies compiling `a·b+c` differently (1.8482413 vs 1.8482416). Bitwise across two kernels is a test result, never a construction. |
| Existing bitwise test pattern | `particle_volume.rs:701-759` `gpu_flip_volume_tight_bounds_matches_original_search_exactly` | Rebuilds an older kernel by string-replacing lines of the generated WGSL; three lattices (one at origin 1000,−1000,1000 with 0.03 cells), scales 1–3, band 0 and 0.6·cell, `search_boundary_blobs` (`:639-698`) at next_up/next_down of every boundary. |
| Harness | `primitives/liquid_surface_tests.rs:27-130`; `liquid_bricks_gpu_tests.rs:88` `fixture(resolution)` builds the Dam Break surface chain at 64/128 (`:508-518` bitwise dense vs bricks) | Reused for every fixture below. |
| Whole-frame oracle | `tests/gpu_proofs/gpu_flip_frame_perf.rs:26-27, 141, 361` | Shipped preset at 64, 300 frames, per-node-type split, FNV hash of every stamped frame's output. |
| Measurement | `crates/manifold-app/src/frame_time.rs:1-33`; `scripts/gpu_queue.py` | House method: bundled release binaries, interleaved A/B under the GPU lock, tick interval p50/p95; node stamps as ratios only. |
| Prior cooperative attempt | `GPU_FLUID_SURFACE_DESIGN.md:1199` | Dropped: 4³ tile, all bins copied at once, 31 KB of the core's 32 KB, one 64-thread group per core. |
| Codegen atom plus hand kernels | `primitives/bokeh_gather.rs:156-167, 244-290` | `boundary_reason: BarrieredReduction` + `wgsl_body` via `standalone_for_boundary_spec` + seven hand passes. |
| `standalone_for_boundary_spec` | `freeze/codegen/entry_points.rs:87-116` vs `:16-39` | Calls `generate_standalone_buffer(spec, &[])` without `P::BUFFER_INDEX`: relabelling `particle_volume` a boundary would silently drop the brick schedule from its codegen kernel. |
| Shader validation | `tests/wgsl_validation.rs:1-60` | Auto-discovers every `.wgsl`, `ValidationFlags::all()` (uniformity included). `workgroupUniformLoad` precedent `gpu_flip_step.wgsl:1331`; 256-wide tile scan `shaders/prefix_scan.wgsl:32-43`. |
| Copy-only mesh chain | `liquid_volume.levelset → liquid_grow → liquid_redistance → liquid_shrink → liquid_smooth_x → _y → _z → liquid_clamp_to_solids` | With Fill Pits 0 and Smoothing Passes 0: grow offset −0.0, redistance off, shrink offset 0, passes 0. No-op contracts: `shaders/offset_lattice_body.wgsl:2`, `redistance_lattice.rs:31,109`, `smooth_lattice.rs:35`. Expected five standalone copies (grow, redistance, shrink, smooth_x, smooth_y), smooth_z folded into the clamp region. Verify with `graph-tool fusion`. |
| Passthrough machinery | `effect_node.rs:767-790` (texture-only), `execution.rs:2115-2330`, `metal_backend.rs:1028-1062` `alias_2d`, `execution.rs:1691-1697` `clear_skip_aliases`, `execution_plan.rs:888-1025` lifetime extension | Lifetime extension walks forward topo order (`:910`), not transitively across a chain of skipping nodes. `skip_passthrough(params, wired_inputs)` cannot see a wired scalar's value; all five controls here are wired. |

## 2. Decisions

**D1 — The codegen kernel stays the general path and the oracle; the cooperative kernel is an internal pass-1 schedule of the same atom.** The macro declaration does not change (`Pointwise` + `buffer_index` already classifies the atom a boundary, `region.rs:1210`). The atom gains `brick_pipeline: Option<GpuComputePipeline>` built from `shaders/particle_volume_brick_gather.wgsl`, self-contained, with its own `Params` and `buf_*` declarations mirroring the generated kernel and pinned by test I2. Pass 0 (no bricks wired) and pass 2 (exterior) keep the codegen kernel; pass 1 dispatches the cooperative kernel through the same indirect header. This is not covered by docs/ADDING_PRIMITIVES.md exclusion 1 (barriered reduction): the mandate asks whether the atom is expressible as a barrier-free per-element function, and `particle_volume` is (exclusion 1 excuses atoms from fusion, not from codegen). It is an explicit, narrowly authorized exception. **Peter, 2026-10-06:** "Just do the hand written one properly once please, I don't want a huge mess of nodes that aren't fully optimised ... Users won't touch these GPU FLIP nodes." The exception holds only while all of these stay true: the atom adds no fusion boundary (it is already one, `region.rs:1208`); both kernels call the same arithmetic helpers; the hand kernel's ABI is verified against the generated one (I2); the generated kernel keeps running passes 0 and 2 and stays the bitwise oracle; a measured saving justified it; its dispatch carries its own label. The build records the exception in ADDING_PRIMITIVES as a new numbered exclusion naming this atom and these conditions; it records Peter's decision, it does not make it. Rejected: relabel as `BarrieredReduction` (misnames a gather, and the boundary codegen entry drops `buffer_index`, so passes 0 and 2 would lose the schedule); teaching the freeze compiler cooperative schedules (compiler infrastructure for one atom); a new primitive (`GPU_FLUID_SURFACE_DESIGN.md:1275` forbids a second level-set atom beside `particle_volume` by name, and the docs/DECOMPOSING_GENERATORS.md section 2.5 (primitive audit) finds nothing to reuse).

**D2 — Exactness by order, not by commutativity.** Each lane visits exactly its own dense window's `(bin, k)` sequence in the dense kernel's order (z, y, x, k ascending), reading staged copies. Signed zero and NaN behaviour of `min` are then moot: the same operands reach the same builtin in the same order. The group's union window is only a loading superset; no lane's predicate is relaxed. Rejected: a per-slot "bin in my window" predicate over the whole union (5–6× more predicate work per lane at scale 1, and order-dependent `min`).

**D3 — The box hoist is part of the experiment.** The loading lane computes `first/last` once per staged blob with the body's expressions (`:120-123`). Without it the cost model predicts break-even. The box lives in threadgroup memory, never on a wire, so `FluidBlob` and its ABI are untouched.

**D4 — Shared helpers and a three-step bitwise gate on a stated device.** `pv_blob_term` and `pv_blob_box` move into `shaders/particle_volume_common.wgsl` (a `wgsl_includes` entry) as the only copy. Writing `fma()` explicitly is a choice of expression, not a guarantee the old compiler chose the same, so it may itself change the baseline. Three separate bitwise gates, in order: original generated kernel → refactored generated kernel → cooperative kernel. The risk is not only contraction: window arithmetic, division, `length`, interpolation, reassociation and exceptional values all matter under `MTLMathMode::Fast` (`device.rs:760`), and WGSL allows signed-zero and exceptional-value freedom. So bitwise is a release gate on the stated device and compiler (Apple M4 Max, the repo's Metal toolchain); the NaN/±inf fixtures are regression observations on that stack, not a proof for every input. Any ULP relaxation changes the contract and stops the work until Peter rules.

**D5 — One build, measured as it goes.** Peter asked for the kernel built properly once, so there is no separate prototype stop. The build session first gathers regime evidence (section 7, P1 step 1), then writes the kernel, proves it bitwise, and times it; the timing gate is the kill gate. The regime probes are evidence about where time goes, not a decisive decomposition: removing the term also removes shape loads and changes register pressure; removing the box sends more candidates through the term; aliasing blob addresses changes cache behaviour. Probes keep their data dependencies and are read alongside the emitted code.

**D6 — Array passthrough is parked behind the over-noding audit.** If the audit (Astra, 2026-10-06) folds grow/redistance/shrink/smooth into one surface stage kernel, a no-op pass is skipped inside that kernel and executor array aliasing is not needed. If it is revived, Astra's review adds required invariants beyond P3-1 to P3-5: aliases cleared on a destination's same-frame tenant change with its owned allocation restored (the texture path does this, `metal_backend.rs:1072`, called at `:772`); a separate borrowed alias map rather than clones written into `buffers_array`; protected external storage with a copy fallback when aliasing is refused; readiness, logical identity and live extent preserved through chained aliases; lifetimes extended through mixed and fan-out chains; the executor's pending-input guard (`execution.rs:2145`) kept.

## 3. The cooperative kernel

**Group = half brick** (256 threads, 8×8×4 nodes), decoded from `workgroup_id.x` exactly as `liquid_brick_map` does (rank = g/2, half = g%2, local = half·256 + lid), so the indirect header and `lattice_bricks` are untouched. Sentinel lanes and lanes with `idx ≥ params.dispatch_count` are inactive: empty window, never store, but reach every barrier and every return the active lanes reach.

Per lane, once, with the body's exact expressions (`:88-108`): `p`, `ijk`, `first_bin`, `last_bin`, `phi = band`.

Group union `U = [min first_bin, max last_bin]` by a 256-lane tree reduction in threadgroup memory (no atomics, keeping the shader eligible for the `atomic_free_shaders` conformance row, `liquid/conformance.rs:227`); inactive lanes contribute identities; read back with `workgroupUniformLoad`.

Synchronization rules, each written into the shader as a comment beside its barrier:

- The `any(bins < 1)` return is uniform (bins come from uniforms) and happens before any barrier; active lanes store `band` first.
- An all-inactive half brick (the union is still the identity after the reduction) takes an explicit uniform empty path: no widths are computed from identities; it falls through to the end with no store.
- Scratch lifetimes: only the union reduction reuses the prefix allocation, and it finishes (barrier) before the first run. Within a run the prefix and the staged blobs are separate storage, both live across every chunk, because loaders search the prefix on every chunk. A run whose total is zero executes no chunk barrier, so an unconditional barrier ends every run, after the lanes' prefix reads and before the next run's scan writes.
- The house scan (`prefix_scan.wgsl:34`) is inclusive with sixteen barriers. The exclusive prefix is built in this order: each lane writes its count and keeps it in a register → barrier → inclusive scan → the total is captured uniformly (`workgroupUniformLoad` of the last inclusive entry) before anything modifies the scan → each lane writes `prefix[t] = inclusive[t] − count_t` into a separate array (an in-place shift would need an extra read-before-write barrier) → lane 0 writes `prefix[256] = total` → barrier before any prefix consumer.
- Indices: within a run, `t` and `i` are run-local (0..256); the global bin is `q = run·256 + i`, decoded as `(U.x0 + q % w, U.y0 + q / w, z)`, and `ranges` is read at that global bin. A lane's row endpoints subtract `run·256` before indexing the 257-entry prefix; an empty row-run intersection performs no prefix read.
- Loader binary searches may diverge; they terminate and reconverge before the next unconditional barrier and contain no collective operation and no return.
- All control flow around barriers derives from uniform or `workgroupUniformLoad` values. naga's uniformity analysis (`wgsl_validation.rs:167`) checks that; the scratch-lifetime rules above are argued, not machine-checked.

```
if union is empty: skip to end                # uniform
for z in U.z0..=U.z1:                         // uniform
  rect = U.x-range × U.y-range, w = width
  lane_z = first_bin.z <= z && z <= last_bin.z    // the lane consumes this slab only if its own window covers it
  for run in 0..ceil(w*h/256):                // uniform
    q = run*256 + t; count_t = ranges[global bin of q].count if q < w*h else 0   // t run-local
    exclusive prefix[0..256], prefix[256] = total, built as in the scan rule above
    lane (if lane_z): for each of its rows y in [y0,y1] ∩ rect: run-local flat range [a,b] = (row ∩ run) − run*256;
      if non-empty, slots [prefix[a], prefix[b+1]) (no prefix read when empty)
    for chunk_base in 0..total step CHUNK:    // uniform
      loader t < CHUNK, g = chunk_base + t < total: run-local bin i = upper-bound search, the i with prefix[i] <= g < prefix[i+1]
        (skips the repeated prefixes of empty bins); global bin of run*256 + i; k = ranges[that bin].start + (g − prefix[i]);
        stage blob and box = pv_blob_box(blob)
      workgroupBarrier()
      lane (if lane_z): for each row: for g in clip([lo,hi), chunk): phi = min(phi, pv_blob_term(p, ijk, slot[g − chunk_base]))
      workgroupBarrier()
    workgroupBarrier()                        // unconditional: prefix reads end before the next run's scan writes
post-gather with the same helpers, same order as the body :140-157; active lanes store buf_levelset[idx]
```

The lane's sequence equals the dense kernel's (Astra checked the ordering argument): the dense loops run z outer, y, x inner, k ascending; the loader lays bins out in flat (y·w + x) order within a z slab and k ascending within a bin; runs partition the bin stream monotonically and chunks partition each run's blob stream monotonically; the lane takes only its own z slabs (the `lane_z` filter the dense kernel applies at `:109`), its own rows and its own x range, in ascending order. A row split across runs resumes before any later row; a bin split across chunks resumes at the next `k`. Empty bins occupy no slots in either.

**Threadgroup memory (CHUNK = 128):** staged blob 40 B (10 f32; `shape_diag.w` and `shape_off.w` are unused by the term), box 6 × i32 = 24 B (not packed: `vec3<i32>(floor(x))` on NaN/±inf must take the body's conversion), prefix 257 × u32, reduction scratch reusing the prefix array: about 9 KB. Under the house's 32 KB figure that is a capacity upper bound of three groups per core, not established occupancy; registers and the scan decide the real number. CHUNK = 256 is about 17 KB, one group per core under that figure. The build tries 128, then 64 and 256.

**Cost model** (per lane per half brick; V = dense visits per node, f = box-pass fraction, c_box ≈ 45 ops including six divisions, c_term ≈ 25 including `sqrt`, I = union inflation ≈ ((8+2r)/(2r+1))²·((4+2r)/(2r+1))):
dense ≈ V·(c_box + f·c_term) + 3V global loads;
cooperative ≈ V·(6 + f·c_term) + 3V threadgroup loads + (I·V/256)·(3 global loads + c_box) + Σ_runs ⌈total_run/CHUNK⌉·(2 barriers + rows·2) + per run (scan, 16+ barriers) + per staged blob (binary search, staging writes, box reads).
At I ≈ 5, V ≈ 10⁴, f ≈ 0.1, CHUNK 128: dense ≈ 475k ops, cooperative ≈ 120k plus the synchronization terms, which are unmeasured. With f ≈ 1 the hoist still removes the box work but leaves the term, so the margin shrinks rather than vanishing. The earlier tile's 15× loss is real evidence; this design's occupancy argument does not yet prove it avoids the same fate, which is why the timing gate decides.

## 4. Exact equivalence

- **E1 candidate sequence.** For every lane, the `(bin, k)` sequence visited, order included, equals the dense body's. Combinatorial proof above, checked by a CPU test over random lattices, windows, run splits and chunk sizes (I1).
- **E2 per-candidate arithmetic.** One helper pair in `particle_volume_common.wgsl`, called by both kernels. The residual risk is everything D4 names (window arithmetic, division, `length`, interpolation, reassociation, exceptional values under `MTLMathMode::Fast`), covered only by D4's three bitwise steps and the whole-frame hashes (I3, I4, I8) on the stated device.
- **E3 after the gather.** Interior union and solid clamp use the same helpers in the same order; the `interior_len` branch is identical and uniform.
- **E4 every input the node accepts.** `band_extra` 0 and above 0; interior unwired, native padding and solver padding; `resolution_scale` 1–4 (8 at the clamp); a lattice origin far from zero with cells near f32 resolution; rectangular lattices whose edge bricks exceed `dims`; empty bricks (border bricks are always active, `lattice_bricks.wgsl:190`); bins holding more than CHUNK blobs; union rects wider than 256 bins (`reach_bins ≥ 5` at scale 1); blobs with `reach ≤ 0`; NaN/±inf centres; `bounds[0] = 0`; `any(bins < 1)`; `dispatch_count` below the lattice total. Each is a named fixture in section 5 (Oracle and proofs).

## 5. Oracle and proofs

The oracle is the shipped codegen kernel, unchanged text, on the same inputs; before the helper refactor it is reconstructed in-test by string replacement (`:704-713`). The dense reference shader is not an oracle: it omits `band_extra` and interior.

| Test | Fixture | Asserts |
|---|---|---|
| `brick_gather_slot_sequence_equals_dense_window_sequence` (CPU) | random bins, ranges, windows, run and chunk splits | E1 exact sequence equality |
| `particle_volume_shared_helpers_match_inline_body_bitwise` | the three lattices of `:720-722`, scales 1–3, bands {0, 0.6·cell}, `search_boundary_blobs` | refactored dense kernel equals the inline kernel, `to_bits` |
| `particle_volume_brick_gather_matches_codegen_bitwise` | same matrix × interior {none, native, solver} × bricks from `node.lattice_bricks` | cooperative pass 1 equals codegen pass 1 on every active node, `to_bits`; exterior untouched |
| `particle_volume_brick_gather_chunk_tails_bitwise` | one bin with 3·CHUNK+1 blobs; `reach_bins ≥ 5`; an edge brick past `dims`; a brick with no blobs; an all-inactive half brick; a run whose total is zero; NaN/inf blobs; blobs with `reach ≤ 0`; `bounds = [0,0]`; `any(bins < 1)`; `dispatch_count` below the lattice total; `resolution_scale` 4 and 8 | bitwise on active nodes; exterior and sentinel storage untouched |
| `particle_volume_brick_gather_dam_break_frames_bitwise` | `liquid_bricks_gpu_tests.rs:88` `fixture(64)` and `(128)`, ticks 30/60/90/120 | bitwise on the live preset lattice |
| `gpu_flip_narrow_band_mesher_values_with_bricks` | the `:762-806` cases with a brick schedule wired | value-level against CPU f64, through the cooperative path |
| `particle_volume_brick_kernel_abi_matches_codegen` | naga reflection of both sources | `Params` members, offsets, types and bindings 0–7 identical; `size_of::<VolumeUniforms>() == 80` |
| `particle_volume_brick_shader_is_valid_wgsl` | auto-discovery in `wgsl_validation.rs` | parses and validates, uniformity included |
| `gpu_flip_frame_perf` output hashes | shipped preset, 300 frames | per-tick hashes identical to the baseline binary's list |

Fused-vs-unfused proofs do not apply: the atom is a boundary by `region.rs:1210`, and I5 pins that it stays one.

## 6. Invariants and enforcement

| Invariant | Check |
|---|---|
| I1 Per-lane candidate sequence equals the dense window sequence | `brick_gather_slot_sequence_equals_dense_window_sequence` |
| I2 Hand kernel ABI equals the generated kernel's | `particle_volume_brick_kernel_abi_matches_codegen` |
| I3 Pass-1 output bitwise equal to codegen pass 1 | `_matches_codegen_bitwise`, `_chunk_tails_bitwise`, `_dam_break_frames_bitwise` |
| I4 Whole-frame hashes unchanged | `gpu_flip_frame_perf` hash diff against the baseline |
| I5 `particle_volume` stays a partition boundary; the cooperative pipeline runs only on pass 1 | `graph-tool fusion` in the P2 gate; `rg -n "brick_pipeline" particle_volume.rs` shows one dispatch site under `brick_pass == 1` |
| I6 No atomics in the cooperative shader | `("node.particle_volume", BRICK_GATHER_SHADER)` added to the GPU FLIP row's `atomic_free_shaders` (`liquid/conformance.rs:396`) |
| I7 Barriers in uniform control flow | naga `ValidationFlags::all()` in `wgsl_validation.rs` |
| I8 The helper refactor changes no bit | `particle_volume_shared_helpers_match_inline_body_bitwise` (original generated → refactored generated, D4 step one) |
| P3-1 Array alias lifetime extends transitively through a chain of skipping nodes | `chained_skip_passthrough_extends_root_lifetime_transitively` |
| P3-2 Array alias requires equal byte capacity and `ArrayType` | `array_passthrough_refuses_capacity_or_layout_mismatch` |
| P3-3 Array aliases clear at frame start | `array_passthrough_clears_next_frame` |
| P3-4 Live extent of an aliased array resolves through the alias | `array_passthrough_propagates_live_extent` |
| P3-5 Wired control values decide the skip | `array_passthrough_reads_wired_control` |

## 7. Phasing

Peter asked for the kernel built properly once: P1 and P2 run as one lane assignment with no stop between them unless P1's timing gate fails. P3 is parked (D6).

**P1 — Build, prove, measure (the kill gate).**
*Entry:* section 1 anchors re-verified on current main; the oracle project confirmed with `project_tool` (res 64, 30 fps, the bundled Dam Break); machine quiet.
*Read-back:* sections 3 and 4 here, `particle_volume_body.wgsl` whole, `liquid_bricks_common.wgsl`, `prefix_scan.wgsl:32-43`, `bokeh_gather.rs:244-290`, `particle_volume.rs:701-759`, `GPU_FLUID_SURFACE_DESIGN.md:1186-1205`.
*Step 1, regime evidence (no production code, about an hour):* dump the blobs, cell ranges, bricks and bounds arrays at ticks 30/60/90/120 (`examples/fluid_capture.rs`; verify those arrays are dumped) and a scratchpad script that reproduces the window and box predicates of `:95-124` in f32 (f64 only as a separate diagnostic), reporting V (visits per active node, p50/p95), f (box-pass fraction), active half bricks A, union inflation I, and max blobs per bin. Optionally three throwaway timing probes on a scratch branch (term replaced by a one-op accumulate that keeps its data dependencies; box test removed; aliased blob addresses), read with the emitted code, as regime evidence only (D5). Report the table before writing the kernel. Not a gate.
*Step 2, the kernel:* `shaders/particle_volume_common.wgsl` (the two helpers), the body refactored onto them, `shaders/particle_volume_brick_gather.wgsl` (CHUNK a `const`), the `brick_pipeline` field prewarmed at install beside `pipeline` (COMPILE_CONTRACT) and the pass-1 dispatch labelled `node.particle_volume.bricks`.
*Step 3, proofs before any timing:* I1 (CPU sequence), I2 (ABI), the three bitwise gates of D4 in order (original generated → refactored generated on the three-lattice matrix; refactored generated → cooperative on the same matrix), the composed shader validated (I7), and the empty-prefix and all-inactive fixtures.
*Step 4, the timing gate:* bundled release binaries (main, branch), interleaved A,B ×3 through `gpu_queue.py` on the oracle project: tick interval p50/p95; one node-stamped run per binary for the particle_volume ratio; one `--frame-clock --stamp-every 3` run per binary to isolate no-tick frames. **Continue iff** the particle_volume ratio ≤ 0.70 on every pair (about 2 ms of 7), no-tick frame GPU p50 down at least 1.5 ms, and tick interval p95 no worse than +0.5 ms. Try CHUNK 128, then 64 and 256, before concluding. If all lose, stop: delete the kernel, keep the regime table, record the result in `GPU_FLUID_SURFACE_DESIGN.md`'s lever table beside the dropped tile, and log a bead.
*Forbidden:* loosening any lane's window; packing the box; atomics; "approximately bitwise"; timing before step 3 is green.
*Test scope:* `cargo test -p manifold-renderer --features gpu-proofs particle_volume` through `gpu_queue.py`; clippy `-p manifold-renderer`.

**P2 — Harden and land (same assignment).**
*Entry:* P1's gate passed with a recorded CHUNK.
*Deliverables:* the remaining section 5 fixtures, the I6 conformance row, the ADDING_PRIMITIVES exclusion naming this atom and D1's conditions (recording Peter's decision), the NODE_CATALOG line, and the lever-table status in `GPU_FLUID_SURFACE_DESIGN.md`. Exterior and sentinel storage is checked untouched, not only active values.
*Gate:* every section 5 test green; `gpu_flip_frame_perf` hashes equal to the pre-change binary's; `graph-tool fusion` region count unchanged; `scripts/landing_gate.py`; Astra reviews the diff before landing.
*Negative:* `rg -n "particle_volume_dense_reference" src/` hits only the test module and `wgsl_validation.rs`; `rg -n "create_compute_pipeline" particle_volume.rs` has exactly one hit (the brick pipeline).
*Demo:* a frame-60 still pair with identical hashes; Peter looks in the app on his own time.
*Test scope:* `-p manifold-renderer --features gpu-proofs` liquid, particle_volume and freeze classify tests; clippy `-p manifold-renderer`.

**P3 — Copy-only array passthrough: parked (D6).** Revive only if the over-noding audit leaves grow, redistance, shrink and the smooths as separate standalone nodes. The seam then changes `Primitive::skip_passthrough(&self, params: &ParamValues, wired_inputs: &[&str])` to take a query that can read a wired scalar's value (the executor resolves it at `execution.rs:2143`, `:1938`; implementors `bokeh_gather.rs`, `motion_blur.rs`, `mux_texture.rs`; trait defs `effect_node.rs:783`, `primitive.rs:708`; forwarder `primitive.rs:1146`), adds a separate borrowed array-alias map, and must satisfy P3-1 to P3-5 plus D6's added invariants, each with a test. Expected saving about 0.2 ms.

## 8. Decided — do not reopen

1. The codegen kernel is the oracle and runs passes 0 and 2; the hand cooperative kernel runs pass 1 only, under D1's explicit exception (Peter approved 2026-10-06).
2. Per-lane exact window iteration, the z filter included; no per-slot predicate over the union (D2).
3. The box hoist is in the kernel (D3).
4. No packing of the box, no atomics; CHUNK is chosen by measurement.
5. Bitwise is a tested gate on the stated device, in three steps; any ULP relaxation goes to Peter (D4).
6. One build; the timing gate is the kill gate and a loss deletes the kernel.
7. No new primitive; no boundary-reason relabel.

## 9. Deferred

- Subgroup ops to let a SIMD group skip rows no lane needs: revive if P1 shows lane divergence above 20% of kernel time.
- i16 box packing: revive if threadgroup memory limits occupancy in P1; needs a conversion proof for NaN/±inf.
- The support box as a typed wire (`node.blob_support`): revive only if Peter rejects D1 and wants the hoist barrier-free; a primitive audit is then owed.
- Interior skip: narrow-band projects only; inert on this preset.
- Whole-brick 512-thread groups: needs the `lattice_bricks` header to change; revive if P1's reduction and scan overhead dominates.
- `node_declared_unchanged` propagation for array aliases: revive when a memoised consumer sits downstream of the chain.
