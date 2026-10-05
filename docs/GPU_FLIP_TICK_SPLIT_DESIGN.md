# GPU FLIP Tick Split — even frames: measure first, pace as an experiment, spread only where it fits

**Status:** IN PROGRESS · 2026-10-06 · Fable 5.1, amended by the Claude lead after Astra's reviews. P0 landed. P1 is a default-Off experiment awaiting BUG-q2s2i (even frames: pacing vs drop rule). P2+ blocked (section 3, Open holes that block P2).
**Prerequisites:** none for P0–P1; display history P1 (`feat/flip-display-history`) for P3
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 before starting any phase.

Peter's item: "Even frames: spread each GPU FLIP solver tick across its interval so frame times are even; up to 3 frames of added display latency is allowed." At the oracle (Dam Break 64, 30 Hz sim, 60 fps project) the render alone fills 94% of a 60 fps frame, so no spreading reaches 60 fps there. The uneven frames are frames that carry no solver tick. P0 shows they happen when no Sim Rate boundary was crossed (section 2). Pacing the content thread to the sim grid (D1) might remove them, but it changes the whole instrument and has no sound load model yet, so it is an Off-by-default experiment, not a fix. Spreading (D2–D10) is designed for the regime where it pays, a scene whose render plus half a tick fits a frame, and is blocked until such a scene exists and the open holes in section 3 close.

Companions: `LIVE_SIM_CLOCK_DESIGN.md` (accepted intervals, cap, drop and reanchor); `GPU_FLIP_DISPLAY_HISTORY_DESIGN.md` (publication slots, cursor; section 3.5 (latency model)); `LIQUID_SOLVER_SEAM_DESIGN.md` section 3.4 (pause and export); `GPU_FLIP_PRESSURE_SOLVE.md` (step passes).

## 1. Audit — what exists (verified 2026-10-06 against 216071de2 = origin/main a551bb175 + test fix; history branch 046862959)

Extend, don't redesign. `R = crates/manifold-renderer/src/node_graph/`, `G = crates/manifold-gpu/src/`, `A = crates/manifold-app/src/`.

| Piece | Where | State |
|---|---|---|
| Tick = one region iteration; S encoded slots, 1 when `one_step` else 6 + hits; masks and history capture per slot; clock_status copied from the last plan | `R/primitives/gpu_flip_step.rs:2532-2538`, `:2577-2682` | one `encode` per slot, whole tick per invocation |
| Solver inputs reread every invocation: steps, interval, limit, flip, gravity, tick_index, epoch, body_count, rows, iterations, ghost, closed_faces, level | `gpu_flip_step.rs:2290-2325` | nothing frozen per tick |
| GPU-resident substep cursor: 48 B plan; `begin_frame` resets it; `schedule` sets `step_dt = 0` once `remaining == 0` | `R/primitives/gpu_flip_clock.rs:236,245-263`; `primitives/shaders/gpu_flip_clock.wgsl:464-475` | step-owned, persists across frames |
| Executor-pooled outputs: `out`, `capped` (2 words/slot + solver tally at `tally`), `clock_status`; released at region end | `gpu_flip_step.rs:2213-2222`, `:1060-1066`; tally writes `capped` `:1758`, move `:1846-1863`; release `R/execution/substep_region.rs:193-206` | do not survive the frame |
| Step-owned persistent storage: lattice arrays, `sorted`, `faces`, `saved_*` masks, clock, history, narrow | `gpu_flip_step.rs:561-593`, `:663-698` | survive frames |
| Stage order inside one slot; gated segments close inside pockets, pressure (`end_gated_segments` `:905`) and extend | `gpu_flip_step.rs:1265-1863`; pockets `:870-914`; `R/primitives/gpu_flip_pressure.rs:741-905`; extend `:786-800` | every stage boundary is outside a segment |
| Boundary: `pending = ticks`; per-iteration scalars (tick index, retired speed); late capture copies `in→out` every iteration, faces/interior only when `captures == pending`, readback per frame | `R/primitives/liquid_state.rs:676`, `:708-715`, `:741-834` | copies partial state if run per piece |
| Prediction refreshed on every retire | `liquid_state.rs:279-291`, `:329-339` | changes between pieces |
| Executor region: boundary once, body per iteration, host sync between iterations, replay span per region run keyed by boundary | `substep_region.rs:100-210`; `R/execution.rs:311` | one span per frame per region |
| Replay: `matches` on pipeline/groups/gate/bindings, inline bytes refreshed; MRU entry preferred; a miss truncates and records on; 3-entry ring | `G/replay.rs:200-235`, `:271-280`; `G/metal/replay.rs:499-512`, `:802-818`, `:650-652` | alternating shapes re-record every frame |
| Domain per frame: settle coupled tick with live wait, `clock.advance`, rows from `first_tick`, `PendingTick`, in-frame `Exchange` | `R/primitives/gpu_flip_domain.rs:740-830`, `:842-874`, `:921-959`; `R/liquid/coupling.rs:39-45`, `:314-347` | one tick per settle |
| Fields: live hits cleared and rebuilt per frame; impulses/forces/hits copied into stable buffers in place | `R/liquid/fields.rs:559-591`, `:699-718` | no per-tick identity |
| Whitewater inside the region; `ticks` forced to 1 when `distance` is wired; holds on `ticks == 0` | `R/primitives/whitewater_step.rs:1443`, `:879-897`; `R/primitives/gpu_flip_preset.rs:1726-1740` | runs every iteration |
| Stats inside the region, `stats_out` aliased into `liquid_state.stats` | `gpu_flip_preset.rs:454-463`; `R/primitives/liquid_stats.rs:217-219` | runs every iteration |
| Publication reads `liquid_state.out` after the region; history branch publishes on `wants_publication(simulation_time)` | `gpu_flip_preset.rs:484-492`; main `R/primitives/liquid_frame.rs:272-367`; 046862959 `liquid_frame.rs:231-271`, `R/liquid/frame_history.rs:181-183` | per frame |
| Live clock: cap 2 intervals, 1 after a late frame (`previous > budget`), drop and reanchor past the cap, display one interval behind | `crates/manifold-physics/src/clock.rs:20-44`, `:162-167`, `:331-347`, `:403-417` | budget = 1/project fps |
| Content thread pacing: deadline at project fps; `LiveLoad.budget = 1/fps` | `A/content_thread.rs:405-446`; `A/frame_timer.rs:179-200`, `:429-441`; `A/content_pipeline.rs:2127-2136` | runs as fast as the GPU allows below 60 |
| Measurement: per-node spans; per-clock decision records; GPU FLIP stage tags; no-tick classification | `A/frame_time.rs`; `R/physics_metrics.rs`; `gpu_flip_step.rs` stage tags | P0 (pending landing) |


## 2. Feasibility from the measurement (Dam Break 64, 30 Hz, 60 fps project, M4 Max, 2026-10-06)

The first measurement (`oracle_main_nodes.log`, timestamped, replay off) split frames 100–360 by whether a tick ran: 238 tick frames mean 32.1 ms, 22 no-tick frames mean 15.6 ms, 260 frames averaging 32.56 fps. The reciprocal median interval is not throughput.

- Per-frame render and publication `r ≈ 15.6 ms` (a frame with no tick). Marginal tick cost `T ≈ 32.1 − 15.6 = 16.5 ms`. Both are conditional on the frame's cadence and on publication, which is itself conditional (`liquid_frame.rs`), so they bound regimes; they are not isolated stage costs.
- 60 fps unsplit needs `r + T ≤ 16.7`. Split into P pieces it needs `r + T/P ≤ 16.7`, so `P ≥ 15` at the oracle. Render alone is 94% of the budget: no spreading reaches 60 fps here.
- Phase creep alone explains the cadence. With long frames L = 31.6 ms, short frames r = 15.6 ms and interval S = 33.3 ms, the cycle `N·L + r = N·S` repeats about every nine tick frames plus one short frame; the short frame shifts the phase too.

**P0 result (`frame-time`, release, paced, 600 frames, no stamps).** Classified by the rule in `frame_time.rs` from each clock's own decisions:

| Frames | Ticked | No tick | No boundary crossed | No boundary, following a reanchor | Restart | Reanchors | Fresh dropped |
|---|---|---|---|---|---|---|---|
| 0–100 (splash) | 87 | 13 | 6 | 6 | 1 | 23 | 0.88 s |
| 100–600 (calm) | 474 | 26 | 26 | 0 | 0 | 8 | 0.28 s |

Measured GPU span sum, calm: tick frames p50 32.8 ms, no-tick p50 12.8 ms. Paced wall intervals hide the difference (no-tick 30.9 ms, tick 32.7 ms): the surface wait absorbs it. A timestamped run (replay off, every frame, after `project_extend` moved before projection) agrees: 25 of 25 calm no-tick frames crossed no boundary (frames 60–360), and the step's stage spans sum to the step span (ratio 1.000). Calm stage p50s, profiling-inflated, usable as relative D3 weights: prepare 9.7, pressure 5.6, pockets 3.6, project_extend 1.0, move 0.9, finish 0.2, clock 0.1 ms. No density span appears in this scene.

So on the oracle, calm no-tick frames happen with no boundary crossed; none follows a reanchor. The classification is a rule over observed decisions, not proof of cause. The calm part still drops about 0.28 s of simulated time per run, through the late-frame rule of `LIVE_SIM_CLOCK_DESIGN.md` (Late frames take one interval): a frame over the 16.7 ms budget caps the next at one interval, and one that then spans two boundaries drops the remainder and reanchors. That is the clock's contract, not a defect. **Open question for Peter:** whether cap one after a late frame is the right policy when the scene sustains 30 Hz but never fits 16.7 ms.

Verdict: spreading cannot deliver even frames at the oracle. Pacing is worth an experiment (D1). Spreading is the answer only where `r + T/P ≤ 1/fps < r + T` with integer `P = sim_interval / frame_interval` (60 fps / 30 Hz: `T ≤ 2·(16.7 − r)`); no scene has been measured there.

## 3. Decisions

**D1 — Pacing to the grid is an Off-by-default experiment.** Setting `settings.physics.live_pacing: Off | Experimental`, default Off, persisted with load migration. Experimental paces the content thread to one sim interval (`m = 1`, 33.3 ms at 30 Hz) only; no automatic `m` selection. Load model: pacing to `m·S` makes about m ticks due per frame, so a frame costs `r + m·T`, not today's `r + T`. m > 2 always exceeds the live cap, and m = 2 loses time once a late frame drops the cap to one, so only m = 1 is in scope. The trigger is Peter's switch, not an EWMA of `last_render_work_ms + last_fence_wait_ms`, which is host load rather than GPU execution time; an automatic mode needs entry and exit thresholds, dwell, recovery and a defined qualifying sample, and is deferred. While pacing, `LiveLoad.budget` is the paced interval, and every clock observer sees the same budget. That keeps "late" meaning late against the display budget; it does not force cap two or prevent drops, because frame deadlines are relative to the previous actual tick, not phase-locked to sim boundaries. Pacing slows the whole instrument: non-water animation cadence, MIDI consumption, Link/OSC polling, outbound transport and audio-layer updates share the content tick, and the timer sleep does not drain commands the way the surface wait does. Export is offline and untouched. Rejected: pacing to `n/fps` ignoring the sim rate, because 30 fps against 24 Hz re-creates a no-tick frame in five. Rejected: spreading at the oracle (section 2).

**D2 — The split applies only where it fits, P ≥ 2 integer.** Per accepted tick `P = round(sim_interval / frame_budget)` when within 1% of an integer and ≥ 2, clamped to the predicted non-empty unit count (D3); otherwise `P = 1` (today's path, byte-identical). `P = 1` also when offline, coupled (D9), narrow band, or D1 pacing is active. Rejected: rational ratios (60/24), because pieces per frame would alternate 2 and 3 and unevenness returns; deferred.

**D3 — Unit = one stage of one slot; the per-tick plan is frozen at acceptance.** Units in order per slot k: u0 prepare (`gpu_flip_step.rs:1265-1663`), u1 pockets + divergence (`:1668-1692`), u2 pressure prepare + solve + tally (`:1710-1758`), u3 project + react + extend_new + constrain (`:1769-1789`), u4 density (`:1790-1839`), u5 move + emit + masks + history capture (`:1845-2005`, `:2627-2674`); the slot's clock dispatch (`:2581-2610`) belongs to its u0. Every cut lies outside a gated segment (audit row 6). At acceptance the domain writes a `TickPlan { tick, interval, P, s_enc, s_pred, weights, cuts: [u8; 8], cursor }` into a fixed ring of 2 (no allocation): `s_enc` = the step's slot count decided once from the retired sample (`:2532-2538`), `s_pred` = retired `steps` word (status word 6, `liquid_state.rs:280-284`) or 6 without one, weights = P0's measured stage EMA for active slots and 0.02 for slots ≥ `s_pred`, `cuts[p]` = first unit where cumulative weight ≥ p/P of the total, `cuts[P] = 6·s_enc`. P is clamped so no `[cuts[p−1], cuts[p])` is empty. The cursor is the exact next unit; prediction refreshes between pieces (`liquid_state.rs:279-291`) change nothing for a tick in progress. Rejected: half-substep units (v1), because `P = 4` leaves empty pieces at `s_pred = 1`. Rejected: cuts inside the pressure loop, because its rounds are gated segments with GPU-resident ranges (`gpu_flip_pressure.rs:1091`); deferred.

**D4 — Continuation storage is step-owned; completed snapshots are separate.** The step allocates at reserve (lattice or capacity change only): `cont.out` (capacity × 32 B), `cont.capped` (pooled `capped` size), `cont.phi_done` (cell bytes), and a second substep-history set. While `P > 1`, every slot binds `cont.out` as its `out` and input for k > 0 (`:2580`) and `cont.capped` for tally, move and the capped mask; the clock plan, lattices and solver buffers already persist. The completing piece ends with copies `cont.out → out`, `cont.capped → capped`, `lattice.phi → cont.phi_done`, the plan → `clock_status`, and swaps the history set; `distance`, `substep_schedule`, `substep_u/v/w` provided outputs (`:2202-2210`) return the completed set while splitting. `liquid_state.out`, `faces`, `interior` are written only by a completing iteration (D6), so they are the coherent snapshot a frame publishes under a completed tick's time even when the same frame runs the next tick's first piece. With `P = 1` the pooled buffers bind directly as today. Rejected: making `out` a provided output and publishing from it, because publication reads after the region (`gpu_flip_preset.rs:484-492`) and would see the next tick's partial particles.

**D5 — Every input of an accepted tick is retained for its pieces.** At unit 0 the step snapshots `TickInputs`: the resolved `StepParams`, `GpuFlipClockParams` (`:2552-2570`), `Step` flags (dynamic, pressure stop, level, ghost, density, narrow), counts, and clones of every input buffer handle (particles, forces, impulses, live_hits, bodies, shapes, atlas, regions, reaction, identity, clock vertices). Later pieces of the tick use the snapshot and ignore `ctx` (`:2290-2325`); a changed control applies from the next tick, which is the invariant "time already simulated keeps the settings live then". Buffers the domain rewrites in place while a tick may be in progress get two instances by tick parity, uploaded only for a new tick: `FieldBuffers.{impulses, forces, live_hits}` (`fields.rs:699-718`) and the clock vertex outputs (`gpu_flip_domain.rs:948-958`); `prune_before` keeps the oldest unfinished tick's lattices (`fields.rs:519-545`); the applied event list keeps a tick's hits until it completes. Body rows are fresh buffers per upload (`R/liquid/body_buffers.rs:80-112`), so a retained handle is enough. Hits cannot arrive for an accepted interval: the clock accepts an interval only once transport has passed its end (`clock.rs:331-343`). Rejected: retaining only force samples and rows (v1), because gravity, FLIP share, iterations, geometry and hit records would rewrite accepted history.

**D6 — Boundary protocol: iterations are pieces, completion is a scalar from the clock owner.** The domain's per-iteration clock output (`substep_region.rs:214-224`) gains scalars `piece_tick`, `piece_from`, `piece_to`, `piece_slots`, `piece_completes`; the executor passes the clock owner's scalars to the boundary's `substep_iteration` (trait default: none). `liquid_state` sets `pending` = pieces this frame (domain output `pieces`, default `ticks`), copies `in→out`, faces, interior, runs its readback and increments `ticks_done` only on a completing iteration, and asserts `piece_tick == ticks_done`. The step takes `piece_*` as inputs with defaults meaning "whole tick" (precedent `R/liquid/clock.rs:14-20`), runs `encode_units(from, to)`, calls `begin_frame` only at unit 0, and refuses a `(epoch, tick, from)` that does not match its snapshot cursor by restarting that tick at unit 0 (an abandoned tick leaves no residue: every stage reads the particle snapshot, retained inputs and arrays written earlier in the same tick; `filled`/`solid_velocity_is_zero` are reconciled by u0). Rejected: letting the step count pieces itself, because the domain owns time and the drop rule.

**D7 — Whitewater and stats run only on completing iterations.** `whitewater_step.rs:1443` changes precedence: a wired `ticks` input wins over the distance rule; the preset wires `piece_completes` into it, so non-completing pieces hold (`:879-897`) and a completed tick runs one whitewater tick at the full interval. `liquid_stats` gains `enabled` (default 1) wired the same way; disabled it encodes nothing and `stats_out` keeps the last completed tick. Rejected: gating by the step's `piece_last` wire alone (v1), because `:1443` ignores it.

**D8 — Replay caches are keyed by piece shape.** `execution.rs:311` keys become `(boundary, shape)`, shape = the clock owner's `substep_replay_shape()` packing pieces this frame, first `from`, last `to`, `s_enc`. Each shape keeps its own 3-entry ring, validation and inline-bytes refresh unchanged (`G/replay.rs:200-235`), one span per region run as today (`substep_region.rs:190-193`), so the A | B steady pattern replays instead of truncating at command 0 every frame (`G/metal/replay.rs:802-818`). The completing piece's two copies to pooled buffers change identity per frame and re-record as a two-command tail. Rejected: a `(tick, a, b)` heuristic key (v1), because identical lists can bind different buffers.

**D9 — Coupled ticks are not split in v1.** Box3D settles tick k−1 with a live wait before tick k starts (`gpu_flip_domain.rs:746-750`); a split would put that wait at every A piece one frame after B, and a `B(k), A(k+1)` frame would need `commit_wait_and_continue` (`substep_region.rs:123-150`) plus a settle, row rewrite and reaction clear (`:921-959`) every frame. Pending states stay today's two: `PendingTick { stamp }` (in flight) and settled. Coupled scenes get D1. Reviving needs a third state (in progress: reaction partial, rows retained, exchange at the last piece) and parity-ring clock vertices; deferred.

**D10 — Export equals live precisely.** For the same accepted intervals with the same inputs at acceptance, a live tick's dispatch list (pipelines, groups, gates, bindings by role, inline bytes) equals the export tick's; pieces move the frame at which commands are encoded, never their order, and `cont.*` substitute pooled buffers one-to-one. Live drops (`clock.rs:337`) already make unconditional equality false; the statement is per accepted interval.


### Open holes that block P2

D3–D10 close the v1 review's holes, but not these. Each needs a written resolution in this doc before P2 starts.

1. **Acceptance is not unit zero.** Queued ticks may begin a frame later, and D5 snapshots at unit 0, contradicting "inputs frozen at acceptance". Whitewater's own parameters are outside `TickInputs`.
2. **Cloned handles do not freeze mutable contents.** Dense emission mutates aliased particle identity in `gpu_flip_step.rs`; restarting a tick after an earlier slot emitted does not restore that counter. D6's "an abandoned tick leaves no residue" needs rollback or reset of every persistent side effect, coordinated with the domain.
3. **Queue accounting.** Accepting two ticks but running only the first eligible piece can leave two unfinished ticks, while section 4 assumes unfinished is 0 or 1. Define queued versus started occupancy, parity-buffer ownership, and abandonment of accepted work.
4. **Replay keys are a heuristic.** A first/last range plus one slot count does not describe every multi-piece sequence, and early binding changes can invalidate the prefix, not only the tail copies (`crates/manifold-gpu/src/replay.rs`). Keep validation and bound the cache's growth.
5. **Dispatch-list equality is not state equality.** When initial mutable storage differs, equal dispatch lists do not give equal numbers or equal export; D10's proof also needs completed-state equality.

## 4. Scheduling and latency

Per frame, after `clock.advance` is given `max_accept = 2 − unfinished` (unfinished ∈ {0, 1}; the clock's own cap and drop rule still apply): run the in-progress tick's next piece; if the previous frame was not late (`LiveLoad`) and the tick is behind its planned completion frame, run its next piece too; then the first piece of a newly accepted tick only while fewer than 2 pieces ran and no second completing piece would follow. Steady state at 60/30: A(k) | B(k) | A(k+1) …, completion at acceptance + P − 1 frames; after a late frame one frame earlier; never later than acceptance + P, else the clock drops as today. Pause: remaining pieces run on paused frames until the tick completes (LIQUID_SOLVER_SEAM_DESIGN.md section 3.4 (pause and export)), at most P frames. Speed: the interval is fixed at acceptance; pieces are unaffected. Reset, backward seek, rate change: new epoch, the in-progress tick is abandoned (D6). Export: P = 1.

Latency, GPU_FLIP_DISPLAY_HISTORY_DESIGN.md section 3.5 (latency model) (046862959 `docs/GPU_FLIP_DISPLAY_HISTORY_DESIGN.md:69-82`): 60 fps / 30 Hz with P = 2 completes one frame later than today, R = 3 → 4: 75 → 92 ms transport lag, +1 frame of the 3 allowed; catch-up frames complete earlier and the cursor's measured deficit absorbs it. 30 fps GPU-bound (oracle) under D1: P = 1, 108 ms as today, +1.7 ms from the 33.3 ms frame. These are arithmetic; P3 measures.


## 5. Invariants and enforcement

1. Nothing partial publishes: `liquid_state.out/faces/interior`, `distance`, history outputs change only on a completing iteration. Test: on a `B(k), A(k+1)` frame the published bytes equal tick k's whole-tick run.
2. Reading the past never changes it: a tick's bytes are a function of its `TickInputs` and particle snapshot. Test: controls changed between pieces leave the in-progress tick's bytes equal to the snapshot run; the next tick takes the new values.
3. No empty piece: property test over `s_enc ∈ {1, 6, 7}`, `s_pred ∈ 1..6`, `P ∈ 1..8`.
4. The cursor is exact: `encode_units(from, to)` for consecutive ranges equals `encode_units(0, 6·s_enc)` as a dispatch list; a mismatched `(epoch, tick, from)` restarts at unit 0.
5. No per-frame allocation, no new shared state: continuation buffers at reserve only; `TickPlan` ring of 2; clock records in fixed storage; the hot-path allocation lint passes.
6. Export equals live per accepted interval (D10): dispatch-list equality plus completed-state equality, P ∈ {1, 2, 3}.
7. Pacing is measured, never assumed: under D1 Experimental, the frame-time report states accepted ticks, no-tick frames and fresh dropped seconds. Pacing does not guarantee zero drops; a drop under pacing is reported, not hidden.

## 6. Phasing

Every phase is one session, ends committed with `cargo clippy -p <touched> -- -D warnings` clean, and is landed by the Claude lead, never the lane.

### P0 — Instrumentation (implemented, pending landing)

- **Entry state:** `origin/main` contains `crates/manifold-physics/src/clock.rs` with `live_cap` and the reanchor branch: `rg -n "fn live_cap|reanchored = " crates/manifold-physics/src/clock.rs`.
- **Read-back:** this doc's section 2 and section 6; `clock.rs` `advance`; `crates/manifold-renderer/src/node_graph/physics_metrics.rs`; `crates/manifold-app/src/frame_time.rs` `probe`. Restate: counters come from the clock's decisions, never from timing; no allocation in the content frame; stage tags are profiling-only and touch no dispatch, binding or replay key.
- **Deliverables:** `ClockFrame.{due, live_cap, fresh_dropped_seconds, transport}`; `physics_metrics::{ClockRecord, ClockMetrics, record_clock}` (four fixed records plus overflow count); records from the GPU FLIP and CPU water domains; GPU FLIP stage tags unconditional plus `gpu_flip.stage.clock`, `project_extend` starting before projection; frame-time per-frame clock line, no-tick classification, wall and GPU split by tick.
- **Gate (positive):** `cargo nextest run -p manifold-physics decision_counters_match_the_clock_over_an_overload_sequence`; `cargo nextest run -p manifold-renderer each_live_clock_keeps_its_own_record`; `cargo nextest run -p manifold-app --features perf-soak frame_time` (includes `no_tick_frames_are_classified_from_the_clock_decisions`, `mixed_clock_frames_keep_per_clock_identity`, `stage_coverage_compares_stage_spans_to_the_step_span`).
- **Gate (negative):** `rg -n 'cfg\(feature = "water-race-probes"\)\]\s*$' -A1 crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step.rs | rg set_profile_tag` returns zero hits; `rg -n "profile_tag|label" crates/manifold-gpu/src/replay.rs` returns zero hits.
- **Acceptance artifact:** `scripts/gpu_queue.py -- <target>/release/manifold frame-time <oracle.manifold> --frames 600 --splash-frames 100 --stamp-every 100000` prints the per-frame clock line and the "solver ticks … classified by this rule" summary; a `--stamp-every 1 --stamp-granularity node` run prints the stage spans / step span ratio within 5%. Results recorded in section 2.

### P1 — Pacing experiment (D1), default Off

- **Entry state:** P0 landed: `rg -n "fn record_clock" crates/manifold-renderer/src/node_graph/physics_metrics.rs` hits; Peter has said he wants to try it.
- **Read-back:** D1 in full; `docs/VSYNC_AND_FRAME_PACING.md`; `crates/manifold-app/src/content_thread.rs` `run_paced_frame`; `frame_timer.rs` deadlines; `LIVE_SIM_CLOCK_DESIGN.md` (Late frames take one interval). Restate: Off by default; m = 1 only; no automatic mode; the budget change reaches every clock observer; commands still drain while the thread waits.
- **Deliverables:** `PhysicsSettings.live_pacing` with load migration (old projects load Off); content thread paces to one sim interval when Experimental and the timer sleep drains commands; `LiveLoad.budget` is the paced interval while pacing; a `frame-time --live-pacing experimental` flag.
- **Gate (positive):**
  - `cargo nextest run -p manifold-app paced_deadline_is_one_sim_interval` (new, `frame_timer.rs`).
  - `cargo nextest run -p manifold-core live_pacing_defaults_off_and_round_trips` (new; Off default, Experimental persists, a file without the field loads Off).
  - `cargo nextest run -p manifold-app paced_wait_drains_commands` (new; a command sent during the paced wait is handled before the next tick).
  - Oracle, both modes through `scripts/gpu_queue.py -- <target>/release/manifold frame-time <oracle.manifold> --frames 600 --splash-frames 100 --stamp-every 100000 [--live-pacing experimental]`: report calm no-tick frames, fresh dropped seconds and interval p95 − p5 side by side.
- **Gate (negative):** `rg -n "live_pacing" crates/ -g '*.rs'` hits only settings, content thread, frame-time and tests; `git diff origin/main -- crates/ | rg '^\+.*Arc<(Mutex|RwLock)'` returns zero hits.
- **Acceptance artifact:** the two oracle reports and the exact launch command for Peter's live trial from the branch binary. Landing on main needs Peter's verdict from that trial.

### P2 — Split core (D2–D10), P = 2 only, uncoupled, no narrow band — BLOCKED

- **Entry state:** every item of section 3 (Open holes that block P2) has a written resolution in this doc; a P0-measured scene with `r + T/2 ≤ 16.7 < r + T`, named with its file and its measured r and T from `frame-time`.
- **Read-back:** D2–D10, section 4, section 5; `gpu_flip_step.rs` encode; `liquid_state.rs`; `substep_region.rs`; `crates/manifold-gpu/src/replay.rs`. Restate the qualifying scene's numbers and the hole resolutions.
- **Deliverables:** resumable `encode_units`, continuation storage, `TickInputs`, `TickPlan`, domain scheduling, boundary scalars, stats and whitewater gating, replay keys.
- **Gate (positive):** `scripts/gpu_proofs_gate.py` with a scope mapping for the split, running these new proofs in `crates/manifold-renderer/tests/gpu_proofs/` (dam break 64, 60 ticks): `split_never_publishes_partial_state` (invariant 1), `split_tick_keeps_inputs_at_acceptance` (2), `split_cuts_are_never_empty` (3, CPU property test, nextest), `split_cursor_matches_whole_tick` (4), `split_export_equals_live_state` (6, dispatch list and completed state), `split_prediction_changes_across_ticks` (1 → 6 → 1), `split_survives_pooled_node_between_pieces`, `split_hits_during_tick_in_progress`, `split_whitewater_holds_on_partial_pieces`, `split_replay_hits_steady_pattern` (`replayed ≥ 95%` after 10 frames of A | B), `split_reset_mid_tick_matches_fresh_run`.
- **Gate (negative):** `split_p1_dispatch_list_matches_main` shows P = 1 byte-identical to today on the oracle; `git diff origin/main -- crates/ | rg '^\+.*Arc<(Mutex|RwLock)'` returns zero hits; the hot-path allocation lint passes.
- **Acceptance artifact:** `frame-time` on the qualifying scene before and after (interval p95 − p5), and a headless PNG pair at a `B(k), A(k+1)` frame showing no partial publication.

### P3 — History and latency

- **Entry state:** P2 landed (`rg -n "fn encode_units" crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step.rs` hits); display history P1 on main.
- **Read-back:** section 4 latency; `GPU_FLIP_DISPLAY_HISTORY_DESIGN.md` section 3.5 (latency model). Restate the latency arithmetic being tested.
- **Deliverables:** P2 under display history; a transport-lag column in the `frame-time` per-frame line.
- **Gate (positive):** `scripts/gpu_queue.py -- <target>/release/manifold frame-time <qualifying.manifold> --frames 600 --splash-frames 100 --stamp-every 100000` reports ≤ 3 frames of added transport lag against P = 1 and frame interval p95 − p5 ≤ 3 ms at 60 fps; `cargo nextest run -p manifold-renderer split_history_publishes_completed_ticks_only` passes.
- **Gate (negative):** the same run with splitting off reproduces today's transport lag within one frame (no regression to the unsplit path), and its report shows zero frames publishing a tick that has not completed.
- **Acceptance artifact:** that report plus Peter's live look at the qualifying scene.

## 7. Consequences, stated honestly

D1, when on, lowers the whole content thread to the sim rate, not only the water, and coarsens MIDI, Link/OSC, transport and audio-layer update timing; that is why it is Off by default. The split does not help the oracle and adds one frame of latency where it applies. Memory at 64³: `cont.out` capacity × 32 B, `cont.capped` ≈ capacity × 8 B, `phi_done` 1 MB, history doubled (≈ 19 MB at 6 slots). Per completing tick: three copies, well under 0.2 ms. Replay cache memory doubles in steady state. Four node contracts change (step inputs, domain outputs, stats `enabled`, whitewater `ticks` precedence), all defaulted so saved graphs run whole ticks.

## 8. Decided — do not reopen

1. No spreading at the GPU-bound oracle. 2. Pacing is opt-in, default Off, m = 1 only, until Peter's trial. 3. Unit = stage of a slot, plan frozen at acceptance. 4. Continuation separate from snapshot. 5. Coupled, narrow, offline never split in v1. 6. Export equality is per accepted interval and needs state equality.

## 9. Deferred

Automatic pacing (EWMA, m selection); rational P (60/24); coupled split (third pending state, exchange at the last piece); narrow band; cuts inside the pressure loop; GPU-resident piece schedule; pacing to a non-vsync-multiple grid at displays other than 60 Hz. Trigger for each: a measured scene that needs it.
