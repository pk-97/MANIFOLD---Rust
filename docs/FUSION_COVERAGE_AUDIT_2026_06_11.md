# Fusion & Optimization Coverage Audit — 2026-06-11

Read-only audit of every silent fail-closed fallback in the freeze compiler, memoization,
data-driven skip, specialization, and dispatch caps. Baseline: commit `1bc34a35` in a
detached worktree (the shared working tree had another session mid-edit in
`freeze/region.rs` / `codegen.rs` / `wgsl_compute.rs`, extending buffer fusion to 3D
texture sampling — those changes are NOT in this audit's baseline, and no tooling was
added to those files). Measurements: `freeze-profile attribute` at 1920x1080, release,
M-series. Full-canvas costs scale roughly 3.5-4x at 4K; particle costs do not scale with
resolution. Per the attribute header, absolute ms run a little above production (split
encoders); per-node shares are the signal.

Method for sweep 1: a `classify_reason` audit mirror of `classify_node`'s exact gate
order plus an all-presets `explain_all_presets` report with a library-wide reason
histogram. Built in the throwaway worktree only (region.rs was contended); diff saved at
`/tmp/fusion_explain_tooling.patch`. Re-deriving it from this section is ~30 min if the
patch is lost. Recommend landing it once region.rs is free — see T1 below.

## Sweep 1 — where every unfused node goes

Histogram over all 46 bundled presets, unfused worker nodes only (sources/final_output
excluded), with classification:

| count | reason (first failing gate) | verdict |
|---|---|---|
| 469 | `fusion_kind` declares Boundary (no contract) | mixed — see breakdown below |
| 56 | eligible but stranded (MIN_REGION_LEN / dropped component) | mostly DELIBERATE |
| 15 | in-loop f16 on a particle loop (region.rs:1085) | DELIBERATE (344374e1, measured) |
| 10 | non-scalar param — `gradient_ramp` Vec3 stops (region.rs:941) | DELIBERATE (memo+256x1 LUT already reclaimed it; measured 0.006 ms) |
| 8 | Texture3D port (region.rs:1009) | KNOWN-GAP — in flight by the concurrent session |
| 4 | resampler `output_canvas_scale != 1:1` (region.rs:993) | KNOWN-GAP (workstream 4 follow-on) |
| 3 | buffer arity (grid_uv_field, array_unpack_vec2, polytope_verts) | DELIBERATE (CPU-by-design / source-shaped) |
| 3 | buffer atom samples 3D texture (region.rs:1216) | KNOWN-GAP — the concurrent session's exact target |
| 3 | texture arity ≠ 1 out (voronoi_2d, block_displace_field) | KNOWN-GAP (multi-output, on the coverage list) |
| 2 | BufferGather (neighbor_smooth, triangulate_grid) | DELIBERATE |
| 2 | `fusion_register_heavy` — apply_radial_burst (region.rs:933) | DELIBERATE gate, but see **S1** |
| 2 | specialization token bound — DoF blurVW quality | KNOWN-GAP (roadmap 4 recompile-on-edit, documented) |

The 469 "no contract" bucket decomposes into: ~330 CPU scalar/control nodes (math,
value, mux_scalar, affine_scalar, triggers, envelopes — zero GPU dispatches, no cost);
~40 CPU array atoms (array_math, generate_range, pack_* — CPU-by-design, measured
µs-level, see sweep 2); ~60 genuine boundary dispatches (scatter/resolve/seed, feedback,
render_lines/text/3d_mesh, DNN/FFI, image_folder); ~25 full-kernel `node.wgsl_compute`
(BlobTracking 7+2 overlays, WireframeDepthGraph 12, BlackHole 4 — wgsl contract v1
covers only fragment-form texture pointwise/source, documented); and **~25 pointwise-
shaped GPU texture atoms that simply never got a fusion contract** — the vocabulary-gap
surprises in S3.

The campaign memory's known-gaps list is CONFIRMED — nothing on it was found to be
wrong, and the per-card numbers reproduce (BlobTracking overlays 0.83 ms at 1080p ≈ the
documented ~5.3 ms at 4K; Watercolor's diffuse region drop is the documented measured
net loss; Bloom is an honest 0-region no-op at 0.197 ms total).

## Ranked surprises (by measured money)

**S1. `apply_radial_burst_to_particles` dispatches full price when idle — ~0.69 ms/frame
each on FluidSimulation AND ParticleText, resolution-independent (~1.4 ms total).**
The comment at `primitives/apply_radial_burst_to_particles.rs:150` says "early-outs when
the burst envelope is ~0", but `run()` has **no CPU-side skip** — it dispatches over
active_count unconditionally; measured 0.688 ms ≈ euler_step's full-work cost, every
frame, burst idle. Whatever per-thread guard the body has, it still pays full particle
buffer traffic. This is comment-vs-behavior drift with real money. Fix: CPU-side dispatch
skip when `amplitude * envelope == 0` — amplitude/envelope are `scalar_or_param` reads,
known before encoding. The node is `aliased_array_io`, so a skip must call
`ctx.mark_gpu_accessed()` (the `@reset_gated` escape hatch, execution.rs stale-output
guard) to declare the in-place buffer intentionally retained. Largest single reclaim
found by this audit; hours of work.

**S2. FluidSim3D's fused buffer region has no dispatch cap — its 0.89x "keep unfused"
verdict is suspect.** Region = container_repel + euler_3d + container_bounds. None of
the 3D particle family declares `fused_dispatch_count_param` (only the 2D five:
euler_step, anti_clump, sample_texture, wrap_torus, simplex_noise_force), so the fused
kernel dispatches at pool CAPACITY — the exact artifact behind the 2D "fused slower
mystery" (2.69→0.68 ms once capped). All three atoms already carry an `active_count`
param; the declaration is a few lines per atom + re-tune. Compounds directly with the
concurrent session's 3D-sampling work (more members → bigger capacity penalty).

**S3. ~25 pointwise-shaped texture atoms have no fusion contract (vocabulary, not
gates).** Measured instances, 1080p:
- `node.affine_transform` — 7 uses (`display_zoom` on every particle generator, Transform,
  StylizedFeedback, ColorCompass). 0.05–0.08 ms each, and it STRANDS its pointwise
  neighbours: ComputeStrangeAttractor's reinhard_tone_map (0.079) → display_zoom (0.051)
  → invert (0.042) would be one region. It's a gather-input pointwise (samples at a
  computed UV — the remap/color_lut shape), already expressible.
- `node.threshold` — HdrBoost prefilter 0.058 ms (would join the existing [gain, mix]
  region); Bloom's has no eligible neighbour (head of a resample chain) — no gain there.
- OilyFluid shading set `lambert_directional` / `matcap_two_tone` / `fresnel_rim` /
  `blinn_specular` — one active per mode; partial mode chains already fuse around them.
- `node.channel_mix` ×2 (StarField — its other 10 atoms fuse into one region already).
- `node.brightness` ×2 (MetallicGlass, 0.15 ms combined) — documented parked (Vec3 param
  + legacy impl); now has a measured cost.
Each is mid-tier (~0.05–0.15 ms/card); collectively ~0.3–0.6 ms across the library.

**S4. A WIRED selector defeats mux branch pruning — BasicShapes renders all 3 shapes
every frame (0.23 ms) though one is shown.** `mux_texture`'s `selected_input_branch`
executor switch only engages when the selector PORT is unwired (mux_texture.rs
composition notes); BasicShapes wires it through trigger logic, so square+diamond+octagon
all dispatch each frame. The selector's producer chain is CPU scalars — its value is
known before encoding, so pruning could key on the live value rather than wiredness.
Related: **the selected-branch passthrough is a full-canvas sampled copy** (OilyFluid
mode_mux 0.092 ms, BasicShapes 0.142 ms, every frame) — the existing skip-alias machinery
(output aliases source) is the shape of a zero-cost fix.

**S5. BlackHole is 1.75 ms/frame, 100% unfused, and absent from the campaign memory.**
1.42 ms in 10 stranded gaussian_blurs (5 H+V pairs — nested-stencil is parked, correctly)
+ 1.12 ms in 4 full wgsl_compute kernels (deflection 0.68 alone). The sky/deflection
subgraph looks static-per-params (changes on camera input only) — the real lever here is
memoization (pure on gaussian_blur + a purity marker for fragment-form wgsl_compute), not
fusion. Only worth a session if the card is in a show.

**S6. Tooling drift in `explain_preset` (zero perf cost, will mislead future sessions).**
Its union-loop replication omits the `!node_is_buffer_atom` guard on texture wires that
`partition_regions` has (region.rs:274), so it prints phantom "COMPONENT ... DROPPED —
buffer member's texture produced inside the region" lines (MetallicGlass [34,49,58]) for
components the real finder never forms — the real partition correctly built [49,58].
Anyone debugging "why didn't X fuse" from that output chases a non-existent drop. Fix
alongside landing the audit tooling (T1). (Also checked: the `install.rs:604`
unreachable-code warning is benign — `#[cfg(test)]` early return, not drift.)

## Sweep 2 — the other optimizers

**Memoization.** Declared pure today: gradient_ramp, affine_scalar, lut1d, math, value,
mux_texture. Verified by full `run()` reads (no time/frame/RNG/carried state; pipeline
fields are caches): **array_math, generate_range, pack_curve_xy, pack_vec4,
consecutive_edges, edges_from_grid_uv, generate_grid_uv, grid_uv_field, uv_field,
mux_scalar, basic_shape, trig_texture** all satisfy the purity contract and don't declare
it. Measured value, however, is small: the CPU array/curve family is **µs-level**
(ConcentricTunnel's entire CPU graph ≈ 5 µs/frame — not worth a session, add `pure:`
opportunistically). The one visible win is `basic_shape` (0.23 ms on BasicShapes,
static between trigger eases) — and it's dominated by the S4 mux fix anyway. The big
memo money is BlackHole's static sky subgraph (S5), which needs pure on gaussian_blur
and a wgsl_compute purity marker — a design step, not a declaration. Verdict: **no
material pure-flag backlog**; the hoisting workstream's Infrared win is not silently
repeating elsewhere except BlackHole.

**Data-driven skip.** `blob_detect_ffi` is still the only `reports_empty_output`
reporter. No new material candidates found: track_persist is correctly excluded
(stateful aging), and every other detector-shaped cost sits in BlobTracking's overlay
kernels, which need the documented passthrough restructure (empty detections → output
aliases source), already a scheduled session. `render_value_overlay` (0.12 ms) should
join that same declaring chain. Verdict: nothing material beyond the planned session.

**Specialization.** Mechanism (install.rs `@static_param` tagging) covers every uniform
field of a fused texture node without a control wire; buffer/particle kernels are
excluded by design (live counts). The over-broad "dynamic" classification is exactly
**control wires from CONSTANT producers**: a `node.value` wired into a fused member's
param shadow (Glitch's amount_value/speed_value) is forever uniform even though it never
moves without an edit. The runtime value-key would make baking them safe; today they
just stay generic. Exact baked-fraction needs a one-line install-time count — add when
region.rs/install.rs are free. DoF measured flat (0.875→0.774 unfused→fused) consistent
with its cost being the blur gather, as documented.

**Dispatch caps.** 2D family complete (5 atoms). 3D family missing entirely — see S2.
Fresh-dst regions correctly keep capacity (unwritten tail garbage).

## Per-card cost table (1080p, release, attribute harness)

| card | unfused | fused | top fused-path residuals |
|---|---|---|---|
| FluidSimulation | 4.410 | 2.222 | burst 0.69 (S1), fused kernel 0.67, scatter 0.35 |
| ParticleText | 5.037 | 2.638 | burst 0.69 (S1), fused kernel 0.65, scatter 0.48, text blurs 0.43 |
| OilyFluid | 3.461 | 1.335 | regions 0.85, feedback 0.28, heightmap_to_normal 0.22, abs+length 0.39 (documented gather-stranded), mode_mux copy 0.09 (S4) |
| MetallicGlass | 3.126 | 2.252 | render_3d_mesh 0.64, triangulate 0.55, heightmap_to_normal 0.65, brightness 0.15 (S3) |
| Watercolor | 1.573 | 1.285 | regions 0.98, luma blur pair 0.29, slope_displace 0.15 |
| DigitalPlants | 2.183 | 2.128 | digital_plants_render monolith 2.63 = 92% (decomposition target, not a fusion gap) |
| BlackHole | 1.747 | — (0 regions) | blurs 1.42, wgsl kernels 1.12 (S5) |
| BlobTracking | 0.969 | — | overlay kernels 0.83 (KNOWN — passthrough session) |
| StarField | 1.183 | 0.394 | healthy 3x |
| VoronoiPrism | 1.076 | 0.200 | healthy 5x |
| Glitch | 1.142 | 0.424 | healthy 2.7x |
| Infrared | 0.054 | — | fully reclaimed by memo+LUT (was 2.1 ms at 4K) |
| Bloom / HdrBoost / DoF | 0.20 / 0.23 / 0.88 | — / 0.17 / 0.77 | small; threshold 0.06 (S3) |

The Liveschool master chain is not reachable headless (GUI fixture); its composition is
covered by the per-card numbers above plus the chain-fusion seam measurements already in
the campaign memory.

## Recommended next sessions, by measured value

1. **Radial-burst CPU-side idle skip** (S1) — ~1.4 ms across FluidSim+ParticleText,
   resolution-independent, small change. Also fix the stale comment.
2. **3D `fused_dispatch_count_param` + re-tune FluidSim3D** (S2) — few lines/atom;
   likely flips the 0.89x verdict; do WITH the in-flight 3D-sampling work.
3. **BlobTracking passthrough restructure** — ~5.3 ms at 4K, already ordered by Peter;
   this audit adds render_value_overlay to its scope.
4. **Mux: live-value branch pruning + selected-input alias** (S4) — ~0.3–0.5 ms across
   BasicShapes/OilyFluid/MriVolume/DoF/Infrared, executor-level, benefits every future
   mux preset.
5. **Vocabulary batch: affine_transform, threshold, channel_mix, shading quartet** (S3)
   — ~0.3–0.6 ms across the library; affine_transform first (it strands neighbours).
6. **BlackHole memo/static-subgraph** (S5) — 1.7 ms, gated on whether the card is used
   on stage.
7. **T1 tooling**: land `classify_reason` + `explain_all_presets`, fix the
   explain_preset union-filter drift (S6), add the install-time specialization
   bake-fraction counter. Patch at `/tmp/fusion_explain_tooling.patch`.

Sweeps that found nothing material: data-driven skip (beyond the planned BlobTracking
session) and the pure-flag backlog (µs-level except basic_shape/BlackHole). The shipped
fused cards (StarField, VoronoiPrism, Glitch, ColorGrade-class) are healthy — no silent
regressions found in what already fuses.
