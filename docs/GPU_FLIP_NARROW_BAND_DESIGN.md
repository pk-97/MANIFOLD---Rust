# GPU FLIP narrow band — particles at the surface, grid water inside

<!-- index: BUG-jyot option B: Ferstl 2016 narrow-band FLIP, fixed three-cell particle support and two-cell velocity combination, interior level set, lifecycle proofs and staged integration. -->

**Status:** IN PROGRESS · 2026-10-03 · Codex · S1 CPU proofs pass and isolated GPU proofs compile; S2 step and S3 seam integration remain unbuilt. GPU execution remains owed.
**Prerequisites:** GPU_FLIP_PRESSURE_SOLVE.md and the liquid particle-frame seam on main `4aab34f86`.
**Execution contract:** read DESIGN_DOC_STANDARD.md sections 5–6 before each stage.

Peter: “It's a step param, default off, and off stays bit-identical.”
This implements option B of GPU_FLIP_STRUCTURAL_OPTIONS.md section 5.
The stage owns its numerical method; no new catalog primitive is needed
(DECOMPOSING_GENERATORS.md section 1.2). Pressure remains the existing operator.
The cost saving is particle work; it does not reduce pressure unknowns.

Reference: F. Ferstl, R. Ando, C. Wojtan, R. Westermann and N. Thuerey,
[Narrow Band FLIP for Liquid Simulations](https://doi.org/10.1111/cgf.12825),
Computer Graphics Forum 35(2), 225–232, 2016. The
[author manuscript](https://www.cs.cit.tum.de/fileadmin/w00cfj/cg/Research/Publications/2016/NBFlip/nbflip.pdf)
is the numerical reference. Companion contracts: GPU_FLIP_PRESSURE_SOLVE.md
(unchanged projection), LIQUID_SOLVER_SEAM_DESIGN.md (publication),
GPU_FLIP_STRUCTURAL_OPTIONS.md (approved scope).

## 1. Audit — what exists (verified 2026-10-03)

| Piece | Anchor | State |
|---|---|---|
| Step and scratch ownership | `primitives/gpu_flip_step.rs` (`StepState`, `encode`) | Extend, do not redesign. |
| Particle distance, gather, density | `primitives/shaders/gpu_flip_step.wgsl` (`particle_distance`, `particles_to_faces`, `density_source`) | Shared numerics; band-only variants must reuse these bodies. |
| Deterministic compaction | `primitives/sort_particles_into_cells.rs` (`ParticleSorter`) | Dead radius-zero slots sort out of the live prefix. |
| Deterministic emission rank | `primitives/prefix_scan.rs` (`PrefixScan`) | Reuse for reseeding. |
| Particle frame publication | `primitives/liquid_frame.rs` (`LiquidFrame`), `liquid/frame_ring.rs` (`FrameRing`) | Interior distance must follow the same tick/epoch and A/B ownership. |
| Native GPU API | `manifold-gpu/src/metal/encoder.rs` | Every pass uses manifold-gpu. |

Paths above are relative to `crates/manifold-renderer/src/node_graph` except
the GPU crate. The particle-ball field is negative but shallow throughout a
filled pool; thresholding it directly cannot find a deep interior. Re-distance
from its zero crossings before deleting any particles.

## 2. Decisions

**D1. Fixed widths.** Particle support is `abs(phi) < 3*h`; deep deletion is
`phi < -3*h`. Keep particles within `3*h` of a solid. Face combination uses
particle velocity at `phi_face >= -2*h` (and within `2*h` of solids), grid
velocity deeper inside. Rejected: density-weighted blending; it is the unstable
coupling the paper replaces. No user tuning of either width.

**D2. Track the interior.** RK4 semi-Lagrangian backtraces advect the previous
cell-centred distance and MAC velocity. Union the particle surface with the
advected distance shrunk by exactly `h`, then initialize a Manhattan distance
from subcell sign crossings using three separable axis sweeps. Rejected:
using the sparse particle union as the whole liquid; that creates an air cavity.

**D3. Preserve the baseline.** `narrow_band = 0` dispatches the existing path
without new scratch or changed inputs. Enabling initializes from the full
particle surface; reset, lattice change and epoch change invalidate history.
Disabling must restore interior particles before returning to dense FLIP.
No reuse of an old epoch's distance or faces.

**D4. Lifecycle.** Retire deep particles by zeroing radius, then use the existing
sort. Newly entering liquid band cells below `-h` are filled to eight with the
existing quarter/three-quarter sites, skipping occupied subcells and solids.
New velocity is interpolated from projected faces. Prefix ranks allocate the
whole reseed deterministically. Never truncate a reseed silently; capacity
shortage must be reported through the existing liquid failure path. Keep the
existing refusal above `2^24` particle slots (the boundary itself is exactly
representable, `gpu_flip_domain.rs` capacity check). No per-cell particle cap
is introduced. Entering means `old_phi <= -3*h` and `-3*h < phi <= -h`;
the previous liquid distance is retained separately from the solid-inclusive
band mask. Candidate sites require `liquid_phi <= -h` and `solid_phi > 0`.

**D5. Consumers.** Publish optional interior distance with the liquid frame's
tick and lattice. Mesh volume uses it below the particle band; whitewater reads
the combined liquid field. This explicitly amends the seam's former prohibition
on solver distance output: a particle-free interior cannot be reconstructed by
the particle mesher. Unwired consumers retain their current path.

## 3. Passes and ownership

New pass source: `primitives/shaders/gpu_flip_narrow_band.wgsl`. S2 introduces
`primitives/gpu_flip_narrow_band.rs` as the Rust owner held by `StepState`;
S1 binds the passes only from its proof harness. The GPU uniform is:

```rust
#[repr(C)]
struct NbParams {
    n: [u32; 3], slots: u32,
    minimum: [f32; 3], h: f32,
    dt: f32, axis: u32, initialized: u32, closed_faces: u32,
}
```

All state is renderer-owned, runtime-only, with buffers resized on lattice or
capacity changes. Pipelines compile at install. Buffers cover every cell/face;
the pressure water mask includes particle-free interior cells. Dense interior
passes are intentional, not a sparse-tile fallback. Distance and face gather
avoid particle searches outside the band. Density source is zero outside it;
the pressure operator itself is untouched.

Consequences: distance history, face advection, masks and reseed scans add
storage and dispatches. The dense particle pool remains allocated to support
surface motion and sources without a cap. GPU execution is forbidden in this
slot session, so compile success cannot establish conservation or performance.

## 4. Invariants & enforcement

| Invariant | Required check |
|---|---|
| Three-cell mask and solid retention | CPU reference band test; `gpu_flip_narrow_band_mask` |
| Exact two-cell sharp switch | CPU reference switch test; `gpu_flip_narrow_band_combine` |
| Interior transport and distance | CPU translation case; `gpu_flip_narrow_band_advect`, `gpu_flip_narrow_band_redistance` |
| Reseed deficit per cell, unique sites, grid velocity | CPU reseed tests; GPU reseed value proof |
| Deep deletion, retained surface, cleared tail | CPU compaction test; `gpu_flip_narrow_band_delete` and sorter integration proof |
| Shader syntax and uniform layout | `gpu_flip_narrow_band_shader_validates`; per-pass values still require GPU execution |
| Off bit-identical, reset and toggle safe | End-to-end dense/off and lifecycle GPU proofs, required before landing |
| No silent capacity loss | CPU refusal test and runtime overflow proof |
| Correct consumer interior | Mesher/whitewater seam value proofs, required before landing |

## 5. Phasing

Each stage follows edit → cargo check → small CPU proof → clippy `-D warnings`
→ exact-path commit. Every cargo command has `CARGO_BUILD_JOBS=4`; tests have
a filter. Never run GPU tests or the app in this session.

**S1 — independent math and isolated passes (CPU-tested, GPU compile-only).** Entry: verify branch and base,
read the reference and sections 1, 3, 5 of structural options. Deliver the
CPU oracle, standalone shader, design and compile-only GPU proofs. Gate:
`python3 -B -m unittest discover -s scripts -p 'test_narrow_band*reference.py'`, CPU shader validation,
renderer check/clippy and gpu-proofs `--no-run`. No runtime integration claim.
Demo: none — L1 numerical fixtures. Forbidden: tuned widths, GPU execution.

The canonical CPU references are `scripts/narrow_band_reference.py` (particle
lifecycle) and `scripts/narrow_band_grid_reference.py` (RK4 advection, sharp
combination, union and brute-force Manhattan distance). The earlier
`scripts/gpu_flip_narrow_band_reference.py` is redundant and is retained
untouched for lead review; its global reseed deficit and exact-coordinate
occupancy differ from the required per-cell deficit and half-cell occupancy.

GPU value-proof filter: `gpu_flip_narrow_band_tests::gpu_tests::` (mask,
scalar/face advection, combination, deletion, rectangular/diagonal distance,
union and reseed). Reseed uses the existing `PrefixScan` and checks that a
shortage writes no particles. These proofs do not yet cover step integration,
sorter compaction or consumer publication.

Run the compiled GPU proofs only on a GPU-capable lane, from slot-4:

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= python3 -B scripts/gpu_queue.py --label gpu-flip-narrow-band -- cargo test -p manifold-renderer --lib --features gpu-proofs gpu_flip_narrow_band_tests::gpu_tests:: -- --test-threads=1
```

**S2 — step integration.** Entry: S1 gates passed; re-read `StepState::encode`
and sorter/scan APIs. Deliver band param, masked gather/density, interior
advection, reseed/delete, exact-count and overflow handling, reset/toggle
semantics. Gates: CPU lifecycle and extent tests, compile-only GPU proofs for
off identity and full lifecycle. Gesture: enable then disable narrow band in a
deep pool. GPU proof execution and observed rendering remain owed to Peter's
GPU-capable lane; do not call an unrun proof passed.

**S3 — liquid seam.** Entry: S2 compiled and CPU-proven; re-read frame ring,
mesher and whitewater inputs. Deliver optional interior distance, publication,
consumer union and contract updates; run `scripts/gen_docs_index.py`. Gates:
CPU layout checks and compile-only seam proofs. Gesture: pause/reset a narrow
band pool; interior remains filled in its published mesh. L2 rendering remains
owed outside this CPU-only session. No app landing until GPU gates pass.

## 6. Decided — do not reopen

1. R=3h and r=2h, sharp coupling, no tuning.
2. Pressure operator unchanged; density source only in the particle band.
3. Shared sorter and prefix scan; no atomics or new lock.
4. Default off and baseline identity are requirements.
5. Only slot-4 edits and exact-path commits; push the feature branch as requested.
   No GPU execution and no main landing in this session.

## 7. Deferred

Sparse interior tiles and alternative pressure operators are separate options;
revive only under their approved workstreams. Performance and visual validation
are required follow-up execution on a GPU-capable lane, not waived acceptance.
