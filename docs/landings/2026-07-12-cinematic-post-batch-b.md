# Cinematic post batch B — P1 (coc_from_depth + DoF) + P2 (ssao_from_depth + SSAO) — landed 2026-07-12

**Branch:** feat/cinematic-post → main · **Level reached:** L1 / target L1 (cluster no-PNG rule,
Peter 2026-07-12 — every gate here is numeric, no rendered evidence).
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS · 2026-07-12 · Sonnet 5 · **P0
SHIPPED 2026-07-12 (D7/I6, both layers, docs/landings/2026-07-12-cinematic-post-batch-a.md) —
derived uniforms are first-class on the texture codegen path AND in fused regions. P1+P2 SHIPPED
2026-07-12 (docs/landings/2026-07-12-cinematic-post-batch-b.md) — node.coc_from_depth + DoF slice,
node.ssao_from_depth + SSAO arm, both wired into the CinematicScene preset. P3–P4 not built.**`

## What shipped

**P1 (`4c1090ab`) — `node.coc_from_depth` + DoF slice of `CinematicScene`.** D1's thin-lens CoC
formula (`f_mm`/`A_mm`/`D_mm`/`S_mm`/`coc_mm`/`coc_px`, `WORLD_TO_MM=1000.0`, `SENSOR_H_MM=24.0`)
implemented verbatim as a `Pointwise`/`[CoincidentTexel]` atom reading `fov_y`/`near`/`far`/
`focus_distance`/`f_stop` entirely via P0's derived-uniforms mechanism from the wired Camera's lens
block — the Camera wire never becomes a GPU binding. Output is `coc_px / max_radius`, matching
`node.variable_blur`'s normalized width contract (the doc's VERIFY-AT-IMPL note, resolved and
recorded in the atom's `composition_notes`). `CinematicScene` (new generator preset): Camera →
camera_lens → render_scene (depth wired) → coc_from_depth → variable_blur H → variable_blur V →
out, with `focus_distance`/`f_stop`/`exposure_ev` cards.

Root-cause fix along the way: the texture-domain standalone codegen path
(`generate_standalone_ext`) had no `wgsl_includes` threading at all (only the buffer path did),
so `coc_from_depth`'s use of the shared `depth_common.wgsl` `linearize_depth` helper failed naga
parsing. Fixed by adding an `includes: &[&str]` parameter mirroring the buffer path, threaded from
`standalone_for_spec` via `P::WGSL_INCLUDES`, all 5 call sites updated. The FUSED texture-region
path has the same latent gap — not fixed (not load-bearing for this preset's topology, `coc_from_depth`
never reaches a fusable region here) — logged as **BUG-135** (renumbered from the worker's
original BUG-127, which collided with an unrelated bug independently logged as BUG-127 by a
concurrent session on `origin/main`; see "Deviations" below).

Interpretive call: D1's "`focus_distance`/`f_stop` port-shadowed" reading was ambiguous between an
ordinary port-shadow param and a wire-overrides-lens-fallback (which would need new compiler work
combining two mechanisms that don't currently compose). Resolved by giving `coc_from_depth` no
port-shadowed scalars of its own — it reads both purely via derived_uniforms from the Camera's
lens, and the preset's cards bind directly to `camera_lens` (the actual "one lens" surface).
Zero-new-mechanism, internally consistent, flagged for Peter to confirm if a per-node override
escape hatch is wanted later.

**P2 (`961d367a`) — `node.ssao_from_depth` + SSAO arm.** D3's algorithm implemented verbatim:
view-space position reconstruction from `linearize_depth` + inverse-perspective xy; normal via
`normalize(cross(...))` from explicit ±1-texel `GatherTexel` reads (no derivative intrinsics);
N=16 golden-angle hemisphere samples per D2 (`r_i = sqrt((i+0.5)/16)`, `θ_i = i·2.399963 + hash`,
`hash = fract(sin(dot(px, vec2(12.9898,78.233)))·43758.5453)·2π`), lifted to a hemisphere via
Malley's method (D3's one open point — the doc committed the 2D spiral/hash but not the 2D→3D
lift; this is the standard technique the formula is built for); range-checked occlusion
accumulation with `bias`; `out.r = 1 - intensity·occlusion/N`. The atom does not touch the color
image — `CinematicScene` wires its output through `node.mix` (Multiply) into the color chain
alongside P1's existing DoF chain, per D3's explicit separation. `ssao_intensity`/`ssao_radius`
cards added; `radius`/`intensity`/`bias` are ordinary (non-port-shadowed) atom params — D3 doesn't
call for port-shadowing and the cards bind directly.

Confirmed via `render_scene.wgsl`'s existing PCSS soft-shadow code (same `GOLDEN_ANGLE = 2.399963`
spiral, different rotation source) that D2's spiral formula was already in use elsewhere in the
codebase under a different rotation scheme — used D2's committed hash exactly per the
no-substitution rule rather than reusing PCSS's.

## Gate results (verbatim — independently re-run by the orchestrating session, not self-reported)

P1:
```
$ cargo build -p manifold-renderer / clippy -D warnings / nextest --lib
   Finished, clean. 1127 tests run: 1127 passed, 3 skipped.

$ cargo test -p manifold-renderer --features gpu-proofs
1429 lib passed + 27 gpu_proofs integration passed; 0 failed anywhere.
Named: coc_from_depth::hand_computed_coc::{case_1..case_5} (I3), ::gpu_tests::
generated_coc_matches_hand_kernel (I1), ::gpu_tests::pinhole_dof_chain_is_bit_clean_passthrough (I2),
::gpu_tests::pinhole_f_stop_gives_all_zero_coc_buffer (I2) — all ok.
```

P2:
```
$ cargo build -p manifold-renderer / clippy -D warnings / nextest --lib
   Finished, clean. 1136 tests run: 1136 passed, 3 skipped.

$ cargo test -p manifold-renderer --features gpu-proofs
1441 lib passed + 27 gpu_proofs integration passed; 0 failed anywhere.
Named: ssao_from_depth::analytic_sanity::flat_plane_gives_zero_occlusion_everywhere_except_bias_tolerance,
::gpu_tests::generated_ssao_matches_hand_kernel (I1b), ::gpu_tests::generated_ssao_matches_cpu_reference_on_synthetic_ramp (I1a),
::gpu_tests::generated_ssao_flat_plane_gives_near_full_visibility — all ok.
```

Post-merge-with-origin/main re-verification (merge touched only docs, no renderer code): build,
clippy, and `nextest --lib` re-run clean (1136/1136) after the merge commit `20a81fb4`.

## Deviations from brief

- **BUG-ID collision, fixed at landing:** P1's worker logged BUG-127 for the fused-texture
  `wgsl_includes` gap. Independently, a concurrent session landed on `origin/main` (merge
  `390e7503`, the corpus-hygiene sweep) that had also assigned BUG-127 to an unrelated bug
  (`decode-worker-silent-drop-wedges-export-flush`, cross-referenced from
  `docs/MEDIA_EXPORT_MAP.md`). Both merged into `docs/BUG_BACKLOG.md` cleanly (different line
  ranges, no git conflict) but collided semantically. Renumbered the fused-texture-codegen entry
  to **BUG-135** (next free id after the merge) rather than the decode-worker one, since the
  latter has an external cross-reference and the former doesn't. No other repo references to the
  old BUG-127 label existed for the fused-texture bug (checked `crates/` and `docs/`).
- P1's port-shadowing interpretive call and P2's non-port-shadowed params — both flagged above,
  both judged reasonable, both worth Peter's eyes.

## Shortcuts confessed (rolled up from phase reports)

P1: none. P2: one, documented — the GPU-vs-CPU synthetic-ramp parity test (I1a) tolerates ≤5%
(measured 2.3%) single-sample boundary flips from cross-platform trig rounding on the binary
occlusion threshold, mirroring `FREEZE_COMPILER_MAP.md` §7's "≈1 ulp, not bit-exact" contract for
out-of-loop texture math. The GPU-vs-GPU hand-kernel test (I1b) passes at tight 1e-4, proving the
codegen path itself is exact.

## Verification debt

None opened. BUG-135 (latent fused-texture-region `wgsl_includes` gap) is tracked, not fixed —
not load-bearing for either shipped preset topology; fix shape is written in the backlog entry for
whoever picks it up.

## Bugs logged

BUG-135 (`docs/BUG_BACKLOG.md`) — fused-texture-region codegen never emits `wgsl_includes` (renumbered
from a colliding BUG-127; see Deviations).

## Click-script for Peter (≤2 minutes)

No user-visible surface yet — `CinematicScene`'s DoF+SSAO chain has no UI hookup outside the
graph editor. Numeric proof: `cargo test -p manifold-renderer --features gpu-proofs
node_graph::primitives::coc_from_depth node_graph::primitives::ssao_from_depth` and confirm all
`ok`. P3 (motion blur) completes the preset's atom set; the first real "look at it" moment is
whenever `CinematicScene` gets a perform-surface entry point, which is outside this wave's scope.
