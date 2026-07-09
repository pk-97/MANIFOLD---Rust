# Perf Budget Gate — a standing frame-time regression gate on the canonical show file

**Status:** PROPOSED design, awaiting Peter approval · 2026-07-09 · Fable
**Prerequisites:** none (extends existing harness + trace infra; UI_HARNESS_UNIFICATION P0 makes the numbers more representative but is not a blocker)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Hot-path discipline is currently prose (CLAUDE.md) plus one *per-phase* gate (the
content-thread work gate, DESIGN_DOC_STANDARD §5) that only fires when a brief remembers to
include it. Nothing watches the trend: multi-display, 3D scenes, and new screens each add
per-frame work a few percent at a time, and on a 53-layer / 2928-clip show file the bleed
becomes a dropped frame on stage before any single phase was individually guilty. This gate
makes the frame budget an enforced invariant instead of a value we argue from code.
Hardening level (§9): conformance treatment — anchors carry re-derivation commands.

## 1. Audit — what exists (verified 2026-07-09, breadth-first; re-derive at P1)

| Piece | Where | State |
|---|---|---|
| Spike-triggered frame trace | `MANIFOLD_RENDER_TRACE=1` (env-gated section breakdown; >20ms fail line already canon in DESIGN_DOC_STANDARD §5) | exists |
| Headless real-project load | harness `project:<abs-path>` scene — ⚠ VERIFY-AT-IMPL: `rg -n 'project:' crates/ -g 'fixtures.rs'` (UI_HARNESS_UNIFICATION_DESIGN.md cites fixtures.rs:68) | exists per doc |
| Canonical heavy fixture | `Liveschool Live Show V6 LEDS.manifold` (load-bearing migration fixture; ~53 layers / 2928 clips) | exists |
| Profiler crate | `manifold-profiler` — ⚠ VERIFY-AT-IMPL: what it already counts per frame before adding any collector | exists |
| GPU frame baseline | 4.5–5.5 ms known from the 2026-06 perf campaign (memory: `gpu-performance-investigation`) | number, no gate |
| Standing perf gate | none — `rg -in 'frame.?budget' docs/ crates/` → zero hits (2026-07-09) | **missing** |

Extend, don't redesign: the gate is a *harness mode plus a comparison script*, not a new
measurement system.

## 2. Decisions

- **D1 — Measure the show, not the parts.** The gate runs the canonical Liveschool fixture
  headlessly for N seconds at project FPS and records content-thread frame times (and GPU
  frame time where the trace already exposes it). Rejected: per-primitive micro-benchmarks —
  they don't compose, and the corpus's perf escapes (BUG-035's 59 ms on-thread conversion)
  were composition effects invisible to unit benchmarks.
- **D2 — Deliberate run, not default CI.** Same posture as `gpu-proofs`: needs the GPU,
  takes minutes, flakes under device contention. Invoked when a landing wave touched
  content-thread or render-path code, and as part of the pre-gig soak (GIG_RESILIENCE
  companion). Rejected: per-commit CI — wall-time cost and contention flake would rot the
  gate into being ignored.
- **D3 — Two thresholds, one absolute and one relative.** Hard fail: any frame >20 ms
  (the line DESIGN_DOC_STANDARD §5 already canonizes). Regression fail: p95 frame time
  >15% above the checked-in baseline. Rejected: absolute-only — it never catches the bleed
  until the cliff; relative-only — it lets a slow baseline ratchet quietly.
- **D4 — Baseline is a checked-in JSON, updated deliberately.** `docs/perf-baselines/…json`
  (machine-tagged; Peter's rig is THE machine — the gate's numbers are only meaningful
  there, stated honestly). Updating the baseline is a reviewed commit with a one-line
  justification, never a side effect of a green run. Rejected: auto-update on pass — that
  is the ratchet leak D3 exists to stop.

## 3. Invariants & enforcement

- **I1 — On the canonical fixture, no content-thread frame exceeds 20 ms.**
  Enforcement: P1's soak gate, exit non-zero.
- **I2 — p95 frame time does not regress >15% against the recorded baseline.**
  Enforcement: same gate, comparison step.
- **I3 — Baseline changes are deliberate.** Enforcement: baseline file lives in `docs/`
  (review surface); the soak tool refuses to write it without `--update-baseline`.

## §. Phasing

**P1 — soak mode + comparison (one session, Sonnet).**
Entry: harness loads the Liveschool fixture headlessly (re-verify the fixtures.rs anchor);
`MANIFOLD_RENDER_TRACE` produces per-frame numbers (read its implementation first — reuse
its section timers as the collector; inventory `manifold-profiler` before adding any new
counter). Read-back: this doc + DESIGN_DOC_STANDARD §5 content-thread gate + the
`hot-paths` constraints in CLAUDE.md. Deliverables: an xtask (working name
`cargo xtask perf-soak <project> --seconds N [--update-baseline]`) emitting a stats JSON
(min/p50/p95/max, worst-frame section breakdown) and a machine-tagged baseline file;
comparison + exit code per D3. Gate — positive: two consecutive runs on an unchanged tree
pass against a fresh baseline (run-to-run noise < the 15% band, measured and reported);
negative: `rg -n 'update_baseline' <tool>` shows the write is flag-gated (I3). Demo: the
stats JSON plus the worst-frame breakdown, read by the orchestrator — L2. Forbidden moves:
a new timing framework beside the trace sections; averaging away spikes (p95 and max are
the deliverable, not the mean); running windowed instead of headless. Test scope: focused
(`-p` the harness crate); no workspace sweep.

**P2 — wire into the protocols (half session, Sonnet).**
Entry: P1 landed, baseline committed. Deliverables: landing-protocol note (gate required
when a wave touched content-thread/render paths — add to the wave-gate list the
orchestrator already runs), pre-gig soak step in GIG_RESILIENCE's checklist, and a
VERIFICATION_DEBT.md line if either integration is deferred. Gate: `rg` proves both docs
name the command. Demo: none — L1.

## §. Decided — do not reopen
1. Show-level measurement, not micro-benchmarks (D1).
2. Deliberate-run posture, gpu-proofs-style (D2).
3. 20 ms absolute + 15% relative, both enforced (D3).
4. Baselines are reviewed commits, machine-tagged, rig-only semantics (D4).

## §. Deferred
- Per-section budgets (compositor vs sync vs UI bridge) — revive when a whole-frame
  regression fires and the breakdown proves too coarse to assign blame.
- Windowed/live-app soak variant — revive with GIG_RESILIENCE P3–P4 (rehearsal soak),
  where the windowed present path matters.
- Memory-growth assertion during soak (leak canary) — revive on first observed
  long-session growth; cheap to bolt onto the same run when needed.
