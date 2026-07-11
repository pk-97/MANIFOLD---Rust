# MESH_DEFORM P3 (curve→mesh) + P4 (scatter + glTF fit) — landed 2026-07-11 (feat/mesh-deform → main)

**Branch:** feat/mesh-deform · **Level reached:** L2 / target L2 (§10)
**Doc status line (quoted verbatim):** SHIPPED 2026-07-11 (`feat/mesh-deform`) — all four phases landed. P1 growth core (`mesh_ramp`, `push_along_normals`, `facet_normals`) + P2 shape deformers (`bend/twist/taper/morph_mesh`) on the freeze codegen path (decided #10); P3 curve→mesh builders (`revolve/extrude/tube`) + P4 `scatter_on_mesh` (multi-pass, hand-authored per the #10 scope boundary) + `gltf_mesh_source` fit/recenter extension. Demos Breathe/TwistColumn/Vine/Lathe/Garden all L2. · 2026-07-10 · Fable (with Peter in the room)

This is the final landing of the design — all four phases now on main. With batch 1
(P1+P2, landed `c6b9f3de`), the whole MESH_DEFORM_AND_CURVE_GEOMETRY design is shipped.

## What landed (P3 + P4)
- **P3** — `node.revolve_curve` (lathe), `node.extrude_curve`, `node.tube_from_path`
  (vine/ribbon). All on the freeze codegen path (`fusion_kind: Pointwise`,
  `input_access: [BufferGather]`, `standalone_for_spec::<Self>()`, computed
  `array_output_capacity`), modeled on `triangulate_grid` — they expand N curve points
  into an M-vertex grid, one thread per output vertex. revolve's `sweep` is UNBOUNDED
  (BUG-039 pin). Demos: `Vine.json`, `Lathe.json`.
- **P4** — `node.scatter_on_mesh` (3-pass area/scan/place instance sampler, hand-authored
  like `spawn_from_mesh`; NOT codegen — multi-pass, the decided-#10 scope boundary;
  deterministic by `wang_hash(seed, index)`; `align_to_normal` tilts instances to the
  sampled triangle normal, verified in Python before shipping). `gltf_mesh_source` grew
  `fit` (`none`/`unit_box`) + `recenter` params, applied on the background parse thread
  (D7, zero per-frame). Demo: `Garden.json`.

## Gate results (verbatim)
P3 (commit 41553f15):
- `revolve_curve::gpu_tests` / `extrude_curve::gpu_tests` / `tube_from_path::gpu_tests`
  (--features gpu-proofs): 2/2 each incl. generated-vs-hand parity + revolve→triangulate
  chain test. Unit: 13 passed (unbounded-sweep, capacity formulas). check-presets 51 ok.
P4 (commit 4615e75c):
- `scatter_on_mesh::gpu_tests` (--features gpu-proofs): 4 passed (determinism,
  different-seed-differs, on-surface-within-bounds, align-to-normal).
- Round-trip (`garden_preset_round_trip.rs`): `garden_outer_params_and_driver_survive_project_reload ... ok` — edited count/scale + an attached driver survive serialize→reload with base values intact and the driver still evaluating.
- gltf: 7 passed incl. `fit_none_is_a_byte_identical_no_op` (old presets unaffected).
- Canonical fixture: `load_liveschool_live_show_v6 ... ok` (genuine — asserts 52 layers, BPM 150.83, not an early return). check-presets 52 ok. catalog_gen 4 passed. clippy clean.

Orchestrator landing sweep (run in the worktree on the merged content — origin/main
`f38291be` brought a freeze/fusion bug-fix cluster BUG-006..011 touching `freeze/codegen.rs`,
so the sweep validates the codegen atoms against the fixed freeze code, not just for regressions):
- `cargo test --workspace`: 66 test suites ok, **0 failures** (clean textual auto-merge of
  codegen.rs — their freeze fixes + the P2 `length` reservation in separate hunks).
- `cargo test -p manifold-renderer --lib --features gpu-proofs` (all 11 mesh-deform atoms'
  gpu_tests, incl. the generated-vs-hand parity oracles): **38 passed, 0 failed** against the
  merged freeze codegen — the parity tests confirm the freeze fixes did not change the
  kernels the atoms generate. gpu_proofs integration binary: 5 passed (incl. `all_wgsl_shaders_validate`).
- `cargo clippy --workspace -- -D warnings`: clean, `Finished` in 30.93s, zero warnings.

## Demo artifacts (orchestrator read all)
- `Vine`: /tmp/vine-03.png → /tmp/vine-10.png — vine grows from a single curl to a full
  multi-turn helix. L2.
- `Lathe`: /tmp/lathe-2pi.png (+8 orbit angles) — smooth revolved vase, no seam crack at
  sweep=2π. L2.
- `Garden`: /tmp/garden.png — noise terrain with ~140 pink bud-flowers on the surface,
  scaled by hash, tilted to local slope where align_to_normal fires. L2.

## Deviations from brief
- P3: `segments`/`steps`/`close`/`sides` left un-port-shadowed (topology ints, not
  performance scalars) — matches triangulate_grid's precedent; the brief's int-exclusion.
- P4: Garden composites terrain + flowers via `node.mix` Max-blend, not depth-correct
  alpha-over (no shared depth buffer between the two render passes) — logged as design
  Deferred #6, not a defect (render_scene has no instancing port; out of P4 scope).
- NODE_CATALOG regenerated in both phases (default-suite `regenerates_in_sync`).

## Shortcuts confessed (rolled up from phase reports)
- P3: none.
- P4: the Max-blend composite above (Deferred #6). Flower geometry is a procedural
  lathed bud (per the design's "procedural stand-ins only" instruction). Otherwise none
  in the scatter/fit deliverables themselves.

## Verification debt
none opened, none carried. All five demos reached L2 target; generator presets have no
L3 flow-driver path (no compositor for the ui-snap harness), so L2 is the ceiling.

## Click-script for Peter (≤2 minutes)
1. `cargo run -p manifold-renderer --bin render-generator-preset -- Vine --param grow=0.3 --out /tmp/v1.png` then `--param grow=1.0 --out /tmp/v2.png` — expect: the vine longer/more-turns in the second.
2. `cargo run -p manifold-renderer --bin render-generator-preset -- Garden --out /tmp/g.png` — expect: flowers scattered ON the terrain, tilted to the slopes, none floating.
3. **Swap in your real flowers .glb:** in Garden's graph, replace the procedural flower chain (`flower_t`→`flower_bud`→`flower_radius`/`flower_height`→`flower_profile`→`flower_revolve`→`flower_tris`) with one `node.gltf_mesh_source`; set its `path` to your `.glb` and `fit=unit_box` (leave `recenter=true`); wire its `vertices` output into `render_copies`' `vertices` input. Nothing else changes — scatter, instancing, terrain are already correct. Expect: your scanned flowers scattered across the terrain at a sane unit scale.
4. To make them breathe (P1): insert `mesh_ramp` → `push_along_normals` between the gltf source and `render_copies`, put an LFO/audio-band on `push_along_normals.amount`. Expect: the scanned flowers pulse with the band.
