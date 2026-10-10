# GPU FLIP display history — retained publications and a presentation cursor

**Status:** IN PROGRESS · 2026-10-06 · P1 and P2 landed; owed: P3, Peter's stage look (section 5). Bead: BUG-ckvpp (display shows every third tick, never interpolated).
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

**Mode.** The cursor runs when `display_cursor` is wired and ≥ 0.5 and the frame is not offline. Everything else is exact: c = r, selection as P1, and the cursor's state is cleared. The domain outputs 1 when not offline and no rigid owner is attached (`gpu_flip_domain.rs:823-832`), else 0; `liquid_frame` re-checks `offline_simulation()` itself (D6), so an offline frame is exact whatever the wire carries.

**Inputs per frame.** r is the domain's `display_time` and D its `dropped_seconds`, both through the production f32 scalars. r is non-decreasing within an epoch: the clock restarts the epoch on any backward transport (`clock.rs:358-359`) and maps a non-decreasing transport through a non-negative Speed history. Δ = max(0, r − r_prev), r_prev from the same epoch. N and N₂ are the newest and second-newest Retired endpoints of the current generation, any run, E the earliest, all read after this frame's retirement (`HistoryCore::retired_bounds`). T is the frame's transport time (`ctx.time.seconds`). Constants: RATE 0.05, FILL_RATE 0.25, HORIZON 0.5 s, WINDOW 2 s, FILL 1 s; GUARD = (N − N₂)/4, or 0 when N₂ does not exist.

**State** lives in `liquid_frame` (`liquid/display_cursor.rs`), never in the clock: c (or none), the epoch, r_prev, T_prev, D_prev, the advancing time Φ, the fill end, and the deficit window.

**Advancing time and the window.** Φ accumulates max(0, T − T_prev) on frames with Δ > 0 and no reanchor (step 5) only; a pause or a Speed 0 stop neither ages nor feeds the window, however long. The window is a fixed ring of 72 buckets, each WINDOW/64 of Φ wide and holding the maximum sample that fell in it; L is the maximum over the 65 buckets ending at the current one, 0 when empty. The span a sample stays in L is between WINDOW and WINDOW·65/64 whatever the frame rate, the storage is allocated once, and nothing overflows: more frames per bucket only raise that bucket's maximum.

**Per frame, in order:** retire; cursor step; select (or hold); cut rule; stamp; reclaim.

1. Epoch. A different epoch clears the state (no c, empty window, Φ kept) and sets r_prev, T_prev, D_prev to this frame's values.
2. Initialization, when there is no c and a Retired endpoint exists: c := max(E, the pin's `presented_time` when the pin is of the current generation, `Presentation::generation`), fill end := Φ + FILL; select. This covers the epoch's first retirement (the pin is of an older generation, so c = E, the earliest even when several retire together) and a mid-epoch switch from exact to cursor (c starts at the picture already shown, never behind it). With no Retired endpoint nothing is selected and the old pin holds (section 3.3).
3. Hold. When Δ = 0 and a pin exists, nothing is selected and c does not move: the picture is held. A late retirement at a run cut or a generation cut does not change the pair while transport is stopped. The one exception is the epoch: after a reset, seek or rate change the cursor has no c, so step 2 shows the new epoch's first endpoint the frame it retires, the jump the user asked for. A lattice change clears the pin (section 3.3) and its republish at unchanged time (section 3.7) is shown when it retires, since there is no pin to hold.
4. No N (current generation has nothing retired, e.g. a field-layout change waiting for its republish): c and the window stay, the old pin holds.
5. Sample. On a frame with Δ > 0 where D did not rise, Φ advances by the frame's transport and behind = max(0, r − N) is recorded at Φ. A frame where D rose (D > D_prev, a clock reanchor: a forward seek or the first frame after a stall, `clock.rs:384`, `:445-457`) neither records nor ages the window: its deficit and its transport jump are time the clock dropped, not retirement delay. Recording it would hold the water 100–130 ms back for WINDOW; aging by it would empty the window in one frame.
6. target = r − L − GUARD; base = c + Δ; u = clamp((target − base)/HORIZON, −k, k), k = FILL_RATE while Φ < fill end, else RATE; c ← min(base + Δ·u, N); select(c).
7. Cut rule: when the pin is of the current generation and its t_A > c, c := t_A. It fires at run cuts, generation cuts and never in steady selection (B is the earliest t > c, A its predecessor, so t_A ≤ c).

**Monotonicity.** c never decreases within an epoch in cursor mode: u ≥ −FILL_RATE > −1 makes base + Δ·u ≥ c, and the N clamp cannot pull c back because c ≤ N already holds — c is only ever set to E, to t_A of a current-generation pin, to a presented time ≤ N, or to a value clamped at N, and N never decreases within a generation. A new generation in the same epoch republishes at the unchanged simulation time, which is ≥ the old N ≥ c. The implementation asserts c ≤ N after every step in debug builds.

**Catch-up on a seek or reanchor (Peter, 2026-10-06: catch up).** A forward seek or a stall that the clock reanchors gives one frame a large Δ. Step 6 then advances c by about Δ and the N clamp lands it on N: the picture jumps to the newest finished snapshot, skipping about two frames of motion in that one frame. The jump selects an already-retired slot; it adds no simulation tick, publication or GPU work. The clock then holds r for one Sim Rate interval (its reanchor plateau) and the cursor holds with it. After the jump c sits about one interval further behind r than equilibrium (r − c ≈ 100 ms against 42 ms at 60/30), because r jumped to the newest accepted state while N trails it by the retire delay; the excess closes with time constant HORIZON, within 10% in about 1.5 s. Accepted: faster closing means a visible fast-forward.

**Leaving cursor mode.** Cursor to exact (coupling attaches, the wire is set to 0, export starts) clears the state and presents r at once; that frame may step back by up to L + GUARD, the one backward move, caused by an explicit mode change. Exact to cursor uses step 2.

**The epoch exception.** "c ≤ N" and "never below the pinned A" are statements about one epoch. A pin left over from the previous epoch can sit past the new epoch's N; it bounds nothing, because step 1 cleared the cursor and step 7 reads only a current-generation pin. Its `presented_time` is reported as is during the hold.

**presented_time** is min(t_A + blend·span, t_B), clamped explicitly so f32 rounding of blend and span never reports a time past the pin's B, which is ≤ the pin generation's N.

**Exact mode at a run cut.** c = r is never moved by the cut rule (D6). When r lies before the first endpoint of the run B moved into, A = B and the picture is that endpoint whole: `presented_time` = t_B > r, ahead of the request by less than one publication gap and never past N. It holds there until r passes t_B. This is P1's behavior, stated.

**Speed 0 and pause.** Pause stops transport, so r stops at once and Δ = 0. Speed 0 does not: r is the simulation one Sim Rate interval of transport ago (`clock.rs:440-441`), so for one interval after Speed reaches 0 it keeps rising toward the frozen simulation time while no new endpoint arrives. Those frames have Δ > 0; c advances with them, still clamped at N, and records shrinking deficits that do not lower L. Then Δ = 0, step 3 holds, and the picture is a still, interpolated frame about L + GUARD behind. The cursor reads Δ only, never the clock's `held` flag, which is set at Speed 0 while r still moves (`clock.rs:400`).

**Equilibrium and recovery.** For constant Δ, L and GUARD the unclamped error obeys e ← e·(1 − Δ/HORIZON), first order with no overshoot, and the cursor settles at lag r − c = L + GUARD; while saturated it closes kΔ per frame, so its speed stays within ±5% of real time after the fill second. L is the observed deficit, not a guarantee: when a slow retirement leaves the window, latency contracts, and a later slow one holds at N again without a reanchor. After a stall that raised L by E_s ends, the old samples hold L for WINDOW, the cursor closes at RATE until the error falls under RATE·HORIZON (25 ms), then exponentially with time constant HORIZON. Recovery to within 10% of the pre-stall λ = r − c takes WINDOW + max(0, E_s − RATE·HORIZON)/RATE + HORIZON·ln(min(E_s, RATE·HORIZON)/(0.1·λ)) of advancing transport: 3.39 s for the modelled stall (E_s = 50 ms, λ = 42 ms). The test bound is 4 s.

**Precision limit.** r, N and D reach `liquid_frame` as f32 scalars, and the tests run the cursor through that conversion. Supported: the f32 step of r at most a tenth of the frame Δ, which holds below 2^14 s (4.5 h) of simulation at Speed 1 and 60 fps, and below 2^11 s (34 min) at Speed 0.1. Past that, Δ jitters by up to one step (it is the difference of two rounded times), and once the step exceeds Δ requests repeat, the cursor holds and the window under-ages. A one-interval reanchor can vanish into `dropped_seconds` once its f32 step exceeds the interval, depending on rounding phase: from 2^19 s (6 days) of dropped time at a 30 Hz Sim Rate and Speed 1, and from 2^15 s (9 h) at Speed 0.1; the limit scales with Sim Rate and Speed. The cure, if a set ever runs that long at low Speed, is an epoch-relative f64 (or split) display time from the domain. `liquid_frame` outputs `presented_time` and `publications_skipped`.

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
| 60 / 30, R 3→6 for 1 s | — | 75→125, r − c within 10% after 3.4 s | — | 3 hits, 2 holds |

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

- `frame_history_selector_matches_frame_ring_within_latest_pair`: retained {0, 1, 2}: r = 1 → (1, 2, 0) and r = 2.5 → (1, 2, 1) as FrameRing; r = 0.5 → (0, 1, 0.5) by design.
- `frame_history_two_tick_frames_publish_frame_end_only`.
- `frame_history_generation_change_republishes_unchanged_time`.
- `frame_history_pinned_pair_survives_reset_until_first_retirement`; `frame_history_run_boundary_cuts_forward`.
- `frame_history_every_free_transition_checks_both_stamps`: random sequences of reset, resize, rewire, encode failure, cancel over pending and pinned slots; no slot is Free while pinned, retained or with an incomplete stamp; an obsolete Pending never Retires, including when the layout recurs.
- `frame_history_consecutive_rejections_hold_and_free`; `frame_history_allocation_refusal_and_budget_exhaustion_count_skips`.
- `frame_history_pause_holds_speed_scales_backwards_requested_holds`.
- `frame_history_coupled_exact_one_two_tick_rejection_reset`.
- `frame_history_slot_use_over_2000_frames` ≤ section 3.8 per scenario.

Cursor tests (P2), in `liquid/display_cursor.rs` over the same model and the real clock, with r, N and D passed through f32 as production does. Every one asserts on every frame: c non-decreasing within the epoch in cursor mode, c ≤ N, `presented_time` ≤ the pin's t_B, blend in [0, 1]. A "hold" is a frame whose presented time equals the previous frame's while Δ > 0.

- `cursor_equilibrium_matches_the_latency_table`: 60/30 R 3 and 4, 30/30 R 3 and 4, 60/24 R 3, 27/30 R 3, 60/30 Speed 0.5; after 3 s the median r − c is within 2 ms of section 3.5's r − c column (42, 58, 75, 108, 52, 117, 21 ms), no hold, blend strictly inside (0, 1) on at least 90% of frames.
- `cursor_retire_stall_holds_at_newest_then_recovers`: 60/30, R 3→6 for 1 s; every hold is at N; r − c returns within 10% of its pre-stall value within 4 s of advancing transport after the stall ends.
- `cursor_jitter_never_steps_backwards`: the clock's jitter pattern, R seeded in 1..=4.
- `cursor_reanchor_catches_up_without_poisoning_the_window`: a 10 s forward seek and a 0.5 s content stall, both reanchored by the clock; the reanchor frame lands c on N, the selected B is a slot already Retired before the frame, at most one publication is made, L is unchanged by that frame, and r − c is back within 10% of its pre-seek value 2 to 2.5 s later.
- `cursor_epoch_start_holds_at_most_two`: reset and backward seek at R 3; the old pin shows until the first retirement, c starts at the earliest Retired (several retiring together included), at most 2 holds in the epoch's first second.
- `cursor_speed_zero_then_resume`: Speed 1 → 0, a stop of 5 s, then 1, at R 3 and R 4; c rises only during the interval after the change, then the pair and blend are frozen; after resuming, at most 1 hold.
- `cursor_pause_longer_than_the_window_keeps_it`: transport frozen 5 s; c, the pin and L are unchanged throughout and on the first resumed frame.
- `cursor_pause_holds_through_late_cuts`: paused with a pending endpoint that retires as a new run, and one that retires in a new generation; the pair does not change while paused. A reset while paused shows the new epoch's first endpoint when it retires.
- `cursor_mode_flip_never_steps_back`: exact → cursor mid-epoch starts at the shown presented time (pin by generation); cursor → exact presents r at once.
- `cursor_speed_change_rescales`: Speed 1 → 2 → 0.5; per-frame advance within ±5% of Δ after each change settles.
- `cursor_f32_at_one_hour`: the 60/30 R 3 case offset to start at 3600 s of simulation; same bounds as the equilibrium test within 1 ms extra.
- `cursor_exact_modes_present_the_request`: offline with `display_cursor` = 1, coupled (0) and unwired; c = r every frame and the selection equals P1's on the same script.
- `cursor_held_frame_reemits_the_pin`: a repeated frame (same r and T), with and without a retirement on it, re-emits the same pin; Φ does not advance.

GPU, lead-run through `gpu_queue.py`: `liquid_frame_live_held_frame_matches_offline` (live exact; after each ticking frame, hold until that publication's writer stamp has completed and it is retired, then assert the selected endpoints, blend and span equal offline's before comparing pixels); `liquid_frame_whitewater_reads_the_selected_slot`; the publication proof without the interior fallback assertion.

## 5. Phasing

**P1 — History with exact selection (landable alone).** `FrameRing` use leaves `liquid_frame` only: sites `liquid_frame.rs:18,78,105-113,164-176,272-292,331,379-395,409`; re-derive with `rg -n 'FrameRing|RingWrite|\bRING\b|copy_gated|clear_interior' crates/manifold-water-surface/src/primitives/liquid_frame.rs`, stop if the count differs (the RING sites at the interior, solid and face allocations, `liquid_frame.rs:209-246,307-361`, belong to the same move). Deliverables: `frame_history.rs` (3.1–3.3, 3.8), the liquid_frame memory-extent rule in `liquid/extent.rs` at H_MAX slots, D4, the clear removal, whitewater retention with builder, preset and loader migration, `presented_time`, `publications_skipped`, every CPU test but the cursor ones, the GPU proofs. Gate: `cargo nextest run -p manifold-nodes frame_history`, clippy `-p manifold-nodes`, scoped GPU gate, a content-thread trace run; negative: the rg above returns nothing, `git diff --stat -- matter_frame.rs frame_ring.rs` is empty. On stage: every eligible frame-end publication is attempted, selection is exact over the retained endpoints, skips are reported in `publications_skipped`; endpoints can still pass unshown when several retire between presentations, and holds at the newest retired endpoint remain under GPU-bound frames; export pictures unchanged where the requested time lies within the latest pair.

**P2 — Cursor.** One session, one commit.

*Entry state.* P1 on main: `rg -n 'fn select|fn pinned|presented_time' crates/manifold-water-liquid/src/frame_history.rs` hits; `rg -n '"display_time", "epoch"' crates/manifold-nodes-water/src/presets/gpu_flip.rs` finds the frame wire (`:492`); `rg -n 'node.liquid_state" => &\[\("dropped_seconds"' crates/manifold-node-engine/src/load/graph_loader.rs` finds the clock-port table (`:722-727`). Any anchor moved: re-derive, then proceed.

*Read-back.* This doc's section 3.3, section 3.4 whole, D3, D6, section 6; `liquid_frame.rs` `run`; `frame_history.rs` `select` and the test model; `graph_loader.rs` `wire_liquid_intervals`. Restate the mode rule, the per-frame order, the cut rule and the epoch exception before code.

*Deliverables.*
- `liquid/display_cursor.rs`: `DisplayCursor`, the state and steps of section 3.4 with the 72-bucket window, allocated once. One entry, `present(&mut self, core: &mut HistoryCore, frame: CursorFrame) -> Option<Presentation>` (r, T, D and epoch in), which runs initialization, hold, sample, advance, `select` and the cut rule; `clear()` for exact frames. CPU only, no GPU types; the cursor tests live here.
- `HistoryCore::retired_bounds()` → (E, N₂, N) of the current generation, any run, and `HistoryCore::current_pin()` (the pin when its generation is current). `select` is unchanged. `Presentation::presented_time` clamps to t_B.
- `gpu_flip_domain`: output `display_cursor` (`ScalarF32`, added to `OUTPUTS`), 1 when `!offline && self.coupled.owner.is_none()`, else 0, set beside `display_time` (`:869`).
- `liquid_frame`: optional inputs `display_cursor` and `dropped_seconds`; the live path calls `present` when the cursor is on and `clear` + `select(r)` otherwise; the offline branch stays exact.
- Wire: the builder's frame wire list (`gpu_flip_preset.rs:492`) gains `display_cursor` and `dropped_seconds`, which covers the Dam Break builder and Add Fluid's liquid body (`gpu_flip_liquid_body`, `:707`); `WaterDamBreakGpuFlip.json` and `WaterDamBreakParticles.json` regenerate from the builder.
- Migration: `wire_liquid_frame_cursor` in `graph_loader.rs`, run after flattening beside `wire_liquid_intervals`. The frame's clock is the node feeding its `epoch` input, never the first incoming wire; when that node is a `GPU_FLIP_DOMAIN_TYPE_ID`, its `display_cursor` and `dropped_seconds` are wired into the frame's unwired inputs of the same names. A frame whose epoch comes from anything else (a Matter domain, nothing) is untouched. Authored wires are kept, including a constant 0. Saved projects get the cursor on load; nothing is rewritten on disk. Loader tests: idempotent, mixed-domain inputs (a Matter domain feeding another input of the frame), every wire-order permutation of the frame's inputs giving the same result, authored wire kept, nested group, save/reload round trip, the `manifold-io` fixture `water_layer_graph_v1160.json`.
- Preset test: extend `liquid_presets_feed_state_dropped_time_from_their_clock_domain` (`gpu_flip_preset.rs:1686`) or add its sibling: every `node.liquid_frame` in the GPU FLIP graphs has exactly one `display_cursor` wire from its own clock domain.
- Tests: section 4's cursor list in `frame_history.rs`'s model (constructor flag for cursor on/off). The GPU proof `liquid_frame_live_held_frame_matches_offline` compares live against offline, so it must run exact: check whether its graph now carries the wire and, if so, remove that wire from the proof's graph explicitly, documented in the test. A green proof reached by loosening its comparison is a failed gate.

*Trace plan.* One temporary `eprintln!` in `liquid_frame::run`, after the cut rule: frame index, T, r, D, c, N, N₂, L, GUARD, blend, r − c, `publications_skipped`; and one at retirement: the frame stamp minus the writer stamp (R). Build the worktree binary with its own `CARGO_TARGET_DIR` after touching every source under `crates/` (BUG-nrdhb (shared target dir links stale crates)), copy it to `$S/manifold-cursor`, and run through `scripts/gpu_queue.py`: `$S/manifold-cursor frame-time $S/preset/gpu_flip_dam_break.manifold --frames 600 --splash-frames 100 --stamp-every 100000` in paced mode (never `--frame-clock`, which waits on the GPU every frame and removes the retire delay), once at the project's resolution and once at `--resolution 128` for GPU-bound. `$S` is the session scratchpad named in the lead's brief. Report the distributions against section 3.5, then delete the probe; `rg -n 'eprintln' crates/manifold-water-surface/src/primitives/liquid_frame.rs` returns what main returns.

*Acceptance.* On the oracle's 60 fps paced trace, after the first second: median r − c ≤ 50 ms; the per-frame law check passes — c recomputed from the probe columns by step 6 and the cut rule matches the logged c within 0.1 ms on every frame (`$S/cursor_law.py`); blend strictly inside (0, 1) on at least 90% of frames; c never decreases. The residual (r − c) − (L + GUARD) is not gated: it is a first-order filter with time constant HORIZON, so every drop in L (a late sample expiring 2 s later) leaves 37% of the step after 0.5 s by design. Report the uncensored residual and absolute r − c distributions and the histogram of retire delay R (frames from publication to retirement) as information. Sustained R = 4 pushing the median over 50 ms is a fail, reported, never exempted. The GPU-bound run is reported, not gated. Frame-time p50 within noise of main, interleaved three times through `gpu_queue.py`, both sides as bundles (bin plus Resources/presets from each side's own SHA; a release binary reads the bundled presets of the worktree it was built in), clean builds, both sides drawing water. Demo: L2, Peter watches the Dam Break in P3.

*Gate.* `cargo nextest run -p manifold-nodes frame_history cursor graph_loader gpu_flip_preset`; `cargo nextest run -p manifold-io` for the fixture; clippy `-p manifold-nodes -- -D warnings`; scoped `scripts/gpu_proofs_gate.py`; the content-thread `MANIFOLD_RENDER_TRACE=1` check. Negative: `git diff --stat origin/main -- crates/manifold-physics` is empty; `rg -n 'display_cursor' crates/manifold-water-gpu-mpm/src/primitives/matter_frame.rs crates/manifold-water-liquid/src/frame_ring.rs` returns nothing.

*Forbidden.* Touching `manifold-physics`; a fixed lag; sampling the window on a reanchor frame; aging it on stopped frames; extrapolation past N; a cursor in export; reading the clock's `held` flag; adapting `select` to the cursor instead of feeding it c; loosening the held-frame proof.

*On stage.* The water glides between published snapshots instead of stepping at 10 Hz, about 2.5 frames behind the beat at 60 fps; a forward seek or a stall jumps straight to the newest finished snapshot (about two frames of motion skipped once) instead of lagging for two seconds; at Speed 0 it settles to a still picture one Sim Rate interval after the knob reaches 0; a reset still shows the old water until the new pour has its first snapshot.

**P3 — Stage acceptance.** Lead-run Dam Break at 60 fps and GPU-bound; Peter looks. Watch list: a hitch in the first second after load or re-pour (empty history and empty window: one or two single-frame holds), and holds for about 0.3 s after a Speed step while L re-measures. If Peter sees them, the one-line contract amendment is keeping the window across epochs (section 3.4 step 1 clears it). If an L drop is ever visible, the fix is the estimator (a decaying maximum instead of hard expiry), never a faster contraction rate: 25% would read as a twitch.

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

