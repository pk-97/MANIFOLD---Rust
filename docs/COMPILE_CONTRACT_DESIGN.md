# Compile Contract — nothing compiles during a show

**Status:** SHIPPED P1–P3 2026-08-21 · k3 (lead) — RT pipelines device-global; data-gated/lazy-bounded pipelines compile at install; `@static_param` specialization deleted (frame cost within noise). Owed: Corrosion probe re-run after WARMUP P7 lands (cold touches during playback: 0).
**Prerequisites:** none (builds on WARMUP_DESIGN P1–P6, all on main)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs)–section 6 (seam briefs) before starting any phase.

The governing insight: **a live rig cannot pay for code at showtime, so code must be
independent of showtime state.** WARMUP_DESIGN's contract — Peter, 2026-08-20:
"load time is free, play time is sacred — nothing happens for the first time during
a show" — splits first-use costs into two kinds. *Data* costs (GLB decodes, accel
builds, feedback seeding) are enumerable from the project file: warmup rehearses
them, and that works — P1–P5 proved it. *Code* costs (shader/pipeline compiles) are
the failure: rehearsing them means enumerating runtime states, and the state space
is combinatorial (Corrosion: 1496 clips; the P5/P6 residue was later-clip scenes
compiling RT kernels on stage). You cannot win a coverage game against a state space
that grows with the show. The end game is not a better rehearsal — it is making
compilation unable to see the state.

Peter, 2026-08-21, on the same shape in chain fusion: "the 'amount' slider does not
determine if it should be frozen in or not, it's a performance slider, not a
structure slider." This doc generalizes that call to every compile in the app:
**kernels may specialize on load-time-known structure, and never on data or
content.**

Companion: `WARMUP_DESIGN.md` (the data-rehearsal half — this doc is the code half),
`FREEZE_COMPILER_MAP.md` (structure-keyed codegen, already compliant post-P6),
`RAYTRACING_DESIGN.md` (RT specialization surface), `MANIFOLD_GPU_ARCHITECTURE.md`
(pipeline cache architecture), `VULKAN_BACKEND_DESIGN.md` (the approved backend this
discipline de-risks).

## 1. Audit — what exists (verified 2026-08-21)

Every runtime pipeline compile in the app, its reuse key, and its classification
(structure-keyed = compliant, data/instance-keyed = violation). Anchors re-verified
against main @ ff7edc285.

| Family | Anchor | Compiles | Reuse key | Class |
|---|---|---|---|---|
| Freeze/fusion chain kernels | `crates/manifold-renderer/src/node_graph/freeze/` | fused segment + per-card kernels | chain topology hash — post-P6 (`preset_runtime/build.rs` `compute_topology_hash`) keys on structure only: effect ids/types, enabled flags, group ids, graph structure version, dims | structure |
| render_scene raster variants | `render_scene.rs:3140` (`aux_variant`), prewarmed at startup `render_scene.rs:3293` (`prewarm_pipelines`, called from `GeneratorRegistry::prewarm_all`) | 8 fixed aux-output permutations (velocity × ao_mask × denoise) | three booleans from RT quality settings — fixed per project settings, prewarmed | structure |
| RT trace PSOs + MSL library | `crates/manifold-gpu/src/metal/raytrace.rs:4918` (library compile), `:4612` (PSO compile), `:4991-5010` (translucency variants) | full SHADOW_RAYS_MSL library + SIX PSOs per tracer: trace×2 (HAS_TRANSLUCENCY constant), upsample, atrous, accumulate, debug×2 | **nothing — owned per `MetalShadowRayTracer` instance. The trigger is tracer construction: `ensure_rt_tracer` (`render_scene.rs:2429-2433`) sets it once per `RenderScene` instance and never resets it, so every fresh RenderScene — a later clip's chain at transport start, re-activation after state-cache eviction — compiles the library + all six PSOs again. N RT scenes = N full MSL library compiles** | **violation (ownership)** |
| Cold-touch counter coverage | `metal/device.rs:463,740,920,1308,1509` (WGSL path records) | — | **the counter fires ONLY in the WGSL path; the MSL/raytrace compile helpers record nothing — the acceptance gate is blind to exactly the violation class above (the 6 residue compiles were all WGSL-path)** | **enforcement gap — P1's first fix** |
| Device pipeline cache | `crates/manifold-gpu/src/metal/device.rs:432` (`create_compute_pipeline`), `:600` (specialized variants) | all WGSL-path compute/render PSOs | global cache keyed on shader source + specialization + formats — the compliant precedent | structure |
| value_overlay (blob labels) | `primitives/render_value_overlay.rs:321` (data-driven skip) | label-drawing pipeline | **first use is data-gated: zero detections at load → skipped in warmup → compiles when the first blob is detected (Corrosion probe: frame ~60)** | **violation (data-gated first use)** |
| wgsl_compute escape hatch | `node_graph/descriptor.rs:321-323` | per-node WGSL source | node source string — structure (fixed by preset def) | structure |
| **wgsl_compute `@static_param` specialization** | `primitives/wgsl_compute.rs:2541-2617` | param values baked as module-scope `const` after `SPEC_STABLE_FRAMES` stable | **post-specialization source text — the key is live param VALUES: a performer holds a slider still → new WGSL → compile on stage** | **violation (data-keyed)** |
| variable_blur quality/weighting | `primitives/gaussian_blur_variable_width.rs:100,167` | QUALITY_LEVEL × WEIGHTING_MODE substitutions | 3×2 fixed enum variants, lazy per-variant map; the startup atom sweep explicitly skips it (`registry.rs:376-379`) — its 6 variants are guaranteed cold | **violation (lazy bounded set — prewarm all 6)** |
| MultiBlend input-count kernel | `primitives/multi_blend.rs:246-249` | `shader_for(k)`, k = wired inputs 1..MAX | k is graph structure, but the variant compiles on first configure | structure (lazy — prewarm at install) |
| Data-gated lazy pipelines (class) | `render_value_overlay.rs:491-508` (get_or_insert AFTER the quad_count==0 early return — confirmed data-gated), `blob_detect_ffi.rs:352-358,401-406`, `watercolor.rs:127-187`, `render_text.rs:369+`, `spawn_from_mesh.rs:187-208`, `hdri_source.rs:320-390` | fixed-source pipelines compiled on first evaluate | source is fixed — but first use is data-gated (zero detections at load → skipped in warmup → compiles when the first blob lands, Corrosion frame ~60) | **violation (data-gated first use)** |
| render_scene lazy RT-adjacent + IBL | `render_scene.rs:2797-2820` (IBL prefilter/irradiance/brdf_lut — gated on async envmap arrival, NOT in `prewarm_pipelines`), `:5907` (hit_dist_extract), `:7042` (upscale_alpha_combine) | fixed-source pipelines, lazy Option fields | device-cached so once-per-process — but first-use lands whenever the data arrives, including mid-show | **violation (same class; P2's inventory must find lazy-Option pipeline fields, not just data-skip declarations)** |
| Clear/blit/utility, compositor, tonemap, upscalers, UI, LED, media | `metal/device.rs:1103-1123`, `layer_compositor.rs:65-77`, `tonemap.rs:73-80`, `fsr1.rs:61-79`, `metalfx_upscaler.rs:55-75` | fixed-source pipelines | fixed | structure — created at construction/startup |

Corrosion probe numbers behind the classifications: post-P6 residue 55 → counter-fix
(detector armed at warmup start, `content_commands.rs:437-441`) → 6 real: 5
`node.wgsl_compute` compiles tied to fresh RT topo keys at transport start + 1
`value_overlay` at frame ~60. BUG-p8oe (warmup-p5-residue-cold-touches) carries the
evidence; probe logs at /tmp/p6_probe2.log.

⚠ VERIFY-AT-IMPL: any family missed here (plugins, LED, media encode, image loader)
is P2's entry-state check, not P1's — P2's fixed inventory method covers it:
`rg -n 'new_compute_pipeline|newComputePipelineStateWithFunction' crates/ -t rust`.

## 2. Decisions

- **D1 — The contract is zero compiles after warmup.** After `LoadProject`'s warmup
  pass completes, no pipeline or library compiles during playback, ever. The
  enforcement oracle is the cold-touch counter (`manifold_foundation::cold_touch`) —
  **but today it only records the WGSL path** (`metal/device.rs:463,740,920,1308,1509`);
  the MSL/raytrace compile helpers (`raytrace.rs:4918`, `:4612`) record nothing, so
  the gate is blind to the biggest known violator. Wiring EVERY compile path into the
  counter is therefore P1's first deliverable — until then `cold touches: 0` proves
  nothing about MSL.
- **D2 — Specialize on structure, never on data.** A compile key may contain: node
  types, graph topology (post-P6 definition), texture formats, fixed feature
  permutations (RT quality settings, HAS_TRANSLUCENCY's two variants — both compiled
  at load). It may never contain: scene identity/content, param values, detection
  counts, arrival state of async data. Content variation flows through uniforms,
  runtime `Option` bindings (the RT hit shaders' existing texture-Option pattern,
  `render_scene.rs:5277-5291`), or bindless tables.
  **This kills the `@static_param` specialization path** (`wgsl_compute.rs:2541-2617`):
  baking a live value into WGSL after `SPEC_STABLE_FRAMES` is the contract's exact
  inverse — delete it; params stay uniforms. Consequences, stated honestly: that path
  exists to let the compiler dead-code-eliminate against a stable value; removing it
  costs whatever those kernels save. Measure before/after on the heaviest fused chain
  (Bloom + Watercolor on the canonical fixture); if the delta is real, the answer is
  codegen improvements, never value-keyed recompile.
  Rejected: rehearse-every-state warmup (the P7-as-first-sketch shape) — the state
  space is combinatorial and grows per show; coverage always leaks.
  Rejected: async compile with a placeholder/fallback frame — a silent quality
  fallback is a forbidden move class repo-wide, and on stage a placeholder frame IS
  the bug.
- **D3 — Pipelines are owned globally; instances own data only.** PSOs live in a
  device-level cache keyed by (source, entry, constants, fixed-function state).
  Per-scene/per-chain structs (`MetalShadowRayTracer`, chain executors, primitive
  instances) own data — accel structures, buffers, histories — and never PSO or
  library fields. **The existing device cache does not cover the MSL path**: it keys
  on WGSL source hash (`metal/device.rs:459`), while the RT tracer compiles raw MSL
  with `MTLFunctionConstantValues`, bypassing cache, counter, and binary archive.
  D3 therefore requires a cache extension keyed on (MSL source hash, entry, function
  constants, language version) — a named deliverable, not a fit-check. The hoist
  moves the full SHADOW_RAYS_MSL library and all six tracer PSOs (trace×2, upsample,
  atrous, accumulate, debug×2) into it; `MetalShadowRayTracer::new` receives them.
  Rejected: per-instance PSO ownership (the audited bug — a fresh `RenderScene`
  instance on chain rebuild/eviction recompiles the library + six PSOs on stage);
  a second per-family cache (one identity system, not two).
- **D4 — Warmup enumerates assets and structure, never states.** The warm pass
  decodes data, builds accels, seeds state, and installs the finite structure-keyed
  kernel set. It never walks clip/parameter combinations.
- **D5 — This is the Vulkan shape, not a Metal corner.** Bindless tables + global
  PSOs + load-time compilation is exactly the descriptor-indexing model
  `VULKAN_BACKEND_DESIGN.md` targets; implementing D2/D3 on Metal removes a porting
  risk instead of adding one.

## 3. Design body

### 3.1 The ownership seam (D3)

`MetalShadowRayTracer` today: `new` compiles the SHADOW_RAYS_MSL library
(`metal/raytrace.rs:4918`) plus six PSOs — trace×2 (`:4991-5010`), upsample, atrous,
accumulate, debug×2 — into instance fields (`:4877-4880` and siblings). Called from
`ensure_rt_tracer` (`render_scene.rs:2429-2433`), once per `RenderScene` instance,
never reset: every fresh RenderScene (later clip's chain, post-eviction
re-activation) recompiles the lot. New shape:

```rust
// metal/raytrace.rs — tracer owns data; code comes in
pub struct MetalShadowRayTracer {
    // ... accels, buffers, tables (unchanged, per-instance data)
    pipelines: RtPipelines,  // shared, from the device cache
}

/// Device-cache-owned: one MSL library + the six PSOs, keyed on
/// (source hash, entry, function constants, language version).
pub struct RtPipelines { /* library handle + six GpuComputePipeline */ }
```

The cache extension (D3) lives beside the WGSL cache in `metal/device.rs`.
**`RtPipelines` is the single cache unit, keyed on the MSL source hash** — the six
PSO entries share one library, so the library is a field of the cache entry, never
a separate per-PSO compile (a cold populate is one library compile + six PSO
creations, exactly once per process). Every compile helper (`compile_pipeline`,
`compile_pipeline_with_constants`, the library compile) routes through it AND
records `ColdTouchKind::PipelineCompile` on a miss (D1's counter coverage). Startup
prewarm populates it once for the full RT set — same pattern as
`render_scene.rs:3293`'s `prewarm_pipelines` registered in
`GeneratorRegistry::prewarm_all`. Tracer construction becomes pure data assembly; a
fresh RenderScene rebuilds accels (async, already bounded — RAYTRACING_DESIGN
section 8.2) and compiles nothing.

### 3.2 Data-gated first use and lazy bounded sets (D2 applied)

`render_value_overlay` and every node with a data-driven skip (`execution.rs:1055`
lists the class: zero blobs, zero particles, zero tracks) must create its pipelines
at install, unconditionally. The skip stays — it saves dispatch, which is the point —
but install is when the pipeline is born. Rule for authors: install-time compile,
runtime-skip dispatch. The same holds for bounded lazy variant sets — a kernel whose
only variation is a fixed enum (variable_blur's quality × weighting, MultiBlend's
input count) prewarms every variant at install; "lazy" is only legal when the variant
set is unbounded, and unbounded sets are exactly what D2 forbids.

### 3.3 What warmup becomes (D4)

The warmup pass drops every "rehearse a state" mechanism and keeps: asset decode,
accel builds, state seeding, structure-keyed kernel install, and the plugin prewarm.
The clip-active pre-roll (WARMUP P5 D11) survives only as the trigger-state priming
it already is — it is NOT extended to later clips. That is the point of D4: coverage
of states is deleted as a strategy, not improved.

### 3.4 Consequences, stated honestly

- Global PSO reuse means a pathological PSO (driver bug on one entry) poisons every
  scene, not one. Mitigation: none needed — same PSO bytes either way.
- Generic kernels pay small steady-state cost vs hypothetical per-scene tailoring.
  The audit found RT already generic (only HAS_TRANSLUCENCY varies, both variants
  precompiled); no current kernel needs content specialization, so this cost is
  hypothetical, not accepted-yet. A future kernel that genuinely wants content
  specialization must escalate — that request is a design review, not a code change.
- The contract makes warmup completion the readiness boundary even harder: if a
  compile escapes the warm, it hits the show. The gate (section 4) is the answer;
  the alternative (compile on stage) is what we have today.

## 4. Invariants & enforcement

- **INV1 — Zero compiles during playback, all paths.** Enforcement: the cold-touch
  counter extended to the MSL path (P1's first deliverable — every compile helper in
  `metal/raytrace.rs` records `PipelineCompile` on a cache miss) + the Corrosion
  probe as the acceptance command (`cargo build -p manifold-app --features perf-soak
  && RUST_LOG=info ./target/debug/manifold rt-capture '<Corrosion path>' --frames
  360` → `cold touches during playback: 0`); nightly `trunk_health.py` runs the
  probe on the RT fixture set so the contract can't rot between shows.
- **INV2 — No PSO/library fields on per-instance structs.** Enforcement: value test —
  construct a tracer, force a fresh `RenderScene` instance (the real trigger), assert
  the cold-touch counter's PipelineCompile total is unchanged. (The naive version —
  watching `compute_pipeline_cache_len()` — is vacuous: MSL compiles never touch
  that cache, so it passes with the bug present. The assertion must ride the
  extended counter from INV1.) Plus review rule in `ADDING_PRIMITIVES.md`: new
  pipelines come from the device cache or a named `prewarm_pipelines` registration.
- **INV3 — Data-gated nodes install pipelines at construction.** Enforcement: the
  gpu-proofs suite constructs each primitive with empty data and asserts its
  pipelines already exist in the device cache (extend the existing scope test,
  docs/ADDING_PRIMITIVES.md). Implies a structural change P2 must name: pipeline
  creation moves OUT of `run()` into an install-time hook per data-skip node.

## 5. Phasing

### P1 — Counter coverage, then the PSO ownership hoist (RT tracer)

- **Entry state:** none beyond main. The per-instance PSO inventory is already
  settled by the design audit + K3 review — do NOT re-derive scope; the RT tracer
  is the confirmed owner.
- **Read-back:** D1–D5, section 3.1, the forbidden moves below; then restate them.
- **Deliverables, in order:** (1) every compile helper in `metal/raytrace.rs`
  (`:4918` library, `:4612` PSO) records `ColdTouchKind::PipelineCompile` — the gate
  is blind until this lands; (2) the MSL cache extension beside the WGSL cache in
  `metal/device.rs`, keyed on (source hash, entry, function constants, language
  version); (3) the hoist: library + all six PSOs (trace×2, upsample, atrous,
  accumulate, debug×2) into `RtPipelines`, `MetalShadowRayTracer::new` receives it;
  (4) startup prewarm registration for the full RT set; (5) the INV2
  counter-based test.
- **Gate:** Corrosion probe `cold touches during playback: 0` (or a named, explained
  residue escalated to Peter); INV2 test green — and it must fail on pre-hoist code
  (a gate that never saw red proves nothing); `scripts/landing_gate.py` green.
- **Acceptance demo:** the probe log — L2.
- **Result (2026-08-21):** landed. Probe count unchanged at 6 — expected: the
  pre-P1 counter was blind to MSL, so the hoist's win is structural (zero MSL
  compiles during playback with the counter now watching) not numeric. The 6
  decompose: 5 fused chain kernels for unwarmed clip topologies → WARMUP_DESIGN
  P7 (clip-topology enumeration — structure, D4-compliant); 1 value_overlay → P2.
  INV2 red→green: 8 compiles on second tracer construction pre-hoist, 0 post.
- **Forbidden moves:** a second pipeline cache beside the device one; async compile
  with fallback; deleting the translucency two-variant scheme (both variants
  precompile — that is the compliant shape, keep it); warming later clips "just in
  case" (D4 forbids state rehearsal); claiming the gate green before deliverable (1)
  lands (a blind gate passing is not a pass).
- **Test scope:** `-p manifold-gpu -p manifold-renderer` + gpu_proofs_gate.

### P2 — Data-gated prewarm + contract hardening

- **Entry state:** P1 landed; probe at 0 or named residue.
- **Read-back:** D2, section 3.2, INV3.
- **Deliverables:** install-time pipeline creation for every data-gated lazy node —
  inventory by lazy-Option pipeline fields AND data-skip declarations, NOT the skip
  query alone: `rg -n 'get_or_insert|OnceCell|LazyLock|OnceLock' crates/manifold-renderer/src/node_graph/primitives -t rust` plus `rg -n 'data-driven skip|Data-driven skip' crates/manifold-renderer/src -t rust`; the audit-named sites are
  `render_value_overlay.rs:491-508`, `render_scene.rs:2797-2820` (IBL ×3), `:5907`,
  `:7042`, `blob_detect_ffi.rs:352-358,401-406`, `watercolor.rs:127-187`,
  `render_text.rs:369+`, `spawn_from_mesh.rs:187-208`, `hdri_source.rs:320-390`;
  prewarm of every bounded lazy variant set — variable_blur's 6
  (quality × weighting) and MultiBlend's 1..MAX_INPUTS — at install; the INV3
  gpu-proofs extension (install-time hook moving pipeline creation out of `run()`);
  the ADDING_PRIMITIVES authoring rule; nightly probe in `trunk_health.py`.
- **Gate:** the INV3 test constructs every data-skip node on empty data and finds
  its pipelines cached; landing_gate green.
- **Acceptance demo:** none — L1 (no user-visible surface; the probe already covers
  the observable end).
- **Forbidden moves:** special-casing value_overlay only (fix the class); keeping
  any "compile on first evaluate" path.

### P3 — Delete `@static_param` specialization

- **Entry state:** P1–P2 landed.
- **Read-back:** D2's deletion paragraph and its honest-cost measurement requirement.
- **Deliverables:** the specialization path removed: `wgsl_compute.rs:2541-2617`,
  the `Marker::StaticParam` variant (`markers.rs:52-55` — documented as "specialization
  eligibility only — never a correctness dependency", so deletion is
  correctness-safe by construction; the generic kernel is the always-correct
  fallback, `wgsl_compute.rs:2502-2507`), and the freeze emit site in `install.rs`;
  params always uniforms; the fusion golden regenerated (`fused_wgsl_snapshot.txt`
  contains `@static_param` bytes — `UPDATE_FUSION_GOLDEN=1` for
  `fused_wgsl_snapshot_unchanged`); negative gate
  `rg -n 'static_param|SPEC_STABLE_FRAMES|StaticParam' crates/ -t rust` = zero hits; a
  before/after frame-cost measurement on the heaviest fused chains (Bloom +
  Watercolor stack, canonical fixture, `MANIFOLD_RENDER_TRACE=1`) reported in the
  phase notes.
- **Gate:** negative rg zero; landing_gate + gpu_proofs green; frame cost within
  noise or the delta named and accepted by Peter.
- **Acceptance demo:** the measurement table — L1.
- **Forbidden moves:** keeping the path behind a flag; "only specialize at load"
  (values change at runtime — a load-time snapshot is a silent wrong-value bake);
  re-adding value-keyed compilation in any form.

## 6. Decided — do not reopen

1. Zero compiles after warmup is the contract (D1).
2. Specialize on structure only; data flows through uniforms/Options/tables (D2) —
   `@static_param` value-baking is deleted, not gated (P3).
3. PSOs are device-global; instances own data (D3).
4. Warmup enumerates assets, never states (D4).
5. This discipline is the Vulkan-compatible shape (D5).

## 7. Deferred

- **Full bindless RT material tables** — revive when the Vulkan backend lands
  (VULKAN_BACKEND_DESIGN) or if a future kernel genuinely needs content-keyed
  specialization and the design review (section 3.4) accepts it.
- **Shipped-binary PSO precompilation (Metal pipeline archives)** — the device
  already has `load_pipeline_archive` (`metal/device.rs:648`); revive if load-time
  compile cost itself becomes the problem (measured: warmup compile time exceeds
  budget on the canonical fixture). Note the archive only covers the WGSL path until
  P1's cache extension exists.
- **Parallel worker-thread PSO compile during warmup** — the standard load-screen
  technique (no fallback, just concurrency). First lever to pull if warmup compile
  time blows the budget; never during playback.
