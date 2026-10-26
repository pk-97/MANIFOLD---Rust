# Water particle pool — the live count on the GPU

<!-- index: Design for S3.4 and S3.5 of the water field wave (the plan's S3.3 and S3.4): GPU FLIP's particle pool keeps room past the fill in every scene, every particle pass in the tick dispatches from a slots-in-use record the GPU writes beside the particle identity, the step always encodes the maximum substeps and the clock plan gates the rest with no readback, and a full pool pauses inflows and sheets with the card saying why. Capacity rule and bytes a slot, the count record and its writers, the pass table, history_slots and its cost, pausing, the deletion list, the job split. -->

**Status:** DESIGN (S3.4 and S3.5 of `WATER_FIELD_WAVE_PLAN.md` section 4, where the plan names them S3.3 and S3.4). Builds on `WATER_TILE_POOL_DESIGN.md` and the S3.3 sort (keys are pool indices). Companions: `LIVE_SIM_CLOCK_DESIGN.md` (the clock plan and inactive slots), `GPU_FLIP_END_GAME_DESIGN.md` D4 (memory follows the water) and D18 (sheeting on by default).

Paths: G = `crates/manifold-water-gpu-flip/src/primitives`, L = `crates/manifold-water-liquid/src`, SF = `crates/manifold-water-surface/src/primitives`, WW = `crates/manifold-water-whitewater/src/primitives`, NW = `crates/manifold-nodes-water/src`, NE = `crates/manifold-node-engine/src`, MN = `crates/manifold-nodes/tests`. Lines are the working tree of 2026-10-27 with S3.3 mid-edit. Files S3.3 is editing (`G/gpu_flip_step/*`, the sort, the sheeting) are anchored by function; the lead re-checks lines at handoff.

## 1. What changes

Today the pool is the fill's count, or with an Inflow every half-cell site up to 2^20 slots. Every particle pass dispatches over the whole pool, dead slots included, sized by a CPU number: the fill's `count` wire, `out_slots`, or a buffer size. The step encodes six substep slots, or one when a readback of an earlier tick's top speed (`retired_max_speed`, a frame or more old) proves one is enough.

After:
- The pool keeps room past the fill in every scene, reserved when the setup is built (section 2).
- The GPU writes the slots in use after every sort and every birth stage, beside the particle identity. Every particle pass in the tick dispatches from that record through an indirect argument (section 3).
- The step always encodes the maximum substeps. The clock plan gates the rest on the GPU, and an idle slot launches no particle, tile or solve work (section 4). `retired_max_speed` and everything feeding it are deleted.
- A birth stage that does not fit gets no slots, and the card says the water is full (section 5).

## 2. Capacity

**Rule.** `P = min(sites, fill + POOL_ROOM_SLOTS, EXACT_F32_COUNT)` for every GPU FLIP domain, whatever its roles. `sites` is every half-cell site of the domain (`∏ 2n`), `fill` the fill's sites, `POOL_ROOM_SLOTS = 1 << 20` (about 1 GB at about 1 kB a slot: today's `AUTO_INFLOW_SLOTS`, renamed, with the same derivation and walk test).
- Why every scene: Sheet Fill Rate is live (it never restarts the water) and D18 turns sheeting on by default, so room can't depend on a role or a rate without a restart when they change.
- Why room past the fill and not a pool total: at 128 the Dam Break's fill (about 2.8 M sites) already passes 2^20, so a total would leave sheets no room exactly where they matter.
- One place: `gpu_flip_geometry` (G/gpu_flip_domain.rs:~190-210), which the node and the extent walk both call. `AUTO_INFLOW_SLOTS`, `role_kinds`, `with_inflow` and `sized_for` (:214-256) go.

**Reserved up front.** P is allocated when the setup changes (Resolution, box, fill: the existing restart, G/gpu_flip_domain.rs:1083) and never at run time. A GPU buffer can't grow on the GPU. Growing it from the CPU needs the count from a readback, a frame or more late, so the pool would still need headroom, and every particle buffer (state, step scratch, history ring, display) would be reallocated between frames: a hitch mid-show. "Grows with the water" means the slots in use grow and shrink on the GPU every substep, and every pass costs what is in use, not what is reserved. The fill keeps holding the P slots (the executor sizes the state and the step from the seed's buffer, `capacity_lineage` in NE/exec/resource_allocation.rs:46-58); its `count` output becomes the filled sites.

**Bytes a slot.** The walk measures 1041 B today (`gpu_flip_auto_inflow_pool_is_about_a_gigabyte`, MN/contracts/node_graph/catalog_tests/gpu_flip_preset.rs:175-209). From the extent rules:

| Buffer | Bytes a slot | After |
|---|---|---|
| fill particles, state `out`, step `out`, step `sorted` | 4 × 32 | stay |
| step saved particles and saved capped (the commit masks) | 32 + 8 | go (section 4) |
| step `capped`, two words | 8 | stays: the move's per-slot push count the stats sum |
| sorter rank and slot input | 8 | stays |
| sheet order, sheet birth list | 4 + 16 | stay (the step proof reads the list) |
| whitewater scratch and emission offsets | 76 + 4 | stay |
| publication pairs | 16 | stay |
| frame history, 16 slots as the walk counts them | 512 | stays (section 8) |
| display: blend, push-out, dots blend and copies, surface sort, blobs, weights | about 190 | stay |

Memory after, at about 0.96 kB a slot: the 64 Dam Break holds P = 1.39 M slots, about 1.3 GB, beside a 0.1 GB lattice. At 128, P = 3.8 M, about 3.7 GB, beside 0.78 GB. D4's 2 GB at 128 already fails today on the fill alone (2.8 M slots, about 2.7 GB): the slot cost has to fall before it can hold, not the room.

## 3. The count on the GPU

**The record.** The identity buffer grows from 16 to 32 bytes (`IDENTITY_BYTES`, L/primitives/particle_identity.rs:5). Words 0-3 stay: next id, epoch, reserved base, reset request. Words 4-6 are `[groups, 1, 1]`, a dispatch triple at 256 threads a group, at least one group so a pass that writes a total still runs on empty water. Word 7 is `n`, the slots in use. Slots [0, n) may hold particles; dead ones there read radius 0 and the next sort drops them. No pass reads a slot at or past `n`. The identity already travels state → step → state and state → frame, and the allocator already reads the live end beside it, so the count lives there. `COUNT_OFFSET = 16` names the record's byte offset.

**Writers** (one thread each, `L/primitives/shaders/particle_identity.wgsl`):
- `seed`: an epoch or a recovery sets `n` to the fill's count, a number the CPU has from params, not a readback.
- `settle` (new): after a sort, `n` = the sort's live end, `ranges[keys - 1].start + .count` (the last pool key since S3.3).
- `reserve`: after a birth stage, `n` = live + the births it grants.

`settle` and `reserve` read the clock plan and write nothing in an inactive slot, as `reserve` does today.

**Gating.** Inside the step, passes dispatch from the step's own 16-byte record `[groups, 1, 1, n]`. `slot_open` (new, one thread, `G/shaders/gpu_flip_step/count.wgsl`) writes it after the clock's plan each slot: active, the identity's words 4-7; inactive, zero groups. `settle` and `reserve` also write it in active slots when the step binds it. So an idle slot's particle passes launch nothing, as its tile passes already don't (`tiles_lists`). Passes outside the step (state, stats, frame, whitewater) run once a tick and read the identity's record.

**The clock.** Its passes use 64-thread groups and gate on their own phase. `marker_args` (new, one thread, G/shaders/gpu_flip_clock.wgsl) runs before each marker reduction: zero groups when `reduction_active(phase)` (gpu_flip_clock.wgsl:353) is false, else `ceil(n / 64)` from the record it is given. It also zeroes the results the three `clear_buffer` calls zero today (G/gpu_flip_clock.rs:255-257). The reduction's later levels keep their direct dispatches (a few groups, shaped for P) and take their lengths from `n` (`ceil(n / 64^k)`). An empty population keeps the cleared aggregate, as now.

**Passes.**

| Pass | Where | Today | After |
|---|---|---|---|
| tiles_mark | G/gpu_flip_step/tiles.rs `encode_tile_allocation` | `groups(count)` | step record |
| sorts 1, 2, 3: count, scatter, stabilise | L/primitives/sort_particles_into_cells.rs `encode_keyed` | `groups(count)` | `SortCount::Record`; `settle` after each |
| sort clear_tail | same | `groups(sorted_capacity)` | not run for a record count: nothing reads past `n` |
| clock marker reduce, classify, remove | G/gpu_flip_clock.rs `dispatch`, `remove_extreme` | a CPU count | clock triple |
| sheeting detect | G/gpu_flip_sheeting.rs `dispatch_with` work[1] | the pool | step record |
| sheeting write, emit write and tally | G/shaders/gpu_flip_sheeting.wgsl `write`; G/shaders/gpu_flip_step/emit.wgsl | stop at the pool's slots | stop at `n` after `reserve`; granted = `n - live` |
| move (`faces_to_particles`) | G/gpu_flip_step/encode.rs; `u.particles` in move.wgsl | `groups(base.particles)` | step record |
| commit masks | G/gpu_flip_step/state.rs `commit_mask` | three whole buffers, every slot | deleted (section 4) |
| tick capture copy `in → out` | G/liquid_state.rs `late_capture` :826-849 | blit of the whole buffer | a one-record-a-thread copy from words 4-6 |
| stats | L/primitives/liquid_stats.rs `run` :248-338 | the fill's `count` wire | `stats_args` (one thread): `ceil(n / 2048)`; `finish` reads it |
| publication sort and gather | L/primitives/particle_publication.rs `encode` :71-160 | the `count` wire; gather over the whole slot | words 4-6; `tiles` from word 4, `count` from word 7; gather to the high-water |
| whitewater emit, dust emit, emission scan level 0, spawn's emitter total | WW/whitewater_step.rs :1462-1466, :1688-1740, :1790-1834, :1957 | every FLIP slot | identity record |

The emit flags and scan run over sites and the sheeting's other passes over cells and ranks: not particle passes.

**Sort and scan.** `SortJob.count` becomes `SortCount::Slots(u32)` (every caller today) or `SortCount::Record { buffer, offset }`. For a record, the shader reads `n` from a new binding at a word the uniform names (`0xffffffff` for slots). `PrefixScan::encode_labelled_counted` runs level 0's blocks and add from a 256-wide record; the higher levels stay sized for the capacity. Values past `n` then reach only outputs past `n`, so no clear is needed.

**Publication tail.** The display reads whole history slots (section 8), so a published slot must keep zeros past its live records. A one-thread `gather_args` sizes the gather to `max(n, hw)`; the gather writes `[0, live)`, zeroes `[live, max(n, hw))` and stores `hw = live` in metadata word 3, reserved today. A new slot's word 3 starts at its slot count (L/frame_history.rs:546-551), so its first publication clears it whole. The digit scan keeps its size; with the stride from word 4 the used entries are its first `16 · tiles`.

**Replay.** Encode replay can't record an indirect dispatch (manifold-gpu `replay_dispatch`: no groups, it runs direct). The tile and solve passes already run that way; the particle passes join them, about a dozen more direct encodes a slot on the content thread. That is in section 4's measurement.

## 4. history_slots without a readback

**Rule.** `history_slots = LIVE_DEFAULT_MAX_STEPS + live_hit_count` every tick, both CPU facts: the formula the extent rule already uses (G/gpu_flip_step/extent.rs:40-41), made one function both call. The step encodes that many slots and the clock plan decides on the GPU which run. Results don't change: the clock schedules the same steps either way, which is what the old "one active slot matches six recorded slots" proof showed.

**Idle slots must cost nothing.** Recording all six slots every frame was retired on 2026-10-04 because live GPU cost rose (LIVE_SIM_CLOCK_DESIGN.md:204). This design removes what an idle slot paid then:
- Particle passes: zero groups (section 3).
- Commit masks: three whole-buffer copies a slot, active or idle (particles at 32 B a slot, capped, φ over the pool), the largest idle cost. They keep `out`, φ and `capped` as the last active slot left them. Every writer of those three now reads the plan (the sort gate, `tiles_lists` and the C lists, the move, emit, sheeting, pockets, the solver tally), so the masks, their saved copies (40 B a slot) and the `set_gate_off` lever whose ungated runs needed them are deleted. The idle-slot proof (section 7, S3.4d) shows nothing writes; a writer it catches gets gated, the masks don't come back.

Still left in an idle slot: each dispatch it encodes (an empty launch and a barrier, and its encode), the clock's one-thread passes, and the passes still dense over the box whose groups return at the top: forces, open fractions, solid velocity, pocket pin and condition, subtract, constrain, remove_crowded, and with an Inflow the emit flags and write over every half-cell site. The tile pool's A passes (WATER_TILE_POOL_DESIGN.md section 5) and tiled emit sites (plan S3 check 4) take most of those to zero groups.

**Cost, measured.** The lead measures as SB was: live tick interval on the Dam Break at 64 and on a still pool, A/B against the parent commit under the GPU lock, plus content-thread encode time per tick in the profiler. If idle slots cost more than the shortcut saved, the next step is the whole-slot skip BUG-e6hdz names: a gated span the GPU runs or skips in one command (manifold-gpu). Never a readback.

## 5. A full pool pauses sources

**Rule.** `reserve` grants a stage every slot it asks for or none: `births = requested` when `requested <= P - live` and the tile pool has not run out this step, else 0. A refused stage writes nothing and `n` stays the live end. Today's partial grant fills sites in scan order (x fastest), so a nearly full pool grows an inflow from one corner and sheets from the lowest cells. All-or-nothing pauses the source instead, and it resumes the first substep its request fits. `reserve`'s `mode` word (1 or 2 today) goes.

**The flag.** No new word. Both sources already pass through `reserve` (sheets at G/gpu_flip_step/encode.rs:696, the inflow at :825), the one gate. It reads the live end from the sort and the tile pool's `exhausted` word (`tiles.counts`, word `free_top_word + 1`, G/gpu_flip_step/tiles.rs:57-67) through a new binding. The tile pool can run short today at 256 (P = 8,456 of 32,768 tiles) and after S4 anywhere.

**Stats.** The words stay: 27 inflow births, 28 inflow births refused, 29 sheet births refused (L/primitives/liquid_stats.rs:45-51). A refused stage adds its whole request to 28 or 29.

**Card.** `births_held_back` (G/liquid_state.rs:128-133) already reads 28 and 29, and `write_fluid_domains` puts `pool_full` on the domain (NW/runtime/state.rs:55-79). Only the text changes, at `crates/manifold-nodes-scene/src/node_graph/scene_vm/setup_check.rs:71`: "The water is full, so inflows and sheets pause until some drains away. Add a Drain or lower Resolution." It covers both pools: a Drain or a coarser Resolution frees particles and tiles alike. It stays on the Resolution and Fill Height rows and latches until the layer is edited, as now. A full pool never stops a tick and never errors.

## 6. Deletions

**The one-step shortcut** (S3.4c and S3.4d):
- G/liquid_state.rs: `"retired_max_speed"` in `LIQUID_STATE_PORTS` (:82); `ReadbackSlot.completed_ticks` (:104-105) with its writes (:880, :905), its log field (:361) and the `accept_retired_stats` parameter (:393, :403); `SyncSample` (:108-115); `usable_speed` (:117-122); outputs `retired_max_speed` and `live_count` (:202-203; `live_count` has no reader); fields `retired_ticks`, `sync_sample`, `sync_sample_tick` (:233-236) and their writes (:403, :416, :714, :752); `retired_speed` (:419-432); `capture_sync_sample` and `synced_speed` (:434-490); the `live_count` scalar (:753-755); `scalars[1]` (:786); `substep_host_synced` (:790-823); the sync capture (:851-853); tests :935-1111; the scalars assert in :915-933 becomes `["tick_index"]`.
- G/gpu_flip_step.rs: `one_step_cfl_safe` (:128-153); inputs `count` (:440), `retired_max_speed` (:453), `initial_obstacle_speed` (:471); `GATE_OFF`, `set_gate_off`, `gating` (:180-200), its uses in encode.rs `gated` (:51-56) and G/gpu_flip_pressure.rs:1038-1039 become unconditional.
- G/gpu_flip_step/primitive.rs: the `count` read (:136-141), the one-step block (:383-416, keeping `substep_count`), `step.count` (:469), the three `commit_mask` calls (:519-552).
- G/gpu_flip_step/state.rs: `saved_particles`, `saved_distance`, `saved_capped` (:43-68), `mask_pipeline` (:12-18), `commit_mask` (:113-142); G/gpu_flip_step/lattice.rs `mask_saved_bytes` (:37-42) and its term (:34); `MASK_SHADER` (G/gpu_flip_step.rs:115), `G/shaders/gpu_flip_commit_mask.wgsl`, and the clock proof that loads it (G/gpu_flip_clock.rs:~1851).
- G/gpu_flip_step/tests.rs: `one_step_bound_requires_a_whole_reference_interval` (~:42-84).
- G/gpu_flip_domain.rs: output `"initial_obstacle_speed"` (:316), `INITIAL_OBSTACLE_SPEED` and its assert (:328-335), the port (:438), `later_obstacle`, `later_obstacle_tick` (:520-521), `NO_OBSTACLE_BOUND` (:526), the later-tick scalars (:618-623), their writes (:656-658, :733, :744), the per-frame entry (:1318-1326), test `gpu_flip_domain_later_ticks_get_their_own_obstacle_bound` (:1560-1590).
- L/bodies.rs: `initial_clock_obstacle_speed` (:557-612) and its tests (:2254-2407).
- NW/presets/gpu_flip.rs: wires `retired_max_speed` (:1434) and `initial_obstacle_speed` (:1435-1439).
- NW/migration/liquid.rs: `("initial_obstacle_speed", ...)` (:47) and the `retired_max_speed` wire (:102-115); test asserts :1301-1306 and :1312-1315; test `retired_speed_wires_preserve_authored_inputs_and_follow_each_steps_state` (:1389-1441).
- MN: `gpu_flip_presets_feed_retired_speed_from_the_particles_state` (contracts/node_graph/catalog_tests/gpu_flip_preset.rs:608); the retired-speed rewire and the `ungated` proof in contracts/node_graph/catalog_tests/gpu_flip_body.rs (:1127-1280, :1585-1600); in contracts/water/primitives/gpu_flip_scene_tests.rs `run_with_retired_speed` (:1037) becomes a plain run, and `gpu_flip_one_active_slot_matches_six_recorded_slots` (:1058) and `gpu_flip_fresh_speed_preserves_force_changes_and_multiple_intervals` (:1671) go.
- NE (A, S3.6): `substep_host_synced` has no implementor left: primitive.rs:595-598 and :1011-1013, exec/effect_node.rs:1288-1294, exec/execution/substep_region.rs:174-178, and the boundary half of `substeps_host_sync_refreshes_the_next_iterations_inputs` (exec/substeps.rs:1306, :1331, :1337-1342, :1625, the `synced` rows :1918-1921).
- docs/LIVE_SIM_CLOCK_DESIGN.md:82, :84 and :196 rewritten to the always-encode rule with no masks.

**Pool sizing** (S3.4h): G/gpu_flip_domain.rs :214-256 and the call at :1024; tests `gpu_flip_domain_makes_room_for_an_inflow` and `gpu_flip_domain_inflow_switched_on_keeps_the_pool` with their `role` helper (:1862-1976), replaced by one capacity test; G/gpu_flip_domain/extent.rs:17 and :25; L/extent.rs `walked_roles` returns only the fill (:224-246), and `crates/manifold-water-gpu-mpm/src/primitives/matter_domain/extent.rs:16` follows; the comment at `crates/manifold-core/src/type_id_migration.rs:263-264` names the new rule.

**CPU count wires, unread once the record lands** (S3.4c, S3.4e): the fill's `count` into the step, stats and frame (NW/presets/gpu_flip.rs:570, :577, :605); the stats `count` param and input; the frame's `count` input and its extent read; the whitewater `count` input (WW/whitewater_step.rs:444, :717, :2247-2248). The state keeps `count`: the seed copy and `seed` need the fill's count.

**RETIRED_PARAMS** (`crates/manifold-core/src/type_id_migration.rs:260`): `node.gpu_flip_step` `count`, `retired_max_speed`, `initial_obstacle_speed`; `node.liquid_stats` `count`; `node.liquid_frame` `count`. Saved shows and four preset fixtures wire these ports and must still load. The lead adds each line with the job that deletes the port (A crate, one line each).

## 7. Jobs

All start after S3.3 is committed. Workers write only; the lead builds each batch once (clippy, then the scoped nextest; GPU proofs through `scripts/gpu_queue.py`). Every water shader edit runs `scripts/wgsl_check.py`. Tests are small scenes (16³ to 24³, a few thousand particles), under 2 s.

| Batch | Job | Worker | Files | What |
|---|---|---|---|---|
| 1 | S3.4a | sonnet | L/primitives/particle_identity.rs, .wgsl; sort_particles_into_cells.rs, .wgsl; prefix_scan.rs, .wgsl; G/liquid_state/extent.rs; the three sort calls in G/gpu_flip_step/encode.rs | the record and its writers; the step calls `settle`; no pass reads the record yet |
| 2 | S3.4b | sonnet | G/gpu_flip_clock.rs, .wgsl; G/gpu_flip_sheeting.rs, .wgsl and its three test files | clock and sheeting take a record |
| 2 | S3.4c | sonnet | G/liquid_state.rs, G/liquid_fill.rs, G/gpu_flip_domain.rs, L/bodies.rs, L/primitives/liquid_stats.rs, .wgsl and extent, NW/presets/gpu_flip.rs, NW/migration/liquid.rs, MN gpu_flip_preset.rs | state, fill and stats on the record; one-step deletion outside the step; all batch-2 wiring |
| 2 | S3.4d | sonnet | G/gpu_flip_step.rs, G/gpu_flip_step/*, G/shaders/gpu_flip_step/*, G/gpu_flip_pressure.rs (gating), step tests, MN gpu_flip_body.rs, gpu_flip_scene_tests.rs | the step on its record, masks deleted |
| 2 | S3.4e | sonnet | L/primitives/particle_publication.rs, .wgsl; L/frame_history.rs; SF/liquid_frame.rs and extent; MN particle_publication_gpu_tests.rs | publication and frame on the record |
| 3 | S3.4f | sonnet | WW/whitewater_step.rs, its fused shader and reference, whitewater tests; the whitewater identity wire in NW/presets/gpu_flip.rs | whitewater on the record |
| 3 | S3.5a | haiku | L/primitives/shaders/particle_identity.wgsl and .rs `reserve`; the two `reserve` calls in G/gpu_flip_step/encode.rs | all-or-nothing, the exhausted word |
| 3 | S3.5b | haiku | `crates/manifold-nodes-scene/src/node_graph/scene_vm/setup_check.rs` | the card line |
| later | S3.4h | haiku | section 6 "Pool sizing" | the pool rule, after S3.4g or the lead's display measurement |
| last | S3.6 | haiku | NE hook, LIVE_SIM_CLOCK_DESIGN.md | A job: the engine hook, the doc paragraphs |

Batch 1 changes no result: the record is written and nobody reads it. Batch 2 must build and commit together: the step, its readers and the deleted inputs meet there. Batch 3's whitewater keeps reading every FLIP slot until S3.4f, which is today's behaviour.

**S3.4a.** Steps:
1. `IDENTITY_BYTES = 32`, `COUNT_OFFSET = 16`; the record's words in the module doc. `seed` writes words 4-7 for its count.
2. `settle` entry and `ParticleIdentity::settle(enc, ranges, keys, identity, plan, step_record: Option<&GpuBuffer>)`. `reserve` writes words 4-7 (`n = live + min(requested, slots - live)`, today's grant) and the step record; `BirthReservation` gains `step_record`.
3. `SortCount` in `SortJob`; every caller passes `SortCount::Slots` (encode.rs `sort_job`, WW/whitewater_step.rs:2078, the node's `run`, the sheeting and step-order tests). A record count dispatches count, scatter and stabilise indirect and skips `clear_tail`.
4. `PrefixScan::encode_labelled_counted`.
5. In encode.rs, `settle` after each of the three sorts with the clock plan; extent `provide("identity", IDENTITY_BYTES)`.
Tests: identity GPU (new `L/primitives/particle_identity/gpu_tests.rs`): `settle` and `reserve` records for live 0, 1, 300 against requested 0, 5, 1000 at 512 slots, `groups = max(1, ceil(n/256))` at n 0, 1, 256, 257, and an inactive plan writes nothing. Sort GPU (NW/primitives/sort_particles_into_cells/gpu_tests.rs): a record count of 300 in 1024 slots, NaN records past it: ranges and `sorted[0, live)` equal the 300-slot sort of a clean buffer, and `sorted` past live keeps its words. Scan GPU: n = 1, 255, 256, 257, 70,000 of 100,000 with stale values past n: the first n outputs equal the CPU inclusive sum. Step GPU: three ticks of a 16³ scene with an inflow, identity word 7 equals stats word 1.

**S3.4b.** Steps:
1. `GpuFlipClockInputs.marker_count` becomes the record buffer; `remove_extreme(enc, particles, count: &GpuBuffer, params)`. `marker_args(phase)` per section 3; level 0, classify and remove dispatch from the clock's triple; the later levels take `ceil(n / 64^k)`; the `count > 0` branches go.
2. `SheetInputs.count` becomes a record (buffer, offset): detect dispatches from it and reads `n`; `write` and `place` stop at `n` after the reservation (`live + before >= n` returns) and report `n - live` as granted (stats[1], stats[3], `SHEET_REFUSED_TAIL`). The standalone proof path binds a record its test writes.
3. Delete the clock's commit-mask proof (~:1851).
Tests: clock GPU: a faster decoy particle at slot n leaves the plan's top speed as it was without it; an inactive plan's triple reads zero groups; `remove_extreme` leaves an over-limit particle past n alive. Sheeting: the existing tests pass a record of their count; a particle past n raises no sheet flag.

**S3.4c.** Steps:
1. liquid_state: the section 6 deletions; `late_capture`'s `in → out` blit becomes a record copy kernel (one 32 B record a thread, from identity words 4-6, built in `prepare_pipelines`); the other captures stay blits.
2. liquid_fill: `count` = `filled_sites(...)`; purpose and doc say so.
3. liquid_stats: `count` param and input go; required input `identity`; `stats_args` per section 3; extent by `items("particles")`.
4. Domain and bodies: the `initial_obstacle_speed` deletions.
5. Preset: delete the fill `count` wires (:570, :577, :605) and the retired-speed and obstacle wires (:1434-1439); add `(step, identity_out) → stats.identity`. Migration: the section 6 lines.
Tests: the trimmed tick-index test (CPU). Capture copy GPU: n = 300 of 1024 copies 300 records and leaves `out` past them untouched. Stats GPU: a record of 300 over a buffer with garbage past it gives words 0-7 equal to a clean 300-slot buffer.

**S3.4d.** Steps:
1. `history_slots(live_hit_count)` in gpu_flip_step.rs, used by `run` and extent.rs:40-41.
2. `StepState.slot_record` (16 B) and `slot_open` in the new count.wgsl (added to `STEP_SHADER`), dispatched after `clock.dispatch` each slot.
3. Every in-step particle pass per section 3: tiles_mark, the three sorts (`SortCount::Record` on `slot_record`, `settle` with it bound), the clock and sheeting with the new APIs, the move indirect with `n` read from the record, emit write and tally stopping at `n`, both `reserve` calls binding `slot_record`.
4. Section 6's step deletions: masks and saved buffers, inputs, `GATE_OFF`.
5. Replace the `GATE_OFF` lever with a gpu-proofs lever `set_encoded_slots(Option<u32>)` for the proof below (lead approval, as `set_all_tiles` was).
Tests: idle slots write nothing: a still 16³ pool (the clock needs one step) for three ticks with the lever at 1 and at the default six: particles, φ, faces and stats bitwise equal. Slots past the count are never read: the same scene with P = n and P = 4n with NaN records past n: bitwise equal particles in [0, n) and equal n.

**S3.4e.** Steps:
1. Publication per section 3: dispatch from identity words 4-6, `tiles` from word 4 and `count` from word 7 in the shader, `gather_args`, the high-water tail; the CPU check becomes source slots ≤ target slots.
2. frame_history: a new slot's metadata word 3 = its slot count (:546-551); confirm the CPU never reads word 3 (`retire`, :614-625).
3. liquid_frame: `count` input goes; `capacity_for` takes the state's slots (:269); the extent reads `items("particles")`.
Tests: publication GPU: a slot holding 1000 records, republished with n = 300 of which 200 live: `[0, 200)` id-sorted as the reference, `[200, 1000)` zero, metadata `[200, epoch, accepted, 200]`. A new slot filled with garbage is all zero past live after its first publication.

**S3.4f.** Steps: `count` input goes; input `identity` (the step's `identity_out`, wired in the preset). The fused emit and dust emit dispatch from identity words 4-6; `EmitParams` and `SpawnParams` read `emitters` from word 7 through a binding; the emission scan runs `encode_labelled_counted`; the scratch stays sized by the FLIP slots. The atom oracle keeps its scalar count. Tests: emission over n FLIP particles with garbage past n equals emission over a clean n-slot buffer.

**S3.5a.** `reserve` per section 5; a binding for the tile counts and the exhausted word index in its params; `mode` goes. Tests: identity GPU: requested ≤ room grants all; requested > room grants 0 and keeps n; exhausted grants 0. Step GPU (gpu_flip_step_tests/emit.rs): an inflow into a pool with no room writes no particle and adds its request to word 28.

**S3.5b.** The text at setup_check.rs:71. The existing kind test (:1324-1338) stays; the text isn't pinned.

**S3.4h.** The rule in `gpu_flip_geometry`, `POOL_ROOM_SLOTS`, the section 6 pool-sizing deletions. Tests: CPU, at Resolution 32, 64 and 128 with no roles, an Inflow, and an Inflow switched off, P is the same for all three, at least the fill, at most the sites and the count rail. The walk test is renamed `gpu_flip_pool_room_is_about_a_gigabyte` and prints bytes a slot at 64 and 128 for D4's report.

## 8. Open

- **The display chain reads the reserved pool.** The history slot is P long, and interpolate, push-out, the surface sort's clear, blobs, bounds, bricks and weights dispatch over it; dead slots cost a write each. With S3.4h every scene reserves room, so the Dam Break's display does that work for 4× its particles at 64. The fix is the display bounded by `count_b`, the fenced count of the slot being drawn: exact, read after its writer finished, never a guess. But interpolate and push-out fuse, and the engine keeps capacity dispatch for fresh-output regions because an unwritten tail would be garbage (NE/freeze/install.rs:2144-2151). So every reader downstream must read within the count before the cap is safe. That is S3.4g, its own short design, with an engine (A) part. S3.4h waits for it, or for the lead to measure the dead tail at 64 and accept it.
- **The frame history** is half of the slot cost: the walk counts all 16 slots (`H_MAX`, L/frame_history.rs:15). How many the live ring holds is unmeasured. Shrinking it is the lever for D4 at 128.
- **Matter** (M2) takes the same `SortCount::Record` and a record beside its own state.
