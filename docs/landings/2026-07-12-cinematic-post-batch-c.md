# Cinematic post batch C — P3 (node.motion_blur tail) — landed 2026-07-12

**Branch:** feat/cinematic-post → main · **Level reached:** L1 / target L1 (cluster no-PNG rule).
**Doc status line (quoted verbatim):** `**Status:** P0–P3 SHIPPED 2026-07-12 · Sonnet 5 · **P0
(D7/I6, both layers, docs/landings/2026-07-12-cinematic-post-batch-a.md) — derived uniforms are
first-class on the texture codegen path AND in fused regions. P1+P2
(docs/landings/2026-07-12-cinematic-post-batch-b.md) — node.coc_from_depth + DoF slice,
node.ssao_from_depth + SSAO arm. P3 (docs/landings/2026-07-12-cinematic-post-batch-c.md) —
node.motion_blur tail. CinematicScene now runs the full DoF+SSAO+motion-blur chain. P4
(node.bokeh_gather swap) is pre-approved but deliberately not run this wave — triggered whenever
Peter wants bokeh over gaussian.**`

## What shipped

**P3 (`0a53043d`) — `node.motion_blur` + the `CinematicScene` tail.** D4's velocity-directed
gather implemented verbatim: `smear_px = velocity_ndc · 0.5 · viewport · (shutter_angle/360)`,
clamped component-wise to `±max_blur_px`; 8 equal-weight taps (N fixed as a WGSL `const`, never a
runtime param) evenly spaced (inclusive endpoints) across `uv − smear_uv/2 .. uv + smear_uv/2`.
`shutter_angle = 0` collapses all 8 taps to the same texel — bit-exact pass-through of the color
input, proven by both a CPU analytic derivation and a GPU byte-compare (I2). `input_access:
[Gather, CoincidentTexel]`: the color input (`in`) uses `Gather` (bilinear, body-computed tap
coordinate — a smear should filter across neighbours), the `velocity` input uses `CoincidentTexel`
(exact integer own-texel load — a directional vector must never be blended with a neighbour's,
that would corrupt the smear direction). `CinematicScene` gained its motion-blur tail: after P2's
SSAO-composited color, `render_scene`'s velocity output + the color signal feed `node.motion_blur`,
Camera-wired for the `shutter_angle` fallback, producing the preset's final output — the full
DoF + SSAO + motion-blur chain the doc's D6 committed to.

Velocity buffer confirmed in code, not doc prose: `node.render_scene`'s `velocity` output port
(`render_scene.rs:169`), `Rg16Float`, lazy-costs-nothing-unless-wired, computed as
`(clip_now.xy/clip_now.w) − (clip_prev.xy/clip_prev.w)` — camera + rigid-object NDC delta
(`render_scene.rs:680`), matching GBUFFER D5's rigid-only honesty this doc already accepted.

Interpretive call, third instance of the same pattern this wave: D4's text says `shutter_angle` is
"port-shadowed override" — plainer language than P1/P2's ambiguous phrasing, but still resolved
the same way. The worker verified in code (not assumed) that no atom in the codebase combines an
ordinary port-shadow param with a derived-uniforms Camera-fallback on one field; `coc_from_depth`
and `ssao_from_depth` both read lens/view fields purely via derived_uniforms. Rather than build
that combined mechanism mid-atom-phase, `node.motion_blur` has no port-shadowed `shutter_angle` of
its own — it reads it entirely via derived_uniforms from the wired Camera, and the preset's
`shutter_angle` card binds directly to `camera_lens`, which already has a real, working
port-shadowed `shutter_angle` param (confirmed live, with a doc comment naming this exact future
use). Same zero-new-mechanism shape as P1's `focus_distance`/`f_stop` and consistent with it —
three phases independently converging on "cards bind to `camera_lens`, atoms read via
derived_uniforms" is worth Peter confirming as the actual pattern rather than three coincidental
workarounds.

## Gate results (verbatim — independently re-run by the orchestrating session, not self-reported)

```
$ cargo build -p manifold-renderer / clippy -p manifold-renderer -- -D warnings / nextest --lib
   Finished, clean (0 warnings; a stale rust-analyzer diagnostics snapshot briefly showed dead_code
   warnings on motion_blur.rs — force-recompiled with `touch` to confirm they don't survive a real
   build, same false-positive pattern hit during P0 and P1 this wave).
   1143 tests run: 1143 passed, 3 skipped.

$ cargo test -p manifold-renderer --features gpu-proofs
1451 lib passed + 27 gpu_proofs integration passed; 0 failed anywhere.
Named: motion_blur::analytic_sanity::zero_shutter_angle_collapses_every_tap_to_the_center_texel (I2),
::gpu_tests::generated_motion_blur_matches_cpu_reference_on_synthetic_ramp (I1a),
::gpu_tests::generated_motion_blur_matches_hand_kernel (I1b),
::gpu_tests::zero_shutter_angle_is_bit_clean_passthrough (I2) — all ok.
```

## Deviations from brief

- Port-shadowing resolution (see "What shipped" above) — explicitly anticipated by the brief's
  escalation gate, resolved with code-verified evidence, consistent with P1/P2's precedent.
- No BUG-ID collisions this landing (the BUG-135 renumber from batch B already resolved the only
  one this wave hit).

## Shortcuts confessed

None.

## Verification debt

None opened. BUG-135 (fused-texture-region `wgsl_includes` gap, logged at batch B) remains open,
untouched by this phase — `motion_blur` never reaches a fusable texture region in this preset's
topology either (same reasoning as `coc_from_depth`: its neighbors are a `Boundary` render pass
upstream and the preset's final output downstream).

## Bugs logged

None new this landing.

## Click-script for Peter (≤2 minutes)

No user-visible surface yet (no perform-surface entry point for `CinematicScene` — outside this
wave's scope). Numeric proof: `cargo test -p manifold-renderer --features gpu-proofs
node_graph::primitives::motion_blur` and confirm all `ok`, or run `cargo test -p manifold-renderer
--features gpu-proofs --test gpu_proofs smoke::every_registered_generator_runs_without_panicking_or_nans`
to confirm the full `CinematicScene` (DoF + SSAO + motion blur, all three arms) loads and renders
one frame cleanly. The wave's first real "look at it" moment is whenever `CinematicScene` gets
wired to a perform-surface card layout — P4 (bokeh swap) is pre-approved and waiting whenever
that's wanted.
