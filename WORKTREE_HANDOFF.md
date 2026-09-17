# P5 dynamic RT checkpoint — 2026-09-17

Unfinished work for BUG-e3p6.4. This is a local checkpoint, not an app landing.
Main was not changed. Source checkpoint: `f442dd763` on the feature branch.
Remote retirement was blocked by automatic approval review because the repository
is public and the source/handoff payload is unpublished. Do not retry publication
without user approval. The lease is released, cache scrubbed, and source checkout
retained safely in slot-0 pending approved remote archival and retirement.
User requested a 30-minute implementation window and tests at the end.

Parent feature branch: `feat/rt-p5-dynamic-scene-path` at
`b3bd00066059ff9c605a0a0e3e7598d317687d25`, based on main `90756f831`.
Earlier commits retained in ancestry: `698667f12` (f8 journey readiness),
`f8976b9cf` (frame validity/export rejection), `b3bd00066` (unverified inline-copy WIP).

## Implemented in this checkpoint

- Restored tracer parameter-buffer compatibility arguments after the inherited
  WIP failed renderer compilation; actual uniforms use inline bytes.
- Retained CPU scratch plus GPU-private ordered snapshots for noninstanced
  descriptors, instanced descriptor-build parameters, and normal-source tables.
  GPU materials and object-motion tables also use ordered inline copies.
  Descriptor preparation accounts for scratch plus destination memory.
- Removed the renderer content-settle/deferred-build policy and completion latch.
  Current mesh revisions choose P5 rebuild/attributes/reuse, with explicit topology
  hints and structural checks. Current AS/table encoding precedes RT flags,
  resource ensures, depth, and tracing.
- Geometry/attribute/appearance changes enter the shared temporal reset decision.
  Material values and texture generations now participate in appearance changes.
  World transforms retain their existing reprojection behavior.
- Pending required inputs reject the whole frame instead of tracing a subset;
  lost resident geometry becomes a structured frame failure. RT candidate memory
  admission includes source/material/motion table growth before AS preparation.
- Regression for three uncommitted noninstanced frames with transforms
  0,+0.5,-0.5 against one resident accel. All three centroid rays match CPU hits.
- Production PresetRuntime first-frame RT dispatch/frame-status proof. This checks
  real RT captures and Complete status, not numerical image correctness.

## Checks performed at the end

All commands run from the slot checkout with `RUSTC_WRAPPER=`.

- `cargo clippy -p manifold-gpu -p manifold-renderer -p manifold-app --tests -- -D warnings`
  PASS after fixing two redundant borrows in descriptor useResource calls.
- `python3 scripts/gpu_proofs_gate.py --filter rt_dynamic_ordering --filter rt_dynamic_current_frame --filter render_scene:: --filter frame_status`
  PASS: 71 renderer tests and 4 GPU proofs, no ignored tests.
- `python3 scripts/gpu_proofs_gate.py --filter rt_dynamic_shading --filter rt_dynamic_fusion --filter mesh_change_`
  PASS: 8 renderer tests and 4 GPU proofs, no ignored tests.
- `cargo test -p manifold-gpu --lib`: 45 passed, 1 failed. The existing
  `tracer_reconstruction_compiles_nothing` uses a process-global compilation
  counter; it observed 47 versus 48 while other GPU tests ran concurrently.
- `cargo test -p manifold-gpu --lib tracer_reconstruction_compiles_nothing -- --test-threads=1`
  PASS in isolation. Do not report the initial parallel run as fully green.
- `git diff --check` PASS. Only comment cleanup followed successful runtime checks.

## Required before P5 can land

1. Finish warmup/candidate integration: preparation currently happens at the
   renderer structural-change boundary. RT-off prewarm and aggregate atomic
   publication are not qualified. Audit prepared steady-state allocations:
   accel update still constructs topology/actions vectors; normal-source texture
   lists, GI materials and existing renderer collections also need accounting.
2. Move MeshTopologyHistory from its existing explicit-cut input to general mesh
   topology revisions plus explicit hints. Shared reset is wired, but production
   history/reset/resume and pending-first-ready proofs remain incomplete.
3. Extend `rt_dynamic_current_frame` beyond the one first-frame dispatch proof:
   production A2/A3, pending-first-ready, renderer-level atomic admission, history
   reset/resume, and explicit update counts. The existing backend atomic-admission
   proof passed; it does not establish renderer candidate-publication atomicity.
4. Qualify the requested VortexFragments + OrderedRecon composition with RT on,
   then actual export. No user project was modified or exported in this window.
   No visual parity or performance claim is supported by this checkpoint.
5. Complete design/D17 status updates once the mechanism is ready to land;
   required landing gate, fresh review/landing and owed demos remain outstanding.
   P6 refit, P7 catalog/export acceptance, P8 bounded performance remain after P5.

The old handoff and campaign ledger remain in BUG-e3p6.4. Follow current Codex
AGENTS.md (native Luna mechanical lanes); old Claude-specific lane instructions
are historical, not the Codex execution mechanism.
