# GPU FLIP display history — retained publications and a presentation cursor

**Status:** APPROVED design, P1 in build, P2 open (section 3.4 items marked open) · 2026-10-06 · Claude, reviewed by Astra. Bead: BUG-ckvpp (display shows every third tick, never interpolated, ~0.25 s late).
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

**Open for P2 (not built):** cursor initialization (proposed: c starts at the epoch's first retired endpoint); GUARD when N₂ is absent (proposed 0); the epoch exception to "never below the pinned A" and "c ≤ N" when an old pin sits past the new epoch's N; the effective presented time at a run cut in exact mode; Speed 0 is not Δ = 0 in general, since the delayed requested-time history (`clock.rs:404`) can still advance; the stall recovery claim (Astra's model returns within 10% 3.15 s after the stall ends, not within 3 s). The rules below are the draft.

Per frame: requested r, Δ = max(0, r − r_prev) in simulation seconds (0 on the epoch's first frame), newest retired N and the one before it N₂. Constants: RATE 0.05, FILL_RATE 0.25, HORIZON 0.5 s, WINDOW 2 s of transport, GUARD = (N − N₂)/4.

1. A frame with Δ > 0 records behind = max(0, r − N) under its transport time; held frames neither sample nor age the window. L = the maximum over samples within WINDOW.
2. target = r − L − GUARD.
3. base = c + Δ; u = clamp((target − base)/HORIZON, −k, k); k = FILL_RATE for the first second of transport after the epoch's first retirement, then RATE.
4. c ← min(base + Δ·u, N), never below the pinned A.

For constant Δ and L the equilibrium is c = target (comparing target with the un-advanced cursor sat Δ ahead of it and hit N every other frame). Δ = 0 holds c within a run even when N advances. L is the observed deficit, not a guarantee: when a slow retirement leaves the window, latency contracts, and another slow one can hold again without a reanchor. The clamp keeps c ≤ N; non-negative advance and the run rule keep it from moving back; the correction can oscillate within ±5% of real time. Exact policy (`display_cursor` unwired or 0, offline, coupled): c = r. `liquid_frame` outputs `presented_time` and `publications_skipped`.

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
| 60 / 30, R 3→6 for 1 s | — | 75→125, back within 3 s | — | 3 hits, 2 holds |

At 60 fps the water trails transport by 75 ms against D10's 33 ms: 42 ms more, 2.5 frames. In the measured case (≈30 fps GPU-bound, R 3–4) 108–142 ms, interpolated every frame, against 170–270 ms stepping today: retained publications remove the throttling component; the retire delay at the GPU-bound frame time remains, and only the P2 trace and Peter's observation say what it is. Holds in the first second after an epoch starts and under a retire-delay increase are expected. These are arithmetic, not runtime numbers.

### 3.6 Whitewater

`liquid_frame` gains `foam_in/bubble_in/spray_in/dust_in` from `liquid_state` and `foam_b/bubble_b/spray_b/dust_b`. The builder (`gpu_flip_preset.rs:636-641`) and `WaterDamBreakGpuFlip.json` move the four interpolate nodes' `particles_b` onto them; the pool and state outputs stay current for the simulation feedback. `graph_loader.rs` migrates saved graphs after flattening (`graph_loader.rs:1222-1233`), so nested groups are seen as flat wires. Predicate: an interpolate node whose `particles_b` is a `liquid_state` class output S.c, whose `blend` and `span` both come from the same `liquid_frame` F, and F's `state` input is that same `liquid_state`. Then: if F's matching class input (`foam_in`, `bubble_in`, `spray_in` or `dust_in`) is unwired, wire S.c into it; if it is wired from anything else, the node is left untouched (conflicting explicit input preserved). Rewire `particles_b` to the matching `foam_b`/`bubble_b`/`spray_b`/`dust_b`. Authored `particles_a`, `count_a`, `count_b` and every other input are kept as they are. A graph already migrated fails the predicate (its `particles_b` comes from the frame), so the pass is idempotent; several interpolators sharing one frame each migrate and share the one class wire. Tests: idempotence, nested group, shared frame, conflicting wire, save/reload round trip. Copies are capacity-sized; zero tails keep radius 0, `count_b = −1` still works, A stays unwired, so `interpolate_particle_frames_body.wgsl:76` rewinds by (1 − blend)·span from the same B time as the water. Spray's acceleration is the domain's current gravity (`gpu_flip_preset.rs:643-647`), not retained: animated gravity errs by ½·|Δg|·τ² over the rewind τ, 5 mm for a 10 m/s² swing over a 33 ms gap; this is an example, not a bound, since two-tick frames and skipped publications widen the gap. Cost: 128 B × whitewater capacity per slot.

### 3.7 Scope and controls

Matter: D7. Coupled: exact at the completed time (`gpu_flip_domain.rs:746`, `:823-826`); the selector changes its pair where older endpoints bracket, so compatibility checks cover coupled one- and two-tick frames, rejection and reset. Export: D6; equal pictures need the same frame grouping and no live drops (`clock.rs:337`). Pause, Speed 0: Δ = 0; a tick already on the GPU still retires into history (section 3.4 of the seam). Speed: Δ scales, GUARD scales with the retained gap, L re-measures. Reset, backward seek, setup or rate change: new epoch, new generation, pinned pair per 3.3. Lattice or field-layout change: new generation, republish at unchanged time. Growth, identity change: new run.

### 3.8 Budget and failure

H_MAX = 16 slots, each admitted through `admit_candidate_bytes`. The model's use (pending plus retained from A): 4 at 60/30, 5–6 at 30/30 and 27/30, 8 through the stall; add the pinned pair (2) and obsolete slots awaiting readers (≤ 2F = 6 at F = 3). The budget covers steady state and one stall; it is not a proof. Exhaustion (no Free slot: admission refused or H_MAX reached) skips the endpoint once per section 3.2, increments `publications_skipped` (admission refusal also raises today's named error); the cursor holds at N when it catches up; nothing pinned, selected or in flight is evicted. Rejected tick: it adds no endpoint; selection proceeds over what is retained (in P1 that holds N; it does not freeze presentation). Never: reusing a slot with an incomplete stamp (the BUG-l7t4 (pool recycling races in-flight command buffers) class); extrapolating past N (D11).

Memory per slot: 32 B × particle capacity + 4 B × (solid nodes + interior cells + three face arrays) + 128 B × whitewater capacity, wired fields only. Dam Break with whitewater 100,000 (review arithmetic from `liquid_fill.rs:38`, `lattice.rs:80`): res 64, 345,792 particles, 28.6 MiB per slot, 458 MiB at 16; res 128, 2,749,312 particles, 139 MiB per slot, 2.18 GiB at 16, excluding solver and render storage. Res 128 needs total-memory evidence before the budget is used there; admission is its memory arm.

## 4. Conviction tests

CPU, `liquid/frame_history.rs`, a model over the real `SimulationClock` with a scripted retire delay.

- `frame_history_constant_cadence_no_clamp_hits_after_warm_up`: 60/30, 60/24, 27/30 and Speed 0.5 at R = 3; after 3 s blend is interior, c advances by Δ(1 ± 0.05), no clamp hits, lag within 1 ms of section 3.5. Today: blend 1, every third endpoint.
- `frame_history_retire_stall_holds_at_newest_then_recovers`: R 3→6 for 1 s; c never decreases, every hold is at N, lag returns within 10% of its pre-stall value (recovery window open, section 3.4).
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

GPU, lead-run through `gpu_queue.py`: `liquid_frame_live_held_frame_matches_offline` (live exact; after each ticking frame, hold until that publication's writer stamp has completed and it is retired, then assert the selected endpoints, blend and span equal offline's before comparing pixels); `liquid_frame_whitewater_reads_the_selected_slot`; the publication proof without the interior fallback assertion.

## 5. Phasing

**P1 — History with exact selection (landable alone).** `FrameRing` use leaves `liquid_frame` only: sites `liquid_frame.rs:18,78,105-113,164-176,272-292,331,379-395,409`; re-derive with `rg -n 'FrameRing|RingWrite|\bRING\b|copy_gated|clear_interior' crates/manifold-renderer/src/node_graph/primitives/liquid_frame.rs`, stop if the count differs. Deliverables: `frame_history.rs` (3.1–3.3, 3.8), D4, the clear removal, whitewater retention with builder, preset and loader migration, `presented_time`, `publications_skipped`, every CPU test but the cursor ones, the GPU proofs. Gate: `cargo nextest run -p manifold-renderer frame_history`, clippy `-p manifold-renderer`, scoped GPU gate, a content-thread trace run; negative: the rg above returns nothing, `git diff --stat -- matter_frame.rs frame_ring.rs` is empty. On stage: every eligible frame-end publication is attempted, selection is exact over the retained endpoints, skips are reported in `publications_skipped`; endpoints can still pass unshown when several retire between presentations, and holds at the newest retired endpoint remain under GPU-bound frames; export pictures unchanged where the requested time lies within the latest pair.

**P2 — Cursor.** Domain output `display_cursor`, the wire through the builder, `graph_loader.rs:727`'s pattern and `gpu_flip_preset.rs:332`'s preset test; section 3.4; the cursor tests; a trace of r, N, c, blend, `publications_skipped` over 300 frames at 60 fps and GPU-bound, reported against section 3.5. Forbidden: touching `manifold-physics`; a fixed lag; extrapolation.

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

