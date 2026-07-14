# FUSION_SOTA batch 1 (P1-P3) — landed 2026-07-14 @ ce6dcba8

**Branch:** feat/fusion-sota · **Level reached:** L1 / target L1 (§10 — all three phases are
pure-refactor / fault-path / instrumentation phases with no user-visible surface; each phase
brief explicitly states `Demo: none — L1`)

**Doc status line (quoted verbatim):**
> **Status:** IN PROGRESS · 2026-07-14 · Fable 5 (with Peter in the room) · Sonnet 5 executing
> P1–P3 SHIPPED (markers module, segment worker robustness, refusal census committed as
> `docs/fusion_census.md` — no D4 default flipped, all four stand). P4–P7 remain.

## Gate results (verbatim)

Per-phase (run by each worker, independently re-verified by the orchestrating session after
every phase):

**P1** — `cargo test -p manifold-renderer --lib node_graph::freeze`: 67 passed, 0 failed, 2
ignored. `cargo clippy -p manifold-renderer -- -D warnings`: clean. Hard gate
`fused_wgsl_snapshot_unchanged` (byte-identical fused WGSL across 48 kernels, all bundled
effect+generator presets): pass. Negative gate `marker_literals_live_in_one_module` (zero
stray `"// @` literals outside `markers.rs`): pass.

**P2** — `cargo test -p manifold-renderer --lib node_graph::freeze`: 69 passed, 0 failed, 2
ignored (includes new `segment_pending_expires_to_refused`,
`segment_worker_panic_refuses_key`). `cargo clippy -p manifold-renderer -- -D warnings`: clean.

**P3** — `cargo test -p manifold-renderer --lib node_graph::freeze`: 70 passed, 0 failed, 3
ignored (includes new `refusal_census_matches_classify_node`). Census test run with
`--ignored --nocapture`: produces `docs/fusion_census.md` from a live run over 57 bundled
presets + the Liveschool fixture. `cargo clippy -p manifold-renderer -- -D warnings`: clean.
Docs-index freshness test: pass after `python3 scripts/gen_docs_index.py`.

Full crate re-verification after P3 (orchestrating session): `cargo test -p manifold-renderer
--lib`: 1216 passed, 0 failed, 4 ignored.

Full workspace sweep at landing (run in the main checkout at merge `ce6dcba8`):
`cargo clippy --workspace -- -D warnings`: clean (only pre-existing Objective-C SDK deprecation
warnings from `manifold-media`'s native decoder plugin, unrelated to this wave). `cargo nextest
run --workspace`: 3325 tests run, 3325 passed, 12 skipped. `cargo deny check bans`: ok.

## Deviations from brief

- P1: the design doc's §1 audit phrase "(7 markers)" undercounts the map's own §5 table (9
  marker forms); the worker built all 9 variants per the map, the authoritative source. Not a
  deviation from D1's intent, a reconciliation of an internal doc inconsistency.
- P3: added `manifold-io` as a `manifold-renderer` dev-dependency (test-only) to load the
  Liveschool fixture for the census — not a new crate-graph edge, since
  `renderer → playback → io` already exists transitively; no such test existed in
  manifold-renderer before.
- No other deviations.

## Shortcuts confessed (rolled up from phase reports)

- P1: none.
- P2: none. Panic-injection test hook is `#[cfg(test)]`-gated only, compiled out of production.
- P3: dispatches-saved-per-family is a documented conservative lower-bound estimator (1 per
  refusal with ≥1 eligible neighbor, or component-size−1 for region-level drops), not a precise
  perf measurement — called out explicitly in the committed `docs/fusion_census.md` rather than
  presented as exact.

## Verification debt

None opened. All three phases hit their stated target level (L1) with hard gates green;
nothing partial was landed.

## Escalations from P3's census (per FUSION_SOTA_DESIGN.md §4's phase brief)

Census numbers (full table in `docs/fusion_census.md`): buffer-fan-out refusals = 0 (D4
trigger: ≥3 — not crossed). Resample refusals = 4 structural instances, but D4's trigger is
runtime hot-chain evidence, which a static def-walk cannot produce — the census documents this
distinction rather than treating the nonzero count as a trigger. **No DEFER→LIFT flip is
warranted by these numbers; all four D4 defaults (Vec3 LIFT, multi-output-texture
LIFT-census-gated, buffer-fan-out DEFER, resample DEFER) stand as designed.** Flagging this
explicitly per the design doc's escalation clause, even though the answer is "no change."

## Click-script for Peter (≤2 minutes)

1. `cat docs/fusion_census.md` — expect: a table of refusal counts per family (fan-out /
   stencil-depth / multi-output / param-type / resample / arity / BufferIndex-shaped / other)
   plus a paragraph reading the numbers against the four D4 defaults, all "stand as-is."
2. `cargo test -p manifold-renderer --lib node_graph::freeze` — expect: all green, no failures
   (70+ tests, a few `#[ignore]`d on-demand census/report tests).
3. Nothing to look at visually — these three phases are compiler-internals hardening with no
   rendering change. The only observable artifact is the census doc above.
