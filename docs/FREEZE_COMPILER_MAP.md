# Freeze Compiler & Graph Runtime — Current-State Map

<!-- index: AUTHORITATIVE current-state map of the freeze/fusion compiler + the graph runtime it leans on (2026-07-03, from a full code read). Pipeline, every cut rule, the marker ABI, the tiered precision contract, caches/kill-switches, executor invariants, test surface, and the honest open edges. Read before any fusion/freeze work; supersedes the status sections of the older fusion design docs. -->

**Status: AUTHORITATIVE map of what is actually in the code, written 2026-07-03
from a full read of the freeze module and the runtime around it.** This is the
doc to read before touching fusion, and the doc a bug hunt attacks. It
supersedes the *status* sections of the older design docs (which record how we
got here, not where we are):

- `GRAPH_FREEZE_COMPILER_DESIGN.md` — the original design + adversarial review.
  Still correct on philosophy (§0, §7, §8); its §12 status ends at "ColorGrade
  only" and its perf-gate plan (§12.3 step 6) describes a mechanism that was
  later **deleted** (see §6 below).
- `CHAIN_FUSION_DESIGN.md` — segment design. Correct except every mention of a
  "per-region gate on every cross-card region": the fuse decision is structural
  now, there is no measurement.
- `archive/BUFFER_CHAIN_FUSION_DESIGN.md` — written before derived-uniform fusion
  shipped. Its blocker table is resolved; FluidSim buffer regions fuse today
  (`fluidsim_buffer_fusion_renders_like_unfused`).
- `GRAPH_COMPILER.md` — a walked-back 2026-05 brainstorm (`for_each_n`).
  Historical only; nothing in it is the shipped compiler.

Fixed 2026-07-14 (fusion-sweep phase 8): `freeze/classify.rs`'s `Gather` /
`BufferGather` / `Source` doc comments no longer claim a gather input or a
buffer atom forces a node to Boundary, or that Source generators can't head a
region — all three fuse today (tier 3 buffer-fusion + generator-as-producer
shipped; only the WIRE feeding a gather/buffer-gather stays external, not the
node). `freeze/mod.rs`'s header now describes the actual multi-stage compiler
(classify/region/codegen/install/segment/space) instead of framing the module
as an oracle-first v1 prototype.

---

## 1. What it is, in one paragraph

Every effect/generator is a JSON graph of small GPU atoms. Run naively, each
atom is one full-canvas dispatch and each wire a VRAM round-trip — ~5× slower
than the old hand-fused Rust effects. The freeze compiler rewrites a graph's
*definition* before it is built: it finds maximal runs of per-element atoms
(regions), generates one WGSL kernel per region (read once → math in registers
→ write once), swaps the run's nodes for one `node.wgsl_compute` node carrying
that kernel, and repoints all live bindings onto it. The unfused graph is never
deleted — it is the editing surface, the fallback, and the test oracle. The
fuse/don't-fuse decision is **structural** (region shape), never measured.

**For the instrument:** this is the machinery that decides what pixels reach
the screen every frame of a show. Its failure mode is not a crash — it is a
*silently different picture* than the editor showed. Every rule below exists to
make that impossible; the invariant list (§9) is what a review must attack.

## 2. File map

| File | Role | Size |
|---|---|---|
| `freeze/classify.rs` | `FusionKind` (Boundary/Pointwise/MultiInputCoincident/Source) + `InputAccess` (Coincident/CoincidentTexel/Gather/GatherTexel/BufferGather). Declared per atom via `primitive!`; default Boundary. | 210 |
| `freeze/space.rs` | Element-space resolution (Canvas / Scaled(n,d) / Concrete(w,h)) — builds the UNFUSED graph + plan and reads `resource_dims`/`resource_canvas_scales`, so the executor's dim policy is the single authority. | 96 |
| `freeze/region.rs` | The finder: classify → union-find growth over coincident same-space wires with convexity check → `build_region` (externals, outputs, spaces, q16 flags) → stencil virtual-chain absorption. Pure, no GPU. | 2838 |
| `freeze/codegen/` (was codegen.rs — Wave 3 P3-C split, 2026-07-22) | WGSL emission. Standalone single-atom kernels (texture / buffer / resolve paths — the single-source `run()` for ~150 converted atoms) + fused multi-atom kernels (texture + buffer). Deterministic text = pipeline-cache key. Modules: mod (facade) 37 · types 486 · uniforms 92 · entry_points 139 · standalone 933 · fused 1488 · dispatch_contract_tests 194 · gpu_tests 3114 | 6483 total |
| `freeze/install.rs` | The rewrite: def surgery (delete members, insert fused `node.wgsl_compute` nodes, rewire), binding retarget, control-wire re-anchor, derived-uniform wiring, in-place-loop detection, build check, all caches, the chain-fusion worker, `should_render_fused`. | 2333 |
| `freeze/segment.rs` | Cross-card concat: N adjacent cards → one namespaced def (`c{i}.` prefixes), seam boundaries stitched out. + `def_is_segment_stateless` eligibility. | 296 |
| `freeze/diff.rs` | GPU texture-diff reducer (max-abs + over-count verdicts) — the oracle's measuring device. | 305 |
| `freeze/proof.rs` | The oracle suite (test-only): ~40 render-two-ways proofs, per-feature. See §10. | 3864 |
| `freeze/reference.rs` | Frozen golden hand-kernels for codegen-drift checks. | 101 |
| `primitives/wgsl_compute.rs` | The host primitive every fused kernel becomes. Ports/params/bindings derived from the WGSL by naga introspection; parses all freeze markers; owns static-param specialization. | 3440 |
| `node_graph/execution_plan.rs` | `compile(graph)`: topo + liveness filter + resource dims/canvas-scale propagation + lifetimes (`free_after`) + persistent resources + late-capture steps + hoistable classification. | 1411 |
| `node_graph/execution.rs` | The executor: per-frame liveness (mux short-circuit), memoized-dataflow skip (`is_pure`), empty-output skip, preview capture, dump/thumbnail pinning, the aliased-output stale guard, end-of-frame feedback texture swap. | 2923 |
| `node_graph/graph_loader.rs` | `instantiate_def`: flatten groups → construct + configure primitives → wires; array output pre-allocation (`array_output_capacity`). | 1838 |
| `preset_runtime/` (was preset_runtime.rs — Wave 3 P3-R split, 2026-07-22; core.rs holds the chain build) | Effect-chain build: segmentation pass → per-card `fused_view_for` → splice. The live entry point for effect fusion. | — |
| `generators/registry.rs` | Generator entry point: `should_render_fused` → `fused_generator_def_for` → `from_def`. | — |
| `chain_dispatch.rs` | Calls `pump_segment_results()` each dispatch (drains the chain-fusion worker). | — |

## 3. The pipeline, end to end

```
EffectGraphDef (canonical | edited | segment-concat)
  → flatten_groups                                   (install + loader both)
  → partition_regions(def, registry)                 (region.rs — pure)
       classify each node → union-find over coincident same-space wires
       (convexity-checked) → build_region → absorb stencil virtual chains
  → fuse_canonical_def_masked                        (install.rs)
       per region: generate_fused (codegen) → naga parse check
       → fused node.wgsl_compute node (params seeded, markers attached)
       → def rewrite (externals→src_e, outputs→dst/dst_k/src_k, control wires)
       → binding retarget (strand ⇒ refuse whole fuse)
  → fused_def_builds                                 (build + element-space verify)
  → content-keyed cache (fused_view_for / fused_generator_def_for / segments)
  → chain splice / generator from_def → instantiate_def → Graph
  → compile() → ExecutionPlan → Executor (per frame)
```

Everything above the cache line is CPU codegen, no GPU device. The GPU pipeline
compile happens downstream in the normal chain/generator build. Segment codegen
runs on a dedicated `chain-fusion-worker` thread; everything else is
content-thread (thread-local caches, no locks).

## 4. The cut rules — when fusion says no

This is the correctness core. All of them fail **closed**: any refusal renders
unfused, which is always correct. Order matters; from `classify_node`:

**Per-node (→ Boundary):**
1. `system.source` / `system.final_output` (graph endpoints).
2. Unknown type, `FusionKind::Boundary`, or no `wgsl_body`.
3. `fusion_register_heavy()` (bespoke inlined simplex — occupancy cliff;
   FluidSim's burst measured fused-slower).
4. Any param that can't lay out as a fusable uniform field (Table/String —
   Vec3/Vec4/Color pass via `param_is_fusable`, P5/D4, 2026-07-14).
   (Binding-targeted *Enum* params are allowed — the retarget rewrites
   `EnumRound`→`IntRound`, identical at the u32 write boundary.)
5. Buffer atoms: see the buffer gate list below.
6. Texture arity: `tex_out == 0` boundary; else Source needs 0-in, everything
   else ≥1-in (P6/D4, 2026-07-14 — was "≠1 texture output", so a MULTI-output
   atom, e.g. voronoi_2d/block_displace_field, was always a boundary; now
   admitted as a region member via the struct-return `BodyOutputs` wrapper
   extended to `generate_fused`, `InputSource::NodeOutput` picking the field
   a wire threads).
7. Resample: any `output_canvas_scale ≠ 1:1` (downsample) — the fused node
   would iterate the wrong grid.
8. Any Texture3D port (texture finder is 2D; 3D fuses only inside buffer
   regions as sampled volumes).
9. A wire into any input that is neither a texture port nor a scalar param
   (e.g. an Array port on a texture atom) — **exemption (P0/D7, 2026-07-12):**
   a wire into a `Camera`-typed CPU-struct port does NOT cut, when the member
   has a non-empty `derived_uniforms()` (i.e. consumes the struct entirely via
   recomputed uniform fields, never as a GPU binding — true by construction,
   since Camera has no WGSL representation). Applies on both the texture
   (`classify_node`) and buffer (`classify_buffer_node`) paths — one shared
   predicate. Any other non-texture non-param wire still cuts.
10. Control PRODUCER (its scalar output drives someone's param) — must survive
    the rewrite so its wire can re-anchor onto the fused port-shadow.
11. In-loop f16 with q16 disabled (`MANIFOLD_FREEZE_Q16=0`).
12. In-loop f16 on a **particle** loop (`cycle_contains_array`) — q16 fixes
    store rounding but not cross-kernel body ULP; a scatter amplifies one ulp
    of force into a visibly different field. Pure-texture loops fuse (tier A).
13. Specialization-token param (blurVW's `quality`) that is binding-targeted or
    control-wired — baked text could diverge from the live value.
14. Final gate: the atom's standalone kernel (with spec tokens substituted)
    must parse through plain naga. No hard-coded atom lists anywhere.

**Buffer-atom gates (`classify_buffer_node`):** ≥1 Array in, exactly 1 Array
out; no texture output; texture *inputs* must be wired sampled 2D/3D (unwired
optional = boundary — the fused node's port would be required and silently kill
the dispatch); no `BufferGather` (neighbor_smooth); no atomic outputs
(scatter); same wire/control-producer rules. **Derived uniforms, ANY declared
name (P0/D7, 2026-07-12, superseding the old name-whitelist rule below):** a
member with `derived_uniforms()` fuses (texture or buffer path) iff its
type_id has a registered recompute in `freeze::derived_uniform_registry`
(`has_recompute`, checked at install time) — data-driven, not name-matched.
vec3-typed derived uniforms (e.g. a camera basis vector) are now sourceable,
via the member's wired `Camera` input routed to the fused node as a
`@camera_external` port (see §5) and recomputed every frame in
`wgsl_compute::evaluate()`. The time-family (`dt_scaled`, `frame_count`,
`time`/`time2`/`time_val`) migrated onto this same mechanism — the old
per-name control wire from `system.generator_input` is gone; every primitive
that declares `derived_uniforms` now also registers a recompute fn next to
its `primitive!` (see the precedent primitives in
`node_graph/primitives/euler_step_particles.rs` and siblings, plus the
camera-consuming `flatten_to_camera_plane.rs`). A member whose type_id has no
registered recompute still fails the region closed, same fail-safe contract
the old whitelist had.

**Union gates (`partition_regions`):** an edge unions two eligible nodes only
if: same domain (texture wire, but never *into* a buffer atom — that's a
gather); coincident-consumed (a gather-consumed wire NEVER unions — the
gathered producer stays an external the body samples); same element space
(texture only); and the merge keeps the *collapsed* forward graph acyclic
(convexity — Watercolor's out-through-a-blur-and-back shape). State-capture
wires are excluded from the forward graph, matching the planner, or legal
feedback loops would read as cycles.

**Region gates (`build_region`):** members topo-sort (cycle ⇒ refuse);
required/gather/buffer inputs must be wired (optional coincident unwired is OK
— fuses as a folded `0u` use flag); gather-from-member ⇒ refuse (defensive);
every escaping wire's consumer must be `final_output`-reachable (a dead
consumer's unbound `dst_k` would early-return the WHOLE fused dispatch and kill
the live outputs too); buffer regions single-output; member element spaces
uniform; a space-mismatched Coincident external is admitted but marked
*sampled* (read via `textureSampleLevel` at uv — the resolution-robust
standalone read); a space-mismatched CoincidentTexel external ⇒ refuse (a
rescaled sample corrupts texel-exact patterns like dither). `MIN_REGION_LEN=2`
unless a virtual chain was absorbed.

**Install refusals (whole card falls back to unfused):** gather members
disagreeing on sampler address mode; a control producer that was itself fused
away; an unsourceable derived uniform; a stranded binding (static, def, or
would-be); fused kernel fails naga; `fused_def_builds` fails — the fused def
must build a real plan AND resolve every region output to the **same element
space** the replaced member had unfused (the ParticleText divergence class,
verified per output).

**Stencil absorption (`absorb_virtual_chains`):** a gather (blur) member may
absorb a producer chain into its fetch — recomputed at each tap's 4 bilinear
corners instead of a canvas round-trip. Gated: chain ≤ 1 node
(`MAX_VIRTUAL_CHAIN` — taps×4 recompute cost), single escape into that one
consumer, all-coincident/gather texture members, same space, same sampler mode,
off any cycle unless the consumer's taps are texel-exact AND the cycle has no
Array stage, nested stencils refused, convexity re-checked per absorption.

## 5. The marker ABI

Codegen (install) and runtime (`node.wgsl_compute`) communicate through WGSL
**comment markers**. This is a stringly-typed contract: producers in
`freeze/codegen.rs` + `freeze/install.rs`, consumers in
`primitives/wgsl_compute.rs` (`introspect` + helpers ~line 1400). Any review
should diff both ends.

| Marker | Emitted by | Means |
|---|---|---|
| `// @fused_output` on a `var<storage, read_write>` array | fused buffer codegen (fresh-dst model) | Output-ONLY port: no input port, no aliased pair — keeps read-only inputs forward deps (the fix for the buffer ordering bug where an aliased output read as a feedback back-edge). |
| `// @dispatch_count_param: n{i}_<p>` | fused buffer codegen (in-place loop regions where all members agree on one `active_count` producer) | Cap the 1D grid at the named uniform's live value instead of buffer capacity (the FluidSim "fused slower" fix — kernel carries the matching guard). |
| `// @sampler_address_mode: repeat\|mirror` on `samp` | fused texture codegen | Create the shared gather sampler at this mode (WGSL can't express address modes). `clamp` emits no marker — byte-identical legacy text. |
| `// @reset_gated` | seed-pattern kernels | Node grows a synthetic optional `reset_trigger` input; dispatches only on integer edges. On skip of an *aliased* kernel it calls `mark_gpu_accessed()` — the executor stale-guard's documented escape hatch. |
| `// @static_param: <field>` (one per line, prefix block) | install, texture regions only (never buffer — detected by `var<storage`) | Field is *eligible* for const-baking (specialization). Excluded: control-wired fields AND outer-card binding targets. Correctness does NOT rest on this classification — see §7. |
| `// @pure` | hand-authored kernels (BlackHole bake) | Author asserts output = f(params, inputs) → memoizer may hold it. |
| `// @fusion: pointwise\|source` (+ `fn body`) | user-authored fragment-form `wgsl_compute` | The node synthesizes its standalone kernel via `generate_standalone` and reports a real `fusion_kind`, so user fragments fuse like built-ins. |
| `// @camera_external: camera_ext_N` (P0/D7, 2026-07-12) | fused texture/buffer codegen, when a region member has a wired `Camera` input | Declares a synthetic, non-introspected `Camera`-typed input port on the fused node — the only channel a CPU-struct with no WGSL representation can travel through the def. Producer wire (Camera→Camera) reuses the ordinary `control_wires` rewrite. |
| `// @derived_uniform_member: <first_field> words=<n> <type_id> [<camera_port>]` (P0/D7, 2026-07-12) | fused texture/buffer codegen, one per region member with non-empty `derived_uniforms()` | The contiguous uniform-buffer block that member's derived fields occupy, and how to refresh it every frame: `wgsl_compute::evaluate()` calls `derived_uniform_registry::recompute(type_id, ctx)` (ctx = frame clock + the routed `@camera_external` value, if named) and packs the result through each field's real `UniformMemberType`. Consulted by `introspect()` to exclude these fields from the generic port-shadow/param set. Kernels carrying this marker are excluded from static-param specialization (§7) — baking would shift the surviving fields' byte offsets. |

Sampler default everywhere is ClampToEdge; markers only encode deviations, so
all-clamp regions keep byte-identical WGSL — and the WGSL text is the
cross-session pipeline-cache key, which makes codegen **determinism
load-bearing** (fields in PARAMS order, stable topo, no map iteration anywhere
in emission).

## 6. The fuse decision is structural (perf gate is GONE)

`should_render_fused(is_watched) = freeze_enabled() && !is_watched`. That's
the whole gate. *Which* atoms fuse = `partition_regions` (MIN_REGION_LEN,
boundaries, register-heavy flags). The 2026-06 measured perf gate
(`perf_gate.rs` tune-all sweep + `verdict_cache.rs` disk caches) was deleted
2026-06-15: its ahash disk keys were process-seeded so every launch re-swept,
and the structural rule reproduced ~17/17 measured verdicts. **Do not
re-propose measurement** (settled — `feedback` memory
`fusion-decision-is-structural`); if a preset regresses, tune the structural
rule (region size / register-heavy / stencil caps).

The `region_mask` parameter on `fuse_canonical_def_masked` survives (tests +
future per-region control) but live callers pass `None`.

## 7. Precision contract (editor == stage)

The hard constraint: closing the editor must not change the look. The actual
contract is tiered — this is written down nowhere else:

1. **Standalone codegen cutover** (hand shader → generated kernel):
   bit-identical, oracle-gated per atom at conversion time (1e-6, most
   bit-exact). Gotchas that recur: match the hand shader's exact arithmetic
   form (`a*(1-t)+b*t`, not `mix()` — fma ULP), GPU-vs-CPU trig, workgroup-size
   divisor changes.
2. **Fused in-loop (feedback) texture regions: bit-exact by construction.**
   f16-stored members get `q16()` (pack2x16float round-trip = rgba16float
   store RTNE) after every body call; fp32-marked members are exact as-is.
   Induction: identical inputs + identical store rounding ⇒ identical forever.
   Proof: `oilyfluid_inloop_f16_fusion_matches_unfused`, watercolor in-loop.
3. **Fused buffer regions: bit-exact** (f32 element registers, same math).
   Proofs: digitalplants / fluidsim / fluidsim3d `*_renders_like_unfused`.
4. **Out-of-loop texture regions: ≈1 ulp, NOT bit-exact, and cannot be.**
   Body-level FMA/inlining differs across kernel contexts; the 2026-06-10
   probe showed q16-everywhere makes it WORSE. Tolerance precedent: the
   quarter-res oracle's 1e-2 / bounded over-count. Known shipping instance:
   MetallicGlass's noise-chain ~1 ulp, amplified by PBR specular to
   max_abs≈1.8 on ~2.6% px — visually a tiny specular shimmer, accepted.
5. **Stencil virtual chains:** corner values bit-identical (exact textureLoads
   + q16 on the chain tail); the residual gap is manual f32 lerp vs the
   hardware filter unit. Fractional-tap absorption inside loops is refused for
   exactly this reason.
6. **Particle loops:** excluded from f16 in-loop fusion entirely (cut rule 12).
   fp32 is an explicit `outputFormats` opt-in on data textures, never a
   compiler default.

**Static-param specialization** (roadmap 4) never changes values: at dispatch,
`wgsl_compute` bakes the `@static_param` fields' LIVE values into a const
variant keyed by that value-set (LRU 4, compiled only after the key holds
`SPEC_STABLE_FRAMES`); any mismatch between live values and a variant's key
serves the generic all-uniform kernel. A per-frame-driven "static" field just
never stabilizes → generic path → correct. Kill: `MANIFOLD_WGSL_SPECIALIZE=0`.
A kernel carrying a `@derived_uniform_member` marker (P0/D7, 2026-07-12) never
specializes at all — baking a const variant would shift the surviving fields'
byte offsets against what `evaluate()`'s recompute pack expects.

## 8. Caches, keys, threads, kill switches

- **Content key** = ahash of `serde_json::to_vec(def)` (BTreeMaps ⇒ stable).
  Per-process seed — fine, all caches are in-memory. Exposed params live in
  `param_values`, NOT the def ⇒ instances differing only in live modulation
  share one kernel, and exposed params are never baked.
- **Caches** (all thread-local to the content thread, all `Box::leak`'d
  `&'static`, all capped at `FUSED_CACHE_CAP=512`, all negative-caching):
  `FUSED_EFFECT_CACHE` (key → `Option<&LoadedPresetView>`),
  `FUSED_GENERATOR_CACHE` (key → `Option<&EffectGraphDef>`),
  `SEGMENT_CACHE` (positional hash of member content keys →
  `Option<&SegmentView>`). Past the cap: recompute-on-miss, never evict
  (values are leaked statics).
- **Segments** compile on the `chain-fusion-worker` thread; lookups return
  Ready/Pending/Refused; `pump_segment_results()` (chain dispatch entry)
  drains results and bumps `SEGMENT_GENERATION` so pending runtimes rebuild.
  Tests never feed the worker (seed via `seed_segment_cache_for_test`).
  Segment eligibility additionally requires stateless cards
  (`def_is_segment_stateless` — positional `c{i}.` prefixes would key feedback
  state by chain position) and at least one region actually spanning a seam.
- **Per-instance user bindings** live off-def; the fused view carries the full
  `fused_retarget` map so the chain builder repoints them at splice time.
- **Kill switches** (env, read once): `MANIFOLD_FREEZE` (master, default on),
  `MANIFOLD_CHAIN_FUSION`, `MANIFOLD_WGSL_SPECIALIZE`,
  `MANIFOLD_FEEDBACK_PINGPONG`, `MANIFOLD_FREEZE_Q16` (per-fuse-build read).
  All default ON. Restart-scoped — the live hot-toggle never shipped.
- **Watched target** (graph open in the editor) always renders unfused — the
  editor's per-node preview samples inner textures. The watched signal is in
  the rebuild keys (effects: `preview_effect` in `compute_topology_hash`;
  generators: per-layer `built_watched`), so open/close re-fuses via rebuild.

## 9. Executor contracts fusion leans on

The worst historical fusion bugs were *interactions* with these. Each is an
invariant a fused def must respect:

1. **Liveness roots** — `final_output` + `aliased_array_io` + state-capture
   primitives (`is_liveness_root`). Plan-level: dead nodes aren't compiled;
   frame-level: `compute_live_steps` prunes mux branches. The finder's
   `final_reachable_nodes` is a deliberate SUBSET (final_output only) — safe:
   under-fusing, never miscompiling.
2. **Unbound-output early-return** — a `WgslCompute` with any unbound storage
   output skips its whole dispatch. Why every fused output must be wired to a
   live consumer (cut rule).
3. **Stale-output guard** — debug_assert: an aliased-array node may not skip
   its dispatch without `mark_gpu_accessed()` (the `@reset_gated` skip uses
   exactly this escape hatch, contract: consumer reads only on reset).
4. **State-capture back edges** — `state_capture_input_ports` wires are
   excluded from cycles by planner AND finder (must stay in lockstep);
   persistent resources are pre-acquired each frame (consumer at step 0 reads
   last frame's write) and cleared to black on first-ever acquire.
5. **Feedback ping-pong** — `node.feedback`'s `out` is a persistent slot
   (`persistent_output_ports` → plan); same-format loops SWAP texture handles
   at `late_capture` (`Backend::swap_texture_2d`, end of frame); cross-format
   loops bridge one dispatch. The editor thumbnail path pins the pre-swap
   texture identity (post-frame re-resolve strobed black on alternate frames).
6. **Element-space authority** — `space.rs` never re-derives the dim rules; it
   builds the unfused plan and reads `resource_dims`/`resource_canvas_scales`.
   The plan's propagation (concrete / canvas-scaled / canvas, mixed-input ⇒
   canvas fallback) is the single source of truth; `fused_def_builds`
   round-trips the fused def through it and rejects drift.
7. **In-place buffer loops** — an `array_feedback` loop buffer is written IN
   PLACE by the fused kernel (`in_place_alias`: consumers rewired off `src_k`),
   because a fresh `dst` would demote the loop to a one-frame-delayed copy.
   Only taken when the output provably threads through `aliased_array_io`
   members back to a verified loop external; forward-produced regions
   (DigitalPlants) keep fresh-dst.
8. **arrayLength buffer-size index** — SPIRV-Cross's buffer-size buffer must be
   pinned to the slot actually bound (`manifold-gpu`), or every
   `arrayLength()` guard silently returns 0 and kills all threads. Historical
   root cause of the "buffer fusion 12% residual"; lives in the backend now.
9. **Memoizer** — pure steps (`is_pure` / `@pure`) skip when param + input
   epochs are unchanged; held slots serve consumers. Fused nodes are not
   `@pure` (they read uniforms every frame) — no interaction today, but a
   review should confirm specialization variants don't confuse epoch tracking.
10. **Derived-uniform recompute (P0/D7, 2026-07-12)** — a fused kernel has no
    per-member `run()`, so any member's `derived_uniforms()` fields (frame-
    derived, or recomputed from a routed `Camera` external) must be refreshed
    every frame from the SAME ambient context the unfused `run()` would have
    read (`freeze::derived_uniform_registry`, consumed in
    `wgsl_compute::evaluate()`; see §5's two new markers). A type_id with no
    registered recompute fails the fuse closed at install time
    (`has_recompute`) — the same fail-safe contract every other cut rule
    follows: refusal always renders unfused, unfused is always correct.

## 10. Test surface & how to debug

- **The oracle pattern** (proof.rs): render the REAL preset fused and unfused
  through the real executor, diff on GPU (`TextureDiff` — max_abs + over-count
  + NaN/Inf classification), assert per contract tier (§7). ~40 proofs cover:
  per-feature fixtures (gather folding, fan-out, source-headed, cross-res
  sampled externals, stencil integer/fractional taps, optional-input folding,
  q16 loops, ping-pong vs copy, seed gating, buffer regions ×3, generators,
  segments) + library-wide sweeps (`every_fused_preset_executes_one_frame`,
  `every_fused_generator_kernel_compiles`, `fusion_coverage_baseline` — a
  loose floor so coverage can't silently collapse).
- **First tool for "why didn't X fuse":** `explain_presets` (#[ignore]d, in
  `region.rs`'s audit module) — prints per-node class + per-wire union verdicts
  + build_region pass/fail per preset. Also `audit_all_presets` for the
  library-wide region census.
- **Dump a fused kernel:** the fused def's nodes carry `wgsl_source` inline —
  print it from any test, or read the def out of `fused_view_for`.
- **Known flake:** heavy GPU proof tests under full-suite parallelism
  occasionally diverge (up to 6 in one run, all pass isolated/serial). Re-run
  isolated before believing a failure. Root cause not found — this is an open
  hazard, not an accepted fact of life.
- **Known pre-existing failures** (not fusion's): DepthOfField prewarm,
  and the Liveschool FluidSimulation Ableton param-id fixture.
- Scope: freeze suite = `cargo test -p manifold-renderer --lib
  node_graph::freeze`. After ANY codegen/macro change run the full
  `-p manifold-renderer --lib` — focused runs miss cross-atom staleness.

## 11. Honest edges (the bug hunt starts here)

**Update 2026-07-03: the hunt ran** (40-agent adversarial workflow; 10 lenses, 2 skeptics
per finding). Outcome: 7 confirmed + 2 split-verdict findings, all documented as
**BUG-006 … BUG-014 in [BUG_BACKLOG.md](BUG_BACKLOG.md)** — including a likely mechanism
for edge #2 below (unchecked Metal command-buffer status, BUG-013). The completeness
critic's round-2 lens list (what got shallow coverage): the executor itself
(`execution.rs`/`execution_plan.rs`, esp. the §9.9 specialization-vs-memoizer question),
`classify.rs`'s gates independent of its stale comments, `space.rs`'s mixed-input canvas
fallback, `diff.rs` (can the oracle itself false-pass?), `reference.rs` golden-update
discipline, `graph_loader.rs`'s consumption of fused defs, the segment Pending-hang path,
and edges #3/#7 below, which no lens engaged.

1. FIXED (2026-07-14, FUSION_SOTA_DESIGN.md P1): the **marker ABI** (§5) now has
   type-level enforcement — `freeze/markers.rs`'s `Marker` enum with `emit`/`parse`
   as the sole wire-format implementation, both codegen/install (producer) and
   `wgsl_compute::introspect` (consumer) compile against it. Negative gate
   `marker_literals_live_in_one_module` proves no stray marker literal exists
   outside `markers.rs`; `fused_wgsl_snapshot_unchanged` proves the refactor
   changed zero emitted bytes.
2. The **suite-parallelism GPU flake** is an eroding safety net — worth a root
   cause before trusting any future red/green signal.
3. FIXED (2026-07-14): **Out-of-loop ≈ulp** now has one named constant pair,
   `OUT_OF_LOOP_ULP_ABS_TOL`/`OUT_OF_LOOP_ULP_REL_TOL` (`freeze/proof.rs`),
   backing the 15 out-of-loop-texture-region proofs that all already shared
   the same (1e-2, 3e-2) texel bound (ColorGrade, camera-derived, gather/warp
   regions, quarter-res, fan-out, etc.) — a name-the-magic-number refactor, no
   proof's pass/fail behavior changed. The per-proof `passes(max_over_fraction)`
   budget stays per-proof by design (§7.4) — that fraction is tuned to each
   kernel's discontinuity profile, not part of the shared texel-level contract.
4. FIXED (2026-07-14): `classify.rs` doc-comment drift (see header note above)
   — the `Gather`/`BufferGather`/`Source` comments no longer mis-describe
   gathers/buffers as forcing Boundary or Source as standalone-only.
5. FIXED (2026-07-14): `def_content_key` normalizes a cloned def — clearing
   `editor_pos`/`title` on every node, including nodes nested inside group
   bodies — before hashing, so a node drag or rename no longer perturbs the
   key. Same "serialize the whole thing and hash the bytes" mechanism as
   before, just fed a cosmetic-fields-cleared clone. Tests:
   `content_key_ignores_editor_pos_drag`, `content_key_ignores_title_rename`,
   `content_key_changes_on_wire_param_or_topology_edit` in
   `node_graph/freeze/install.rs`. Residual: `GroupDef::tint` (the group
   header accent colour) is also purely cosmetic and still participates in
   the hash — left alone since it wasn't in this fix's scope; same minor-churn
   character as the original issue, just narrower.
6. Buffer fan-out regions, nested stencils, Table/String params, and
   resampler-into-region remain deliberate boundaries — under-fusing by
   design. (Vec3/Vec4/Color params lifted P5; multi-output texture atoms
   — voronoi_2d, block_displace_field — lifted P6, FUSION_SOTA_DESIGN.md D4.)
7. FIXED (2026-07-14, FUSION_SOTA_DESIGN.md P7): `leak_params`/`leak_ports`/
   `Box::leak` of views are gone — fused caches (`FUSED_EFFECT_CACHE`/
   `FUSED_GENERATOR_CACHE`/`SEGMENT_CACHE`) hold `Arc<T>` with owned
   `Vec`/`String` interiors; at cap, LRU evicts the least-recently-hit entry
   instead of refusing to insert. Negative gate: `rg 'Box::leak'
   crates/manifold-renderer/src/node_graph/freeze/` returns zero hits
   (`freeze_has_no_leaks`). Pathological edit-spam past 512 shapes now evicts
   and frees instead of leaking per rebuild.
8. FIXED (2026-07-14, FUSION_SOTA_DESIGN.md P2): segment `Pending` can no
   longer hang forever — `SEGMENT_PENDING` carries enqueue timestamps;
   `pump_segment_results` expires anything past `SEGMENT_COMPILE_DEADLINE`
   (60s) into the negative cache with one log line, and a worker panic is now
   caught (`catch_unwind`) and negative-caches the key instead of killing the
   thread. A genuinely hung (not panicking) OS thread still can't be reaped —
   the fix makes the CHAIN stop waiting and the state visible, not the thread
   reclaimed.
