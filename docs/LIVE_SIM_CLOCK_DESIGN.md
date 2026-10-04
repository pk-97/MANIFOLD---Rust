# Live sim clock — capped fixed steps, slow motion under overload

**Status:** APPROVED rework, not built · 2026-10-04 · the shipped catch-up clock is retired; the capped fixed-step clock is BUG-g75v.11 (capped live clock rework) in BUG-g75v (GPU water campaign). Owed: that rework and an observed editor render of the HUD rows. See section 8 (Resolved decisions).

<!-- index: Live physics clock: fixed Sim Rate steps capped per frame, leftover time dropped under overload (rework approved 2026-10-04); timestamped hits, reference-engine CFL rule, Box3D substeps and editor HUD lag. -->

Peter's ruling, 2026-10-04: live sims follow the real-time sim and fluid-tool model (games, EmberGen, Notch, TouchDesigner). Each frame runs at most two fixed Sim Rate steps and drops any leftover time, so an overloaded machine plays the water in slow motion instead of exploding it. “Never go slow Mo was too ambitious and physically not possible.” Hits still apply at their own moments. Authored Simulation Speed remains intentional time scaling. The audit, decisions and implementation seam below describe the shipped catch-up clock this ruling retires; section 8 (Resolved decisions) is the contract.

Scope is **every live physics consumer**: GPU FLIP and whitewater, MPM, coupled and uncoupled Box3D, CPU FLIP, and stateless particle steps. The binding constraints are timing correctness, no per-frame allocation or blocking readback, and existing content-thread ownership. This groundwork adds no serialized project state, commands, identities, locks, threads or channels.

Companions: [LIQUID_SOLVER_SEAM_DESIGN.md](LIQUID_SOLVER_SEAM_DESIGN.md) owns the present coupling seam; [GPU_WHITEWATER_DESIGN.md](GPU_WHITEWATER_DESIGN.md) owns the pool; [CORE_ENGINE_MAP.md](CORE_ENGINE_MAP.md) owns beats and transport; [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) owns native Metal access. Section 9 records the runtime migration and its remaining verification.

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
| Box3D accumulator and worker budget | `node_graph/physics.rs:169`, `:446`, `:854`, `:1020`, `:1025`, `:1141`, `:1151`, `:1186`, `:1196`, `:1215`, `:1234` | Exists. Worker runs min(due,max_ticks), retaining debt; animation, coupling, physics_time and accumulator use TICK. Remove independent time accumulation for the coupled pair. Uncoupled live conversion is included in P3. |
| Native duration/substeps | repo `crates/manifold-physics/src/lib.rs:675`, `crates/manifold-physics/native/box3d/bridge.c:207`, `crates/manifold-physics/native/box3d/include/box3d/box3d.h:53` | Exists: `step(&mut self, dt: Seconds, substeps: u32)` reaches `b3World_Step(timeStep, subStepCount)`. No new native API needed. |
| GPU step duration, travel and pose offset | `node_graph/primitives/gpu_flip_step.rs:69`, `:77`, `:220`, `:230`, `:1402`, `:1403`, `:1411`, `:1492`, `:1502`, `:1513`, `:1547` | Exists. Steps clamps 1..64; dt=TICK/Steps; travel uses Top Speed × dt/h; density rate=1/dt; pose time=(k+1)dt. Supply dt and elapsed pose offset separately. |
| Body pressure parameters | `node_graph/primitives/gpu_flip_bodies.rs:52`, `:198`; `node_graph/primitives/shaders/gpu_flip_bodies.wgsl:43`, `:155`, `:190` | Exists. Pose offset arrives via uniform; pressure already represents dt·P/ρ. Never multiply captured impulse by dt twice. |
| Core WGSL time consumers | `node_graph/primitives/shaders/gpu_flip_step.wgsl:43`, `:54`, `:362`, `:394`, `:420`, `:441`, `:675`, `:745`, `:752`, `:1673`, `:1888`, `:1913`, `:1980` | Exists. Body/source pose uses tick_seconds; gravity/force and RK3/density move use step_dt; density source uses reciprocal rate. Impulse gate combines tick_index and first substep. PIC/FLIP blend is per substep. Seed encodes tick_index×64+substep, requiring new non-aliasing progression if subdivisions exceed 64. Core GPU FLIP WGSL has no independent 1/60 literal: Rust supplies the fixed assumption. |
| Pressure solver | `node_graph/primitives/shaders/gpu_flip_pressure.wgsl:1`; `node_graph/primitives/gpu_flip_step.rs:1513` | Exists. Scaled pressure, no separate fixed tick literal; density source supplies inverse duration. Prove dimensional units under variable dt. |
| Solid distance at end pose | `node_graph/primitives/liquid_solid_distance.rs:72`, `:118`; `node_graph/primitives/shaders/liquid_solid_distance_body.wgsl:35`, `:66` | Exists. Rust defaults to TICK; WGSL translates/rotates by tick_seconds. Wire actual pose offset. |
| Whitewater orchestration | `node_graph/primitives/whitewater_step.rs:297`, `:791`, `:805`, `:988`, `:1039`, `:1057`, `:1068`, `:1092` | Exists. Emits once per frame scaled by ticks, runs lifecycle once per tick with dt=TICK. Emission/advection/age/preserve need accepted durations and corresponding liquid state. |
| Hidden whitewater fixed durations | `node_graph/primitives/shaders/emission_count_body.wgsl:10`, `:29`, `:33`; `node_graph/primitives/shaders/spawn_whitewater_body.wgsl:19`, `:197` | Exists. EC_TICK sets emission rate, rounded per tick then multiplied by ticks. SW_TICK sets spawn-cylinder travel. Both need explicit dt. |
| Whitewater duration-aware atoms | `node_graph/primitives/advect_whitewater.rs:92`, `:132`; `node_graph/primitives/age_whitewater.rs:46`, `:80`; `node_graph/primitives/shaders/advect_whitewater_body.wgsl:149`, `:187`, `:191`, `:196`, `:254`, `:256`; `node_graph/primitives/shaders/age_whitewater_body.wgsl:12`, `:22` | Exists. dt inputs already exist, Rust defaults to 1/60. Preserve receives dt at whitewater_step:1092. Per-update drag/blend/probability still need proofs. |
| Foam preservation | `node_graph/primitives/preserve_foam.rs:64`, `:128`; `node_graph/primitives/shaders/preserve_foam_body.wgsl:46`, `:83` | Exists. Rust defaults dt to 1/60; shader increases lifetime by rate × density factor × dt. Supply accepted duration. |
| Whitewater CPU oracles | `node_graph/primitives/whitewater_particle_cpu.rs:319`, `:414`; `node_graph/primitives/whitewater_pool_cpu.rs:50`, `:423`, `:449` | Exists. Spawn/emission use literal 60; lifecycle defaults use fixed dt. Parameterize live oracles alongside WGSL. |
| Display publication and ring | `node_graph/primitives/liquid_frame.rs:114`, `:147`, `:211`, `:216`; `node_graph/liquid/frame_ring.rs:42`, `:77`, `:117` | Exists. Timestamped frames already interpolate unequal spacing. Retire to actual completed time, never new interval count × TICK. |
| Existing HUD | repo `crates/manifold-app/src/content_state.rs:133`, `crates/manifold-app/src/app_render.rs:3201`, `:3209`, `crates/manifold-ui/src/panels/perf_hud.rs:42`, `:186`, `:237` | Exists. Snapshot/PerfMetrics have physics_backlog_seconds; row displays seconds. Extend coverage and display “sim behind by X ms”. BUG-az3 records that the editor's own HUD never ticks. |
| CPU specification | `live_sim_clock_reference.rs` (Clock, Frame, Step, cfl_step) | Genuinely new, test-only. Reuses typed Seconds and compares unaffected behaviour against LiquidClock; no runtime alternate path. |

Re-derive with `rg -n 'TICK|FIXED_TICK|60\.0|tick_seconds|step_dt|simulation_time|display_time|ticks_done|accumulator' crates/manifold-renderer/src/node_graph/{liquid,primitives,physics.rs}` and follow GPU FLIP dependencies. Use `rg -n 'LiquidClock|TickSamples|PendingTick'` for callers. P2 must freeze the post-BUG-gjys caller inventory before editing; new/missing consumers stop the brief for lead review. Integration planning here is conformance-level under DESIGN_DOC_STANDARD section 9: pending upstream signatures are explicitly not executable migration instructions.

## 2. Decisions

**D1 — No discarded time.** Apply the existing Speed anchors, then accept every owed Sim Rate interval up to the last boundary transport has reached (D7); a partial interval stays owed until its boundary. Live runs the owed intervals as one span, export runs one tick each. One sequence identifies that span; it does not measure elapsed time. CFL selects numerical subdivisions; the last allowed FLIP substep takes the entire remainder. Event boundaries split that accepted substep without spending another numerical step. Rejected: dropped-time reanchoring and capped fixed-tick debt bursts, both of which retime the show.

**D2 — Budget counts outer intervals.** Event and CFL boundaries can require additional numerical subdivisions. “B long kernels with no splits” is forbidden: it cannot preserve both hit moments and stability. **Consequences, stated honestly:** outer budget does not prove bounded wall time. The numerical cap follows section 8: its last allowed substep consumes the remainder; hit splits remain mandatory.

**D3 — Sample/apply by time, identify by epoch/sequence.** Preserve authored replay and event ordering/receipts. Half-open [start,end) intervals own events; ties retain input order. Integrate to the hit, apply once, then continue. Already tick-quantized hits retain their tick; never round again to a stretched edge. Held input discards impulses as today. Rejected: one frame-wide impulse lattice applied at its beginning or end.

**D4 — Preserve control semantics.** Reset counter changes (undo included), setup changes, explicit restart and backward seek reseed once. Pause retains epoch and water. `SimulationClock` integrates Speed anchors on the transport timeline; source capture, rigid impulses, fluid workers, GPU domains and offline history drain use that mapping. An edit applies from its observation or the accepted transport endpoint, whichever is later. A late source observation reads its original historical time but cannot retime accepted work. Accepted frames retain immutable history snapshots, so later edits and export-frame partitioning cannot change their interval endpoints. Speed 0 marks input held but can complete the preceding interval at its previous speed. Beats remain transport authority; Seconds belong at the sim seam.

**D5 — Port CFL, do not invent it.** Use section 4's reference rule. Existing travel_cells sizes the spatial halo after dt selection; it is not a replacement timestep heuristic.

**D6 — Lag uses completed time.** Lag=max(target−completed,0), in ms. Successfully stretched time is not lag, and is never subtracted from target. Submitted GPU time is not completed time. Display delay and audio-analysis latency are separate.

**D7 — Shared Sim Rate (Peter, 2026-10-03).** `ProjectSettings.physics.simRate` (shared `manifold_foundation::settings::PhysicsSettings`, re-exported by the physics API) sets one boundary grid for live and export: 15/20/30/60 Hz, new projects 30 Hz, missing saved values 60 Hz, one epoch per undoable rate edit, per-solver stability subdivisions unchanged. Both modes accept the same boundaries, so equal clock inputs give equal intervals; authored values are sampled on them. The display time is the simulation one interval of transport ago, read through the Speed history, so it stays inside the accepted pair however the frames jitter.


## 3. Clock, events and ownership

`manifold_physics::clock::SimulationClock` owns transport/Speed anchors, epochs and accepted `ClockFrame` intervals. `manifold_physics::stepping` owns `FramePlan`, `StepInterval`, CFL/minimum/final-cap scheduling, event traversal and completion receipts. Numerical helpers return `LiveStepOutcome<T>`: a defined value plus an advisory diagnostic, never a live-frame stopping `Err`. Stateless particle consumers use the physics duration adapter on the playback-owned delta.

Ordinals identify accepted work in both modes. Authored samples and pose velocities use accepted interval boundaries. `EventQueue::begin_interval` delivers original source timestamps in half-open intervals. GPU scheduling consumes current-state maxima directly in encoder order. Fenced state readbacks carry completion endpoints and cap/error flags; submission alone never establishes completion. Coupled rigid settlement uses the fluid interval stored in `PendingTick`.

GPU FLIP records one slot when fenced marker stats describe the exact incoming tick and prove that the first CFL step finishes the interval. Freshness compares integer tick ordinals in LiquidState, not rounded times; stale/missing readbacks, later intervals in the same frame, reset and recovery cannot authorize the shortcut. It currently requires Steps 1, no clock obstacle/source vertices or hits, and no dynamic coupling or narrow-band transition. A 1% margin from the CFL ceil boundary absorbs CPU/GPU rounding without changing the GPU scheduler, its six-step cap or its physics. The published history count is the recorded count, so retained rows from older frames are never consumed. Other cases still record the maximum numerical-slot shape; removing that remaining CPU preparation is unfinished (BUG-e6hdz (idle slot cost)).

An inactive slot's solve, pocket sweeps, over-C passes, tile retire and history face gathers take zero threadgroups. Clock reductions return before scanning populations or writing scratch: scheduling checks remaining time, while post-step cleanup checks the accepted duration so the final active step still removes outliers. Empty populations retain cleared aggregates without a scratch clear or reduction. Public faces and body reactions need no backup/restore masks because all their writers use the actual clock plan. Particle, distance, capped and narrow-history masks remain, as do the small schedule-publication and argument commands. An inactive slot must preserve persistent state, including the separating-solid pressure mask; density-solve scratch cannot update the next active step’s constraints. Replay storage reserves the full measured shape between visits. Matter selects its interval subdivision count from the same f32 duration the GPU consumes, so f64 transport subtraction noise cannot change an export’s numerical schedule.

The test-only `live_sim_clock_reference.rs` remains a CPU specification oracle. It is not a second runtime clock. GPU FLIP retains the reference maximum of six numerical steps even if authored Steps requests a larger minimum; the native final-step rule then owns the remainder.

## 4. Variable-step solver plan

### Reference engine rule

Vendored repo source `crates/manifold-fluids/native/flip_engine/fluidsimulation.cpp:11088` predicts initial source speed plus force acceleration over dt; `:11112` measures marker maximum; `:11128` considers eligible obstacles including coupled bodies, but only under rigid coupling or adaptive obstacle time stepping (`_isAdaptiveObstacleTimeSteppingEnabled`, off by default); `:11163` implements `_calculateNextTimeStep`; `:11430` implements `nextUpdateTimeStep`. `fluidsimulation.h:2290`, `:2291`, `:2294` default to min 1, max 6 and CFL 5. These anchors describe the locally modified vendored reference, not current upstream. MIT credit is in the module header and THIRD_PARTY_NOTICES.md.

1. First frame/first substep uses predicted initial speed; later substeps measure current marker maximum. With fluid present/generating and a coupled body present, take max with eligible obstacle speed. GPU FLIP has no adaptive-obstacle control, so an uncoupled collider never splits an interval: a collider that jumps between frames would otherwise sweep the water along its path at the jump's speed (`LiquidBodies::prepare_clock_vertices`).
2. With epsilon exactly 1e-6, `limit = CFL*h/(max_speed+epsilon)`.
3. Enabled surface tension also limits to `condition*sqrt(h³)*sqrt(1/(constant+epsilon))`; enabled source-color mixing to `1/(rate+epsilon)`.
4. `count=max(ceil(frame_duration/limit),1)`; return `frame_duration/count`.
5. Recompute each substep. nextUpdateTimeStep clips to remaining duration and the minimum-step schedule. Legacy internal update stretches its final allowed step to the remainder (`:11461`); externally stepped update throws on cap exhaustion with time remaining (`:11446`). Do not claim that legacy final stretch remains CFL-limited.

P1's `cfl_step` ports steps 2–4; the caller supplies measured/predicted max speed and enabled restrictions. It is not a GPU reduction or adaptive solver. Tests cover epsilon-sensitive ceil, surface tension and color limits.

Live integration chooses the earliest CFL, event or interval endpoint, advances and recomputes from the new state. Keep authored Steps as minimum subdivisions, analogous to _minFrameTimeSteps; start with reference CFL 5. Apply optional restrictions only for features actually present, not by adding GPU features. Current Top Speed is a cap, not the reference's measured maximum. Section 8 selects GPU-local current maxima without blocking readback; stale cached maxima cannot silently replace a current bound.

### GPU, whitewater and Box3D

Each segment carries step_dt, elapsed pose offset and event metadata. Density rate stays 1/step_dt; pressure coupling retains scaled impulse units. Gravity, forces, body/source motion and RK3 use the same duration. Splitting never reapplies an impulse. Current step_in_tick==0 reset/seed logic must distinguish interval-start from event-start. Adaptive counts beyond 64 need non-aliasing seed progression.

Whitewater rates/spawn travel consume durations, including the BUG-imy3.1 turbulence and dust counts, dust spawn travel, and influence decay/spread supplied with `StepFrame.dt`; lifecycle ages/advects/preserves over accepted time against the corresponding liquid state. Live time/event equality does not imply equal particle counts or trajectories across fps. PIC/FLIP blend, bubble drag, foam preservation and pressure tolerances need small CPU value proofs; dt plumbing alone is not a fluid-quality proof.

Box3D already accepts longer steps. For segment duration d, use `subStepCount=max(4,ceil(4*d/TICK))`, keeping internal resolution at most TICK/4. Retain animation/collision microstep boundaries, split at hits before PhysicsWorld::step, integrate the reaction over the same interval and apply it once. Both solvers must accept the endpoint before publication. No second fixed accumulator for the coupled pair.

### HUD

Reuse ContentState.physics_backlog_seconds → PerfMetrics.physics_backlog_seconds → existing HUD row. Aggregate maximum target−completed across active worlds including GPU FLIP retirement; reset discards old-epoch lag. Display **sim behind by X ms**, converting units in UI. At 24 fps ordinary sub-tick residue can be below 16.67 ms; in-flight work can add more. Completed stretched time must not inflate lag.

P4 resolves BUG-az3: feed metrics and explicitly tick/push the editor's own HUD through its real presentation path, with an editor toggle for that existing overlay. Its separate UIRoot lacks the main root's update route. A snapshot harness manually calling push_values is not proof. No new HUD window. Audio-analysis latency is excluded.

## 5. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| Equal time at 20/24/30/60 fps and budgets 1/2/3 | `live_sim_clock_same_time_at_20_24_30_60_fps` |
| Interior hits, ties and end boundaries apply at their moments | `live_sim_clock_hits_land_inside_long_steps` checks impulse-driven displacement |
| Export schedule matches full-rate live | `export_matches_live_project_intervals_and_cfl_steps` compares interval and CFL step sequences at multiple project and export rates |
| Pause, Speed, seeks, reset/setup/explicit restart | `live_sim_clock_pause_seek_speed_reset_match_today`, `live_sim_clock_pause_resume_same_epoch` |
| Blocked target/debt retained, then stretched catch-up | `live_sim_clock_blocked_retains_debt_then_stretches` |
| Reference CFL epsilon/ceil/restrictions | `live_sim_clock_cfl_reference_rule` |
| Submitted is not completed; coupled receipts/endpoints agree | `live_interval_completion_receipts`, `live_interval_coupled_endpoints`; lead GPU conformance still required |
| Main/editor HUD consumes content lag, cap and error flags | `content_snapshot_updates_main_and_editor_huds_then_holds_when_paused`; rendered observation remains lead-owned |
| Runtime clock and scheduling settings are shared | Physics clock/particle tests and native interval proofs; lead GPU export proof still required |

## 6. Phasing

### P1 — CPU groundwork (this lane)

**Entry/read-back:** bd show BUG-7qzk; verify slot-1 branch/base; read AGENTS, design standard, clock tests and CFL source. D1–D6 bind this work. Findings are dropped target time, coupled cap, fixed solver/whitewater durations. **Deliverables:** this doc, live_sim_clock_reference.rs, test-only lib registration, notices and generated index. **Gate:** `CARGO_BUILD_JOBS=4 cargo check -p manifold-renderer --tests`, then `CARGO_BUILD_JOBS=4 cargo test -p manifold-renderer --lib live_sim_clock_reference::`; focused renderer clippy with tests; `python3 scripts/gen_docs_index.py`; `git diff --check`. Cargo uses the absolute slot manifest; command-local RUSTC_WRAPPER override is allowed if sandbox blocks sccache. **Negative gate/forbidden:** no diff in clock.rs, coupling.rs, tick_samples.rs, gpu_flip_domain.rs or matter_domain.rs; no runtime registration, GPU execution or app. **Scope/demo:** CPU only, none — L1. Exact-path commit, no push; lead retains active slot for integration.

**Phase note (2026-10-03):** The checkpoint evidence is historical. Current runtime changes and verification obligations are in section 9.

### P2 — Integration seam specification

**Entry/read-back:** BUG-gjys merged; section-8 decisions recorded before implementation. Re-read changed anchors and re-run audit commands; restate D1–D6, caller inventory and work-bound policy. **Deliverables:** full old→new signatures/caller list for duration, endpoint and completion metadata; amend this doc to full-treatment level; CPU small-lattice proofs for speed inputs, whitewater duration and scaled pressure/body-reaction units. Reuse physics stepping and receipts. **Gate:** affected-crate check/clippy and named CPU tests; negative inventory audit for unmapped consumers. Exact test commands and a mechanically executable P3/P4 brief are outputs of this phase before worker dispatch. **Forbidden:** implementing blocked GPU scheduling, stale-speed substitution, new locks/dependencies. **Demo:** none — L1.

**Phase note (2026-10-03):** The checkpoint evidence is historical. Current runtime changes and verification obligations are in section 9.

### P3 — One accepted interval through GPU FLIP and coupled Box3D

**Entry/read-back:** P2 full seam brief/proofs green, blockers resolved; reread all timing consumers from fresh anchors. Restate duration/endpoint mappings, event consumption and export contract. **Deliverables:** shared plans, variable GPU steps, event splits, whitewater durations, longer Box3D steps/substeps, completion receipts and affected contract updates; CPU tests named `live_interval_completion_receipts`, `live_interval_coupled_endpoints`, `live_interval_whitewater_duration`, `export_matches_live_project_intervals_and_cfl_steps`, plus GPU value proofs. **Gate:** focused check/clippy/CPU tests; Codex compiles GPU proofs only with `cargo test -p manifold-renderer --no-run --features gpu-proofs`; lead runs scoped scripts/gpu_proofs_gate.py. P2 freezes exact commands and filters before dispatch. **Negative gate:** no TICK-derived elapsed time or dropped-target path on migrated live GPU FLIP sites; Matter, CPU FLIP, particles and uncoupled Box3D share the duration contract. **Demo:** bounded lead-run coupled-water timing trace at 20/24/30/60 fps, numeric target/completed/event comparisons plus Peter's observation (L2 target). **Forbidden:** app/GPU exploration by this lane, dropped time, fixed-only fallback, batch-applied interior hits, ordinal×TICK completion. No serialized changes.

**Phase note (2026-10-03):** The checkpoint evidence is historical. Current runtime changes and verification obligations are in section 9.

### P4 — Existing editor HUD, including BUG-az3

**Entry/read-back:** P3 completion telemetry available; reread snapshot/metrics/HUD/editor presentation and BUG-az3. D6 requires completed-time lag, not stretched-time accounting. **Deliverables:** row in ms, actual editor metric/tick/toggle path; `editor-sim-lag` UI flow opening HUD, playing/pausing, asserting changing then stable held text. **Gesture:** performer opens editor HUD while water runs. **Gate:** focused app/UI check/clippy and lag formatting/aggregation tests; lead runs the named flow via scripts/run_ui_flows.py and landing gate. P2 supplies the exact flow command supported by that script before dispatch. **Demo:** flow artifact, L3 target. **Forbidden:** second HUD, UI state writes, snapshot-only push_values shortcut. No serialization changes; scope is this row and BUG-az3 plumbing.

**Phase note (2026-10-03):** The checkpoint evidence is historical. Current runtime changes and verification obligations are in section 9.

## 7. Decided — do not reopen

1. Live runs at most two fixed Sim Rate steps per frame and drops the leftover time (2026-10-04). Retired: covering owed time with longer stretched intervals.
2. Export follows D7.
3. Hits split at their own moments, including multiple moments per frame.
4. Port reference CFL inside each fixed step; Box3D steps the same fixed intervals.
5. Preserve Speed/pause/epoch rules; overload slow motion is reported on the HUD and never mistaken for authored Speed.
6. Reuse editor HUD, report target−completed, address BUG-az3.
7. Runtime timing claims require the current CPU proofs and lead-run GPU evidence; historical checkpoint checks do not verify this follow-up.

## 8. Resolved decisions and deferred scope

**Peter's ruling, 2026-10-04 — the contract for BUG-g75v.11 (capped live clock rework), part of BUG-g75v (GPU water campaign):** live physics uses fixed Sim Rate steps. Each frame runs at most two of them; time beyond that is dropped and the clock reanchors, so an overloaded machine plays in slow motion. The HUD shows the dropped time as “sim behind” and warns while it happens. No step is ever stretched to catch up. Within one fixed step, FLIP substeps follow the FLIP Fluids CFL rule exactly as the engine runs one frame, so any final-substep remainder is bounded by one Sim Rate step, as in the engine. The frame records only the substeps that run; a frame never pays for unused substeps. Export keeps exact steps (D7). Only genuinely non-finite state reports a numerical error, and the show continues with the HUD reporting it.

How substeps are counted without recording unused ones is open: measure first (the rework bead lists the comparison against pre-overnight main), then choose. A count sized from the last completed step's top speed, read when its fence retires, is acceptable; stalling the content thread for a readback is not.

The clock accepts the earliest two complete intervals. A partial remainder stays owed during ordinary pacing; when a third complete interval is owed, the whole remainder is discarded and the transport grid starts again at the current observation. The cumulative discarded duration resets with the epoch, while the HUD reports only newly discarded time and outstanding completion lag.

Timestamped hits observed beyond the accepted ceiling map to its closing simulation boundary. Half-open ownership delivers them once in the next accepted interval, preserving source order and impulse strength, without creating intervals for the discarded transport. Pause and Speed 0 still discard incoming hits. An advancing busy worker is not paused merely because its capped simulation timestamp is unchanged. Closing collider samples remain attached to the accepted interval; the next authored start may jump across the discarded gap without sweeping a collider through that gap or advancing dynamic bodies.

**Retired 2026-10-04:** the 2026-10-03 never-slow-motion ruling, the final substep taking all remaining time of an unbounded live span, the marker speed limit measured over a multi-interval span, and the GPU-local maxima decision that made every frame record all six substep slots. Why: on a machine that cannot simulate a second of water in a second of GPU, keeping time and keeping the water stable cannot both hold. The stretched step made res 128 explode within six steps (60–90x the stable step; evidence in BUG-969p6 (overload rule)), catch-up made late frames later, and live GPU cost in the app rose above pre-overnight main (Peter, 2026-10-04).

**Included by Peter’s follow-up:** CPU fluids, Matter, particles and uncoupled Box3D adopt the common contract. Audio-analysis latency is separate. Loaded live playback can still differ from a full-project-rate run. GPU/visual execution belongs to the lead. No serialized project-format change or new quality control is authorized.


## 9. Implementation seam

This section describes the shipped catch-up clock that the 2026-10-04 ruling replaces. Production uses the shared physics clock, accepted native CPU and Box3D intervals, GPU current-state scheduling, timestamped impulse lattices, actual body sample durations, and retired completion and status readbacks. Graph installation gives existing graphs their duration and status wires, including accepted-duration pose sampling for the whitewater obstacle-source grid. No project-format fields, locks or channels were added. The fixed-export branches and the live time-drop and debt-burst policies are gone.

`liquid/clock.rs` is a small compatibility adapter and `live_sim_clock_reference.rs` a test-only oracle; neither is a second runtime clock. The main and editor HUDs share `perf_metrics_from_content_state`; a CPU flow test drives both from `ContentState` through play and pause, lag, cap and nonfinite flags.

The marker speed limit measures against the accepted frame. A live late frame runs its owed intervals as one span, so the GPU clock (`limit_interval`, from `SimulationClock::interval_simulated_duration`) and the CPU engine (`set_speed_limit_interval`) cap that frame at one Sim Rate interval of simulated time, scaled by Speed; export passes none and measures each interval. A late span then removes only the markers on-time frames would. Proofs: the `gpu_flip_clock` histogram proof across spans of 1 and 4 intervals, `fluid::tests::live_span_measures_its_speed_limit_against_one_sim_rate_interval`, and the `manifold-fluids` test `frame::tests::live_span_removes_only_the_markers_its_configured_interval_removes`.

Verification lives in the scoped GPU gate: `gpu_flip_clock::gpu_tests::`, the still pool, hydrostatic column, replay and body reaction proofs, Matter's 30/60 fps export, and the liquid conformance export, coupled frame rate and nonfinite proofs. The cache-reader test needs `RUST_MIN_STACK=8388608`. Owed: an observed editor render of the HUD rows.
