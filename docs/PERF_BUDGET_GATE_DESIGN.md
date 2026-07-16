# Perf Budget Gate — a standing frame-time regression gate on the canonical show file

**Status:** APPROVED 2026-07-09 (Peter) — design ready, awaiting build (Sonnet, P1–P2b–P3) · design 2026-07-09 · Fable · amended 2026-07-14 (Fable + Peter): D5/D6 added — per-node GPU attribution pass (P2) + real-time pacing with `--start` targeting; protocol wiring renumbered to P3 · amended 2026-07-16 (Fable + Peter): D7/P2b added — bare-glb import-graph mode (BUG-189 is the first customer); all D7 decisions closed, zero executor discretion
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
| Per-dispatch GPU timestamping | `manifold-gpu` `metal/profiling.rs` — real Metal counter-sample API (`MTLCounterSampleBuffer`, stage-boundary samples, calibrated to CPU clock); dormant unless `GpuEncoder::enable_dispatch_profiling` is called. Node-graph executor already labels every step (`execution.rs` step tags) — ⚠ VERIFY-AT-IMPL: `rg -n 'enable_dispatch_profiling' crates/`. Enable pattern proven headless in `freeze_profile.rs` (~line 1343). Verified 2026-07-14. | exists |
| Per-node attribution on a *project* frame | `freeze_profile` profiles isolated presets only — nothing runs the sampler over a loaded show file | **missing** (P2) |

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
- **D5 — Real-time pacing, targeted windows.** The soak paces at project FPS: `--seconds 30`
  costs 30 wall-clock seconds. Targeting is the answer to long sets, not speed: a
  `--start <beats>` flag seeks the transport so a run soaks the passage under suspicion
  (e.g. the FluidSim3D clip behind BUG-156) instead of playing from the top. Rejected:
  uncapped faster-than-realtime rendering — headless has no vsync so it's possible, but it
  starves video-decode readahead of wall time (stalls that wouldn't happen on stage) and
  runs thermals hotter than any real set, i.e. it distorts exactly what the gate measures.
  Revivable for pure-synthesized projects; see Deferred.
- **D6 — Attribution is a second, profiled pass — never the gate pass.** `--profile` re-runs
  the same window with `enable_dispatch_profiling` on the frame's encoder and emits a
  per-node breakdown of the worst frames: node/primitive label, ms, share of frame — the
  same per-dispatch attribution as an Xcode GPU capture, as JSON from a CLI. This is the
  diagnosis step when the gate fails (and the optimization map for agents working a heavy
  project). It is structurally separate from the gate because profiled mode trades batching
  for resolution: on Apple silicon counters sample at stage boundaries only, so profiled
  frames give every dispatch its own encoder — per-span *shares* are trustworthy, absolute
  totals are inflated. Gate numbers (I1/I2, baselines) therefore come exclusively from
  unprofiled frames; a profiled run never writes a baseline and never sets the exit code.
  Two granularity honesty notes: under the freeze compiler a fused pipeline is one dispatch,
  so attribution is per *compiled step* (fused-kernel), not per source node — identical to
  what Xcode capture shows; and the sampler buffer has fixed capacity (two samples per
  span), so P2 must verify capacity/overflow behavior against a Liveschool-scale frame
  before trusting whole-project output. Rejected: profiling inside the gate run (lies about
  totals); a separate profiling tool beside perf-soak (the sampler plumbing and the project
  loader are the same — one tool, two passes).

- **D7 — Bare-glb input runs the import graph on a sibling frame loop; shared plumbing,
  separate loop, report-only.** (Added 2026-07-16; first customer is BUG-189's ~10 ms
  resolution-independent floor.) The tool dispatches on input extension: `.manifold` →
  the P1 project soak (content thread, real-time pacing, baselines, thresholds);
  `.glb`/`.gltf` → import-graph mode: `assemble_import_graph` → `PresetRuntime::
  from_def_with_device` at `Rgba16Float` into a `RenderTarget`, the exact construction
  `render_import.rs` (main + module doc) already proves — **never** a wrapper project and
  **never** routed through the project loader or content thread; the two paths share the
  stats emitter, the output JSON schema (`"mode": "project" | "import"`), and the D6
  sampler plumbing, nothing else. Import mode is **report-only**: it never reads or writes
  a baseline and never sets a threshold exit code (I3/I4 are untouched by it); exit
  non-zero only on run failure (import error, convergence failure). Warmup: render until
  `runtime.io_pending()` is false AND 3 consecutive byte-stable frames
  (`readback_raw_halves`, the render-import convergence pattern), cap 300 frames, exit
  non-zero on cap; the measured window then runs with NO readbacks and NO pacing sleep —
  frames back-to-back. D5's real-time-pacing rationale (decode readahead starvation) does
  not apply: import graphs carry no wall-clock-coupled media; back-to-back is the honest
  sustained-load measurement (verified 2026-07-16 on the AMG fixture: paced and unpaced
  medians agree). Measured window: 300 frames default (`--frames N`), reporting
  min/p50/p95/max of true GPU time (`commit_and_wait_completed_timed`) and of CPU encode
  wall time, same JSON shape as project mode. Flags: `--size WxH` (import mode only,
  default 1920x1080) — passing `--size` with a `.manifold` input is a usage error, as is
  `--seconds`/`--start`/`--update-baseline` with a glb input (no-silent-fallbacks: reject,
  don't ignore); `--profile` carries identical D6 semantics (K=5 worst frames by GPU time,
  per-dispatch attribution, shares-not-totals honesty). Rejected: wrapping the glb in a
  minimal project to reuse the P1 loop — it drags the compositor/sync stack into a
  measurement whose whole point is isolating the import graph, and manufactures a fixture
  nobody performs with.

## 3. Invariants & enforcement

- **I1 — On the canonical fixture, no content-thread frame exceeds 20 ms.**
  Enforcement: P1's soak gate, exit non-zero.
- **I2 — p95 frame time does not regress >15% against the recorded baseline.**
  Enforcement: same gate, comparison step.
- **I3 — Baseline changes are deliberate.** Enforcement: baseline file lives in `docs/`
  (review surface); the soak tool refuses to write it without `--update-baseline`.
- **I4 — Profiled runs never gate.** A `--profile` run refuses `--update-baseline` and
  always exits 0 on threshold checks (it reports, it doesn't judge). Enforcement: flag
  exclusivity in the xtask + a `rg`-provable guard (P2 negative gate).

## §. Phasing

**P1 — soak mode + comparison (one session, Sonnet).**
Entry: harness loads the Liveschool fixture headlessly (re-verify the fixtures.rs anchor);
`MANIFOLD_RENDER_TRACE` produces per-frame numbers (read its implementation first — reuse
its section timers as the collector; inventory `manifold-profiler` before adding any new
counter). Read-back: this doc + DESIGN_DOC_STANDARD §5 content-thread gate + the
`hot-paths` constraints in CLAUDE.md. Deliverables: an xtask (working name
`cargo xtask perf-soak <project> --seconds N [--start <beats>] [--update-baseline]`)
emitting a stats JSON (min/p50/p95/max, worst-frame section breakdown) and a
machine-tagged baseline file; comparison + exit code per D3; `--start` seeks the
transport before soaking (D5) so runs can target a suspect passage. Gate — positive: two consecutive runs on an unchanged tree
pass against a fresh baseline (run-to-run noise < the 15% band, measured and reported);
negative: `rg -n 'update_baseline' <tool>` shows the write is flag-gated (I3). Demo: the
stats JSON plus the worst-frame breakdown, read by the orchestrator — L2. Forbidden moves:
a new timing framework beside the trace sections; averaging away spikes (p95 and max are
the deliverable, not the mean); running windowed instead of headless. Test scope: focused
(`-p` the harness crate); no workspace sweep.

**P2 — profiled attribution pass (one session, Sonnet).**
Entry: P1 landed; read `manifold-gpu` `metal/profiling.rs` top-of-module doc (the
stage-boundary/encoder-splitting contract) and `freeze_profile.rs`'s enable pattern
end-to-end before wiring anything. Deliverables: a `--profile` flag on the same xtask
(D6) — re-runs the soak window with `enable_dispatch_profiling` on the frame encoder and
emits per-node attribution JSON for the K worst frames (node/primitive label, ms,
share-of-frame; K default 5), plus a capacity check: assert-and-report if the sampler
buffer would overflow on the project's span count (verify against the Liveschool fixture
— its frame has an order of magnitude more dispatches than freeze_profile's single
presets). Gate — positive: on the Liveschool fixture, a profiled run's top-node shares
are stable across two consecutive runs (rank order of the top 5 unchanged); negative:
`rg -n 'update_baseline' <tool>` proves `--profile` cannot reach the baseline write and
cannot set a failing exit code (I4). Demo: the attribution JSON for the fixture's worst
frame, read by the orchestrator — L2. Forbidden moves: gating on profiled totals (D6);
inventing a second label scheme beside the executor's step tags; leaving overflow silent
(no-silent-fallbacks). Test scope: focused (`-p` the harness crate + `-p manifold-gpu
--lib`); GPU feature run only if profiling.rs itself is touched.

**P2b — bare-glb import-graph mode (half session, Sonnet, own executor).**
Entry: P2 landed or gate-green in the same worktree. Read end-to-end before wiring:
`render_import.rs` (whole file — the import-graph construction AND the
io_pending/byte-stability convergence loop are both load-bearing) and `profile_scene` in
`freeze_profile.rs` (the warmup + `commit_and_wait_completed_timed` measurement arm).
Deliverables: extension-dispatch on the xtask input per D7; the import-mode frame loop
(D7 warmup, then unpaced measured window, `--frames` default 300, `--size` default
1920x1080); stats JSON with `"mode": "import"`; `--profile` working on import mode with
the same attribution JSON and K=5; usage errors per D7's flag matrix. Gate — positive:
on `tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb` at `--size 3840x2160`, an
unprofiled run emits stats JSON whose GPU p50 lands in the 10–25 ms band (sanity anchor:
13.5 ms measured 2026-07-16 on the M4 Max — a wildly different number means the loop is
wrong, not the GPU), and a profiled run's top-5 rank order is stable across two
consecutive runs; negative: `rg -n 'update_baseline'` proves the baseline write is
unreachable from import mode, and `--size` on a `.manifold` input plus `--start` on a
glb input both exit with usage errors. Demo: the attribution JSON for the AMG's worst
frame — L2. Forbidden moves: a wrapper project (D7 rejected it); any pacing sleep in the
import-mode measured window; readbacks inside the measured window; a second output
schema; touching the P1 project loop. Test scope: focused (`-p` the harness crate);
GPU feature run not required (the tool itself is the GPU proof).

**P3 — wire into the protocols (half session, Sonnet).**
Entry: P1+P2+P2b landed, baseline committed. Deliverables: landing-protocol note (gate required
when a wave touched content-thread/render paths — add to the wave-gate list the
orchestrator already runs), pre-gig soak step in GIG_RESILIENCE's checklist, and a
VERIFICATION_DEBT.md line if either integration is deferred. Gate: `rg` proves both docs
name the command. Demo: none — L1.

## §. Decided — do not reopen
1. Show-level measurement, not micro-benchmarks (D1).
2. Deliberate-run posture, gpu-proofs-style (D2).
3. 20 ms absolute + 15% relative, both enforced (D3).
4. Baselines are reviewed commits, machine-tagged, rig-only semantics (D4).
5. Real-time pacing; long sets are handled by `--start` targeting, not speedup (D5).
6. Attribution is a separate profiled pass; gate numbers only from unprofiled frames (D6).
7. Bare-glb mode is a sibling frame loop — shared emitters/sampler, no wrapper project,
   report-only, unpaced measured window, D7's exact flag matrix (D7).

## §. Deferred
- Per-section budgets (compositor vs sync vs UI bridge) — revive when a whole-frame
  regression fires and the breakdown proves too coarse to assign blame.
- Windowed/live-app soak variant — revive with GIG_RESILIENCE P3–P4 (rehearsal soak),
  where the windowed present path matters.
- Memory-growth assertion during soak (leak canary) — revive on first observed
  long-session growth; cheap to bolt onto the same run when needed.
- Uncapped faster-than-realtime soak (for compressing whole-set runs) — revive if
  targeted `--start` windows prove insufficient in practice; only honest for projects
  with no wall-clock-coupled media (no video decode), and say so in the tool's output.
- Per-source-node attribution through fused kernels (splitting a fused pipeline's cost
  across its member nodes) — revive only if per-compiled-step attribution proves too
  coarse on a real regression; would need freeze-compiler cooperation, not sampler work.
