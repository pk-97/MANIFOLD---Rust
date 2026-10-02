# Live sim clock — water keeps transport time under load

**Status:** IN PROGRESS · 2026-10-03 · Codex · BUG-7qzk. Design and standalone CPU reference only; runtime integration is not built.
**Prerequisites:** BUG-gjys must land before integration; P1 is independent. P2–P4 require the decisions in section 8.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md sections 5–6 before starting any phase.

<!-- index: GPU FLIP timing audit, transport-locked live intervals, timestamped hits, reference-engine CFL rule, Box3D substeps and editor HUD lag; standalone CPU specification only. -->

Peter's ruling: “live sims are never in slow motion.” “When a frame owes more ticks than its live budget, run the budgeted steps with each covering more time, so sim time stays locked to the transport.” “Export keeps exact fixed 60 Hz ticks.” “A hit (audio or force trigger) that falls inside a stretched step still applies at its own moment.” Live water may take a different numerical trajectory from export, but cannot silently lose seconds. Authored Simulation Speed remains intentional time scaling.

Scope is **GPU FLIP water only**, including whitewater and its Box3D coupling seam. The binding constraints are timing correctness, no per-frame allocation or blocking readback, and existing content-thread ownership. This groundwork adds no serialized project state, commands, identities, locks, threads or channels.

Companions: [LIQUID_SOLVER_SEAM_DESIGN.md](LIQUID_SOLVER_SEAM_DESIGN.md) owns the present coupling seam; [GPU_WHITEWATER_DESIGN.md](GPU_WHITEWATER_DESIGN.md) owns the pool; [CORE_ENGINE_MAP.md](CORE_ENGINE_MAP.md) owns beats and transport; [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) owns native Metal access. Integration must update their affected contracts; this groundwork does not claim their current runtime has changed.

## 1. Audit — what exists (verified 2026-10-03)

Snapshot: slot-1, `feat/live-sim-clock`, base `cbb4be66f`. **Extend, don't redesign.** Paths below are relative to `crates/manifold-renderer/src/` unless explicitly repository-relative. Anchors describe this base, not the concurrent BUG-gjys result. No runtime or visual behaviour was observed.

| Consumer / assumption | File:line | State and treatment |
|---|---|---|
| Nominal tick and display interpolation | `node_graph/fluid.rs:48`, `:57` | Exists. Retain 60 Hz nominal tick and timestamp interpolation; duration must cease being implied by iteration count. |
| LiquidClock transport, epoch, Speed and tick IDs | `node_graph/liquid/clock.rs:68`, `:90`, `:119`, `:157`, `:168` | Exists. Speed integrates the preceding interval; reset/setup/backward seek create epochs. Live cap drops target seconds and reanchors. Replace dropping after BUG-gjys. |
| TickSamples replay and retention | `node_graph/liquid/tick_samples.rs:40`, `:73`, `:100`, `:129` | Exists. Samples integer tick starts; drop recovery substitutes current values. Needs interval-boundary sampling without retiming old hits. |
| Domain replay, readiness, clock and exchange | `node_graph/primitives/gpu_flip_domain.rs:462`, `:513`, `:526`, `:598`, `:669`, `:703`, `:720`, `:754`, `:769`, `:825` | Exists. Live coupling caps to 0/1; offline exchanges owed ticks. Pending input holds output. Completed rigid time is completed ticks × TICK. Carry actual endpoints. |
| State-region iteration count and index | `node_graph/primitives/liquid_state.rs:33`, `:60`, `:168`, `:264`, `:293` | Exists. Iterations advance ticks_done. Needs duration/endpoints per iteration; ordinal identity is not seconds. |
| Fields, impulses and receipts | `node_graph/liquid/fields.rs:192`, `:244`, `:275`, `:282`, `:308`, `:367` | Exists. first_tick derives from simulation_time/TICK; EventQueue has fixed TICK; one impulse lattice accepts one impulse tick per frame; hits stamp latest simulated time. Needs multiple event boundaries with existing commit/cancel receipts. |
| Body history and velocities | `node_graph/liquid/bodies.rs:377`, `:485` | Exists. TickSamples drives poses; differences divide by TICK. Divide by actual sample separation; preserve role enable changes and angular motion. |
| Coupled rigid completion and duration validation | `node_graph/liquid/coupling.rs:38`, `:296`, `:374`, `:456`, `:466` | Exists. PendingTick has ordinal/fence; settlement derives time from ordinal; exchange rejects non-TICK duration. Store interval with epoch/sequence and retire both solvers to its endpoint. |
| Box3D accumulator and worker budget | `node_graph/physics.rs:169`, `:446`, `:854`, `:1020`, `:1025`, `:1141`, `:1151`, `:1186`, `:1196`, `:1215`, `:1234` | Exists. Worker runs min(due,max_ticks), retaining debt; animation, coupling, physics_time and accumulator use TICK. Remove independent time accumulation for the coupled pair. Uncoupled conversion is deferred. |
| Native duration/substeps | repo `crates/manifold-physics/src/lib.rs:675`, `crates/manifold-physics/native/box3d/bridge.c:207`, `crates/manifold-physics/native/box3d/include/box3d/box3d.h:53` | Exists: `step(&mut self, dt: Seconds, substeps: u32)` reaches `b3World_Step(timeStep, subStepCount)`. No new native API needed. |
| GPU step duration, travel and pose offset | `node_graph/primitives/gpu_flip_step.rs:69`, `:77`, `:220`, `:230`, `:1402`, `:1403`, `:1411`, `:1492`, `:1502`, `:1513`, `:1547` | Exists. Steps clamps 1..64; dt=TICK/Steps; travel uses Top Speed × dt/h; density rate=1/dt; pose time=(k+1)dt. Supply dt and elapsed pose offset separately. |
| Body pressure parameters | `node_graph/primitives/gpu_flip_bodies.rs:52`, `:198`; `node_graph/primitives/shaders/gpu_flip_bodies.wgsl:43`, `:155`, `:190` | Exists. Pose offset arrives via uniform; pressure already represents dt·P/ρ. Never multiply captured impulse by dt twice. |
| Core WGSL time consumers | `node_graph/primitives/shaders/gpu_flip_step.wgsl:43`, `:54`, `:362`, `:394`, `:420`, `:441`, `:675`, `:745`, `:752`, `:1673`, `:1888`, `:1913`, `:1980` | Exists. Body/source pose uses tick_seconds; gravity/force and RK3/density move use step_dt; density source uses reciprocal rate. Impulse gate combines tick_index and first substep. PIC/FLIP blend is per substep. Seed encodes tick_index×64+substep, requiring new non-aliasing progression if subdivisions exceed 64. Core GPU FLIP WGSL has no independent 1/60 literal: Rust supplies the fixed assumption. |
| Pressure solver | `node_graph/primitives/shaders/gpu_flip_pressure.wgsl:1`; `node_graph/primitives/gpu_flip_step.rs:1513` | Exists. Scaled pressure, no separate fixed tick literal; density source supplies inverse duration. Prove dimensional units under variable dt. |
| Solid distance at end pose | `node_graph/primitives/liquid_solid_distance.rs:72`, `:118`; `node_graph/primitives/shaders/liquid_solid_distance_body.wgsl:35`, `:66` | Exists. Rust defaults to TICK; WGSL translates/rotates by tick_seconds. Wire actual pose offset. |
| Whitewater orchestration | `node_graph/primitives/whitewater_step.rs:297`, `:791`, `:805`, `:988`, `:1039`, `:1057`, `:1068`, `:1092` | Exists. Emits once per frame scaled by ticks, runs lifecycle once per tick with dt=TICK. Emission/advection/age/preserve need accepted durations and corresponding liquid state. |
| Hidden whitewater fixed durations | `node_graph/primitives/shaders/emission_count_body.wgsl:10`, `:29`, `:33`; `node_graph/primitives/shaders/spawn_whitewater_body.wgsl:19`, `:197` | Exists. EC_TICK sets emission rate, rounded per tick then multiplied by ticks. SW_TICK sets spawn-cylinder travel. Both need explicit dt; export must retain per-tick rounding order. |
| Whitewater duration-aware atoms | `node_graph/primitives/advect_whitewater.rs:92`, `:132`; `node_graph/primitives/age_whitewater.rs:46`, `:80`; `node_graph/primitives/shaders/advect_whitewater_body.wgsl:149`, `:187`, `:191`, `:196`, `:254`, `:256`; `node_graph/primitives/shaders/age_whitewater_body.wgsl:12`, `:22` | Exists. dt inputs already exist, Rust defaults to 1/60. Preserve receives dt at whitewater_step:1092. Per-update drag/blend/probability still need proofs. |
| Foam preservation | `node_graph/primitives/preserve_foam.rs:64`, `:128`; `node_graph/primitives/shaders/preserve_foam_body.wgsl:46`, `:83` | Exists. Rust defaults dt to 1/60; shader increases lifetime by rate × density factor × dt. Supply accepted duration. |
| Whitewater CPU oracles | `node_graph/primitives/whitewater_particle_cpu.rs:319`, `:414`; `node_graph/primitives/whitewater_pool_cpu.rs:50`, `:423`, `:449` | Exists. Spawn/emission use literal 60; lifecycle defaults use fixed dt. Parameterize live oracles alongside WGSL; retain fixed export fixtures. |
| Display publication and ring | `node_graph/primitives/liquid_frame.rs:114`, `:147`, `:211`, `:216`; `node_graph/liquid/frame_ring.rs:42`, `:77`, `:117` | Exists. Timestamped frames already interpolate unequal spacing. Retire to actual completed time, never new interval count × TICK. |
| Existing HUD | repo `crates/manifold-app/src/content_state.rs:133`, `crates/manifold-app/src/app_render.rs:3201`, `:3209`, `crates/manifold-ui/src/panels/perf_hud.rs:42`, `:186`, `:237` | Exists. Snapshot/PerfMetrics have physics_backlog_seconds; row displays seconds. Extend coverage and display “sim behind by X ms”. BUG-az3 records that the editor's own HUD never ticks. |
| CPU specification | `live_sim_clock_reference.rs` (Clock, Frame, Step, cfl_step) | Genuinely new, test-only. Reuses typed Seconds and compares unaffected behaviour against LiquidClock; no runtime alternate path. |

Re-derive with `rg -n 'TICK|FIXED_TICK|60\.0|tick_seconds|step_dt|simulation_time|display_time|ticks_done|accumulator' crates/manifold-renderer/src/node_graph/{liquid,primitives,physics.rs}` and follow GPU FLIP dependencies. Use `rg -n 'LiquidClock|TickSamples|PendingTick'` for callers. P2 must freeze the post-BUG-gjys caller inventory before editing; new/missing consumers stop the brief for lead review. Integration planning here is conformance-level under DESIGN_DOC_STANDARD section 9: pending upstream signatures are explicitly not executable migration instructions.

## 2. Decisions

**D1 — No discarded time.** Apply existing Speed anchors, then cover every owed whole 60 Hz tick. Fractional residue below one tick is quantization, not cumulative slow motion. For N owed ticks and positive budget B, emit min(N,B) equal outer intervals covering N/60 seconds. Export emits N exact nominal ticks. Zero budget means blocked: retain debt and stretch the next available frame. Rejected: dropped-time reanchoring, because it retimes the show. Rejected: capped fixed ticks with indefinitely accumulating debt, because sustained overload produces slow motion.

**D2 — Budget counts outer intervals.** Event and CFL boundaries can require additional numerical subdivisions. “B long kernels with no splits” is forbidden: it cannot preserve both hit moments and stability. **Consequences, stated honestly:** outer budget does not prove bounded wall time. The hard numerical-work bound is a blocking integration decision in section 8.

**D3 — Sample/apply by time, identify by epoch/sequence.** Preserve authored replay and event ordering/receipts. Half-open [start,end) intervals own events; ties retain input order. Integrate to the hit, apply once, then continue. Already tick-quantized hits retain their tick; never round again to a stretched edge. Held input discards impulses as today. Rejected: one frame-wide impulse lattice applied at its beginning or end.

**D4 — Preserve control semantics.** Reset counter changes (undo included), setup changes, explicit restart and backward seek reseed once. Pause retains epoch and water. Speed edits affect the interval after observation; Speed 0 marks input held but can complete the preceding interval at its previous speed, as LiquidClock does. Beats remain transport authority; Seconds belong at the sim seam. No serialization changes.

**D5 — Port CFL, do not invent it.** Use section 4's reference rule. Existing travel_cells sizes the spatial halo after dt selection; it is not a replacement timestep heuristic. Export retains fixed outer ticks and existing solver settings/ordering; adaptive live stepping must not silently retune export.

**D6 — Lag uses completed time.** Lag=max(target−completed,0), in ms. Successfully stretched time is not lag, and is never subtracted from target. Submitted GPU time is not completed time. Display delay and audio-analysis latency are separate.

## 3. Clock, events and ownership

The exact private P1 seam lives in `crates/manifold-renderer/src/live_sim_clock_reference.rs`: `Clock::advance(Input) -> Frame`, `Frame::step(u64) -> Step`, `Step::visit(&[Seconds], impl FnMut(Action))`, and `Frame::lag(Seconds) -> Seconds`. `Step { start: Seconds, end: Seconds }` and `Action::{Integrate(Step), Hit(usize)}` separate integration from event application. Input includes transport, Speed, reset, setup change, offline and budget. Frame includes epoch/restarted/held, target/sim/display times, start, interval count and stretched seconds.

Equal partition and event traversal allocate nothing; the caller owns sorted event storage. The CPU oracle assumes its planned intervals execute synchronously. It is compiled only by `#[cfg(test)]`, with no runtime feature switch or dead-code suppression. It is not an event queue replacement: existing receipts, held-event discard and commit/cancel integration remain future work.

Production must separate planning, submission and completion. The content-owned domain retains target/plans; fence retirement advances completed time. The coupled pair completes at the lesser accepted endpoint of both solvers. Reuse PendingTick and the frame fence, not a second scheduler. Transform events through the Speed segment active when observed before changing anchors. Retain replay samples until successful submission; failure cannot consume receipts or claim completion.

⚠ VERIFY-AT-IMPL: after BUG-gjys merges, P2 must specify exact old→new signatures for ClockFrame, PendingTick, TickSamples, fields and state-region duration wires. Read those files and rerun section 1's rg commands. Ultimate reusable policy belongs alongside `manifold-physics::stepping`; do not wire an additional renderer clock in production. Matter/CPU adoption is outside this slice.

Worked migrations: `TICK/steps` → accepted duration/subdivisions after CFL selection; `(completed+1)*TICK` → accepted interval.end; body pose difference/TICK → divide by sample separation; whitewater tick count as elapsed time → accepted durations. A duration scalar alone cannot replace endpoints or event timestamps.

## 4. Variable-step solver plan

### Reference engine rule

Vendored repo source `crates/manifold-fluids/native/flip_engine/fluidsimulation.cpp:11088` predicts initial source speed plus force acceleration over dt; `:11112` measures marker maximum; `:11128` considers eligible obstacles including coupled bodies; `:11163` implements `_calculateNextTimeStep`; `:11430` implements `nextUpdateTimeStep`. `fluidsimulation.h:2290`, `:2291`, `:2294` default to min 1, max 6 and CFL 5. These anchors describe the locally modified vendored reference, not current upstream. MIT credit is in the module header and THIRD_PARTY_NOTICES.md.

1. First frame/first substep uses predicted initial speed; later substeps measure current marker maximum. With fluid present/generating, take max with eligible obstacle speed.
2. With epsilon exactly 1e-6, `limit = CFL*h/(max_speed+epsilon)`.
3. Enabled surface tension also limits to `condition*sqrt(h³)*sqrt(1/(constant+epsilon))`; enabled source-color mixing to `1/(rate+epsilon)`.
4. `count=max(ceil(frame_duration/limit),1)`; return `frame_duration/count`.
5. Recompute each substep. nextUpdateTimeStep clips to remaining duration and the minimum-step schedule. Legacy internal update stretches its final allowed step to the remainder (`:11461`); externally stepped update throws on cap exhaustion with time remaining (`:11446`). Do not claim that legacy final stretch remains CFL-limited.

P1's `cfl_step` ports steps 2–4; the caller supplies measured/predicted max speed and enabled restrictions. It is not a GPU reduction or adaptive solver. Tests cover epsilon-sensitive ceil, surface tension and color limits.

Live integration chooses the earliest CFL, event or interval endpoint, advances and recomputes from the new state. Keep authored Steps as minimum subdivisions, analogous to _minFrameTimeSteps; start with reference CFL 5. Apply optional restrictions only for features actually present, not by adding GPU features. Current Top Speed is a cap, not the reference's measured maximum. Sourcing current maxima without blocking readback is a P2 blocker; stale cached maxima cannot silently replace a current bound.

### GPU, whitewater and Box3D

Each segment carries step_dt, elapsed pose offset and event metadata. Density rate stays 1/step_dt; pressure coupling retains scaled impulse units. Gravity, forces, body/source motion and RK3 use the same duration. Splitting never reapplies an impulse. Current step_in_tick==0 reset/seed logic must distinguish interval-start from event-start. Adaptive counts beyond 64 need non-aliasing seed progression.

Whitewater rates/spawn travel consume durations; lifecycle ages/advects/preserves over accepted time against the corresponding liquid state. Preserve export's nominal-tick rounding and generation order. Live time/event equality does not imply equal particle counts or trajectories across fps. PIC/FLIP blend, bubble drag, foam preservation and pressure tolerances need small CPU value proofs; dt plumbing alone is not a fluid-quality proof.

Box3D already accepts longer steps. For segment duration d, use `subStepCount=max(4,ceil(4*d/TICK))`, keeping internal resolution at most TICK/4. Retain animation/collision microstep boundaries, split at hits before PhysicsWorld::step, integrate the reaction over the same interval and apply it once. Both solvers must accept the endpoint before publication. No second fixed accumulator for the coupled pair.

### HUD

Reuse ContentState.physics_backlog_seconds → PerfMetrics.physics_backlog_seconds → existing HUD row. Aggregate maximum target−completed across active worlds including GPU FLIP retirement; reset discards old-epoch lag. Display **sim behind by X ms**, converting units in UI. At 24 fps ordinary sub-tick residue can be below 16.67 ms; in-flight work can add more. Completed stretched time must not inflate lag.

P4 resolves BUG-az3: feed metrics and explicitly tick/push the editor's own HUD through its real presentation path, with an editor toggle for that existing overlay. Its separate UIRoot lacks the main root's update route. A snapshot harness manually calling push_values is not proof. No new HUD window. Audio-analysis latency is excluded.

## 5. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| Equal time at 20/24/30/60 fps and budgets 1/2/3 | `live_sim_clock_same_time_at_20_24_30_60_fps` |
| Interior hits, ties and end boundaries apply at their moments | `live_sim_clock_hits_land_inside_long_steps` checks impulse-driven displacement |
| Export exact ticks independent of live budget | `live_sim_clock_export_unchanged` compares values with LiquidClock |
| Pause, Speed, seeks, reset/setup/explicit restart | `live_sim_clock_pause_seek_speed_reset_match_today`, `live_sim_clock_pause_resume_same_epoch` |
| Blocked target/debt retained, then stretched catch-up | `live_sim_clock_blocked_retains_debt_then_stretches` |
| Reference CFL epsilon/ceil/restrictions | `live_sim_clock_cfl_reference_rule` |
| Submitted is not completed; coupled receipts/endpoints agree | Enforcement: none in P1 — P3 must add fence/receipt tests before landing |
| Actual editor lag text updates | Enforcement: none in P1 — P4 must resolve BUG-az3 with a UI flow |
| No production clock or protected-file edits | cfg(test) registration; protected-file diff must be empty |

## 6. Phasing

### P1 — CPU groundwork (this lane)

**Entry/read-back:** bd show BUG-7qzk; verify slot-1 branch/base; read AGENTS, design standard, clock tests and CFL source. D1–D6 bind this work. Findings are dropped target time, coupled cap, fixed solver/whitewater durations. **Deliverables:** this doc, live_sim_clock_reference.rs, test-only lib registration, notices and generated index. **Gate:** `CARGO_BUILD_JOBS=4 cargo check -p manifold-renderer --tests`, then `CARGO_BUILD_JOBS=4 cargo test -p manifold-renderer --lib live_sim_clock_reference::`; focused renderer clippy with tests; `python3 scripts/gen_docs_index.py`; `git diff --check`. Cargo uses the absolute slot manifest; command-local RUSTC_WRAPPER override is allowed if sandbox blocks sccache. **Negative gate/forbidden:** no diff in clock.rs, coupling.rs, tick_samples.rs, gpu_flip_domain.rs or matter_domain.rs; no runtime registration, GPU execution or app. **Scope/demo:** CPU only, none — L1. Exact-path commit, no push; lead retains active slot for integration.

### P2 — Integration seam specification (blocked)

**Entry/read-back:** BUG-gjys merged; section-8 blockers resolved before implementation. Re-read changed anchors and re-run audit commands; restate D1–D6, caller inventory and work-bound policy. **Deliverables:** full old→new signatures/caller list for duration, endpoint and completion metadata; amend this doc to full-treatment level; CPU small-lattice proofs for speed inputs, whitewater duration and scaled pressure/body-reaction units. Reuse physics stepping and receipts. **Gate:** affected-crate check/clippy and named CPU tests; negative inventory audit for unmapped consumers. Exact test commands and a mechanically executable P3/P4 brief are outputs of this phase before worker dispatch. **Forbidden:** implementing blocked GPU scheduling, stale-speed substitution, new locks/dependencies. **Demo:** none — L1.

### P3 — One accepted interval through GPU FLIP and coupled Box3D

**Entry/read-back:** P2 full seam brief/proofs green, blockers resolved; reread all timing consumers from fresh anchors. Restate duration/endpoint mappings, event consumption and export contract. **Deliverables:** shared plans, variable GPU steps, event splits, whitewater durations, longer Box3D steps/substeps, completion receipts and affected contract updates; CPU tests named `live_interval_completion_receipts`, `live_interval_coupled_endpoints`, `live_interval_whitewater_duration`, `live_interval_export_fixed`, plus GPU value proofs. **Gate:** focused check/clippy/CPU tests; Codex compiles GPU proofs only with `cargo test -p manifold-renderer --no-run --features gpu-proofs`; lead runs scoped scripts/gpu_proofs_gate.py. P2 freezes exact commands and filters before dispatch. **Negative gate:** no TICK-derived elapsed time or dropped-target path on migrated live GPU FLIP sites; Matter remains outside scope. **Demo:** bounded lead-run coupled-water timing trace at 20/24/30/60 fps, numeric target/completed/event comparisons plus Peter's observation (L2 target). **Forbidden:** app/GPU exploration by this lane, dropped time, fixed-only fallback, batch-applied interior hits, ordinal×TICK completion. No serialized changes; retain fixed export fixtures.

### P4 — Existing editor HUD, including BUG-az3

**Entry/read-back:** P3 completion telemetry available; reread snapshot/metrics/HUD/editor presentation and BUG-az3. D6 requires completed-time lag, not stretched-time accounting. **Deliverables:** row in ms, actual editor metric/tick/toggle path; `editor-sim-lag` UI flow opening HUD, playing/pausing, asserting changing then stable held text. **Gesture:** performer opens editor HUD while water runs. **Gate:** focused app/UI check/clippy and lag formatting/aggregation tests; lead runs the named flow via scripts/run_ui_flows.py and landing gate. P2 supplies the exact flow command supported by that script before dispatch. **Demo:** flow artifact, L3 target. **Forbidden:** second HUD, UI state writes, snapshot-only push_values shortcut. No serialization changes; scope is this row and BUG-az3 plumbing.

## 7. Decided — do not reopen

1. Live covers owed time with longer budgeted outer intervals; never dropped-time reanchoring.
2. Export retains exact 60 Hz ticks and solver settings/ordering.
3. Hits split at their own moments, including multiple moments per frame.
4. Port reference CFL; Box3D uses longer steps with more substeps.
5. Preserve Speed/pause/epoch rules; scheduling delay is not authored slow motion.
6. Reuse editor HUD, report target−completed, address BUG-az3.
7. This groundwork changes none of the five protected files or production GPU behaviour.

## 8. Deferred and blocking decisions

**Blocking P2–P4, decider Peter with lead's numerical evidence:** total numerical-work limit when CFL/events require more subdivisions than the outer budget. Strict finite work, unconditional catch-up and arbitrary speed/event density cannot all be guaranteed. Recommendation: externally-stepped reference fail-explicitly behaviour on exhaustion, never hidden time loss; the bound and live failure presentation require approval. CPU clock coverage is not a performance proof.

**Blocking P2–P4, decider lead before Peter approval:** current fluid/obstacle maxima for adaptive dispatch without content-thread readback stalls. Recommendation: GPU-local current-state reduction/scheduling, initial prediction matching the reference. A bounded architecture proof must define exact seam, error propagation and tests; substituting Top Speed or delayed readback is not a faithful port.

**Deferred:** CPU fluids, Matter, particles and uncoupled Box3D adoption — revive under audited tasks after this slice proves the common contract. Audio-analysis latency — separate task. Trajectory/bitwise live-export parity — not promised; revive only with Peter's numerical-fidelity requirement. GPU/visual verification — lead integration stage. No new primitive, project format or quality control is proposed here.
