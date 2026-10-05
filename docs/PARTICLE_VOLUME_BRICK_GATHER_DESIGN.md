# Cooperative brick gather for `node.particle_volume`

**Status:** APPROVED in direction (Peter, D1) · 2026-10-06 · Fable (design), Claude lead · owed: Astra review, then one build session (P0 folded into P1).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase. Anchors are at origin/main 5a1218c89.

`node.particle_volume` builds the water's level set: every lattice node gathers the fluid blobs near it and keeps the minimum distance. On the GPU FLIP Dam Break oracle (res 64, 30 fps project) it costs about 7 ms on every frame, half of a frame without a solver tick. A frame with a tick costs about 35 ms against a 33.3 ms budget.

The cooperative gather is not mainly a bandwidth trick. Today every node recomputes, for every blob it visits, the blob's support box, `floor((c ± support − lmin)/h)` (six divisions, six floors, twelve subtractions), before it knows whether the blob can touch it (`particle_volume_body.wgsl:120-124`). A brick that stages each blob once can compute that box once for 256 nodes. Pure load sharing is predicted to break even at best, and the house already tried a naive tile and lost 15× (`docs/GPU_FLUID_SURFACE_DESIGN.md:1199`: 4³ tile, 31 KB of threadgroup memory, 30.4 ms vs 1.95 ms). This design differs in three ways: streamed chunks of at most 9 KB, per-lane exact range iteration instead of a per-slot predicate, and the box hoist. P0 measures whether the box test is where the 7 ms goes before any of it is built.

On stage: if P1 passes its gate, particle_volume drops from about 7 to at most 4.5 ms on every frame, tick frames land near 32 ms, and the Dam Break holds a steady 30 fps. The surface is identical pixel for pixel (hash-equal); Peter stops feeling the tick.

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

**D1 — The codegen kernel stays the general path and the oracle; the cooperative kernel is an internal pass-1 schedule of the same atom.** The macro declaration does not change (`Pointwise` + `buffer_index` already classifies the atom a boundary, `region.rs:1210`). The atom gains `brick_pipeline: Option<GpuComputePipeline>` built from `shaders/particle_volume_brick_gather.wgsl`, self-contained, with its own `Params` and `buf_*` declarations mirroring the generated kernel and pinned by test I2. Pass 0 (no bricks wired) and pass 2 (exterior) keep the codegen kernel; pass 1 dispatches the cooperative kernel through the same indirect header. By the letter of docs/ADDING_PRIMITIVES.md exclusion 1 (barriered reduction), the cooperative kernel is outside the per-element mandate; the atom's body stays generated and in mandate. **Peter, 2026-10-06:** "Just do the hand written one properly once please, I don't want a huge mess of nodes that aren't fully optimised ... Users won't touch these GPU FLIP nodes." So the hand-written kernel is approved, built once and properly, not as a node chain. It is a third shape, an in-mandate atom with an internal barriered kernel for its main pass. If he agrees, exclusion 1 gains one sentence recording it and its conditions: the codegen kernel stays the general path and bitwise oracle, the internal kernel is proven bitwise on the atom's fixtures, a measured saving justified it, and its dispatch carries its own label. Rejected: relabel as `BarrieredReduction` (misnames a gather, and the boundary codegen entry drops `buffer_index`, so passes 0 and 2 would lose the schedule); teaching the freeze compiler cooperative schedules (compiler infrastructure for one atom); a new primitive (`GPU_FLUID_SURFACE_DESIGN.md:1275` forbids a second level-set atom beside `particle_volume` by name, and the docs/DECOMPOSING_GENERATORS.md section 2.5 (primitive audit) finds nothing to reuse).

**D2 — Exactness by order, not by commutativity.** Each lane visits exactly its own dense window's `(bin, k)` sequence in the dense kernel's order (z, y, x, k ascending), reading staged copies. Signed zero and NaN behaviour of `min` are then moot: the same operands reach the same builtin in the same order. The group's union window is only a loading superset; no lane's predicate is relaxed. Rejected: a per-slot "bin in my window" predicate over the whole union (5–6× more predicate work per lane at scale 1, and order-dependent `min`).

**D3 — The box hoist is part of the experiment.** The loading lane computes `first/last` once per staged blob with the body's expressions (`:120-123`). Without it the cost model predicts break-even. The box lives in threadgroup memory, never on a wire, so `FluidBlob` and its ABI are untouched.

**D4 — Contraction hygiene plus a test, no construction claim.** `pv_blob_term` and `pv_blob_box` move into `shaders/particle_volume_common.wgsl` (a `wgsl_includes` entry) as the only copy, with every `a*b+c` written as explicit `fma()`. The dense body calls the helpers; that refactor is proven bitwise against the inline text in-test (the `:701-759` string-replace pattern). If the GPU bitwise test still finds last-place differences, the MSL cache is inspected for the diverging `fma`; then accepting a stated ULP bound or stopping is Peter's call. "Same picture bit for bit" is a gate, not an assumption.

**D5 — Measure before prototyping, kill before building.** P0 adds no production code and decides whether the box test (hoistable) or the term (not hoistable) owns the 7 ms. P1 is the smallest kernel that can pass the bitwise test and be timed; it is thrown away if it loses.

**D6 — Array passthrough is its own phase (P3)**, about 0.2 ms, with the planner fix it needs; each of Astra's conditions is an invariant with a check (section 6 (Invariants and enforcement)).

## 3. The cooperative kernel

**Group = half brick** (256 threads, 8×8×4 nodes), decoded from `workgroup_id.x` exactly as `liquid_brick_map` does (rank = g/2, half = g%2, local = half·256 + lid), so the indirect header and `lattice_bricks` are untouched. Sentinel lanes and lanes with `idx ≥ params.dispatch_count` are inactive: empty window, never store, execute every barrier. Uniform early return only on `any(bins < 1)`, before any barrier, writing `band` for active lanes.

Per lane, once, with the body's exact expressions (`:88-108`): `p`, `ijk`, `first_bin`, `last_bin`, `phi = band`.

Group union `U = [min first_bin, max last_bin]` by a 256-lane tree reduction in threadgroup memory (no atomics, keeping the shader eligible for the `atomic_free_shaders` conformance row, `liquid/conformance.rs:227`); inactive lanes contribute identities; read back with `workgroupUniformLoad`.

All control flow around barriers derives from uniform or `workgroupUniformLoad` values; naga's uniformity analysis is the check.

```
for z in U.z0..=U.z1:                         // uniform
  rect = U.x-range × U.y-range, w = width
  for run in 0..ceil(w*h/256):                // uniform
    t's bin = run*256 + t (if < w*h); count_t = ranges[bin].count else 0
    tile scan (prefix_scan.wgsl:32-43)        // exclusive prefix[t]; total via workgroupUniformLoad
    lane: for each of its rows y in [y0,y1] ∩ rect: flat range [a,b] = row ∩ run → slots [prefix[a], prefix[b+1])
    for chunk_base in 0..total step CHUNK:    // uniform
      loader t < CHUNK, g = chunk_base + t < total: bin i by binary search on prefix;
        k = ranges[i].start + (g − prefix[i]); stage blob and box = pv_blob_box(blob)
      workgroupBarrier()
      lane: for each row: for g in clip([lo,hi), chunk): phi = min(phi, pv_blob_term(p, ijk, slot[g − chunk_base]))
      workgroupBarrier()
post-gather with the same helpers, same order as the body :140-157; active lanes store buf_levelset[idx]
```

The lane's sequence equals the dense kernel's: the dense loops run z outer, y, x inner, k ascending; the loader lays bins out in flat (y·w + x) order within a z slab and k ascending within a bin; runs and chunks slice that sequence monotonically; the lane only takes its own rows and x range. Rows straddling a run boundary are clipped per run and stay contiguous. Empty bins occupy no slots in either.

**Threadgroup memory (CHUNK = 128):** staged blob 40 B (10 f32; `shape_diag.w` and `shape_off.w` are unused by the term), box 6 × i32 = 24 B (not packed: `vec3<i32>(floor(x))` on NaN/±inf must take the body's conversion), prefix 257 × u32 = 1 KB, reduction scratch reusing the prefix array. About 9 KB: three groups per core at the house's 32 KB figure, against the dropped tile's single 64-thread group. CHUNK = 256 is about 17 KB, two groups, half the barriers. P1 tries 128, 256 and 64.

**Cost model** (per lane per half brick; V = dense visits per node, f = box-pass fraction, c_box ≈ 45 ops including six divisions, c_term ≈ 25 including `sqrt`, I = union inflation ≈ ((8+2r)/(2r+1))²·((4+2r)/(2r+1))):
dense ≈ V·(c_box + f·c_term) + 3V global loads;
cooperative ≈ V·(6 + f·c_term) + 3V threadgroup loads + (I·V/256)·(3 global loads + c_box) + chunks·(2 barriers + rows·2), chunks = I·V/CHUNK.
At I ≈ 5, V ≈ 10⁴, f ≈ 0.1, CHUNK 128: dense ≈ 475k ops, cooperative ≈ 120k. Barriers are the honest cost: about 400–800 per half brick, 0.3–0.6 ms in total, which is why CHUNK is a variant. **If P0 finds f ≈ 1 (every visit pays the term), the hoist saves nothing and the kernel is predicted to lose: stop at P0.**

## 4. Exact equivalence

- **E1 candidate sequence.** For every lane, the `(bin, k)` sequence visited, order included, equals the dense body's. Combinatorial proof above, checked by a CPU test over random lattices, windows, run splits and chunk sizes (I1).
- **E2 per-candidate arithmetic.** One helper pair in `particle_volume_common.wgsl`, explicit `fma()`, called by both kernels. The residual risk is compiler contraction under `MTLMathMode::Fast`, covered only by the GPU bitwise tests (I3, I4) and the D4 escalation.
- **E3 after the gather.** Interior union and solid clamp use the same helpers in the same order; the `interior_len` branch is identical and uniform.
- **E4 every input the node accepts.** `band_extra` 0 and above 0; interior unwired, native padding and solver padding; `resolution_scale` 1–4 (8 at the clamp); a lattice origin far from zero with cells near f32 resolution; rectangular lattices whose edge bricks exceed `dims`; empty bricks (border bricks are always active, `lattice_bricks.wgsl:190`); bins holding more than CHUNK blobs; union rects wider than 256 bins (`reach_bins ≥ 5` at scale 1); blobs with `reach ≤ 0`; NaN/±inf centres; `bounds[0] = 0`; `any(bins < 1)`; `dispatch_count` below the lattice total. Each is a named fixture in section 5 (Oracle and proofs).

## 5. Oracle and proofs

The oracle is the shipped codegen kernel, unchanged text, on the same inputs; before the helper refactor it is reconstructed in-test by string replacement (`:704-713`). The dense reference shader is not an oracle: it omits `band_extra` and interior.

| Test | Fixture | Asserts |
|---|---|---|
| `brick_gather_slot_sequence_equals_dense_window_sequence` (CPU) | random bins, ranges, windows, run and chunk splits | E1 exact sequence equality |
| `particle_volume_shared_helpers_match_inline_body_bitwise` | the three lattices of `:720-722`, scales 1–3, bands {0, 0.6·cell}, `search_boundary_blobs` | refactored dense kernel equals the inline kernel, `to_bits` |
| `particle_volume_brick_gather_matches_codegen_bitwise` | same matrix × interior {none, native, solver} × bricks from `node.lattice_bricks` | cooperative pass 1 equals codegen pass 1 on every active node, `to_bits`; exterior untouched |
| `particle_volume_brick_gather_chunk_tails_bitwise` | one bin with 3·CHUNK+1 blobs; `reach_bins ≥ 5`; an edge brick past `dims`; a brick with no blobs; NaN/inf blobs; `bounds = [0,0]` | bitwise |
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
| I8 Helpers have no implicit contraction sites | `rg -n '\*[^;]*\+' shaders/particle_volume_common.wgsl` → zero hits outside `fma(` |
| P3-1 Array alias lifetime extends transitively through a chain of skipping nodes | `chained_skip_passthrough_extends_root_lifetime_transitively` |
| P3-2 Array alias requires equal byte capacity and `ArrayType` | `array_passthrough_refuses_capacity_or_layout_mismatch` |
| P3-3 Array aliases clear at frame start | `array_passthrough_clears_next_frame` |
| P3-4 Live extent of an aliased array resolves through the alias | `array_passthrough_propagates_live_extent` |
| P3-5 Wired control values decide the skip | `array_passthrough_reads_wired_control` |

## 7. Phasing

**P0 — Regime and statistics (no production code; one session).**
*Entry:* section 1 anchors re-verified on current main; the oracle project confirmed with `project_tool` (res 64, 30 fps, the bundled Dam Break); machine quiet.
*Read-back:* sections 3 and 4 here, `particle_volume_body.wgsl` whole, `GPU_FLUID_SURFACE_DESIGN.md:1186-1205`.
*Deliverables:* (a) dump the blobs, cell ranges, bricks and bounds arrays at ticks 30/60/90/120 (`examples/fluid_capture.rs`; verify those arrays are dumped) and a scratchpad script reproducing `:95-124` in f64 that reports V (visits per active node, p50/p95), f (box-pass fraction), active half bricks A, union inflation I, and max blobs per bin; (b) three throwaway timing kernels on a scratch branch, never landed: V-noterm (term replaced by a one-op accumulate), V-nobox (box test removed), V-aliased (`buf_blobs[k & 255u]`), each measured as a particle_volume node-stamp ratio to the baseline, three runs each through `scripts/gpu_queue.py`.
*Gate:* a table of V, f, A, I and the three ratios. **Continue to P1 only if** (baseline − V-noterm) ≥ 50% of baseline, meaning the box test plus loads own at least half the time. Otherwise stop, record the result in `GPU_FLUID_SURFACE_DESIGN.md`'s lever table beside the dropped tile, and name the interior skip and subgroup work as what is left.
*Forbidden:* landing any timing kernel; taking V from the audit's estimate instead of the dump.
*Test scope:* none.

**P1 — Prototype and kill gate (one session).**
*Entry:* P0 passed; Peter answered D1.
*Read-back:* section 3, `liquid_bricks_common.wgsl`, `prefix_scan.wgsl:32-43`, `bokeh_gather.rs:244-290`, `particle_volume.rs:701-759`.
*Deliverables:* `shaders/particle_volume_common.wgsl` (helpers, explicit `fma`), the body refactored onto them, `shaders/particle_volume_brick_gather.wgsl` (CHUNK a `const`, variants 64/128/256 by text substitution at pipeline creation in the prototype only), the `brick_pipeline` field and pass-1 dispatch labelled `node.particle_volume.bricks`, tests I1, I2, I3 (`_matches_codegen_bitwise` on the three-lattice matrix at minimum) and I7.
*Gate (measure):* bundled release binaries (main, branch), interleaved A,B ×3 through `gpu_queue.py` on the oracle project: tick interval p50/p95, plus one node-stamped run per binary for the particle_volume ratio and one `--frame-clock --stamp-every 3` run per binary to isolate no-tick frames. **Continue iff** the particle_volume ratio ≤ 0.70 on every pair (about 2 ms of 7), no-tick frame GPU p50 down at least 1.5 ms, and tick interval p95 no worse than +0.5 ms. Try CHUNK 256, then 64, before concluding; if all lose, stop and record the result.
*Forbidden:* loosening any lane's window; packing the box; atomics; "approximately bitwise".
*Test scope:* `cargo test -p manifold-renderer --features gpu-proofs particle_volume`; clippy `-p manifold-renderer`.

**P2 — Harden and land (one session).**
*Entry:* P1 passed with a recorded CHUNK.
*Deliverables:* the remaining fixtures (`_chunk_tails_bitwise`, `_dam_break_frames_bitwise`, `gpu_flip_narrow_band_mesher_values_with_bricks`), the I6 conformance row, the I8 gate, `brick_pipeline` prewarmed at install beside `pipeline` (COMPILE_CONTRACT), the ADDING_PRIMITIVES exclusion-1 sentence (D1), the NODE_CATALOG line, and the lever-table status in `GPU_FLUID_SURFACE_DESIGN.md`.
*Gate:* every section 5 test green; `gpu_flip_frame_perf` hashes equal to the pre-change binary's; `graph-tool fusion` region count unchanged; `scripts/landing_gate.py`.
*Negative:* `rg -n "particle_volume_dense_reference" src/` hits only the test module and `wgsl_validation.rs`; `rg -n "create_compute_pipeline" particle_volume.rs` has exactly one hit (the brick pipeline).
*Demo:* a frame-60 still pair with identical hashes; Peter looks in the app on his own time.
*Test scope:* `-p manifold-renderer --features gpu-proofs` liquid, particle_volume and freeze classify tests; clippy `-p manifold-renderer`.

**P3 — Copy-only array passthrough (one session, independent of P1 and P2).**
*Entry:* `graph-tool fusion WaterDamBreakGpuFlip.json` lists which of grow, redistance, shrink, smooth_x, smooth_y, smooth_z run standalone; if fewer than three do, record that and close the phase (the saving is below noise).
*Seam brief (old → new):* `Primitive::skip_passthrough(&self, params: &ParamValues, wired_inputs: &[&str])` → `skip_passthrough(&self, q: &SkipQuery<'_>)` with `SkipQuery { params, wired_inputs, scalar: &dyn Fn(&str) -> Option<f32> }`; the executor resolves a wired scalar through its backend (`execution.rs:2143`, `:1938`). Call-site inventory (re-run `rg -n "fn skip_passthrough\(" crates/manifold-renderer/src`; stop if the count differs): trait defs `effect_node.rs:783`, `primitive.rs:708`; forwarder `primitive.rs:1146`; implementors `bokeh_gather.rs`, `motion_blur.rs`, `mux_texture.rs`; executor calls `execution.rs:1938, 2143`; tests `execution.rs:4738, 6043`. Rename first, compiler-driven.
*Deliverables:* `Backend::alias_array(src, dst) -> bool` mirroring `alias_2d` over `buffers_array`, tracked in `skip_aliased_slots` and cleared by `clear_skip_aliases`; an executor branch for `Array` ports requiring equal byte capacity and equal planned `ArrayType`; live-extent resolution through the alias; the planner's lifetime extension made transitive (`execution_plan.rs:910`); `skip_passthrough` on `offset_lattice` (`offset == 0.0`), `redistance_lattice` (`enabled == 0.0`) and `smooth_lattice` (`passes.round() == 0`), declining when a wired control's value is unavailable. Not in v1: `node_declared_unchanged` propagation for arrays.
*Gate:* P3-1 to P3-5 green; `fluid_bricks_lattice_and_mesh_bit_identical_dense_64` and `gpu_flip_frame_perf` hashes unchanged; A/B as in P1 with the mesh-chain stamps as ratios (expected about 0.2 ms in total).
*Negative:* `rg -n "fn skip_passthrough\(&self, _params" src/` → zero.
*Round-trip:* save and reload the oracle project; modulate Fill Pits 0 → 0.1 → 0 live: the chain must evaluate at 0.1 and alias again at 0.
*Performer gesture:* dragging Smoothing Passes 0 → 2 → 0 on stage.
*Test scope:* `-p manifold-renderer` execution, execution_plan and liquid tests; `--features gpu-proofs` liquid bricks; clippy `-p manifold-renderer`.

## 8. Decided — do not reopen

1. The codegen kernel is the oracle and the general path; the cooperative kernel runs pass 1 only (D1, pending Peter's yes on the exclusion sentence).
2. Per-lane exact window iteration; no per-slot predicate over the union (D2).
3. The box hoist is in the prototype (D3).
4. No packing of the box, no atomics; CHUNK is chosen by P1 measurement.
5. Bitwise is proven by test; contraction risk escalates to Peter (D4).
6. P0 before P1; P1 is thrown away on a lost gate.
7. No new primitive; no boundary-reason relabel.

## 9. Deferred

- Subgroup ops to let a SIMD group skip rows no lane needs: revive if P1 shows lane divergence above 20% of kernel time.
- i16 box packing: revive if threadgroup memory limits occupancy in P1; needs a conversion proof for NaN/±inf.
- The support box as a typed wire (`node.blob_support`): revive only if Peter rejects D1 and wants the hoist barrier-free; a primitive audit is then owed.
- Interior skip: narrow-band projects only; inert on this preset.
- Whole-brick 512-thread groups: needs the `lattice_bricks` header to change; revive if P1's reduction and scan overhead dominates.
- `node_declared_unchanged` propagation for array aliases: revive when a memoised consumer sits downstream of the chain.
