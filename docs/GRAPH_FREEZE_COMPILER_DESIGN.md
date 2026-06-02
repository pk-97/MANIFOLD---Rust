# Graph Freeze / Fusion Compiler — Design

**Status:** DRAFT for review (2026-06-02). Branch `freeze-compiler`. Companion to `GRAPH_FREEZE_PHASE0_FINDINGS.md` (the measurements this design is aimed at). This is the keystone design — the `wgsl_body` calling convention (§4) and the region model (§3) are the decisions to sign off before implementation rollout.

Authored autonomously overnight per Peter's mandate ("go as far as sensible; make high-quality/safe design decisions; state-of-the-art but friendly for AI agents to develop and debug"). Decisions here are proposals; nothing is rolled out across the atom library until reviewed.

**⚠ ADVERSARIAL REVIEW COMPLETE (2026-06-02) — read §11 before building.** An 8-dimension adversarial review (5 dimensions returned structured findings; 3 — codegen, freeze-specialization, debuggability — failed to emit structured output and should be re-reviewed) found **6 blockers + 17 majors**, all grounded in code. The spine held (WGSL-out + `Backend` trait + spirv-opt structural fusion; the unfused-oracle framing; ColorGrade as first target). But several body sections are now superseded by §11 — specifically: §4 (the `wgsl_body` convention was under-specified), §7 (the oracle tolerance is unsound as written), §0 pillar 2 / §5 / §9 (Vulkan barriers are NOT free — explicit build item), and §4b/§10 (the buffer-domain win is unmeasured and partly mis-targeted). **§11 catalogs every finding with its resolution. Do NOT build the harness or compiler until the §11-flagged keystone decisions are signed off.**

---

## 0. Goal & principles

Turn a graph of small typed atoms into the fewest, fastest GPU kernels that produce a bit-equivalent result, automatically, without the author giving up composability or live control. Four principles, in priority order:

1. **Correctness is gated, not hoped.** Every fused output is verified against the unfused oracle (§7). Nothing ships unverified. A fused kernel that can't be proven equivalent falls back to unfused.
2. **Backend-agnostic.** The compiler emits **WGSL** and touches the GPU only through `manifold-gpu` (`GpuDevice`/`GpuEncoder`/`GpuTexture`) and the `node_graph::Backend` trait. No Metal types, no MSL, no raw `metal` crate. WGSL → naga → SPIR-V → spirv-opt → {Metal: spirv-cross→MSL · Vulkan: SPIR-V direct} is the existing pipeline; fusion plugs into its front end. Cross-dispatch sync uses `GpuEncoder::pipeline_barrier` (Metal no-op, Vulkan real). A `VulkanBackend` drops in with zero compiler changes.
3. **AI-/human-debuggable.** A fused kernel is inspectable (dump the generated WGSL), localizable (per-region diff against unfused), and reversible (fall back to unfused per-region). Region decisions are explicit and logged, never magic. This is a hard requirement, not a nicety — agents must be able to develop and debug this.
4. **Automatic, invisible, no new authoring surface.** No "freeze button" (§6). Fusion + specialization are driven by graph structure + expose-state, compiled in the background, cached. The author composes atoms as today.

---

## 1. What the compiler operates on

The existing graph IR (`Graph` → `compile()` → `ExecutionPlan`). The fusion pass is an **added stage in `compile()`**, after topo-sort + lifetime analysis, before kernel/pipeline creation. It does not change the graph the editor shows; it changes how the plan maps to dispatches. Groups are already flattened (`manifold-core::flatten::flatten_groups`) before this runs, so the compiler sees a flat DAG.

Two scopes, same pass:
- **Per-card** (one effect/generator graph) — runs when the card's graph or its specialization inputs change.
- **Per-chain (LTO)** — the runtime already splices a layer's effect rack into one `ChainGraph` and rebuilds on rack edits; the fusion pass runs over that unified graph, fusing **across** card boundaries. Cards must therefore stay optimized *graphs*, never opaque kernels (§6).

---

## 2. The bandwidth thesis (why this exists)

Phase 0 measured ~0.25–0.3 ms per full-canvas pass at 4K, math-independent — a bandwidth-bound read+write round-trip of the intermediate. N chained per-element atoms = N round-trips. Fusion collapses a run of per-element ops into one kernel: read once, all math in registers, write once. Same for buffers (an intermediate `Array<T>` round-trips VRAM identically). The win scales with the number of round-trips removed.

---

## 3. Domain model & region growing (the core algorithm)

The compiler is **domain-parametric**. An "element" is a pixel (Texture2D), an array sample (Array/buffer), or a voxel (Texture3D). The kernel iterates the element-space; the body runs per element.

Classify each node by `(domain, element_space, kind)`:

- **`Pointwise`** — output element depends only on the same input element + uniforms (gain, contrast, euler_step, array_math element-wise). Fusable.
- **`MultiInputCoincident`** — reads 2+ inputs at the *same* element (mix, compose, dither). Fusable (multiple reads at own coord).
- **`SingleDependentGather`** — one sample at a computed coord (remap/UV-warp, color_lut). Fusable: the coord math + the single sample + any blend inline into one kernel.
- **`BoundedFixedMultiTap`** — a small constant number of samples (chromatic's 3 taps). Fusable (bounded unrolled reads).
- **`Boundary`** — everything else: large/variable multi-tap (blur, convolution, Sobel, LIC), stateful/feedback, resolution/length change (downsample/resample/compaction), cross-element reads (neighbor smooth, reductions, sort), domain crossings (scatter_particles, sample_texture_at_particles, resolve_accumulator, render_3d), FFI/DNN.

**A fusable region** = a maximal connected subgraph of fusable nodes in the **same domain and element-space**. Region growing: walk the topo order, union adjacent fusable nodes sharing domain+element-space, cut at any boundary or domain crossing. Each region → one fused kernel. Boundaries stay their own dispatch and form the seams.

Classification is declared per atom, not inferred — each primitive states its `fusion_kind` (default `Boundary`, so an unclassified atom is never wrongly fused). Conservative by construction.

---

## 4. The `wgsl_body` calling convention (keystone decision)

A fusable atom declares its math as a WGSL **function**, not a whole `@compute` shader. The `primitive!` macro grows an optional `wgsl_body` slot; atoms without it stay full-kernel (and are `Boundary` by default). Calling convention by domain:

- **Texture pixel:** `fn body(c: vec4<f32>, uv: vec2<f32>, /*params via a generated uniform struct*/) -> vec4<f32>`
- **Buffer element:** `fn body(e: Elem, index: u32, /*params*/) -> Elem` where `Elem` is the channel-struct emitted from the wire's `_SPECS` (§5).
- **Multi-input / gather:** the body receives the extra inputs as additional `vec4`/sampler args; the codegen supplies the coincident reads or the single dependent sample.

The body uses **no globals** (no `@builtin`, no bindings) — purity is enforced so it inlines anywhere. The full-kernel form remains the escape hatch for anything genuinely not inlinable.

**This is the slot to review.** Getting the signature right (params passing, multi-input, gather) is what makes the whole library convertible cleanly. Proposal: start texture-pointwise (`fn(vec4, uv, params) -> vec4`), prove it, then extend to multi-input/gather/buffer once the shape holds.

---

## 5. Codegen (region → one kernel)

For each region, generate one WGSL kernel:

1. **Iteration wrapper** by domain — pixel grid (`@workgroup_size(16,16)`, guard `id.xy < dims`), or element index (`@workgroup_size(64)`, guard `i < count`), or voxel.
2. **One input read** per distinct source the region needs; chain the body calls threading the value through a local; **one output write**.
3. **Merge bindings/uniforms** — union the region's atoms' params into one generated uniform struct (std430), dedup samplers, map each body's param refs to the merged struct.
4. **Buffer intermediate types** come from the channel system: read the wire's `_SPECS`, emit `struct Elem { … }` with std430 offsets mechanically (CHANNEL_TYPE_SYSTEM.md §16.3.1). Dead-channel elimination: load/store only the channels the region touches (§16.3.2). Pure rename/reorder atoms erase (§16.3.4).
5. **Hand to the existing backend pipeline** — the generated WGSL goes through naga → SPIR-V → spirv-opt → backend. spirv-opt's `InlineExhaustive` + CCP + DCE (already configured in `manifold-gpu/src/shader_common.rs`) inlines the body calls, folds baked constants, and threads intermediates into registers. **The compiler does structural fusion + graph-level constant-folding/DCE; spirv-opt does the intra-kernel scalar optimization.** No bespoke optimizer.

Backend-agnostic throughout: WGSL out, pipeline via `GpuDevice::create_compute_pipeline`, dispatch via `GpuEncoder`, cross-region sync via `pipeline_barrier`. A region's kernel is identical IR on Metal and Vulkan; only the final lowering differs.

---

## 6. Specialization & the no-button model

Fusion (structural, params live) is **always on**. Specialization (bake constants → DCE dead branches) is driven by **expose-state**, not a button:

- Exposed params (bound to MIDI/Ableton/LFO) stay **live uniforms**.
- Unexposed params are de-facto constants (nothing drives them) → baked → constant-fold + DCE. Free, reversible (re-generalize if exposed later).
- **Stability-gated:** don't bake a param while it's being actively dragged (would churn-recompile); bake when the config settles (idle / perform-mode entry). Tuning stays live+fused; specialization lands on settle.
- **Chain LTO:** the per-chain pass fuses across cards. Frozen cards stay optimized graphs so this works (an opaque kernel would block cross-card fusion — the native black-box limitation).
- **Async + cached:** compile on a worker off the UI + content-render threads; run the unfused (always-correct) chain immediately; hot-swap the fused kernel when ready; cache per config (the WGSL→…→pipeline result caches in the binary archive across sessions). Rack edits never stall.
- **UX surface = a status badge** (fused / baking… / baked ✓), not a gate.

---

## 7. Verification (the oracle — built before any rollout)

The frozen build is a *mechanical transform* of the unfused graph, so the unfused graph is a **free exact oracle** for any input. The harness (generic, one for the whole library — no per-effect authoring):

- Feed any graph → fuse it → render/run both the fused and unfused versions on the same inputs → diff.
- **Fuzz** random inputs + random exposed-param values (not fixed samples) — hits the edges where fixed-sample tests leak.
- **Diff per fusion-region**, not just final output — the compiler knows the region boundaries, so a mismatch localizes to the offending region.
- **Diff on the GPU** (a max-abs-diff reduction → read back a scalar, not the image) — keeps it fast.
- **Tolerance = f16-ULP, in the more-accurate direction** (fusion is more precise: f32 registers vs Rgba16Float round-trips). Not byte-exact.
- **Cheap GPU-free layer first:** structural/IR checks on the transform (every input wire mapped, every body present, only frozen params folded) catch mechanical codegen bugs instantly; the numerical diff catches semantic ones.

Deterministic effects get the automated pixel gate; chaotic/temporal effects fall back to structural checks + visual inspection (per existing practice).

---

## 8. Debuggability / AI-agent friendliness (a requirement, not a feature)

- **Dump the generated WGSL** for any region (a debug flag / artifact), so an agent or human reads exactly what fused.
- **Per-region attribution** — the plan records which atoms went into which kernel and why a boundary cut. Logged, inspectable.
- **Fallback switch** — any region can be forced unfused (global or per-region), so a suspect fusion is isolatable in one step. The unfused path is always correct and always available.
- **The oracle is the contract** — an agent extending the compiler runs the harness; a regression localizes to a region + shows the diverging pixels/elements. Development is gated by a green oracle, not by reading shader assembly.
- **Conservative defaults** — unclassified atoms are `Boundary`; the compiler never fuses something it wasn't told is fusable.

---

## 9. Build sequence

1. ✅ Phase 0 — profile + sweep (texture); Phase 0-b — buffer bench.
2. **Verification harness** (§7) first — the oracle gates everything after.
3. **`wgsl_body` convention** (§4) + convert a first batch of texture-pointwise atoms; ship inert (atoms still run normally, now also expose a body).
4. **Fusion pass v1** — texture-pointwise regions only, per-card; validate ColorGrade (7→1) bit-equivalent via the harness; measure the win.
5. Expand the classifier (multi-input → gather → buffer domain), then chain LTO, then the specialization/expose-state baking. Each step gated by the oracle.

**Deferred for review before rollout:** the broad atom conversion, the generic multi-domain compiler, and chain LTO — all ride on the §4 convention + §3 region model being signed off.

---

## 10. Open questions

- **Register pressure / occupancy ceiling.** Over-fusing a long region (esp. buffer kernels over millions of elements) can spill registers and lower occupancy. Mitigation: heavy/stateful ops are boundaries anyway, so regions are short; add a cost-model cap on region length if a pathological case appears. Measure, don't pre-optimize.
- **Param-as-uniform vs specialized variant** for discrete exposed enums (blend mode): compile a variant per option vs a live uniform. Start with uniforms; specialize discretes later if measured worth it.
- **Cross-region barrier minimization** — `pipeline_barrier` only where a region reads what the previous wrote. *(§11.C corrects this: the barrier read/write sets are NOT currently materialized.)*

---

## 11. Adversarial review — findings & resolutions (2026-06-02)

8 independent adversarial reviewers, one per dimension. 5 returned structured findings (region-algorithm, wgsl-body-convention, backend-portability, verification-oracle, performance-occupancy); 3 (codegen, freeze-specialization, debuggability) failed to emit structured output and were **re-reviewed inline in §11.G** (2026-06-03). The first pass surfaced **6 blockers + 17 majors**; the re-review adds **1 blocker + 6 majors** — all grounded in specific files. Resolution legend: **DECIDED** = a high-quality/safe fix I'm confident in; **FLAGGED** = needs Peter's judgment or measurement before build.

### 11.A Correctness — resolution & element-space (region algorithm)
- **[BLOCKER] Coincident reads are two classes, not one.** `mix` samples by normalized UV (resolution-robust); `dither` uses `textureLoad` at integer texel (correct only when inputs share dims). Fusing a texel-load atom across a resolution-changing seam reads garbage — silently wrong. **DECIDED:** read-semantics becomes a per-input property of the convention — split `CoincidentUV` (sampler, rescales) vs `CoincidentTexel` (load, requires producer dims == region dims); region growing refuses to fuse a `CoincidentTexel` input across a resolution/scale seam.
- **[MAJOR] "Same element-space" must reuse the executor's dim policy.** Element-space is resolved against the runtime canvas via a three-way category (concrete dims / canvas-scale / canvas-default, `execution_plan.rs`). **DECIDED:** define equivalence in those same terms, reuse `resource_canvas_scales`/`resource_dims`, cut on any disagreement; add a region-growing test mirroring the OilyFluid quarter-res-mixed-with-canvas fixtures.
- **[MAJOR] Dispatch-grid extent must be declared per atom.** `mix` grids from output dims, `remap` from source dims. **DECIDED:** iteration-extent (output-sized vs named-input-sized) is part of the `wgsl_body` contract; fuse only atoms whose extent resolves to the same texture identity; a gather drives the grid from its destination, never its sampled source.

### 11.B The `wgsl_body` convention (keystone — most revised; sign-off needed)
- **[BLOCKER] Buffer signature needs symmetric multi-input.** `euler_step_particles` reads `particles[i]` AND `forces[i]`; the single-input buffer signature can't fuse the force chain (the named target). **DECIDED:** `fn body(a: ElemA, b: ElemB, …, index: u32, count: u32, frame: FrameState, params) -> ElemOut`.
- **[BLOCKER] `array_math` (named buffer target) is CPU-only** — `mapped_ptr`, content thread, no WGSL, no VRAM round-trip (`array_math.rs`). The bandwidth thesis doesn't apply. **DECIDED/CORRECTION:** scope buffer-domain fusion to *GPU-dispatched* buffer atoms (euler/scatter/force chain); **exclude the CPU array-math/curve-math family** from §4b. DigitalPlants' array_math "win" was overstated. (Migrating those to GPU would reintroduce the GPU→CPU fence the CPU path exists to avoid — not free.)
- **[MAJOR] Convention needs frame-state + count, not just params** (`euler` uses `delta*60` and an `active_count` guard). **DECIDED:** add a frame-state arg (time/delta/frame) distinct from author params, and an explicit `count`.
- **[MAJOR] A param can be a runtime WIRE** (LFO/BeatGate/audio via `scalar_or_param` — the live performance instrument). **DECIDED:** the param-arg sources from `{constant, uniform, in-region register}` — if the producing node is fused into the region it's a register; if it's a boundary node, it arrives as a per-dispatch uniform written from that node's output.
- **[MAJOR] Gather contradicts the "no bindings" purity rule.** A dependent sample IS a texture+sampler access. **DECIDED:** split purity — own-pixel/coincident reads are codegen-supplied register values (pure body); gather bodies get a declared sampled-texture+sampler arg (codegen owns the binding and **preserves the exact filter + address mode** of the unfused atom). "Pure modulo declared sampled-texture args."
- **[MAJOR] Purity is "enforced" but no mechanism was designed.** **DECIDED:** a structural naga check on the body fragment (reject `@builtin` refs, non-declared global bindings, storage writes, implicit-gradient `textureSample`); build on `collect_globals_from_function` (`shader_common.rs`). A compile-time gate, not oracle-only.

### 11.C Backend portability / Vulkan (weakest link — largely build-items, some FLAGGED)
- **[BLOCKER] Barrier API is texture-only.** Buffer hazards use a separate primitive (`compute_memory_barrier_buffers`); a fused buffer-region seam is a storage-buffer RAW `pipeline_barrier` can't express → Vulkan corruption. **DECIDED:** make the barrier API resource-kind-complete (textures + buffers + storage images) **before any buffer-domain fusion**.
- **[BLOCKER] Current correctness silently relies on Metal's serial-encoder auto-ordering** (no texture barriers exist anywhere; storage-image writes ordered implicitly). Fusion changes the barrier topology Vulkan must satisfy; "identical IR, only lowering differs" is true of the *kernel* but false of *inter-region sync*. **DECIDED + FLAGGED:** add a section specifying, per region seam and per feedback boundary, the exact barrier (resources + access masks + storage-image layout transition). The `barrier_analysis` pass + kind-complete barriers are an **explicit build item, not "free."** Must be validated against MoltenVK — the Metal headless bench never exercises it.
- **[MAJOR] `barrier_analysis` doesn't exist; `pipeline_barrier` has zero callers; the RAW read/write sets aren't materialized** (lifetime analysis tracks slots, not barrier transitions). **CORRECTION:** reclassify §0-pillar-2/§9 "drops in with zero compiler changes" → a scoped build item over both texture and buffer resources.
- **[MAJOR] Feedback cross-frame + within-frame state-capture back-edge sync** unaddressed for Vulkan. **FLAGGED.**
- **[MINOR] f16 (`use_half`) precision is not bit-portable across backends**, and node-graph atoms currently don't take the half path. **DECIDED:** fused regions run f32, never inherit `use_half`; the oracle must run per-backend before Vulkan ships.

### 11.D Verification oracle (must be hardened before it can gate)
- **[BLOCKER] The f16-ULP "more-accurate-direction" tolerance is unsound across discontinuities** (clamp/fract/smoothstep/min/max — ColorGrade is full of them: past a threshold, more input precision ≠ closer to truth; the f16-rounded oracle and f32 fused value land on opposite sides). **DECIDED:** replace with a **two-sided per-pixel max-abs + relative bound**, sized by worst-case post-discontinuity amplification of one f16 quantum, plus a discontinuity-aware metric (tolerate a small fraction of boundary pixels via count-of-large-diffs). Drop the directional assumption.
- **[MAJOR] Per-region diff is infeasible as written** — region-boundary intermediates are pool-recycled before frame end (`backend.rs`). **DECIDED:** a verification-only mode that either disables pool aliasing (unique slot per `ResourceId`) or snapshots each region output to a readback target before release. Document the cost.
- **[MAJOR] Fuzzing must be bounded** — unbounded params manufacture false Inf/NaN divergences (f16 overflow at 65504) and can hide real bugs. **DECIDED:** bound the fuzzer to each `ParamDef.range` + a realistic input dynamic range; require bit-identical NaN/Inf classification (never silently skip saturated pixels).
- **[MAJOR] Add resolution + canvas-scale to the fuzz axes; use real multi-res presets (Bloom, OilyFluid, FluidSim) as fixtures** — to catch the §11.A resolution-seam class, which a single-canvas harness never reproduces. **DECIDED.**
- **[MINOR] Keep the GPU-free structural/IR layer as the primary guard against systematic sub-tolerance bias** (verify baked constants stored at f32; no reassociation changed folding). The oracle is no longer byte-exact like the existing parity harness — document why.

### 11.E Performance (the buffer win must be MEASURED before sign-off)
- **[MAJOR] Buffer chains are in-place aliased mutation, not VRAM round-trips** (`aliased_array_io` — the force chain accumulates into ONE cache-resident buffer). The texture-domain 0.3 ms/step will NOT transfer. **FLAGGED:** measure the buffer break-even (hand-fused force chain at 100K / 1M / 16M particles) **before committing the co-equal-domain framing.** Phase 0-b confirmed FluidSim is buffer-*bound*; whether fusion *helps* it as much as textures is unproven.
- **[MAJOR] Register pressure/occupancy is the PRIMARY buffer-fusion failure mode** (simplex noise + 3D sample + full 64-byte Particle struct live over 16M elements can halve/quarter occupancy). **DECIDED:** a register/occupancy cost-model cap is a **required input of buffer-fusion v1**, not a "later" footnote; cut a region when occupancy would drop below threshold.
- **[MAJOR] Fusion forfeits the serial encoder's free cross-dispatch overlap** (Apple GPUs pipeline adjacent unbarriered dispatches). **DECIDED:** name it as a fusion cost; the ColorGrade 7→1 must be measured against the real *overlapping* unfused baseline — the win may be below the predicted 3–6×.
- **[MAJOR] Break-even never quantified; "always-on" can regress short regions.** **DECIDED:** add a **perf gate** — after fusing, time fused-vs-unfused on GPU and keep the fused kernel only if faster by a margin (so "always-on" is safe: a non-paying region falls back to its already-correct unfused form). Min region length ~L≥3 for texture, TBD for buffer.
- **[MINOR] Phase 0 `ms/step` is an average (`ms_frame/steps`), not per-dispatch.** **DECIDED:** instrument real per-dispatch GPU timestamps for ColorGrade before Phase 4 to ground the 3–6× number the initiative rests on.
- **[MINOR] Dead-channel DCE is harder than §5.4 implies** — the particle shaders are hand-written structs (full load/store), not Channels-typed (`channel_get`). **FLAGGED:** either gate buffer dead-channel on a Channels migration, or measure whether spirv-opt's existing load/store-elim already drops untouched-field stores.

### 11.F Net assessment & gate
The spine is sound. Before building: revise §4 (convention) and §7 (oracle) per the above and **sign them off** (keystone decisions), make the barrier API kind-complete (§11.C), and **measure the buffer break-even** (§11.E). Re-review the 3 dimensions that didn't return (codegen, freeze-specialization, debuggability) — **done in §11.G**. The texture-pointwise ColorGrade path is the lowest-risk first build once §4/§7 are settled; the buffer domain is gated on measurement + the register cost-model + the kind-complete barriers.

### 11.G Re-review of the 3 non-returning dimensions (2026-06-03)
Done inline (read-only audit, main context). The headline: **one BLOCKER, and it lands in the *specialization* phase (build step 5), not the first ColorGrade fusion (step 4)** — so the lowest-risk first build is unaffected. The codegen findings that DO bite at first fusion are namespacing + uniform layout (both DECIDED). Debuggability has no blockers — the always-retained unfused graph makes it structurally sound.

**Codegen (§5).** Grounded against `manifold-gpu/src/shader_common.rs` (the real spirv-opt pass list: `InlineExhaustive`, `ConditionalConstantPropagation`, `AggressiveDCE`, `EliminateDeadConstant`/`Members`, `Local{Single,Multi}StoreElim`).
- **[MAJOR] §5 contradicts §6 on baked params.** §5.3 merges *all* params into one uniform struct; §6 bakes unexposed params for CCP+DCE to fold. But a uniform read is **not** a compile-time constant — `ConditionalConstantPropagation` cannot fold it, `AggressiveDCE` cannot drop the branch it guards. **DECIDED:** codegen splits the param set — **exposed → uniform field; baked (unexposed) → emitted as a WGSL `const`/literal** (or pipeline-override constant) so CCP actually folds and DCE actually prunes. The "merge into one uniform" applies to the *exposed* subset only. (This is why fusion-only step 4, where every param stays a live uniform, gets structural fusion but NOT the constant-fold/branch-prune win — that win arrives with baking in step 5.)
- **[MAJOR] Symbol collision on mechanical body-chaining.** Two atoms (or two instances of one atom) that each define a helper `fn hash(...)` / a private struct collide when concatenated into one module. **DECIDED:** namespace every atom's body + its private helpers with a per-node unique prefix at codegen; this also makes codegen **deterministic** (stable prefixes), which the cross-session pipeline cache (§6) needs for hits.
- **[MAJOR] Merged uniform must obey WGSL uniform layout, not naive concatenation** (16-byte alignment, vec3→16-byte pad — the recurring `feedback_wgsl_vec3_alignment` / `feedback_uniform_alignment` / `feedback_naga_uniform_size_rule` bug classes). **DECIDED:** emit the merged exposed-param block as a **read-only storage buffer (std430)** rather than a uniform — std430 packing is mechanical and sidesteps the std140 vec3/array stride traps; param updates are infrequent (per settle, not per dispatch) so the uniform-fast-path is irrelevant here.
- **[MINOR] Gather read path.** §5.2 "one input read" is stale w.r.t. §11.B: a region containing a gather has two read paths — codegen-supplied register values (pointwise/coincident) AND a declared sampled-texture+sampler arg (gather, exact filter/address-mode preserved). Cross-ref §11.B; no new issue.
- **[MINOR] Encoder bind-cache.** The generated pipeline's binding layout must honor the existing `setBytes ↔ setBuffer` same-slot invalidation rule (`feedback_encoder_bind_cache_invalidation`, the FluidSim2D page-fault root cause). Not new, but generated bind layouts must not reuse a slot across resource kinds without invalidation.

**Freeze-specialization (§6).** This is the dimension with teeth — it's where "automatic, just works" meets the live-performance instrument.
- **[BLOCKER] "Unexposed = constant" is the wrong predicate and silently kills live control.** §6 reads "unexposed" as the UI notion (no MIDI/Ableton/LFO binding). But `param_values` is written every frame by drivers/Ableton/envelopes/MIDI (`feedback_param_values_is_performance_surface`), and a param can also be driven by a **control wire** (port-shadows-param, `project_control_wires_port_shadows_param`) or a `scalar_or_param` runtime wire (§11.B) with no "user binding" at all. Baking such a param freezes it at one value — the effect goes dead on stage with no error. **DECIDED:** bake-eligibility is a **conservative taint analysis** — a param is bakeable only if it has **no per-frame writer of any kind** (no user binding, no control wire, no `scalar_or_param` source, no envelope/automation, no driver). Default to NOT bakeable; bake only what's provably static. This is `feedback_no_silent_fallbacks` applied to specialization: never silently substitute a frozen value for a live one.
- **[MAJOR] Bake unit is per-instance-config, not per-type.** Two rack cards of the same effect type have different `param_values` + expose-states; the frozen kernel + cache key must be `(graph-def hash, per-instance expose/bake config)`. Chain-LTO across cards must key on the concrete per-instance configs, and a param exposed on card B forces a live-region boundary even if card A's neighbors are baked. **DECIDED.**
- **[MAJOR] Invalidation on every mutation path.** Edit / undo / redo / rebind / expose / unexpose must invalidate the bake and **cancel any in-flight async compile for the now-stale key** (`UndoRedoManager` drives mutations; the cache key must include the full bake-relevant config hash). **DECIDED.**
- **[MAJOR] Async hot-swap must not introduce shared mutable state.** §6's "swap the fused kernel when ready" cannot be a shared pipeline cell (`no_new_shared_state` hard rule). **DECIDED:** the worker posts a compiled artifact; the **content thread installs it between frames** via the existing command/state path — the render thread only ever reads an immutable snapshot. Spec this as a content-thread state transition, not an `Arc<Mutex<Pipeline>>`.
- **[MINOR] Exposing a param mid-show triggers an async recompile** → a brief unfused (correct, slightly slower) window. Acceptable; name it so it's not a surprise on stage.

**Debuggability (§8).** No blockers — the retained-unfused-graph architecture (it's the oracle AND the immediate-run path AND the fallback) makes this dimension structurally sound. Gaps are additive:
- **[MINOR] Global perform-mode kill-switch.** Per-region fallback exists; add an explicit **"never fuse tonight" master disable** (config/env). A timing bug becomes the show — the performer needs a one-flip retreat to the known-good unfused path without per-region fiddling.
- **[MINOR] Dump must reach past WGSL.** §8 dumps generated WGSL, but the inline+fold (and the §11.C f16/portability bugs) happen at SPIR-V/MSL lowering. The debug artifact should optionally include post-spirv-opt disassembly, not just the WGSL.
- **[MINOR] Emit a minimal reproducer on oracle failure** — `(fuzz seed, param vector, resolution, region id, backend)` — so an AI agent can replay deterministically. "Development gated by a green oracle" is only agent-actionable if a red oracle hands back a one-command repro.
- **[MINOR] Warn on Boundary-by-default (unclassified) atoms.** Conservative default (unclassified → Boundary) is safe but silently under-fuses a newly-added atom (perf stagnation, no error). Log unclassified-vs-by-design boundaries at build so the conversion backlog is visible (`no silent caps`).
- **[MINOR] Attribution keys on stable NodeId** (`project_node_id_targeting`), consistent with bindings — so an agent's region→atom map survives handle churn.

**Net of §11.G:** no change to the gate for the first build. Step 4 (texture-pointwise ColorGrade fusion, all params live) needs only the two DECIDED codegen items (namespacing, std430 param buffer). The BLOCKER (bake-eligibility taint) and the baked-as-const codegen item are correctly scoped to step 5 (specialization) and must be settled before any param is baked. Debuggability adds five low-cost ergonomics items, none gating.
