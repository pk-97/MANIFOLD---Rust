# dof-polish lane, Phase 1+2 (node.coc_dilate + CINEMATIC_POST P4 bokeh_gather) — landed 2026-07-13 @ 2f6f55c8

**Branch:** feat/dof-polish · **Level reached:** L2 (before/after PNG looked at by the orchestrating session; Peter's own confirmation on a richer depth-discontinuity scene is still owed) / target L2 per the amended CINEMATIC_POST demo rule
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — P0–P4 SHIPPED · Sonnet 5 · ... **P4 SHIPPED 2026-07-13 (Sonnet 5, `dof-polish` worktree/branch `feat/dof-polish`)** — BUG-137's `node.coc_dilate` (standalone neighborhood-max atom) landed first, then `node.bokeh_gather` (D5's 32-tap occlusion-aware disc gather) replaced the two `variable_blur` H/V nodes, still consuming `coc_dilate`'s dilated CoC. `CinematicScene` now runs the full DoF(dilated+bokeh)+SSAO+motion-blur chain. Orchestrator before/after PNG look-pass (see BUG-137) showed the silhouette-bleed halo visibly gone; Peter's own confirmation on a richer depth-discontinuity scene is still owed. BUG-138 (blockiness) left OPEN — P4's effect on it wasn't isolated by a dedicated large-CoC-radius look. P5/P6 open; BUG-136 (motion blur no visible effect) is this lane's next phase.**`

## Gate results (verbatim)

Phase 1 (`node.coc_dilate`), independently re-run by the orchestrating session in the worktree:
```
$ cargo test -p manifold-renderer --features gpu-proofs coc_dilate
test node_graph::primitives::coc_dilate::tests::has_no_params ... ok
test node_graph::primitives::coc_dilate::tests::primitive_registers_as_palette_atom ... ok
test node_graph::primitives::coc_dilate::tests::declares_single_texture_input_and_output ... ok
test node_graph::primitives::coc_dilate::gpu_tests::flat_field_dilate_is_a_no_op ... ok
test node_graph::primitives::coc_dilate::gpu_tests::generated_dilate_matches_hand_kernel_and_cpu_reference ... ok
test result: ok. 5 passed; 0 failed

$ rg 'create_compute_pipeline\(include_str' crates/manifold-renderer/src/node_graph/primitives/coc_dilate.rs
(0 hits, exit 1)

$ cargo clippy -p manifold-renderer -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.11s   (clean)

$ cargo run -p manifold-renderer --bin check-presets
57 presets: 57 ok, 0 failed (0.24s)
```

Phase 2 (`node.bokeh_gather`), independently re-run:
```
$ cargo test -p manifold-renderer --features gpu-proofs bokeh_gather
test node_graph::primitives::bokeh_gather::tests::uniform_struct_is_16_bytes ... ok
test node_graph::primitives::bokeh_gather::tests::primitive_registers_as_palette_atom ... ok
test node_graph::primitives::bokeh_gather::tests::has_max_radius_param_only ... ok
test node_graph::primitives::bokeh_gather::tests::declares_in_width_inputs_and_texture_output ... ok
test node_graph::primitives::bokeh_gather::gpu_tests::generated_bokeh_gather_matches_cpu_reference_on_synthetic_fixture ... ok
test node_graph::primitives::bokeh_gather::gpu_tests::generated_bokeh_gather_matches_hand_kernel ... ok
test node_graph::primitives::bokeh_gather::gpu_tests::zero_coc_is_bit_clean_passthrough ... ok
test result: ok. 7 passed; 0 failed

$ rg 'create_compute_pipeline\(include_str' crates/manifold-renderer/src/node_graph/primitives/bokeh_gather.rs
(0 hits, exit 1)

$ cargo clippy -p manifold-renderer -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.15s   (clean)

$ cargo run -p manifold-renderer --bin check-presets
57 presets: 57 ok, 0 failed (0.23s)
```

Landing sweep, run in the warm main checkout before push:
```
$ python3 scripts/gen_docs_index.py
Wrote docs/README.md — 153 docs indexed. (no changes — docs edited, not added/renamed)

$ cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 39.31s   (clean; only 2 pre-existing
AVFoundation deprecation warnings in manifold-media native code, unrelated to this lane)

$ cargo nextest run --workspace
Summary [14.322s] 3160 tests run: 3160 passed, 8 skipped

$ cargo deny check bans
bans ok

$ cargo test -p manifold-renderer --features gpu-proofs   (full sweep, blast-radius confirmation)
all passed, 0 failed (lib + gpu_proofs integration binary + doctests)

$ python3 .claude/hooks/bug_status.py --check
bug-backlog status: clean   (after a follow-up commit — see Deviations)
```

## Deviations from brief

- **BUG-137's status line needed a second commit.** The first landing merge triggered the
  post-merge housekeeper: `BUG-137: status FIXED but filed under ## Open (should be ## Fixed)` and
  `BUG-137: resolved but still listed in the open-bug index`. Fixed by running the worktree's own
  `bug_status.py --write` (main refuses `--write`) to reflow the entry into `## Fixed`, then
  manually removing its now-stale row from the hand-authored "Index of open bugs" table (a
  markdown table `bug_status.py` reads but doesn't rewrite). Committed as a follow-up
  (`8cec204e`) and merged in the same landing before push — no drift reached origin/main.
- **BUG-137 marked FIXED, not left OPEN**, deviating from both phase workers' own conservative
  "left OPEN pending Peter's look-pass" language. Rationale: the orchestrating session (this
  session) DID run the before/after PNG look-pass the workers explicitly deferred, and it showed
  the silhouette-bleed halo BUG-137 names is visibly gone. Status is `FIXED 2026-07-13, pending
  Peter's confirmation on a real depth-discontinuity scene` — matching the precedent set by
  BUG-119 in the `scene-ladder-state` memory (fixed-but-owed-a-look, not reopened-as-open). The
  test scene (`CinematicScene`, one flat mesh) doesn't have a true foreground/background depth
  split, so this is an honest partial confirmation, not a full close.
- **BUG-138 deliberately left OPEN**, not claimed fixed "by construction." No dedicated
  large-CoC-radius (e.g. 64px) test was run to isolate the blockiness improvement bokeh_gather is
  expected to deliver — the design doc itself only speculates "likely." Left for a future
  dedicated look or Peter's own observation.
- The bokeh_gather CPU-reference parity test's tolerance (Phase 2 worker) was loosened from a
  flat 1e-4 to a per-texel-flip-tolerant / mean-error-bound pair, following the exact precedent
  already in `ssao_from_depth.rs`'s `boundary_flip_count` pattern (verified to exist, not
  invented) — cross-shader-compile trig ULP variance near the `step()` occlusion threshold, not
  an algorithm bug. Confirmed via a control fixture forcing every tap's weight to 1 (the threshold
  can never flip) still showing the same divergence.

## Shortcuts confessed (rolled up from phase reports)

None from either phase worker. Orchestrator's own PNG rendering used a temporary before/after
swap of `CinematicScene.json` via `git show <prior-commit>:<path>` → render → restore; confirmed
`git status` clean on the preset file after each render, no stray diff reached the commit.

## Verification debt

None opened. BUG-137's remaining gap (Peter's own look on a real depth-discontinuity scene) and
BUG-138 (blockiness, not isolated) are tracked as their own backlog entries, not VD entries — they
are pre-existing open bugs, not new debt introduced by this landing.

## Click-script for Peter (≤2 minutes)

1. Open `CinematicScene` (or any preset/project using `node.coc_dilate` → `node.bokeh_gather`) —
   expect: it loads and renders without error, same as before this landing.
2. Look at a scene with real depth separation (e.g. `SceneLadders.manifold` or any multi-object
   scene with the DoF chain wired) with `f_stop` set low enough to defocus the background —
   expect: no hard seam/ring artifacts at the edge of an in-focus foreground object against a
   blurred background (BUG-137's fix); smoother, less blocky falloff in heavily out-of-focus areas
   vs. the old two-pass gaussian (BUG-138's hoped-for side effect, unconfirmed).
3. If either still looks wrong, that's the live confirmation this landing is pending — flag back
   to close BUG-137's Status line fully or reopen with specifics.
