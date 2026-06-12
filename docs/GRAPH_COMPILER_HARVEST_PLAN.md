# Graph Compiler — Harvest Sprint Plan

Six measured, mechanical items that close out the graph compiler perf campaign.
Combined expected reclaim ~5–8 ms of the ~22 ms Liveschool 4K frame → locked 4K60
with headroom. After this sprint the compiler is "done" for the current goal; the
deep-layer items (multi-queue, temporal scheduling, cost-model regions, e-graphs,
precision inference) are next-gen targets and explicitly OUT of scope here.

Heap aliasing is also OUT: it buys memory headroom not FPS, needs a lifetime-interval
packer + fence handling, and we parked it until memory pressure is the live constraint.

Source measurements: `docs/FUSION_COVERAGE_AUDIT_2026_06_11.md` (per-card table +
ranked surprises S1–S6). All "measured" numbers below are 1080p attribute-harness
unless marked 4K.

## Sprint rules

- **One commit per item, each with its own focused proof** (parity test or
  `freeze-profile attribute` before/after). The chain-fusion harvest "looked done"
  three times and each time an on-stage repro found a bug a bulk suite missed —
  do not batch unverified items.
- One full battery at the END of the sprint (workspace clippy, full
  `-p manifold-renderer --lib`, check-presets, bundled_presets), not per item.
  Known pre-existing fails (NOT yours): WireframeDepthGraph 42×42 blit,
  temporal feedback_seed frame-1, lut1d white_hot parity, plugin_prewarm DoF,
  user_binding reshape, liveschool Ableton mapping, catalog drift. Freeze/GPU
  proof tests FLAKE under suite parallelism — re-run isolated before believing
  a failure.
- After any preset JSON edit: `cargo run -p manifold-renderer --bin check-presets`,
  then `cargo test -p manifold-renderer --lib bundled_presets` (check-presets does
  not execute a GPU frame).
- Re-tune verdicts with `freeze-profile tune` after anything that changes fused
  kernel cost (items 2, 3).
- Model tiers: items 1–4 are Opus 4.8 Low territory (pattern-following against
  existing mechanisms with existing proofs to copy). Items 5–6 touch the executor
  skip machinery and the segment/harvest pipeline — run at medium or higher.
- Order is cheapest-first so the sprint banks wins early.

---

## 1. Radial-burst idle skip (S1) — ~1.4 ms, resolution-independent

**What:** `node.apply_radial_burst_to_particles` (and its 3D sibling) dispatches
full price every frame even when the burst is idle — 0.688 ms each on
FluidSimulation AND ParticleText. The comment at
`primitives/apply_radial_burst_to_particles.rs:150` claims an early-out that
does not exist CPU-side.

**Fix:** in `run()`, skip the dispatch when `amplitude * envelope == 0`. Both are
`scalar_or_param` reads, known before encoding. Fix the stale comment. Apply the
same skip to `apply_radial_burst_3d_to_particles`.

**Trap (the one that matters):** the node is `aliased_array_io` — the executor's
stale-output guard (execution.rs, debug_assert ~line 680) forbids an aliased node
skipping its dispatch. On skip you MUST call `ctx.mark_gpu_accessed()` to declare
the in-place buffer intentionally retained. This is the exact `@reset_gated`
pattern from commit ffa4d82b — copy it.

**Proof:** copy the seed-gate test shape (`particletext/fluidsim3d_seed_gate_matches_ungated`):
idle burst renders bit-identical with and without the skip, and the skip doesn't
trip the guard. Then `freeze-profile attribute FluidSimulation` before/after —
expect the 0.69 ms burst line to drop to ~0.

## 2. FluidSim3D dispatch caps (S2) — 1–2 ms on-card if the verdict flips

**What:** FluidSim3D's fused buffer region (container_repel + euler_3d +
container_bounds) dispatches at pool CAPACITY because none of the 3D particle
family declares `fused_dispatch_count_param`. Its 0.89x "keep unfused" verdict
is the same artifact as the 2D fused-slower mystery (2.69→0.68 ms once capped).

**Fix:** declare `fused_dispatch_count_param` = "active_count" on the 3D family
(container_repel_force_3d, euler_step_particles_3d, container_bounds_3d — check
the rest of the 3D atoms in the region). All three already carry an
`active_count` param; the 2D five (euler_step, anti_clump, sample_texture,
wrap_torus, simplex_noise_force) are the exact template. Then re-tune.

**Trap:** install requires ALL in-place-loop members of a region to agree on ONE
producer wire for the count; fresh-dst regions correctly keep capacity (unwritten
tail is garbage) — don't "fix" that.

**Proof:** the fluidsim oracle pattern asserts the `// @dispatch_count_param:`
marker lands in the fused WGSL; `freeze-profile tune` re-run — expect the 0.89x
verdict to flip to FUSE. If it doesn't flip, the verdict is now honest data —
stop, don't force it.

## 3. Vocabulary batch (S3) — ~0.3–0.6 ms library-wide

**What:** ~25 pointwise-shaped texture atoms have no fusion contract (no
`fusion_kind`/`wgsl_body`), so they're boundaries that strand their neighbours.
Ranked by measured money:

1. `node.affine_transform` — 7 uses, 0.05–0.08 ms each, AND it strands
   neighbours (ComputeStrangeAttractor: tone_map → display_zoom → invert would
   be one region). It samples at a computed UV — the remap/color_lut
   gather-input-pointwise shape, already expressible. Do this one first.
2. `node.threshold` — HdrBoost prefilter 0.058 ms (joins the existing
   [gain, mix] region). Bloom's instance has no eligible neighbour — no gain
   there, don't chase it.
3. OilyFluid shading quartet — lambert_directional / matcap_two_tone /
   fresnel_rim / blinn_specular (one active per mode).
4. `node.channel_mix` ×2 (StarField).
5. `node.brightness` — SKIP: Vec3 param (fused codegen lacks Vec3) + legacy
   hand impl; documented parked. Don't unpark it inside a batch sprint.

**Fix per atom:** add `wgsl_body` + fusion classification, same single-source
cutover pattern as the ~110 already-converted atoms (see
`docs/ADDING_PRIMITIVES.md` + any recent converted atom as template). Match the
hand shader's exact arithmetic form — mix()/fma can differ by a ULP from the
explicit form (the lerp_instance_fields lesson).

**Proof:** per-atom standalone codegen parity oracle (the established pattern);
visual/temporal effects skip GPU parity per the standing rule — check-presets +
bundled_presets covers the rest. Re-tune affected cards. NO per-atom test for
one-line bodies (feedback_dont_test_stdlib_or_one_line_shaders).

## 4. BlackHole memo (S5) — 1.7 ms, card-local

**What:** BlackHole is 1.75 ms/frame, 100% unfused: 1.42 ms in 10 stranded
gaussian_blurs + 1.12 ms in 4 full wgsl_compute kernels. The sky/deflection
subgraph is static-per-params (changes on camera input only). The lever is
MEMOIZATION, not fusion — nested-stencil (blur→blur) is parked, correctly.

**Fix:** (a) `pure:` on `node.gaussian_blur` (read run() first to verify the
purity contract — no time/frame/RNG/carried state; pipeline fields are caches);
(b) a purity marker for fragment-form `node.wgsl_compute` (a `// @pure` source
marker, parallel to `// @fusion:`) so the deflection kernels join the HOISTABLE
closure. (b) is a small design step — the marker must only apply to fragment-form
sources, never full kernels with state.

**Proof:** the Infrared hoisting tests are the template (e278aaa0 + 544f1c55);
`freeze-profile attribute BlackHole` — expect the static subgraph to drop to ~0
after frame 1, and a param/camera change to re-render exactly once.

## 5. Mux pruning + passthrough alias (S4) — ~1–2 ms at 4K (executor-level; medium+ model)

**What:** two related costs on `node.mux_texture`:
(a) a WIRED selector defeats branch pruning — `selected_input_branch` only
engages when the selector port is UNWIRED; BasicShapes wires it through trigger
logic, so all 3 shapes render every frame (0.23 ms).
(b) the selected-branch passthrough is a full-canvas sampled COPY (OilyFluid
0.092, BasicShapes 0.142 ms, every frame).

**Fix:** (a) the selector's producer chain is CPU scalars — its value is known
before encoding; key pruning on the LIVE resolved value instead of wiredness.
(b) when the mux output format/dims match the selected input, alias output to
source via the existing skip-alias machinery (the draw-atom
`skip_passthrough_ports` shape, commits ccdcf093+552e8b44 on this branch).

**Traps:** live-value pruning must re-evaluate every frame (the selector can be
beat-driven — switching shapes mid-bar is the performance use case; a stale
pruned branch = wrong shape on stage). Mux is a ROUTER and must stay a fusion
boundary — don't convert it. Deliberately NOT shrinking/aliasing where consumers
do texel-exact reads at mux dims (MriVolume mux→sharpen neighborhood reads,
DoF mux→invert): alias only when dims+format are identical, sampled-copy
otherwise. TEST FIXTURE GOTCHA: a skip-alias test graph needs a consumer on the
node's out or the planner never allocates the slot and the alias silently falls
through to evaluate.

**Proof:** BasicShapes renders bit-identical with one shape's dispatches gone
(attribute shows square/diamond/octagon → only selected); selector flip
mid-render switches branches with no stale frame; OilyFluid mode_mux copy line
drops to ~0.

## 6. Chain-fusion remaining legs (~0.17 ms/seam + first-seconds correctness; medium+ / Fable-tier)

The four open legs from the chain-fusion build (branch history: `chain-fusion`,
8 commits; design doc `docs/CHAIN_FUSION_DESIGN.md`):

1. **Project-load prewarm** — segments today compile on first dispatch; the
   first seconds of a show render per-card (fallback covers correctness, not
   perf). Enqueue segment compiles for the loaded arrangement's chains at
   project load. This is the SHOW-CRITICAL leg — first scene of a gig.
2. **Per-card tuned region-mask seeding** — feed each card's greedy gate
   region-mask result into segment partitions (worker currently fuses
   mask-less; the segment gate decides holistically).
3. **Greedy region masks for segments** — the live-edit gate is currently
   fused-all vs baseline; bring the leave-one-out mask search to segments.
4. **Generator→first-card seam** — crosses the generator/effect runtimes; the
   biggest per-seam win left but the most plumbing. If it balloons, cut it and
   ship legs 1–3; it can be its own session.

**Traps (all learned the hard way, all have repro history):** gate baseline is
concat of per-card FUSED defs, not raw atoms (first cut overstated 3.42x vs
honest 1.28x). Harvest gates are load-bearing: membership gate (card-set change
= reset), upstream-prefix gate (reorder before a stateful card = reset that
card), shadow-slot rule (persistent textures MOVE via take_render_target, never
replace_texture_2d — borrowed shadows freeze ping-pong). **Harvest proofs must
run ≥3 post-rebuild frames** — a frozen ping-pong looks correct on frame 1.
cfg(test) never enqueues the worker; use seed_segment_cache_for_test.

**Proof:** per-leg freeze tests in the established segment suite +
`freeze-profile chain` seam measurements; prewarm proven by a load-then-
first-dispatch test asserting the segment cache is warm.

---

## End-of-sprint battery

1. `cargo clippy --workspace -- -D warnings`
2. Full `cargo test -p manifold-renderer --lib` (compare against the known
   pre-existing fail list above; re-run any freeze/GPU failure isolated)
3. `check-presets` (46/46) + `bundled_presets`
4. `freeze-profile tune` — verdict diff vs pre-sprint; `freeze-profile
   attribute` on FluidSimulation, ParticleText, OilyFluid, BasicShapes,
   BlackHole, FluidSim3D
5. Record before/after per-card table in this doc.

## Peter's visual pass (one batch, end of sprint)

- FluidSimulation + ParticleText: burst still fires correctly when triggered
  (idle skip must not eat the attack frame)
- BasicShapes: shape switching live, beat-driven selector
- OilyFluid: mode switching across all four shading modes
- BlackHole: camera move re-renders the sky correctly
- Carry-over still pending from previous sessions: Draw-atom BlobTracking
  rebuild, Infrared 256×1 ramp, FluidSimulation.json wire edit
