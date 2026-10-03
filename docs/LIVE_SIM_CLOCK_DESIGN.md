# Live sim clock — water keeps transport time under load

**Status:** IN PROGRESS · 2026-10-03 · Codex · BUG-7qzk. P1 reference and shared policy implemented; P2 is partial; P3 runtime clock migration is NOT implemented; P4 HUD plumbing implemented but not observed. This branch is not ready to land. No GPU or visual verification claimed. Final further Cargo work is storage-blocked at 48.3 GiB free (50 GiB reserve).
**Prerequisites:** BUG-gjys is present in slot-1 at `b424888f6`. Section 8 decisions were resolved by Peter on 2026-10-03.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md sections 5–6 before starting any phase.

<!-- index: GPU FLIP timing audit, transport-locked live intervals, timestamped hits, reference-engine CFL rule, Box3D substeps and editor HUD lag; CPU policy, duration-aware whitewater and HUD plumbing; runtime clock migration incomplete. -->

Peter's ruling: “live sims are never in slow motion.” “When a frame owes more ticks than its live budget, run the budgeted steps with each covering more time, so sim time stays locked to the transport.” “Export keeps exact fixed 60 Hz ticks.” “A hit (audio or force trigger) that falls inside a stretched step still applies at its own moment.” Live water may take a different numerical trajectory from export, but cannot silently lose seconds. Authored Simulation Speed remains intentional time scaling.

Scope is **GPU FLIP water only**, including whitewater and its Box3D coupling seam. The binding constraints are timing correctness, no per-frame allocation or blocking readback, and existing content-thread ownership. This groundwork adds no serialized project state, commands, identities, locks, threads or channels.

Companions: [LIQUID_SOLVER_SEAM_DESIGN.md](LIQUID_SOLVER_SEAM_DESIGN.md) owns the present coupling seam; [GPU_WHITEWATER_DESIGN.md](GPU_WHITEWATER_DESIGN.md) owns the pool; [CORE_ENGINE_MAP.md](CORE_ENGINE_MAP.md) owns beats and transport; [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) owns native Metal access. Integration must update their affected contracts; this groundwork does not claim their current runtime has changed.

## 1. Audit — what exists (verified 2026-10-03)

Initial P1 snapshot: slot-1, `feat/live-sim-clock`, base `cbb4be66f`. Execution restarted at verified tip `b424888f6`, including BUG-gjys. **Extend, don't redesign.** Paths below are relative to `crates/manifold-renderer/src/` unless explicitly repository-relative. Anchors describe this base, not the concurrent BUG-gjys result. No runtime or visual behaviour was observed.

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

**D2 — Budget counts outer intervals.** Event and CFL boundaries can require additional numerical subdivisions. “B long kernels with no splits” is forbidden: it cannot preserve both hit moments and stability. **Consequences, stated honestly:** outer budget does not prove bounded wall time. The numerical cap follows section 8: its last allowed substep consumes the remainder; hit splits remain mandatory.

**D3 — Sample/apply by time, identify by epoch/sequence.** Preserve authored replay and event ordering/receipts. Half-open [start,end) intervals own events; ties retain input order. Integrate to the hit, apply once, then continue. Already tick-quantized hits retain their tick; never round again to a stretched edge. Held input discards impulses as today. Rejected: one frame-wide impulse lattice applied at its beginning or end.

**D4 — Preserve control semantics.** Reset counter changes (undo included), setup changes, explicit restart and backward seek reseed once. Pause retains epoch and water. Speed edits affect the interval after observation; Speed 0 marks input held but can complete the preceding interval at its previous speed, as LiquidClock does. Beats remain transport authority; Seconds belong at the sim seam. No serialization changes.

**D5 — Port CFL, do not invent it.** Use section 4's reference rule. Existing travel_cells sizes the spatial halo after dt selection; it is not a replacement timestep heuristic. Export retains fixed outer ticks and existing solver settings/ordering; adaptive live stepping must not silently retune export.

**D6 — Lag uses completed time.** Lag=max(target−completed,0), in ms. Successfully stretched time is not lag, and is never subtracted from target. Submitted GPU time is not completed time. Display delay and audio-analysis latency are separate.

## 3. Clock, events and ownership

The exact private P1 seam lives in `crates/manifold-renderer/src/live_sim_clock_reference.rs`: `Clock::advance(Input) -> Frame`, `Frame::step(u64) -> Step`, `Step::visit(&[Seconds], impl FnMut(Action))`, and `Frame::lag(Seconds) -> Seconds`. `Step { start: Seconds, end: Seconds }` and `Action::{Integrate(Step), Hit(usize)}` separate integration from event application. Input includes transport, Speed, reset, setup change, offline and budget. Frame includes epoch/restarted/held, target/sim/display times, start, interval count and stretched seconds.

Equal partition and event traversal allocate nothing; the caller owns sorted event storage. The CPU oracle assumes its planned intervals execute synchronously. It is compiled only by `#[cfg(test)]`, with no runtime feature switch or dead-code suppression. It is not an event queue replacement: existing receipts, held-event discard and commit/cancel integration remain future work.

Production must separate planning, submission and completion. The content-owned domain retains target/plans; fence retirement advances completed time. The coupled pair completes at the lesser accepted endpoint of both solvers. Reuse PendingTick and the frame fence, not a second scheduler. Transform events through the Speed segment active when observed before changing anchors. Retain replay samples until successful submission; failure cannot consume receipts or claim completion.

⚠ STILL REQUIRED: BUG-gjys is present and the fixed-tick callers were re-audited; P2 must finish specifying exact old→new signatures for ClockFrame, PendingTick, TickSamples, fields and state-region duration wires. Read those files and rerun section 1's rg commands. Ultimate reusable policy belongs alongside `manifold-physics::stepping`; do not wire an additional renderer clock in production. Matter/CPU adoption is outside this slice.

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

Live integration chooses the earliest CFL, event or interval endpoint, advances and recomputes from the new state. Keep authored Steps as minimum subdivisions, analogous to _minFrameTimeSteps; start with reference CFL 5. Apply optional restrictions only for features actually present, not by adding GPU features. Current Top Speed is a cap, not the reference's measured maximum. Section 8 selects GPU-local current maxima without blocking readback; stale cached maxima cannot silently replace a current bound.

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

**Phase note (2026-10-03):** shared `manifold-physics::stepping` now contains allocation-free frame partitions, half-open hit traversal, reference CFL/minimum/final-cap scheduling, marker speed-limit policy, Box3D substep calculation and completion receipts. Ten focused CPU tests pass, including cap exhaustion preserving the full interval without error. All 12 renderer reference tests pass, including internal live cap timing, hit-time displacement inside its final remainder and CPU WGSL validation. This is CPU policy evidence, not production timing evidence.

### P2 — Integration seam specification

**Entry/read-back:** BUG-gjys merged; section-8 decisions recorded before implementation. Re-read changed anchors and re-run audit commands; restate D1–D6, caller inventory and work-bound policy. **Deliverables:** full old→new signatures/caller list for duration, endpoint and completion metadata; amend this doc to full-treatment level; CPU small-lattice proofs for speed inputs, whitewater duration and scaled pressure/body-reaction units. Reuse physics stepping and receipts. **Gate:** affected-crate check/clippy and named CPU tests; negative inventory audit for unmapped consumers. Exact test commands and a mechanically executable P3/P4 brief are outputs of this phase before worker dispatch. **Forbidden:** implementing blocked GPU scheduling, stale-speed substitution, new locks/dependencies. **Demo:** none — L1.

**Phase note (2026-10-03, PARTIAL):** current-state GPU reduction decision is settled in section 8. Section 9 records the implemented test-only storage seam and the remaining runtime signatures. The GPU scheduler is not registered in production; no stale readback maximum is used. Whitewater CPU duration and telemetry aggregation proofs pass. The complete executable clock/replay/coupling migration brief remains unfinished, so the P2 exit gate has not passed.

### P3 — One accepted interval through GPU FLIP and coupled Box3D

**Entry/read-back:** P2 full seam brief/proofs green, blockers resolved; reread all timing consumers from fresh anchors. Restate duration/endpoint mappings, event consumption and export contract. **Deliverables:** shared plans, variable GPU steps, event splits, whitewater durations, longer Box3D steps/substeps, completion receipts and affected contract updates; CPU tests named `live_interval_completion_receipts`, `live_interval_coupled_endpoints`, `live_interval_whitewater_duration`, `live_interval_export_fixed`, plus GPU value proofs. **Gate:** focused check/clippy/CPU tests; Codex compiles GPU proofs only with `cargo test -p manifold-renderer --no-run --features gpu-proofs`; lead runs scoped scripts/gpu_proofs_gate.py. P2 freezes exact commands and filters before dispatch. **Negative gate:** no TICK-derived elapsed time or dropped-target path on migrated live GPU FLIP sites; Matter remains outside scope. **Demo:** bounded lead-run coupled-water timing trace at 20/24/30/60 fps, numeric target/completed/event comparisons plus Peter's observation (L2 target). **Forbidden:** app/GPU exploration by this lane, dropped time, fixed-only fallback, batch-applied interior hits, ordinal×TICK completion. No serialized changes; retain fixed export fixtures.

**Phase note (2026-10-03, PARTIAL):** emission count and spawn travel now accept explicit duration through their existing freeze-codegen atoms; whitewater lifecycle forwards it. Existing GPU fixtures retain `TICK`; new value proofs compare 0.1-second output with CPU expected values. LiquidState carries submitted endpoints in existing retired readback slots and reports target-minus-completed; live GPU FLIP nonfinite particle state requests a visible reseed instead of latching the tick budget to zero. Offline and other solver fault policies remain unchanged. The production LiquidClock still drops overdue time, fields still group impulses by tick, GPU FLIP still uses `TICK/steps`, and coupled Box3D still advances fixed ticks. Consequently none of the P3 live-clock, exact interior-hit, adaptive-step, GPU speed-limit, or coupled-endpoint behaviour is claimed complete.

### P4 — Existing editor HUD, including BUG-az3

**Entry/read-back:** P3 completion telemetry available; reread snapshot/metrics/HUD/editor presentation and BUG-az3. D6 requires completed-time lag, not stretched-time accounting. **Deliverables:** row in ms, actual editor metric/tick/toggle path; `editor-sim-lag` UI flow opening HUD, playing/pausing, asserting changing then stable held text. **Gesture:** performer opens editor HUD while water runs. **Gate:** focused app/UI check/clippy and lag formatting/aggregation tests; lead runs the named flow via scripts/run_ui_flows.py and landing gate. P2 supplies the exact flow command supported by that script before dispatch. **Demo:** flow artifact, L3 target. **Forbidden:** second HUD, UI state writes, snapshot-only push_values shortcut. No serialization changes; scope is this row and BUG-az3 plumbing.

**Phase note (2026-10-03, PARTIAL):** ContentState and PerfMetrics carry cap/nonfinite flags; the existing HUD displays completed-time lag in milliseconds and explicit warning/error rows. The editor gets the same metric snapshot and ticks its own overlay in `editor_bridge` immediately before presentation. Shift+backtick toggles that HUD; bare backtick retains the existing debug overlay. The clock-source label now borrows its static name, avoiding a new per-frame String allocation. Four UI CPU tests and the app `editor_tick_pushes_live_sim_hud_values` CPU test pass. No observed editor play/pause flow or GPU-backed cap producer exists yet; BUG-az3 is not declared verified or closed.

## 7. Decided — do not reopen

1. Live covers owed time with longer budgeted outer intervals; never dropped-time reanchoring.
2. Export retains exact 60 Hz ticks and solver settings/ordering.
3. Hits split at their own moments, including multiple moments per frame.
4. Port reference CFL; Box3D uses longer steps with more substeps.
5. Preserve Speed/pause/epoch rules; scheduling delay is not authored slow motion.
6. Reuse editor HUD, report target−completed, address BUG-az3.
7. The original P1 reference changed no runtime timing. The current partial implementation changes duration inputs and diagnostics; it does not establish a working live-clock migration.

## 8. Resolved decisions and deferred scope

**Peter's ruling, 2026-10-03 — binding for P1–P4:** live simulations never run in unintended slow motion, discard time, crash, panic, stop or freeze the show. Simulation time stays locked to transport (with authored Simulation Speed). CFL and hit boundaries may require more work than the live outer budget: execute the work and let the frame run long. The perf HUD reports completed-time lag and clearly warns whenever the frame hits its numerical substep cap.

Port the internal FLIP Fluids `nextUpdateTimeStep` rule exactly: when `_currentFrameTimeStepNumber == _maxFrameTimeSteps - 1`, that final numerical substep takes **all remaining frame time**, even when this bends CFL. Never port the externally-stepped cap-exhaustion throw to live. Hits inside that final stretched step still split integration at their own timestamps; event boundaries do not discard its remainder. Port `_getMarkerParticleSpeedLimit` including MANIFOLD's final `max(maxspeed, _maxFrameTimeSteps * speedLimitStep)`: a relative outlier that fits the configured frame's CFL/substep allowance must survive. Only genuinely non-finite state may report a numerical error, and the show must continue with the HUD reporting it. Export remains exact fixed 60 Hz outer ticks and keeps its ordering/settings.

**Maxima decision — GPU-local current-state reduction:** reduce current marker velocities and eligible obstacle point velocities immediately before GPU timestep selection, in encoder order. First frame/first substep uses source-speed prediction plus constant-force acceleration over the frame, as the reference does. The result stays in GPU storage; the timestep scheduler consumes it there. Neither Top Speed nor last-frame readback substitutes for current maxima. Existing completion fences may retire diagnostic flags/endpoints; they must never stall the content thread to fetch a CFL maximum. The exact storage/consumer seam and value tests are recorded in P2 below as they are implemented.

**Deferred:** CPU fluids, Matter, particles and uncoupled Box3D adoption; revive after this slice proves the common contract. Audio-analysis latency is separate. Live/export trajectory or bitwise equality is not promised. GPU/visual execution belongs to the lead. No serialized project-format change or new quality control is authorized.


## 9. Current implementation seam and outstanding work

### Implemented CPU and GPU proof seam

`manifold_physics::stepping` owns `StepInterval { start: Seconds, end: Seconds }`, `FramePlan { start, end, intervals }`, `LiveStepSchedule::new(start, frame_duration, min_steps, max_steps)` and `next(current_cfl_duration) -> Result<Option<ScheduledStep>, LiveStepError>`. `ScheduledStep` carries `interval` and `hit_cap`; cap exhaustion is not an error. `StepInterval::visit_hits` emits integrate/hit actions without allocation and preserves input order at equal timestamps. `CompletionReceipt { epoch, sequence, interval }` and `CompletionLedger::retire` reject stale/out-of-order or discontinuous completion; the caller must first establish fence retirement. They do not themselves wait on a GPU or imply submission completed.

`gpu_flip_clock.rs` is registered only under `cfg(all(test, feature="gpu-proofs"))`. Its constructor allocates persistent ping-pong reduction scratch sized from input capacities, three 16-byte maxima/status records, and a 32-byte plan. `begin_frame(encoder, &GpuFlipClockParams)` initializes the GPU cursor; `dispatch(encoder, GpuFlipClockInputs, &params) -> GpuFlipClockPlan` reduces current marker/source/obstacle buffers and updates that cursor in encoder order. Every reduction pass has workgroup barriers plus explicit encoder buffer barriers. Zero-sized populations clear scratch, including after a previous nonempty frame. The plan is `{ step_dt, elapsed, remaining, maximum_speed, cap_hit, nonfinite, step_index, pad }`; downstream kernels are intended to bind this storage directly. Only proofs copy it back after completion.

Marker inputs use the existing 32-byte FluidParticle layout and exclude radius-zero unused slots. Body inputs use 96-byte records: position/eligibility, linear velocity, acceleration, angular velocity, angular acceleration and centroid. The producer must mark all enabled coupled hull vertices eligible, including vertices outside the liquid box; noncoupled obstacle eligibility follows the engine domain check. Source velocity includes authored fluid velocity and enabled object motion. The initial-frame flag is supplied only for the first frame; later scheduling calls use marker state. Source prediction adds force magnitude times frame duration, without allowing vector cancellation. Coupled point speed uses the maximum of initial and predicted endpoint norms from `RigidFluidCoupling::pointSpeed`. **The runtime producer for these body records and the downstream GPU plan consumer are not implemented.**

EmissionCount and SpawnWhitewater add an optional `dt` scalar with a fixed-60-Hz default. Their body snippets receive it through the existing freeze-codegen mechanism; no alternate hand shader replaces those atoms. `WhitewaterStep::StepFrame` carries `dt`, forwarding it to emission, spawn, advection, age and preservation. Export fixtures explicitly supply `TICK`. This plumbing does not supply the missing accepted live interval from the clock.

### Remaining production migration (not executable or completed)

| Current seam | Required migration still outstanding |
|---|---|
| `liquid/clock.rs::ClockFrame { ticks, simulation_time, dropped_seconds, ... }` and `LiquidClock::advance` | Use a shared transport-target plan with typed endpoints and separate submission/completion; remove live dropped-time reanchoring for GPU FLIP. Retain Matter behaviour until its separate adoption. |
| `tick_samples.rs::TickSamples::{request,observe,settle,span,get}` indexed by tick ordinal | Request and retain authored samples by actual interval boundaries; sequence identifies work but never derives elapsed seconds. Existing replay/receipt commit-cancel semantics must survive. |
| `fields.rs` event queue and one-tick impulse lattice | Preserve original timestamps, map through the observed Speed segment, split at every hit, and consume once only after accepted submission. Remove the multiple-impulse-ticks refusal only after the replacement exists. |
| `gpu_flip_domain.rs` iteration and preset duration wires | Publish actual accepted endpoints/duration per region iteration; connect plan/status buffers to the step and completion path. Current target-time output is diagnostics only. |
| `gpu_flip_step.rs` and its shader, pressure/body/source passes | Consume GPU plan duration/elapsed offset, recompute maxima each numerical step, implement capped remainder/event splits and the GPU marker speed-limit/removal port; preserve fixed export ordering. The helper alone changes none of these passes. |
| `liquid/coupling.rs::PendingTick { tick, stamp }` and `LiquidRigidOwner::settle_ready` | Carry `CompletionReceipt`; advance rigid state over the same accepted interval after reaction retirement, publish the lesser solver endpoint and retain exact impulse units. |
| `physics.rs::RigidSimulation::advance_worker` fixed accumulator and body pose differences divided by `TICK` | Add an accepted-interval entry point with no independent coupled accumulator, use `max(4,ceil(4*d/TICK))` Box3D substeps, and use actual pose sample separation. |
| LiquidState fence diagnostics and HUD cap field | Connect retired cap flags from the real scheduler. Currently the runtime telemetry call supplies `false` for cap; the displayed warning has only CPU/UI fixture coverage. Verify NaN recovery with both water and coupled bodies. |
| P4 `editor-sim-lag` flow | Extend the real editor flow harness to play/pause and assert changing then held lag. The current script fixture builder does not expose the editor scene, so no fake snapshot-only flow was added. |

No complete files have been established as redundant. After migration, remove GPU FLIP dropped-time branches, fixed-tick elapsed derivations, and the single-impulse-tick guard where replaced; `clock.rs` and `tick_samples.rs` still have Matter/current runtime callers and are not deletion candidates. No files were deleted.

### Exact lead-run GPU proofs

P1 has no GPU tests. P2 architecture proofs (full names):

- `node_graph::primitives::gpu_flip_clock::gpu_tests::gpu_flip_clock_marker_reduction_value_proof`
- `node_graph::primitives::gpu_flip_clock::gpu_tests::gpu_flip_clock_obstacle_acceleration_angular_value_proof`
- `node_graph::primitives::gpu_flip_clock::gpu_tests::gpu_flip_clock_source_prediction_and_restrictions_value_proof`
- `node_graph::primitives::gpu_flip_clock::gpu_tests::gpu_flip_clock_cap_and_nonfinite_value_proof`

P3 duration atoms and affected fusion/export ordering:

- `node_graph::primitives::whitewater_particle_tests::live_interval_whitewater_emission_duration`
- `node_graph::primitives::whitewater_particle_tests::live_interval_whitewater_spawn_duration`
- `node_graph::primitives::whitewater_particle_tests::whitewater_emitter_chain_fused_matches_unfused`
- `node_graph::primitives::whitewater_particle_tests::whitewater_spawn_chain_fused_matches_unfused`
- `node_graph::primitives::whitewater_particle_tests::emission_count_rounds_per_tick`
- Integration binary `gpu_proofs`: `liquid_conformance::liquid_nonfinite_live_flip_reseeds_without_stopping_clock` (new 16-cubed live fixture), plus existing `liquid_conformance::liquid_nonfinite_tick_not_published` (offline policy).

Run from slot-1, one command at a time:

```sh
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= python3 scripts/gpu_queue.py --label live-clock-p2 -- cargo test -p manifold-renderer --features gpu-proofs --lib gpu_flip_clock::gpu_tests:: -- --test-threads=1
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= python3 scripts/gpu_queue.py --label live-clock-p3 -- cargo test -p manifold-renderer --features gpu-proofs --lib whitewater_particle_tests:: -- --test-threads=1
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= python3 scripts/gpu_queue.py --label live-clock-recovery -- cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs liquid_conformance::liquid_nonfinite -- --test-threads=1
```

`scripts/gpu_scope.py` adds narrow mappings from `gpu_flip_clock.rs` and `shaders/gpu_flip_clock.wgsl` to `gpu_flip_clock::gpu_tests::`; and from `emission_count.rs`, `spawn_whitewater.rs` and their two `_body.wgsl` files to `whitewater_particle_tests::`. `ScopeTests.test_live_clock_and_duration_atoms_select_value_proofs` passes. The existing whitewater-step/state/domain mappings still select their sibling proofs; these new filters are not a substitute for the scoped landing gate after P3 is complete.

P4 has no GPU test pretending to prove the live editor path. The required `editor-sim-lag` flow is missing. For an eventual bounded manual observation, launch this exact checkout under the queue, open the graph editor and use Shift+backtick:

```sh
cd '/Users/peterkiemann/MANIFOLD - Rust/.claude/worktrees/slot-1'
CARGO_BUILD_JOBS=4 RUSTC_WRAPPER= python3 scripts/gpu_queue.py --label live-clock-editor -- cargo run -p manifold-app
```

No GPU proof, render, or app was run by this lane. No commit or push was made. BUG-7qzk and BUG-az3 remain open.


### Validation record (2026-10-03)

All Cargo commands ran in slot-1 with `CARGO_BUILD_JOBS=4 RUSTC_WRAPPER=` and only one Cargo process at a time.

| Command after that environment prefix | Result |
|---|---|
| `cargo test -p manifold-physics --lib stepping::tests::live_` | PASS: 10 CPU tests, including full-time cap completion and the MANIFOLD speed-limit floor. An initial fixture float-type error was fixed before this passing run. |
| `cargo test -p manifold-renderer --lib live_sim_clock_reference::` | PASS: 12 CPU tests, including 20/24/30/60 fps, export fixed ticks, pause/Speed/reset, cap hits, interior hit displacement, and Naga shader validation. Initial unrelated concurrent external-step model edits had two Seconds/f64 type errors; that model was subsequently replaced by the binding internal live rule. |
| `cargo test -p manifold-renderer --lib live_interval_` | PASS: 2 CPU tests: whitewater duration and lag/flag aggregation. |
| `cargo test -p manifold-ui --lib panels::perf_hud::tests::` | PASS: 4 CPU tests. |
| `cargo test -p manifold-app --bin manifold ui_root::tick_parity_tests::editor_tick_pushes_live_sim_hud_values` | PASS: 1 CPU test; no app window or GPU was created. |
| `cargo check -p manifold-physics -p manifold-renderer -p manifold-ui -p manifold-app` | PASS. Later recovery scoping and fixture edits also compiled through clippy. |
| `cargo clippy -p manifold-physics -p manifold-ui -p manifold-app --tests -- -D warnings` | PASS. Existing native AVFoundation deprecation warnings were emitted by the media build script. |
| `cargo clippy -p manifold-renderer --tests --features gpu-proofs -- -D warnings` | PASS before the final one-line spawn-fusion duration fixture edit. That final fixture compiled in the successful GPU-proof no-run build. The final clippy rerun was blocked by storage admission at 48.3 GiB free against the 50 GiB reserve, so exact-final-state clippy is unverified. |
| `cargo test -p manifold-renderer --features gpu-proofs --no-run` | PASS: all renderer GPU-proof test binaries compile; none executed. |

The storage guard initially blocked Cargo at 49.1 GiB free against its 50 GiB reserve. Free space later rose above the reserve without this lane deleting files, and required checks resumed. After the required GPU-proof compilation, free space fell to 48.3 GiB and the final clippy rerun was refused. No cleanup or guard bypass was attempted. The production migration was already incomplete before this final storage block.

`scripts/codex_checks.py --base b424888f6 --json` selects the four touched crates and reports no unmapped GPU paths. Its CPU tooling checks: `test_codex_checks.py` PASS (8); `test_gpu_scope.py` PASS (35). `test_landing_gate.py` FAIL (31 pass, one failure): `LandingTests.test_timeout_retains_partial_output_and_kills_grandchildren` measured 63.02 seconds against `<60`; the timeout implementation was not changed. `test_gpu_proofs_gate.py` was blocked by the execution-budget hook, including after one exact-command permit, so its Python unittests did not run. This was not a GPU-test execution attempt. No guards or tooling were weakened. The generated docs index was refreshed; `git diff --check` passes.

### Changed files by phase

- P1: `crates/manifold-physics/src/stepping.rs`, `crates/manifold-renderer/src/live_sim_clock_reference.rs`, `THIRD_PARTY_NOTICES.md`.
- P2: new `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_clock.rs` and `shaders/gpu_flip_clock.wgsl`, test-only registration in `primitives/mod.rs`, plus `scripts/gpu_scope.py` and `scripts/test_gpu_scope.py`.
- P3 partial: under `crates/manifold-renderer/src/node_graph/`, `physics_metrics.rs`, primitives `gpu_flip_domain.rs`, `gpu_flip_preset.rs`, `liquid_state.rs`, `emission_count.rs`, `spawn_whitewater.rs`, `whitewater_step.rs`, the emission/spawn `_body.wgsl` snippets, `whitewater_particle_cpu.rs`, `whitewater_particle_tests.rs`, `whitewater_step_tests.rs`; plus `crates/manifold-renderer/tests/gpu_proofs/liquid_conformance.rs`.
- P4 partial: `crates/manifold-ui/src/panels/perf_hud.rs`; app `app_render.rs`, `content_state.rs`, `content_thread.rs`, `editor_bridge.rs`, `ui_root/mod.rs`, `window_input.rs`, `ui_snapshot/render.rs`.
- Contracts/status: this document, `docs/GPU_WHITEWATER_DESIGN.md`, `docs/LIQUID_SOLVER_SEAM_DESIGN.md`, generated `docs/README.md`.
