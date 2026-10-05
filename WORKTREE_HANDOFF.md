# BUG-lxxl (coarse solve) — stage 3 handoff

Slot: /Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-5
Branch: feat/lentine-coarse-solve
Verified base/unchanged HEAD: facab5e87281e6c7a32da5a016cd8131e0fc6586

Stage 2 is committed; the lead's component_coarse_matches_reference GPU run
PASSED. Stage 3 is implemented as uncommitted edits. No git writes, pushes,
GPU tests, app runs or file deletions occurred. The lead owns verification.
Keep this active slot; do not retire it from this worker handoff.

The standalone gpu_flip_lentine::ComponentProjection owns ComponentSolver and
adds alpha-zero boundary velocity scatter, direct weighted local Cholesky
projection, true fine-cell residual reporting and explicit failure publication.
Each block component has a lowest-cell gauge. Local correction preserves outer
and anchor velocities bitwise. Closed/dry/missing faces retain input values.
The step, pressure solver and their shaders remain byte-identical to HEAD;
both Solve Level paths are unchanged. Stage 4 belongs to the integration lane.

Units remain integrated fine flux at h=1, with the reference potential sign.
Velocity vec4 records match links: positive-axis xyz and outward aggregate
explicit-anchor w. This standalone layout is not the step's face layout or
stage-5 mixed-surface treatment. Low walls belong in the complete source.
Caller input/output aliasing is forbidden. Storage including stage 2 is
120 * cell_count + 80 bytes, excluding caller inputs. No per-encode allocation,
CPU graph construction or readback is added; performance is unmeasured.

Local RHS uses actual represented scatter deltas and complete moving-solid /
prescribed sources. Incompatibility, invalid data and nonpositive/nonfinite
pivots have explicit statuses and poison all stage-3 outputs. No mean removal,
pivot floors, fallbacks or new iteration/resolution/quality caps are added.
Non-converged coarse status propagates; stage 2 retains its existing CG budget.

The new component_projection_matches_reference proof invokes the f64 reference
with --gpu-projection-fixtures. Ten cases compare boundary/final velocities,
local pressures/gauges, physical source conservation, true residuals, fixed
faces, input immutability and poisoned scratch reuse. Additional cases reject
invalid inputs and lost scatter precision, then verify recovery after failure.
GPU execution is pending; CPU checks cannot establish Metal execution/values.

Passed (all Cargo commands used CARGO_BUILD_JOBS=4 RUSTC_WRAPPER=):
- cargo check -p manifold-renderer
- cargo clippy -p manifold-renderer --lib --tests --features gpu-proofs -- -D warnings
- cargo test -p manifold-renderer --lib gpu_flip_lentine::tests (2 CPU tests:
  Naga/layout/atomic validation and reference projection invariants)
- scripts/lentine_reference.py (15 tests, including 7 projection tests)
- Projection exporter: 10 finite JSON cases, one incompatible status
- git diff --check; existing step/pressure Rust and shaders match HEAD
- Design status header: 95 words, within the 120-word lifecycle limit

- cargo test -p manifold-renderer --lib --no-run --features gpu-proofs
  gpu_flip_lentine::gpu_tests::component_projection_matches_reference: passed.

A final CPU-only rerun with --features gpu-proofs was blocked before execution
by storage admission: 48.3 GiB free against its 50 GiB reserve. The two CPU
tests had already passed; no files were deleted to make space. The final shader
change only removed an ineffective negative-water check from the unchanged
stage-2 path (dry cells were already skipped); stage-3 validation remains.
Initial validation caught a reserved WGSL identifier and a byte-cast inference
error in the new proof; both were corrected before the passed checks above.

All Cargo checks used this slot's absolute --manifest-path. RUSTC_WRAPPER=
keeps these checks independent of the sandbox-incompatible sccache service.

Lead's exact GPU proof command (works from any directory):

```sh
'/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-5/scripts/gpu_queue.py' --label BUG-lxxl-lentine-projection -- env CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= cargo test --manifest-path '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-5/Cargo.toml' -p manifold-renderer --lib --features gpu-proofs node_graph::primitives::gpu_flip_lentine::gpu_tests::component_projection_matches_reference -- --exact --nocapture
```

Changed files: gpu_flip_lentine.rs and shaders/gpu_flip_lentine.wgsl;
scripts/lentine_reference.py; docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md;
this handoff (already untracked at entry). The header records stage 2 as passed.
No files deleted. Redundant files identified: none. The inherited standalone
lentine_flux_main gather/proof remains useful as the stage-1 conservation oracle.

BUG-lxxl remains open. Owed: stage-3 GPU verification, stage-4 Solve Level 1
integration, stage-5 connected mixed-surface solve, stage-6 body/density coupling.
