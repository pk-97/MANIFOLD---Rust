# GPU FLIP display history — retained publications and a presentation cursor

**Status:** APPROVED design, P1 landed, P2 contract written (section 3.4, section 5 P2 brief), awaiting review before build · 2026-10-06 · Claude, reviewed by Astra. Bead: BUG-ckvpp (display shows every third tick, never interpolated, ~0.25 s late).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5–section 6 before starting any phase.

Companions: LIQUID_SOLVER_SEAM_DESIGN.md section 3.1 (Particle frame) and section 3.4 (Clock, pause, speed, reset, export); GPU_FLUID_SURFACE_DESIGN.md D10/D11; LIVE_SIM_CLOCK_DESIGN.md section 8 (Resolved decisions).

## 1. Problem

Measured with a temporary probe in `liquid_frame` on the GPU FLIP Dam Break (res 64, Sim Rate 30 Hz, 60 fps project running ~30 fps GPU-bound): a publication is pending every frame; the A/B pair is always 0.1 s apart; t_B trails the simulation by 0.17–0.27 s; the requested display time is always past t_B, so blend is 1.000 every frame. On stage the water moves in 10 Hz steps a quarter second late.

Three code facts make it. `liquid_frame` starts a publication only when none is pending (`primitives/liquid_frame.rs:272`) and retires one only when its stamp completed (`:169-179`); with the content thread up to three frames ahead of the GPU, only every third endpoint is published. `FrameRing` keeps the latest pair only (`liquid/frame_ring.rs:95-104`). The requested display time is the simulation one interval of transport ago through the Speed history (`manifold-physics/src/clock.rs:400-404`), newer than anything retired. A publication happens on the render frame that crosses a sim boundary, not at the boundary, and retires whenever the GPU finishes that frame; the surface ring bounds outstanding frames (`content_pipeline.rs:1722`, `SURFACE_COUNT` 3), not their transport spacing, so no constant lag brackets the retired pair.

## 2. Decisions

**D1 — Retain history, Liquid Frame only.** `FrameHistory` replaces `FrameRing` inside `liquid_frame`: a budget of complete slots, every frame-end endpoint published into a free slot, several pending at once, selection by bracketing a presented time. Rejected: a fixed display lag (false under 24/60 cadence and two-tick frames).

**D2 — The presented time is a cursor.** Live uncoupled GPU FLIP presents at a cursor that advances by the simulation delta and sizes its lag to the measured availability deficit (section 3.4). Rejected: a fixed margin behind the newest retired endpoint; the worst cadence step (two-tick frames, GPU-bound frames) is what the margin must cover, and only measuring finds it.

**D3 — Policy in the domain, mechanism in the frame node.** The physics clock is untouched. The domain outputs `display_cursor` (1 live uncoupled, 0 offline or coupled); `liquid_frame` takes it as an optional input, unwired meaning exact. Rejected: a global clock lag; it breaks Matter (`matter_domain.rs:835`) and coupled water, which must sit at `owner.completed_time()` (`gpu_flip_domain.rs:823-826`).

**D4 — No slot reads another slot.** The rejected-tick interior fallback copy (`liquid/grid.rs:168`, one caller, `liquid_frame.rs:331`) is deleted; a rejected publication is never selectable. `InteriorOps` stays for `PublishedFaces::publish` (`grid.rs:282`). Rejected: per-slot source-reader tracking.

**D5 — Whitewater is retained per slot.** The four class arrays become `liquid_frame` inputs copied into the publication; the interpolate nodes read the selected slot's copy. Rejected: rewinding the current whitewater state by (simulation_time − cursor), up to 0.15 s of straight-line rewind, water and foam from different ticks.

**D6 — Export is exact.** Offline commit-waits and presents at the requested time whatever the wire says.

**D7 — Matter keeps `FrameRing`.** Its publication shader reads the previous slot for a rejected tick (`shaders/matter_frame.wgsl:47`) and it finishes without CPU retirement (`matter_frame.rs:215`); moving it violates D4 or needs its own redesign. Rejected: one history type for both producers.

## 3. Contract

### 3.1 Slots, generations, runs

A publication generation is (epoch, lattice key, wired field set and field sizes). A run inside a generation starts at particle-capacity growth or an identity change (renumbering, reseed). A slot is allocated whole before its first publication (particles at the run's capacity, the 16-byte metadata, every wired field at the generation's size) under one memory admission, and is written by nothing but its publication until it is Free. Per slot: generation, run, t, count, identity, field presence, `writer_stamp`, `reader_stamp`, state. Run identity is assigned at retirement, in publication order, from the retired metadata; it is never inferred from unretired storage.

| State | Meaning | Leaves when |
|---|---|---|
| Free | unpinned, outside retained history, both stamps complete, no content | a publication writes it; `writer_stamp` = this frame's stamp (`metal/retire.rs:175-183`) |
| Pending | publication encoded | writer complete, generation current: metadata word 2 ≠ 0 → Retired, else Rejected. Generation obsolete → Obsolete; metadata discarded |
| Retired | accepted endpoint; selectable | older than the pinned A, or generation obsolete → Obsolete |
| Rejected, Obsolete | never selectable; may still be pinned or read by a frame in flight | unpinned and both stamps complete → Free |

**Every transition to Free, from any state and on any path (epoch reset, resize, wiring change, encode failure, cancel), requires the slot to be unpinned, outside retained history, `is_complete(writer_stamp)` and `is_complete(reader_stamp)`; a stale completion never retires a slot into the history.** Completion of an earlier frame's reader stamp does not release a pin that is still current; reclamation runs after this frame's pin is chosen and restamped. A Pending slot whose generation went obsolete stays Obsolete when its writer completes, even if a later generation has the same layout tuple (A→B→A): obsolescence is decided by generation number, never by comparing layouts. Every frame stamps the slots it exposes (the pinned pair, its fields, the whitewater copies) with the frame's stamp; today nothing tracks selected-slot readers (`liquid_frame.rs:369`). The bulk interior clears on epoch change, lattice change and growth (`liquid_frame.rs:236-250`, `:284-290`) are deleted: a publication writes every wired field whole. Offline, `commit_wait_and_continue` (`metal/encoder.rs:2833`) completes the publication without advancing the frame event: the wait releases the writer (`writer_stamp` := 0, complete by definition), the metadata is read at once and the slot becomes Retired or Rejected by the same acceptance test as live (word 2 ≠ 0); the reader stamp from this frame's selection still gates reuse.

### 3.2 Publication

`newest_submitted` is (generation, time). A frame publishes when the inputs are ready and the generation changed or `simulation_time > newest_submitted.time`; a lattice or field change at unchanged time republishes, as `frame_ring.rs:47` forces today. Only the frame's last tick exists to publish (`liquid_state.rs:763-770`): **every frame-end endpoint is published; the intermediate tick of a two-tick frame is not** (the tick split is later work). A rejected publication still advances `newest_submitted`. A refused allocation or exhausted budget skips the endpoint: `newest_submitted` advances as if it were published, so held frames do not retry it, and `publications_skipped` increments once for that endpoint. A later frame-end endpoint (newer time or generation) is attempted normally. `publications_skipped` counts for the node's lifetime and is never reset by epoch or generation change.

### 3.3 Selection and the pinned pair

Candidates are Retired slots of the current generation, all runs, sorted by t. B is the earliest with t > c, or the newest; A is B's predecessor when it lies in B's run, else B. The result is pinned as the shown presentation — A, B, blend, span and the output descriptor (count, identity, field presence) — and stays stamped while shown whatever its slots' later states. A held pin is re-emitted as pinned; blend is never recomputed from a new request against an old pair. Nothing retired yet in the generation keeps the previous pin: after a reset the old water shows until the new epoch's first endpoint, then cuts; a lattice change marks outputs pending, as today. When B moves to a newer run, c jumps forward to B's run — the intentional discontinuity, the only move not made by the cursor rule, never backwards. The strict `t > c` reproduces FrameRing's pair whenever the requested time lies within or after the latest pair (every steady export frame); it differs only when older retained endpoints bracket it, which FrameRing cannot represent. `blend, span = display_blend(c, t_A, t_B)` (`fluid.rs:60`). Exact selection can pass retained endpoints without showing them when the requested time jumps, or when several retire between two presentations. `presented_time` is the effective sampled time, t_A + blend·span of the pinned presentation, not the request: in P1 the request may lie past N while the picture holds N.

### 3.4 Cursor

**Mode.** The cursor runs when `display_cursor` is wired and ≥ 0.5 and the frame is not offline. Everything else is exact: c = r. The domain outputs 1 when not offline and no rigid owner is attached (`gpu_flip_domain.rs:823-832`), else 0; `liquid_frame` re-checks `offline_simulation()` itself (D6).

**Inputs per frame.** r is the domain's `display_time`; it is non-decreasing within an epoch, because the clock restarts the epoch on any backward transport (`clock.rs:358-359`) and maps a non-decreasing transport through a non-negative Speed history. Δ = max(0, r − r_prev), with r_prev taken from the same epoch, else Δ = 0. N and N₂ are the newest and second-newest Retired endpoints of the current generation, any run, read after this frame's retirement. T is the frame's transport time (`ctx.time.seconds`). Constants: RATE 0.05, FILL_RATE 0.25, HORIZON 0.5 s, WINDOW 2 s of transport; GUARD = (N − N₂)/4, or 0 when N₂ does not exist.

**State** lives in `liquid_frame`, never in the clock: c, r_prev, the epoch it belongs to, the transport time of its fill start, and the deficit window (a fixed ring of (T, behind) samples, allocated once).

**Per frame, in order:** retire; advance the cursor; `select(c)`; apply the cut rule; stamp; reclaim.

0. Epoch. When the layout's epoch differs from the cursor's, the cursor is cleared: no c, empty window. Until the epoch's first retirement nothing is selected (the old pin holds, section 3.3). On the first frame that has a Retired endpoint in the epoch, c := the earliest Retired time in the generation and the fill start := T; the remaining steps run on later frames. A generation change inside the epoch (lattice or field layout) does not clear the cursor; the cut rule moves it.
1. A frame with Δ > 0 records behind = max(0, r − N) at T. A frame with Δ = 0 neither records nor ages the window. L = the maximum over samples with T − T_sample ≤ WINDOW, 0 when none.
2. target = r − L − GUARD.
3. base = c + Δ; u = clamp((target − base)/HORIZON, −k, k); k = FILL_RATE while T − fill start < 1 s, then RATE.
4. c ← min(base + Δ·u, N). Since u ≥ −RATE > −1, c never decreases.
5. Cut rule, after `select(c)`: when the pin is of the current epoch and its t_A > c, c := t_A. This is the run or generation cut of section 3.3 and the only jump; in steady selection t_A ≤ c holds by construction (B is the earliest t > c, A its predecessor).

**The epoch exception.** "Never below the pinned A" and "c ≤ N" are statements about one epoch. A pin left over from the previous epoch can sit past the new epoch's N (an old picture at t = 40 s while the new water is at 0.1 s); it bounds nothing, because step 0 cleared the cursor and step 5 only reads a current-epoch pin. Its `presented_time` is reported as is during the hold: the old water's time, not comparable with the new r. The invariant "presented_time ≤ N" is checked against the pin's own generation.

**Exact mode at a run cut.** c = r is never moved by the cut rule (export must present the requested time, D6). When r lies before the first endpoint of the run B moved into, A = B and the picture is that endpoint whole: `presented_time` = t_B > r, ahead of the request by less than one publication gap and never past N. It holds there until r passes t_B. This is the existing exact behavior, stated, not a change.

**Speed 0 and pause.** Pause stops transport, so r stops at once and Δ = 0. Speed 0 does not: r is the simulation one Sim Rate interval of transport ago (`clock.rs:440-441`), so for one interval after Speed reaches 0 it keeps rising toward the frozen simulation time while no new endpoint arrives. Those frames have Δ > 0; the cursor advances with them, still clamped at N, and records shrinking deficits that do not lower L. After that interval r is constant, Δ = 0, the window freezes, and c stays where it stopped, between two retained endpoints: a still, interpolated picture. The cursor reads Δ only, never the clock's `held` flag, which is set at Speed 0 while r still moves (`clock.rs:400`).

**Equilibrium and recovery.** For constant Δ and L the cursor settles at c = target, lag r − c = L + GUARD. The clamp keeps c ≤ N; a monotone r and u > −1 keep it from moving back; the correction stays within ±5% of real time. L is the observed deficit, not a guarantee: when a slow retirement leaves the window, latency contracts, and a later slow one holds at N again without a reanchor. After a stall that raised L by E seconds ends, the old samples hold L for WINDOW, then the cursor closes the gap at RATE until the error falls under RATE·HORIZON (25 ms), then exponentially with time constant HORIZON. Recovery to within 10% of the pre-stall lag λ therefore takes WINDOW + max(0, E − RATE·HORIZON)/RATE + HORIZON·ln(min(E, RATE·HORIZON)/(0.1·λ)) seconds of transport at Speed 1: 3.10 s for the modelled stall (E = 50 ms, λ = 75 ms). The test bound is 3.5 s.

`r` reaches `liquid_frame` as an f32 scalar; at one hour of simulation its step is 0.24 ms against a 16.7 ms frame Δ, which the cursor tolerates. `liquid_frame` outputs `presented_time` and `publications_skipped`.

### 3.5 Latency (model)

Corrected recurrence, one endpoint per frame boundary crossing, retirement R frames after publication, 12 s per case, statistics after 3 s:

| Case | R | Transport lag | r − c | Clamp hits |
|---|---|---|---|---|
| 60 fps / 30 Hz | 3 | 75 ms | 42 ms | 0 |
| 60 / 30 | 4 | 92 ms | 58 ms | 0 |
| 30 / 30 | 3 | 108 ms | 75 ms | 0 |
| 30 / 30 | 4 | 142 ms | 108 ms | 0 |
| 60 / 24 | 3 | 94 ms | 52 ms | 0 |
| 27 / 30 (one or two ticks a frame) | 3 | 150 ms | 117 ms | 0 |
| 60 / 30, Speed 0.5 | 3 | 75 ms (37.5 sim) | 21 ms | 0 |
| 60 / 30, R 3→6 for 1 s | — | 75→125, within 10% after 3.1 s | — | 3 hits, 2 holds |

At 60 fps the water trails transport by 75 ms against D10's 33 ms: 42 ms more, 2.5 frames. In the measured case (≈30 fps GPU-bound, R 3–4) 108–142 ms, interpolated every frame, against 170–270 ms stepping today: retained publications remove the throttling component; the retire delay at the GPU-bound frame time remains, and only the P2 trace and Peter's observation say what it is. Holds in the first second after an epoch starts and under a retire-delay increase are expected. These are arithmetic, not runtime numbers.

### 3.6 Whitewater

`liquid_frame` gains `foam_in/bubble_in/spray_in/dust_in` from `liquid_state` and `foam_b/bubble_b/spray_b/dust_b`. The builder (`gpu_flip_preset.rs:636-641`) and `WaterDamBreakGpuFlip.json` move the four interpolate nodes' `particles_b` onto them; the pool and state outputs stay current for the simulation feedback. `graph_loader.rs` migrates saved graphs after flattening (`graph_loader.rs:1222-1233`), so nested groups are seen as flat wires. Predicate: an interpolate node whose `particles_b` is a `liquid_state` class output S.c, whose `blend` and `span` both come from the same `liquid_frame` F, and F's `state` input is that same `liquid_state`. Then: if F's matching class input (`foam_in`, `bubble_in`, `spray_in` or `dust_in`) is unwired, wire S.c into it; if it is wired from anything else, the node is left untouched (conflicting explicit input preserved). Rewire `particles_b` to the matching `foam_b`/`bubble_b`/`spray_b`/`dust_b`. Authored `particles_a`, `count_a`, `count_b` and every other input are kept as they are. A graph already migrated fails the predicate (its `particles_b` comes from the frame), so the pass is idempotent; several interpolators sharing one frame each migrate and share the one class wire. Tests: idempotence, nested group, shared frame, conflicting wire, save/reload round trip. Copies are capacity-sized; zero tails keep radius 0, `count_b = −1` still works, A stays unwired, so `interpolate_particle_frames_body.wgsl:76` rewinds by (1 − blend)·span from the same B time as the water. Spray's acceleration is the domain's current gravity (`gpu_flip_preset.rs:643-647`), not retained: animated gravity errs by ½·|Δg|·τ² over the rewind τ, 5 mm for a 10 m/s² swing over a 33 ms gap; this is an example, not a bound, since two-tick frames and skipped publications widen the gap. Cost: 128 B × whitewater capacity per slot.

### 3.7 Scope and controls

Matter: D7. Coupled: exact at the completed time (`gpu_flip_domain.rs:746`, `:823-826`); the selector changes its pair where older endpoints bracket, so compatibility checks cover coupled one- and two-tick frames, rejection and reset. Export: D6; equal pictures need the same frame grouping and no live drops (`clock.rs:337`). Pause: Δ = 0. Speed 0: Δ > 0 for one Sim Rate interval, then 0 (section 3.4). Either way a tick already on the GPU still retires into history (section 3.4 of the seam). Speed: Δ scales, GUARD scales with the retained gap, L re-measures. Reset, backward seek, setup or rate change: new epoch, new generation, pinned pair per 3.3. Lattice or field-layout change: new generation, republish at unchanged time. Growth, identity change: new run.

### 3.8 Budget and failure

H_MAX = 16 slots, each admitted through `admit_candidate_bytes`. The model's use (pending plus retained from A): 4 at 60/30, 5–6 at 30/30 and 27/30, 8 through the stall; add the pinned pair (2) and obsolete slots awaiting readers (≤ 2F = 6 at F = 3). The budget covers steady state and one stall; it is not a proof. Exhaustion (no Free slot: admission refused or H_MAX reached) skips the endpoint once per section 3.2, increments `publications_skipped` (admission refusal also raises today's named error); the cursor holds at N when it catches up; nothing pinned, selected or in flight is evicted. Rejected tick: it adds no endpoint; selection proceeds over what is retained (in P1 that holds N; it does not freeze presentation). Never: reusing a slot with an incomplete stamp (the BUG-l7t4 (pool recycling races in-flight command buffers) class); extrapolating past N (D11).

Memory per slot: 32 B × particle capacity + 4 B × (solid nodes + interior cells + three face arrays) + 128 B × whitewater capacity, wired fields only. Dam Break with whitewater 100,000 (review arithmetic from `liquid_fill.rs:38`, `lattice.rs:80`): res 64, 345,792 particles, 28.6 MiB per slot, 458 MiB at 16; res 128, 2,749,312 particles, 139 MiB per slot, 2.18 GiB at 16, excluding solver and render storage. Res 128 needs total-memory evidence before the budget is used there; admission is its memory arm.

## 4. Conviction tests

CPU, `liquid/frame_history.rs`, a model over the real `SimulationClock` with a scripted retire delay.

- `frame_history_constant_cadence_no_clamp_hits_after_warm_up`: 60/30, 60/24, 27/30 and Speed 0.5 at R = 3; after 3 s blend is interior, c advances by Δ(1 ± 0.05), no clamp hits, lag within 1 ms of section 3.5. Today: blend 1, every third endpoint.
- `frame_history_retire_stall_holds_at_newest_then_recovers`: R 3→6 for 1 s; c never decreases, every hold is at N, lag returns within 10% of its pre-stall value within 3.5 s of transport after the stall ends (section 3.4, Equilibrium and recovery).
- `frame_history_jitter_and_reanchor_never_step_backwards`: the clock's jitter pattern, R seeded in 1..4; c monotone within a run.
- `frame_history_selector_matches_frame_ring_within_latest_pair`: retained {0, 1, 2}: r = 1 → (1, 2, 0) and r = 2.5 → (1, 2, 1) as FrameRing; r = 0.5 → (0, 1, 0.5) by design.
- `frame_history_two_tick_frames_publish_frame_end_only`.
- `frame_history_generation_change_republishes_unchanged_time`.
- `frame_history_pinned_pair_survives_reset_until_first_retirement`; `frame_history_run_boundary_cuts_forward`.
- `frame_history_every_free_transition_checks_both_stamps`: random sequences of reset, resize, rewire, encode failure, cancel over pending and pinned slots; no slot is Free while pinned, retained or with an incomplete stamp; an obsolete Pending never Retires, including when the layout recurs.
- `frame_history_consecutive_rejections_hold_and_free`; `frame_history_allocation_refusal_and_budget_exhaustion_count_skips`.
- `frame_history_pause_holds_speed_scales_backwards_requested_holds`.
- `frame_history_coupled_exact_one_two_tick_rejection_reset`.
- `frame_history_slot_use_over_2000_frames` ≤ section 3.8 per scenario.

Cursor tests (P2), same model with the cursor on unless named exact. Every one asserts, on every frame, c non-decreasing within the epoch, `presented_time` ≤ the newest Retired endpoint of the pin's generation, and no extrapolation (blend in [0, 1]).

- `cursor_equilibrium_matches_the_model`: the four cadences of the first test; after 3 s, r − c within 1 ms of L + GUARD, and blend strictly inside (0, 1) on at least 90% of frames.
- `cursor_speed_zero_advances_one_interval_then_freezes`: 60/30, Speed 1 → 0 at 4 s; c rises only during the interval after the change, then is constant for 2 s with blend unchanged frame to frame; resuming Speed 1 produces no clamp hit in the first 0.5 s (the window was frozen, not aged).
- `cursor_speed_change_rescales`: Speed 1 → 2 → 0.5; c's advance per frame tracks Δ within ±5% after each change settles; no backward step.
- `cursor_pause_holds_still`: transport frozen 2 s; Δ = 0 every frame, c, the pin and the window unchanged.
- `cursor_reset_epoch_ignores_the_old_pin`: run to t = 40 s, reset; the old pin is shown until the first retirement, the cursor then starts at the new epoch's earliest Retired endpoint, never clamped by the old A; same for a backward seek.
- `cursor_generation_change_cuts_forward`: lattice change mid-run; c jumps to the republished endpoint once, then advances normally.
- `cursor_exact_modes_present_the_request`: offline, coupled (`display_cursor` 0) and unwired; c = r every frame, selection identical to P1's on the same script, and at a run cut `presented_time` = t_B ≥ r.
- `cursor_held_frame_reemits_the_pin`: a frame repeated with unchanged r and transport (and a frame where a new endpoint retires with Δ = 0) re-emits the same presented time; the window does not age.
- The stall test above, plus `frame_history_jitter_and_reanchor_never_step_backwards` with the cursor on.

GPU, lead-run through `gpu_queue.py`: `liquid_frame_live_held_frame_matches_offline` (live exact; after each ticking frame, hold until that publication's writer stamp has completed and it is retired, then assert the selected endpoints, blend and span equal offline's before comparing pixels); `liquid_frame_whitewater_reads_the_selected_slot`; the publication proof without the interior fallback assertion.

## 5. Phasing

**P1 — History with exact selection (landable alone).** `FrameRing` use leaves `liquid_frame` only: sites `liquid_frame.rs:18,78,105-113,164-176,272-292,331,379-395,409`; re-derive with `rg -n 'FrameRing|RingWrite|\bRING\b|copy_gated|clear_interior' crates/manifold-renderer/src/node_graph/primitives/liquid_frame.rs`, stop if the count differs (the RING sites at the interior, solid and face allocations, `liquid_frame.rs:209-246,307-361`, belong to the same move). Deliverables: `frame_history.rs` (3.1–3.3, 3.8), the liquid_frame memory-extent rule in `liquid/extent.rs` at H_MAX slots, D4, the clear removal, whitewater retention with builder, preset and loader migration, `presented_time`, `publications_skipped`, every CPU test but the cursor ones, the GPU proofs. Gate: `cargo nextest run -p manifold-renderer frame_history`, clippy `-p manifold-renderer`, scoped GPU gate, a content-thread trace run; negative: the rg above returns nothing, `git diff --stat -- matter_frame.rs frame_ring.rs` is empty. On stage: every eligible frame-end publication is attempted, selection is exact over the retained endpoints, skips are reported in `publications_skipped`; endpoints can still pass unshown when several retire between presentations, and holds at the newest retired endpoint remain under GPU-bound frames; export pictures unchanged where the requested time lies within the latest pair.

**P2 — Cursor.** One session, one commit.

*Entry state.* P1 on main: `rg -n 'fn select|fn pinned|presented_time' crates/manifold-renderer/src/node_graph/liquid/frame_history.rs` hits; `rg -n '"display_time", "epoch"' crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs` finds the frame wire (`:492`); `rg -n 'node.liquid_state" => &\[\("dropped_seconds"' crates/manifold-renderer/src/node_graph/graph_loader.rs` finds the clock-port table (`:722-727`). Any anchor moved: re-derive, then proceed.

*Read-back.* This doc's section 3.3, section 3.4 whole, D3, D6, section 6; `liquid_frame.rs` `run`; `frame_history.rs` `select` and the test model; `graph_loader.rs` `wire_liquid_intervals`. Restate the mode rule, the per-frame order, the cut rule and the epoch exception before code.

*Deliverables.*
- `liquid/display_cursor.rs`: `DisplayCursor` (state per section 3.4, a fixed sample ring, no per-frame allocation) with one entry, `advance(epoch, r, transport, newest_two) -> Option<f64>`, and `cut(t_a)`. CPU only, no GPU types.
- `HistoryCore::newest_two()` (N, N₂ of the current generation, any run) and the layout's epoch. `select` is unchanged.
- `gpu_flip_domain`: output `display_cursor` (`ScalarF32`, added to `OUTPUTS`), 1 when `!offline && self.coupled.owner.is_none()`, else 0, set beside `display_time` (`:869`).
- `liquid_frame`: optional input `display_cursor`; `run` follows section 3.4's order on the live path; the offline branch stays exact.
- Wire: the builder's frame wire list (`gpu_flip_preset.rs:492`) gains `display_cursor`, which covers the Dam Break builder and Add Fluid's liquid body (`gpu_flip_liquid_body`, `:707`); `WaterDamBreakGpuFlip.json` and `WaterDamBreakParticles.json` regenerate from the builder.
- Migration: the clock-port table in `wire_liquid_intervals` gains `"node.liquid_frame" => &[("display_cursor", "display_cursor")]`, applied only when the source is a `GPU_FLIP_DOMAIN_TYPE_ID` node (a Matter domain has no such output; a dangling wire is the forbidden silent failure). An authored `display_cursor` wire, including a constant 0, is kept. Saved projects get the cursor on load; nothing is rewritten on disk. Loader tests: idempotent, Matter-sourced frame untouched, authored wire kept, nested group, the `manifold-io` fixture `water_layer_graph_v1160.json` loads wired.
- Preset test: extend `liquid_presets_feed_state_dropped_time_from_their_clock_domain` (`gpu_flip_preset.rs:1686`) or add its sibling: every `node.liquid_frame` in the GPU FLIP graphs has exactly one `display_cursor` wire from its own clock domain.
- Tests: section 4's cursor list in `frame_history.rs`'s model (constructor flag for cursor on/off). The GPU proof `liquid_frame_live_held_frame_matches_offline` compares live against offline, so it must run exact: check whether its graph now carries the wire and, if so, remove that wire from the proof's graph explicitly, documented in the test. A green proof reached by loosening its comparison is a failed gate.

*Trace plan.* One temporary `eprintln!` in `liquid_frame::run`, after the cut rule: frame index, T, r, c, N, N₂, blend, r − c, `publications_skipped`. Build the worktree binary with its own `CARGO_TARGET_DIR` after touching every source under `crates/` (BUG-nrdhb (shared target dir links stale crates)), copy it to `$S/manifold-cursor`, and run through `scripts/gpu_queue.py`: `$S/manifold-cursor frame-time $S/preset/gpu_flip_dam_break.manifold --frames 600 --splash-frames 100 --stamp-every 100000` in paced mode (never `--frame-clock`, which waits on the GPU every frame and removes the retire delay), once at the project's resolution and once at `--resolution 128` for GPU-bound. `$S` is the session scratchpad named in the lead's brief. Report the distributions against section 3.5, then delete the probe; `rg -n 'eprintln' crates/manifold-renderer/src/node_graph/primitives/liquid_frame.rs` returns what main returns.

*Acceptance.* On the oracle, after the first second: blend strictly inside (0, 1) on at least 90% of frames; c never decreases; r − c ≤ 50 ms (3 frames at 60 fps) at p95 in the 60 fps run, reported (not gated) for the GPU-bound run. Frame-time p50 within noise of main: `$S/time_set.sh main:manifold-final:gpu_flip_dam_break.manifold cursor:manifold-cursor:gpu_flip_dam_break.manifold`, interleaved three times through `gpu_queue.py`, clean builds; the cursor's per-frame work is a few float compares, so a regression means a wiring mistake. Demo: L2, Peter watches the Dam Break in P3.

*Gate.* `cargo nextest run -p manifold-renderer frame_history cursor graph_loader gpu_flip_preset`; `cargo nextest run -p manifold-io` for the fixture; clippy `-p manifold-renderer -- -D warnings`; scoped `scripts/gpu_proofs_gate.py`; the content-thread `MANIFOLD_RENDER_TRACE=1` check. Negative: `git diff --stat origin/main -- crates/manifold-physics` is empty; `rg -n 'display_cursor' crates/manifold-renderer/src/node_graph/primitives/matter_frame.rs crates/manifold-renderer/src/node_graph/liquid/frame_ring.rs` returns nothing.

*Forbidden.* Touching `manifold-physics`; a fixed lag; extrapolation past N; a cursor in export; reading the clock's `held` flag; adapting `select` to the cursor instead of feeding it c; loosening the held-frame proof.

*On stage.* The water glides between published snapshots instead of stepping at 10 Hz, about 2.5 frames behind the beat at 60 fps; at Speed 0 it settles to a still picture one Sim Rate interval after the knob reaches 0; a reset still shows the old water until the new pour has its first snapshot.

**P3 — Stage acceptance.** Lead-run Dam Break at 60 fps and GPU-bound; Peter looks.

## 6. Decided — do not reopen

1. Export presents at the requested time; the cursor is live-only.
2. Coupled water sits at the rigid completed time.
3. No slot reads another slot; no slot is written except by its publication.
4. A pair never straddles a run; the run cut moves forward only.
5. Matter stays on `FrameRing`.

## 7. Deferred

| Item | Revives when |
|---|---|
| GPU-resident count and acceptance: publications selectable without CPU retirement. Removes the retire-delay term, not cadence jitter. Cost: `count_a/b` become buffer reads in every seam consumer, the sorted tail needs a sentinel id for the binary search, fused kernels gain two gathers, rejected ticks need a GPU-side repeat per field, the CPU no longer knows what is shown. | the P3 lag exceeds the allowance at 60 fps, or the tick split lands |
| Publishing intermediate ticks of a two-tick frame | the tick split |
| HUD row "display behind by X ms" from `presented_time` | LIVE_SIM_CLOCK P4 |
| Packed whitewater retention | admission refusals on whitewater at res 128 |

