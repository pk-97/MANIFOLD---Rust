# MESH_DEFORM P1 (growth core) + P2 (shape deformers) — landed 2026-07-11 @ merge of feat/mesh-deform (b0ad33f2)

**Branch:** feat/mesh-deform · **Level reached:** L2 / target L2 (§10)
**Doc status line (quoted verbatim):** IN PROGRESS — P1 (growth core) + P2 (shape deformers) SHIPPED 2026-07-11 (`feat/mesh-deform`); all atoms on the freeze codegen path per decided #10. P3 (curve→mesh) + P4 (scatter + glTF fit) remain. · 2026-07-10 · Fable (with Peter in the room)

## What landed
Seven new fusable deform atoms, all on the freeze/graph-compiler codegen path
(`wgsl_body` + `fusion_kind` + `input_access`, runtime pipeline from
`standalone_for_spec::<Self>()`, hand-WGSL retained only as gpu_tests parity oracle) —
per Peter's 2026-07-11 rule that every node must work with the graph compiler in full
(design decided #10).

- **P1** — `node.mesh_ramp` (weights producer, Coincident), `node.push_along_normals`
  (Coincident + optional Coincident weights + optional Texture2D field),
  `node.facet_normals` (per-vertex BufferGather flat-normal reset). Demo: `Breathe.json`.
- **P2** — `node.bend_mesh`, `node.twist_mesh`, `node.taper_mesh` (all Coincident
  Pointwise), `node.morph_mesh` (two Coincident array inputs). Demo: `TwistColumn.json`.
  bend/twist rotate normals exactly (D4); bend/twist `angle` params ship UNBOUNDED
  (`range: None`, BUG-039 regression pin, asserted in tests).

`facet_normals` is necessarily a fusion boundary (it gathers 3 triangle verts) — the
pure per-vertex chain (breathe→twist→taper) fuses to ~1 dispatch, which is the case
decided #10 targets; a chain cuts at facet, correctly.

## Gate results (verbatim)
P1 (commit 1f55a3fb, codegen rework):
- `mesh_ramp::gpu_tests` 3 passed; `push_along_normals::gpu_tests` 5 passed (incl.
  `generated_matches_hand_kernel_all_modes` — all 4 weights×field combos);
  `facet_normals::gpu_tests` 4 passed. All under `--features gpu-proofs`.

P2 (commit b0ad33f2):
- `cargo test -p manifold-renderer --lib "_mesh::gpu_tests" --features gpu-proofs`:
  20 passed, 0 failed (parity + exact-normals-past-2π + count/uv + weights-degrade ×4).
- `cargo test -p manifold-renderer --lib "_mesh::tests"`: 36 passed incl.
  `bend_mesh_angle_is_unbounded`, `twist_mesh_angle_is_unbounded`.
- `cargo run -p manifold-renderer --bin check-presets`: 49 presets, 49 ok, 0 failed.
- `cargo test -p manifold-renderer --lib catalog_gen`: 4/4 (regenerates_in_sync).
- `cargo clippy -p manifold-renderer --features gpu-proofs -- -D warnings`: clean.
- Negative: `create_compute_pipeline(include_str!` across the 4 new files → zero hits;
  `range: Some` never near `angle` in bend/twist.

Orchestrator confirmation gate (after merging origin/main dd31cde4 into the branch):
- `cargo test -p manifold-renderer --lib`: **1044 passed, 0 failed, 3 ignored** (1.43s).

## Demo artifacts (orchestrator read all 5 PNGs)
- `Breathe`: /tmp/breathe-phase02.png vs /tmp/breathe-phase09.png — the mesh_ramp
  deformation crease walks along the mesh between phase 0.2 and 0.9 (amount held
  constant by the LFO, isolating phase). L2.
- `TwistColumn`: /tmp/twist-a.png (angle≈0.3, near-flat), /tmp/twist-b.png (≈π, one
  curl), /tmp/twist-c.png (≈2.5π, ~2½ continuous corkscrew loops) — continuous
  rotation past 2π, no wrap seam. The BUG-039 gesture (saw LFO full revolutions). L2.

## Deviations from brief
- P1 was first built as plain hand-WGSL, then reworked onto the codegen path after
  Peter's decided-#10 ruling. The plain-WGSL version never landed.
- P2 added `"length"` to `codegen.rs`'s `wgsl_safe_field` RESERVED list (taper_mesh's
  committed param name collides with the WGSL `length()` builtin; renamed to `p_length`
  in generated WGSL only — outward param id/port name stay `length`). Design comment
  invited exactly this extension point.
- Node catalog (`docs/NODE_CATALOG.md`, `docs/node_catalog.json`) regenerated — the
  default-suite `regenerates_in_sync` test enforces it; not in the original gate list.

## Shortcuts confessed (rolled up from phase reports)
- P1: `short_weights_degrade_to_one_for_the_tail` test uses a full-size physical buffer
  with `weights_len=2` (not a literal 2-element buffer) — the coincident codegen
  pre-reads `buf_weights[idx]` so a genuinely short buffer would OOB; the graph always
  matches array capacities so a short physical wire never reaches the atom at runtime.
  Documented in the test.
- P2: none. One undocumented-in-doc convention chosen and documented in-code: the
  bend-axis rotation plane (bend axis A rotates the pair (A, next(A)) about the third
  axis, cyclic X→Y→Z→X); covered by an analytic exact-normals gpu_test.

## Verification debt
none opened, none carried. Both demos reached their L2 target; the flow driver has no
compositor for generator presets (no L3 path exists for these), so L2 is the ceiling.

## Click-script for Peter (≤2 minutes)
1. `cargo run -p manifold-renderer --bin render-generator-preset -- Breathe --param phase=0.2 --out /tmp/b1.png` then `--param phase=0.9 --out /tmp/b2.png` — expect: the raised crease band sits at a different position along the mesh in the two images (the growth mask walking).
2. `cargo run -p manifold-renderer --bin render-generator-preset -- TwistColumn --frames 315 --out /tmp/t.png` — expect: the strip's far end wound into multiple continuous corkscrew loops, no seam/snap.
3. In-app: drop `mesh_ramp` → `twist_mesh` after any mesh source, put a saw LFO on `twist_mesh.angle`, and confirm it spins through full revolutions without stalling at 2π.
