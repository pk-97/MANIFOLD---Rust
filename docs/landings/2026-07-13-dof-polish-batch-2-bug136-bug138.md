# dof-polish lane, Phase 3+4 (BUG-136 diagnosis + BUG-138 fix) — landed 2026-07-13

**Branch:** feat/dof-polish · **Level reached:** BUG-138: L1 (numeric gate only, per the cluster's
no-PNG-for-atom-work precedent — the atom is no longer in `CinematicScene`'s gated demo chain).
BUG-136: not applicable — no code shipped, this phase's deliverable is a runtime-proven diagnosis
and an explicit escalation, not a fix.
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — P0–P4 SHIPPED · Sonnet 5 · ...
BUG-138 (blockiness) FIXED 2026-07-13 in `node.variable_blur` itself (scales sub-tap density with
CoC radius above an 8px `step_size` threshold, byte-identical below it) — the atom is no longer in
`CinematicScene`'s chain but remains user-wireable elsewhere. BUG-136 (motion blur no visible
effect) — the dof-polish lane ran both committed runtime probes (shutter_angle at uniform-pack
time, a velocity texel during a headless orbit) against the shipped `CinematicScene` graph: both
check out clean every frame, and a `shutter=0` vs `shutter=181.05` headless render diff shows a
real shader-level visual delta. This exonerates the graph wiring, shader math, matrix bookkeeping,
derived-uniform packing, and velocity buffer end to end — the bug does not reproduce headlessly.
**ESCALATED, not fixed:** the remaining suspects (UI slider-drag propagation cadence into the
content-thread graph; whether the render loop ticks continuously outside active playback) live
entirely in the live app's interactive layer, which this lane's headless workers cannot observe —
needs either a live repro session with Peter or a design decision on which layer to instrument.
P5/P6 open.**`

## Gate results (verbatim)

BUG-138 fix, independently re-run by the orchestrating session in the worktree:
```
$ cargo test -p manifold-renderer --features gpu-proofs variable_width
test node_graph::primitives::gaussian_blur_variable_width::tests::bug_138_small_radius_stays_at_the_original_fixed_tap_count ... ok
test node_graph::primitives::gaussian_blur_variable_width::tests::bug_138_large_radius_scales_tap_count_above_the_old_fixed_ceiling ... ok
test node_graph::primitives::gaussian_blur_variable_width::tests::bug_138_subtap_count_is_monotonic_non_decreasing_in_step_size ... ok
test node_graph::primitives::gaussian_blur_variable_width::tests::gaussian_blur_variable_width_declares_two_texture_inputs_and_one_output ... ok
test node_graph::primitives::gaussian_blur_variable_width::tests::primitive_registers_as_palette_atom ... ok
test node_graph::primitives::gaussian_blur_variable_width::tests::gaussian_blur_variable_width_has_axis_radius_quality_weighting_params ... ok
test node_graph::freeze::codegen::gpu_tests::generated_gaussian_blur_variable_width_matches_original ... ok
test node_graph::freeze::proof::fused_variable_width_blur_matches_unfused ... ok
test result: ok. 8 passed; 0 failed

$ cargo clippy -p manifold-renderer -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.15s   (clean)

$ cargo run -p manifold-renderer --bin check-presets
57 presets: 57 ok, 0 failed (0.28s)

$ cargo test -p manifold-renderer --features gpu-proofs   (full sweep)
1466 passed; 0 failed; 21 ignored  (lib) + 27 passed gpu_proofs binary + all doctests — 0 FAILED lines anywhere

$ cargo nextest run -p manifold-renderer --lib
1153 passed, 3 skipped, 0 failed
```

BUG-136 diagnosis — no code changed, no build/test gate applies. The evidence IS the gate:
```
BUG136-RS view_proj_delta_sum=0.073722 velocity_wired=true
BUG136-RS velocity_center_texel=(9.25e-5, 5.16e-5) nonzero_texels=7103 max_mag=0.0102 max_at=(97,130)
BUG136-MB run() shutter_angle=181.05
```
30/30 frames, both probe values clean; `node.motion_blur` used its standalone codegen path 30/30
times (0 fused-recompute calls, ruling out original suspect 2 — fused-vs-standalone routing —
outright, since Gather atoms structurally never fuse with their producer); a `shutter=0` vs
`shutter=181.05` full headless render diff at 640x360/30 frames showed a real visual delta
(`ImageChops.difference` bbox `(188,116,478,293)`, max channel delta 7/255, nonzero mean).

Landing sweep, run in the warm main checkout before push:
```
$ python3 scripts/gen_docs_index.py
Wrote docs/README.md — 153 docs indexed. (no changes)

$ cargo clippy --workspace -- -D warnings
(clean; only pre-existing AVFoundation deprecation warnings, unrelated)

$ cargo nextest run --workspace
(see verbatim summary line in the push step below)

$ cargo deny check bans
bans ok

$ python3 .claude/hooks/bug_status.py --check
bug-backlog status: clean
```

## Deviations from brief

- **BUG-136 was not fixed — this was the expected/correct outcome, not a shortfall.** The phase
  brief explicitly named "escalate if structural" as a valid exit. The worker's diagnosis is
  runtime-evidenced (not re-derived from the existing static-read addendum) and independently
  spot-checked by the orchestrating session: no leftover debug instrumentation, no scratch files,
  clean `git status`, and the addendum text matches the worker's self-report verbatim.
- **BUG-138's fix shape ("densify sub-taps above an 8px step_size threshold, capped at 4x") is the
  literal fallback the bug's own entry named ("scale tap count with radius"), not a deviation.**
  The worker surveyed `node.gaussian_blur`'s existing adaptive-radius analytic Gaussian
  (`sg_blur_linear`) per the mandated audit and explicitly rejected reusing it wholesale — it
  lacks `variable_blur`'s per-tap CoC weighting, and re-deriving analytic weights would itself
  have been the "fancier invented algorithm" the brief said to avoid. Recorded here since it's a
  real alternative a future session might otherwise re-propose.
- Neither phase touched `CinematicScene`, `coc_dilate`, or `bokeh_gather` — confirmed via each
  worker's own file-change list and a `git diff --stat` review before landing.

## Shortcuts confessed (rolled up from phase reports)

None from either phase worker.

## Verification debt

None opened. BUG-136 stays OPEN in the backlog with its full runtime-probe evidence — it is
pre-existing debt (not new), now narrowed to a specific layer (live UI/scheduling) instead of
three undifferentiated suspects. Closing it requires a live app session, out of scope for a
headless orchestration lane.

## Click-script for Peter (≤2 minutes)

1. Wire `node.variable_blur` into any preset/graph with a large CoC/width input (e.g. push
   `max_radius` to 64px, quality High) — expect: smoother falloff in heavily out-of-focus areas,
   no discrete-ring/blocky artifacts, vs. how it looked before this landing.
2. For BUG-136 — this needs YOU, live: open `CinematicScene` (or any project with the motion-blur
   chain) in the running app, set `lens.shutter_angle` well above 0 (e.g. 181) via the card, and
   orbit the camera by dragging. Watch for visible smear. If there's still none, that confirms the
   bug lives in the UI-drag-to-content-thread propagation path or the render-loop-tick-while-idle
   path — the two suspects this landing narrowed it to — and whichever one turns out to be the
   cause is the next lane's starting point.
