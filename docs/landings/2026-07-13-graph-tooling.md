# Landing report — GRAPH_TOOLING P1–P4 (2026-07-13)

**Design:** `docs/GRAPH_TOOLING_DESIGN.md` (authored, approved, and executed same day; Fable orchestrating 4 Sonnet workers, one warm worktree, branch `feat/graph-tooling-design`).
**Status line now reads:** `**Status:** SHIPPED — P1–P4 built, gated, and landed 2026-07-13 (same-day design→execution, Fable orchestrating Sonnet workers). Deferred items in §7 remain open.`

## What shipped

- **P1** `99772a50` — `node_graph::validate::validate_def` extracted from check_presets (one validator, loader-faithful by construction); `graph-tool` bin, `validate` verb (`--json`); check_presets now a thin walker.
- **P2** `997fac58` + **P2b** `a426440e` — `BoundaryReason` (8 variants) declared on all 110 boundary primitives; meta-test `every_boundary_atom_declares_its_reason` = pure `is_fusable() XOR boundary_reason()` walk, no undeclared middle; `CONVERSION_DEBT_LEDGER` = 13 atoms; catalog gains `fusion` field (237 nodes). 14 escalations triaged by orchestrator against kernel structure (grep barriers/atomics, pipeline counts, purpose strings) — verdicts in the P2b commit.
- **P3** `380e2167` — `graph-tool fusion` verb (flattens groups first per D10, calls `partition_regions`, machine-verified equal to the freeze pipeline's partition across every bundled effect preset — permanent test `fusion_verb_matches_freeze_partition`); glb importer output now runs `validate_def` before reaching the project (D6, errors never warnings); doc pointers in DECOMPOSING_GENERATORS / ADDING_PRIMITIVES / CLAUDE.md.
- **P4** `c6d809f0` — card lints in `validate_def` (5 errors, 3 warnings per D8); `docs/CARD_AUTHORING.md` (D9 intent table); found+fixed a real shipped bug: EdgeDetect.json's dead "Mode" slider (card promised 3 edge modes; node is Sobel-only).

## Gate results (orchestrator-run)

- Held-out fixtures (authored outside the worktree, workers never saw them): 5 invalid-graph classes (P1) + 5 invalid-card classes (P4) — **all 10 fail with errors naming node/port/param**. L2.
- 57/57 bundled presets validate clean (zero errors); bundled pass count unchanged through the check_presets rewire.
- Full `-p manifold-renderer --lib`: 1170 passed / 0 failed. Clippy clean per phase.
- Landing sweep (warm main checkout, post-merge): recorded in the landing commit.

## For Peter — triage owed (not fixed, per D8)

11 bundled presets trip card WARNINGS: range-after-remap (lint h) on BlobTracking, VoronoiPrism, Glitch, Bloom, FluidSim2D, MetallicGlass, ParticleText, BlossomWire, TwistColumn, OilyFluid; defaults-disagreement (lint g) on ApricotWeather. Each may be intentional (over-range drive) or a real card bug — one pass in the app decides promotion per class (design §7 Deferred).

## Click-script (≤2 min)

1. `cargo run -p manifold-renderer --bin graph-tool -- validate crates/manifold-renderer/assets/effect-presets/Bloom.json --kind effect` → expect `OK … (with warnings)` and one WARN naming `(node.mix).amount`.
2. `cargo run -p manifold-renderer --bin graph-tool -- fusion crates/manifold-renderer/assets/effect-presets/DepthOfField.json` → expect 19 nodes, 2 regions, ~14 dispatches, region membership listed.
3. Drag a known-good .glb into the app → imports as before; the validate hook only speaks on failure.

## Deviations & debt

- P4 fixtures live as inline JSON in tests (P1 left no `tests/fixtures/invalid-graphs/` dir); held-out property preserved via orchestrator-side scratchpad fixtures.
- `Composite` binding targets unlinted (runtime-resolved; zero bundled users) — added to design §7 Deferred with trigger.
- New bundled-preset validate test creates a Metal device in the default sweep (~2s, allocation only, no dispatch) — accepted per design's honest-cost note; first suspect if the sweep ever flakes.
- Verification debt: none beyond the above — no VD entries opened.
