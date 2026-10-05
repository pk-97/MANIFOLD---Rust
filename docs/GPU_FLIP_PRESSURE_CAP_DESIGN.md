# GPU FLIP pressure cap 900 — one round template, run in chunks

<!-- index: BUG-fwp2n (unused solver rounds still cost encode time) and BUG-6rki6 (pressure cap 900 with tolerance stop): the round index moves to a GPU counter, one walked round becomes a transactional replay template, rounds execute in geometric chunks guarded after the stop, and Max Iterations becomes a slider with a migration. -->

**Status:** APPROVED · 2026-10-06 · Claude (design) with Astra's binding review folded in · C4, C5, C5b, C6 owed (section 9 (Phasing)); tracked in BUG-fwp2n (unused solver rounds still cost encode time) and BUG-6rki6 (pressure cap 900 with tolerance stop).
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

The GPU water solve stops on FLIP Fluids' tolerance in 10 to 15 rounds, but every round up to the cap is encoded for every solve of every clock slot. Raising the cap from 64 to the engine's 900 with nothing else changed moved the Dam Break oracle's paced tick from 36.8 to 51.0 ms p50, all of it CPU: the replay layer validating ~51 dispatch keys per round for every round up to the cap (a CPU sample: `replay_dispatch_in`, `Recording::matches`, `refresh_bytes`, memcmp, `Gate::dispatch`). The fix makes every round byte-identical, walks one round per solve, and lets the GPU execute it as many times as the solve needs, in a few large executes rather than one per round.

On stage: the pressure solve gets the engine's full 900-round headroom for the hard frames, a Max Iterations slider, and the same answer live and in export, without the encode cost of rounds that never run.

Companions: `GPU_FLIP_PRESSURE_SOLVE.md` (the solve this changes), `ENCODE_REPLAY_DESIGN.md` (the replay layer this extends; its D10 gated segments), `GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` (the tile lists the post-stop guards read).

## 1. Audit — what exists (verified 2026-10-06 at `6b4766f9f`)

| Piece | Where | State |
|---|---|---|
| Round cap | `gpu_flip_pressure.rs:46` `MAX_ITERATIONS = 64`; `gpu_flip_pressure.wgsl:133` `ROUNDS = 64u` | extend |
| Round loop, one gated segment a round (two for a plain body product) | `gpu_flip_pressure.rs:826-904` | rewrite as a template |
| Round index from the CPU (`Params.slot = i`) | `gpu_flip_pressure.rs:829-830`, `:887`; shader `dot_finalize_main`, `direction_main`, `update_main`, `check_main` (`wgsl:1005-1110`) | move to `progress[1]` |
| Arm writes every gate triple and range entry | `wgsl:1153-1165` `arm_main` | extend to chunk entries |
| Stop zeroes gate triples and later range entries | `wgsl:1080-1088` `stop` | extend to chunk entries |
| Gated segment replay | `metal/replay.rs:465-474` `OpenSegment`, `:555-588` `dispatch_compute_gated`, `:247-272` `place_for`, `:340-403` `execute` | extend with templates |
| Recording, key, stats | `replay.rs:32-52`, `:71-77` `GateKey`, `:200-237` | extend |
| Truncation | `replay.rs:182-196` `Recording::truncate`, `metal/replay.rs:219-242` `ReplayStore::truncate` | not transactional: misses zero-command allocations |
| Dead execute GPU cost | `metal/replay_tests.rs:745` `replay_segment_dead_cost_probe` | ~3 µs each |
| Clock slots per tick | `gpu_flip_step.rs:2532` (one slot on a proven-safe retired speed sample, else six), loop `:2577` | unchanged; drives D3 |
| Iterations control | `gpu_flip_step.rs:2162` "Iterations (0 = Auto)", `:2185-2190` `read_iterations`, `:70` `AUTO_PRESSURE_ITERATIONS` | replace with section 7 |
| Card migration precedent | `crates/manifold-io/src/migrations/solve_level_card_v1180.rs` | copy the shape |
| Lentine flux check | `gpu_flip_lentine.rs:210` | keeps its own 64 |

Extend, don't redesign: the gated segment, the arm/stop pair and the tile lists already carry the live/dead decision on the GPU.

## 2. Decisions

- **D1. The round index comes from a GPU counter.** `progress[1]` is the completed-round count; the shader reads it instead of `Params.slot`, so every round's dispatches are byte-identical. Rejected: one template per round index (no saving).
- **D2. One round is walked once per solve and validated as a template.** The replay layer runs it as many times as the cap allows. Rejected: validating every round (the measured 46k key checks per solve at 900).
- **D3. Chunked executes are mandatory.** A dead execute costs ~3 µs and up to six clock slots each encode a solve per tick: 900 single-round executes per solve is ~2.5 ms of GPU per solve over cap 64 and ~15 ms per six-slot tick (Astra, review point 1). Chunks cover rounds `1, 2, 4, 8, 16, 32, 32, …, remainder`: 33 executes at cap 900; a stop at round 12–14 wastes 1–3 guarded rounds inside the 8-round chunk. 32 is the starting maximum, a measured tuning value, not a verified one. Rejected: a single-round head then single executes (hundreds of executes); a solve-level gate alone (zeroing ranges cancels no CPU execute call).
- **D4. Rounds inside a chunk run after the stop, so every round dispatch is guarded on the GPU** (section 5). Rejected: a global `armed → gate` substitution, because outside-round transfers (the pre-arm restriction, the final prolongation, body reaction) must still run after convergence.
- **D5. The template body is a restricted recorder, not `&mut GpuEncoder`.** It can only issue gated dispatches; plain dispatches, copies, nesting, span closure and encoder escape are not expressible. A template either commits whole or rolls back every provisional allocation and runs directly. Rejected: a flush hook that fails the template (a flush cannot suppress the operation that caused it) and a `debug_assert` on plain dispatches (release builds keep running).
- **D6. Positive saved `iterations` keep their meaning (`Fixed(n)`); Max Iterations controls Auto only.** Rejected: reinterpreting saved counts as a cap.
- **D7. One cap for live and export.** Both read the same step inputs; there is no second cap.
- **D8. Lentine keeps a local 64-round constant.** It is a flux check, not the shipped solve.

## 3. The template replay API

`crates/manifold-gpu/src/metal/replay.rs`, `crates/manifold-gpu/src/replay.rs`.

```rust
/// Where a template's copies run: copy c executes by the range entry
/// `first + c * stride` of `ranges` (entries of `GATED_RANGE_BYTES`).
pub struct TemplateRanges<'a> { pub ranges: &'a GpuBuffer, pub first: u32, pub stride: u32 }

/// The only thing a template body can do: issue gated dispatches.
pub struct GatedRecorder<'e> { /* &'e mut GpuEncoder + mode, private */ }
impl GatedRecorder<'_> {
    pub fn dispatch_gated(&mut self, pipeline: &GpuComputePipeline, bindings: &[GpuBinding],
        groups: [u32; 3], gate: &GpuBuffer, gate_offset: u64, label: &str);
}

impl GpuEncoder {
    /// Run `body` as one gated round the GPU executes `copies` times. Inside a
    /// replaying span the body is walked once and recorded or validated as a
    /// template of exactly `commands` gated dispatches; anywhere it cannot be
    /// taken whole, nothing of the walk reaches the GPU and `body` runs
    /// `copies` times directly. `Err` from `body` rolls back and returns.
    pub fn repeat_gated_template(&mut self, at: TemplateRanges, commands: u32, copies: u32,
        body: impl FnMut(&mut GatedRecorder) -> Result<(), String>) -> Result<(), String>;
}
```

Contract:

- **Validation before anything is encoded.** `commands > 0`, `copies > 0`, and the last entry `first + (copies − 1)·stride` inside `ranges`, all in checked arithmetic; `stride` is in entries, the stored offset in bytes. A violation is `Err`.
- **Transaction.** Opening a template closes any open segment and checkpoints: span cursor and mode, recording length, store command count, segment count, arena cursor, and the `replayed`/`recorded`/`direct` stats. Every walked dispatch is provisional. Commit: the walk took exactly `commands` dispatches and nothing failed; the stored segment carries `copies` and `stride`.
- **Failure** (undercount, overcount, a pipeline without replay support, a full key, no segment buffer, no arena room, a full span, `Err`): roll back before anything flushes. Rollback truncates the recording and store to the checkpoint, drops every segment buffer not owned by a kept command (the zero-command case: `place_for` made the buffer and `alloc_bytes` then failed), restores the arena cursor and the provisional stats, and leaves the span in a coherent mode: `Record` at the checkpoint (the recording now ends there), or `Full` after a capacity failure. Then `templates_direct += 1` and, unless the failure was `Err`, the body runs `copies` times directly: each gated dispatch an indirect dispatch on its gate triple, today's path.
- **Flushes respect the transaction.** The internal flush on allocation failure inside `replay_dispatch_in` does not run while a template is open; `flush_span` rolls an open template back before its early-return check. Rollback never touches commands at or before `pending_start`.
- **Capacity accounting is whole-template.** A capacity failure adds the whole template's commands and arena bytes to the store's shortage, measured by walking the rest of the body without encoding, so the next visit's `reserve` can fit it.
- **Body discipline.** The body must issue identical dispatches every time it runs and have no CPU side effects visible outside it: it may run once to walk and `copies` more times directly.
- **Identity.** `GateKey` gains `copies` and `stride` (0 for a plain segment); with chunks (C5) it also carries the chunk layout and replica capacity. A changed copy count, stride, layout or length misses and re-records one template.
- **Execute.** The stored segment runs `copies` times, a buffer barrier before each copy after the first, range offset `offset + c·stride·GATED_RANGE_BYTES`. Resources are declared once per stretch and cover every copy (the range buffer and the shared arenas included). Inline bytes are one set of arena slots shared by every copy; `refresh_bytes` rewrites a slot once when its content changes.
- **Stats.** `templates_recorded`, `templates_replayed`, `templates_direct` (whole-template fallbacks); `segments_replayed` counts physical segment executes, never logical rounds. Provisional validations of a rolled-back template are not counted as replayed or recorded.
- **Vulkan.** Not built and not verified: the twin would be a secondary command buffer recorded with `SIMULTANEOUS_USE` and executed under per-copy conditional rendering. Nothing here is evidence that the Metal contract ports.

### 3.1 Chunks (C5)

The committed template is one validated round of R commands. On cold recording the store instantiates a chunk buffer of `max_chunk · R` commands: the round replicated `max_chunk` times, every replica naming the same pipelines, buffers and arena slots, each command with `setBarrier`. A chunk of c rounds executes the prefix `{0, c·R}` of that buffer by a GPU-written range entry. The final partial chunk is a shorter prefix, so one buffer serves every chunk. Warm refresh writes the shared bytes once. Barriers: every recorded command's, the stretch boundaries', and a buffer barrier between executes, all kept.

### 3.2 The template on every path (C5b)

**Problem.** The template lives in the frame replay ring. Whenever a frame encodes directly, the body is unrolled on the CPU to the cap. That happens with no span, dispatch profiling, `MANIFOLD_GPU_DIAGNOSTICS=1`, `MANIFOLD_ENCODE_REPLAY=0`, array dumps, or the ring busy (all three entries still in flight). Measured on the Dam Break oracle at cap 900 on a profiled (direct) frame: `node.gpu_flip_step` CPU preparation p50 is about 137 ms, against 10–15 ms at cap 64 on main. Replayed frames are unaffected. A busy ring is exactly when the GPU is already behind, so the fallback turns a small hitch into a stall. This is also the part of BUG-fwp2n (unused solver rounds still cost encode time) that is still open.

**Rule.** Pressure rounds always run as GPU-gated chunked executes of the template. That holds on replayed frames, direct frames, cold frames, profiled frames and export. The CPU never unrolls rounds. There is no 64-round fallback and no CPU unrolled path at all. The only answer to a template that cannot be built is the solve's named `Err`.

**Where the template lives.** It moves out of the frame replay ring into a `GpuTemplateStore` that the caller owns (`PressureSolver`, one per solver). The type lives in manifold-gpu. The frame span then treats a template as an opaque execute stretch: it closes validation before the stretch and resumes after it, and never records the template's commands.
- **Slots.** The store holds a small ring of slots. Each slot has the replicated ICB (`commands × chunk` commands), its own arena for inline bytes, its retained resource list, its key, and the last command buffer that executed it.
- **Key.** Every dispatch's key from the walked round, including the inline bytes and the identity of every bound buffer, plus `copies`, `stride` and `chunk`.
- **Warm visit.** A slot with an equal key is executed as is, even while a previous frame still runs it. Executing an ICB is read-only on the GPU, and nothing is written to it.
- **Changed key.** The visit takes an idle slot, or grows the ring by one if every slot is in flight, rewrites that slot, and marks it with this frame's command buffer. In flight, a slot is never rewritten.
- **Bound on growth.** The ring grows to frames in flight × distinct keys a frame, which is small. A hard cap (8) turns runaway growth into the named `Err`, never a CPU unroll.

**Cold frames.** No frame lacks the template. The first visit, or a changed key, walks the body once (as the transaction does today, Dry preflight included). It writes the round into one replica, copies it into the other `chunk − 1` replicas, and executes. That is one walk plus `R × chunk` ICB command writes, paid once per key. Proof T9 below bounds it.

**Profiling.** Profiled frames run the template too, so perf-soak CPU numbers come to measure the live path. Per-dispatch timing inside the rounds is gone by design.
- **Tag granularity.** The rounds are one span, the solve's "pressure rounds", ended by the execute stretch as replayed stretches already are.
- **Dispatch granularity.** The rounds are one span per execute.
- **Ranking passes inside a round.** Use a capped `Stop::Fixed(1)` solve. It walks the body once, then executes one round.

**Capture and dump.**
- `MANIFOLD_GPU_DIAGNOSTICS=1` keeps frame replay off and the template on. Each execute is wrapped in `pushDebugGroup` with the round's label, so incident attribution names the pressure rounds.
- An Xcode GPU capture shows the ICB commands per execute.
- Array dumps disable frame replay only. They read arrays at node boundaries, after the solve, so nothing they read lives inside a round.
- `MANIFOLD_ENCODE_REPLAY=0` stays the frame replay kill switch and does not cover templates. Removing the CPU path removes the switch's meaning for rounds. The guard is the bit-identity proof.

**Export, today and after.**
- **Today:** export renders through the same `ContentPipeline` and executor as live (`crates/manifold-app/src/content_export.rs`). `encode_replay` defaults on (`node_graph/execution.rs`), so export frames replay unless the ring is busy or a dump is on. Export renders as fast as the GPU allows, so ring-busy frames are more likely than live, and those frames unroll to the cap. How often that happens in export has not been measured.
- **After C5b:** export runs the same executes as live, whatever the ring does. The live == export claim for pressure no longer depends on ring state.

**Traps on a non-replay encoder.**
- **Residency.** ICB commands bind by GPU address (`setInheritBuffers(false)`). The executing compute encoder must declare every referenced buffer with `useResources`: the solver buffers, the slot's arena, the range buffer and the gate. Today only replay execute stretches do this, so the direct path needs the same call.
- **Lifetime.** The ICB holds addresses, not references. The slot retains its resource list. A solver `prepare` that reallocates a buffer changes the key's buffer identity, so the stale slot is never executed.
- **Command buffer marking.** Each slot records the command buffer that executes it, on every encoder path, profiled sampled encoders included. A missed mark lets a later visit rewrite an ICB in flight.
- **Ordering.** The arm and the gate write ranges earlier in the same command buffer. A buffer barrier before each execute is required on the direct encoder too, as `execute` does today. The direct encoder must not batch the execute into an encoder that started before the arm without that barrier.
- **Unbound bindings.** With `setInheritBuffers(false)`, a binding the entry point references but the command leaves out is undefined. `unbound_binding` must hold for every template dispatch before C5b ships. That needs BUG-cnyc8 (smooth binding 4 mismatch) fixed first: bind binding 4 on every smooth dispatch, or split the entry point.
- **Profiled encoders.** An execute inside a sampled encoder is untested. T11 below proves timestamps bracket it and the bits match.
- **Body errors.** A body `Err` stays atomic: the Dry preflight runs before any slot is touched.

**Proofs (C5b gate).**
- **T8.** Direct == replay, bit for bit, at cap 64 and at cap 900, over the C0 golden cases and the body golden. It covers:
  - direct frames forced by no span, profiling on, a busy ring (three entries held in flight), and `MANIFOLD_ENCODE_REPLAY=0`;
  - both goldens unchanged at cap 64.
- **T9.** CPU encode of a direct frame at cap 900 is within 10% of cap 64, warm, on Dam Break 64³. The cold visit's cost is reported separately and bounded at ≤ 2 ms.
- **T10.** The arm/stop range proof (`pressure_module_chunk_ranges_arm_and_stop_on_the_gpu`) runs on the direct path too: range entries read back, and scalar and record sentinels past the completed rounds.
- **T11.** A profiled frame's pressure is bit-identical to an unprofiled one, and its rounds show as one tagged span.
- **T12.** Rewrite safety: a key change while a slot is in flight takes or grows another slot. The in-flight slot's results are unchanged, read back after both frames complete. Past the slot cap, the solve returns the named `Err` and encodes nothing.
- **T13 (oracle).** Frame-time on Dam Break, interleaved A/B against main at cap 64 through `gpu_queue`. Tick interval p50 and p95 for plain frames and for frames stamped every 10. On stamped frames, `node.gpu_flip_step` CPU preparation at cap 900 is within 10% of main's.

## 4. Shader: the round index from a GPU counter (C1)

`shaders/gpu_flip_pressure.wgsl`. Invariant: before active round k, `progress[1] == k`; after termination it holds the completed-round count. An inactive clock slot's `check_main` returns before touching progress, so the invariant is conditional on an active slot.

- `dot_finalize_main`: `scalars[2u * u32(progress[1]) + u.slot] = total`, `u.slot` the parity 0 or 1; Rust calls `dot_finalize(.., 0)` and `(.., 1)`. Binds `progress` (11).
- `direction_main`, `update_main`: `let k = u32(progress[1]);` with today's arithmetic. Bindings: `update` reaches exactly `MAX_BINDINGS` 12 (`gpu_flip_pressure.rs:1064`).
- `check_main` (round mode): `let k = u32(progress[1]); progress[4u + k] = norm; progress[1] = f32(k + 1u);` then the stop test with `stop(k + 1u)`. Mode 1 (the start) already writes `progress[1] = 0.0`: the counter reset, before the rounds.
- Rust: the round's `Params` lose `slot: i`; `arm` keeps `slot: after` and `tally` keeps `slot: word`, both outside rounds. Integers to 900 are exact in f32.
- At cap 64 with per-round executes this changes no arithmetic: C1 is bit-identical to the C0 golden.

## 5. Post-stop guards (C4)

Rounds inside a chunk past the stop run with their recorded groups. Each kernel decides on the GPU that it is dead. The solve-live value is the single-workgroup gate triple (`Slots::single`), zeroed by `stop`, independent of any level's tile count. Early returns are uniform per workgroup and come before any barrier.

- **Pressure `listed()`** inside rounds reads the mutable gate counts (zeroed by the stop) instead of `armed`; preparation and outside-round transfers keep `armed`.
- **Body partial and product `listed()`** likewise read gate counts inside rounds.
- **`dot_finalize`** returns before reduction or scalar writes when stopped.
- **Round `check`** returns before reduction, record writes, counter advance or another stop; the mode-1 start check still runs after the arm.
- **`coarse_solve`** guards its writes (it has no stop check today).
- **`impulse_finalize`** guards before its reduction and its writes to sums and reaction; its clock guard does not see convergence.
- **Dense `zero_main`** guards in-round zeroing (each prolongation target and the fine body product); outside-round zeroing is unguarded.
- **Bindings.** After C1 `update` fills `MAX_BINDINGS`. List-based kernels rebind the existing list-count binding to `gate` inside rounds instead of adding one; singleton kernels (`dot_finalize`, `check`, `coarse_solve`, `impulse_finalize`) get an explicit binding plan in C4, checked against each pipeline's binding count.
- **Level-k bodies.** The `Plain` round (level k > 0 with bodies) becomes one gated segment of `v_cycle + 11 + 3k` commands: `Slots` gains a dense triple per level that `arm_main` writes and `stop` zeroes, `zero` gets a gated form on it, and the in-round chain uses the gated `Gate`. The pre-arm restriction and the post-stop final prolongation stay plain. After C4, `Plain` exists only under the test lever `gating()` off, whose fixture pins the cap at 64.
- **Stop by round coverage.** `stop(first)` zeroes the per-round entries from `first` and every chunk entry whose first round is at or after `first`; the chunk executing the stop already read its range.

## 6. Density rearming

`gpu_flip_step.rs:1824-1836` solves again on the same buffers with `phi: None` and no bodies. Each solve dispatches `arm`, which rewrites every gate triple, per-round entry and chunk entry live (with chunks: the whole chunk-range plan and the solve-live triple), and the start check resets the counter. The density template is its own stored segment after the pressure one. A pressure solve stopped at round k cannot shorten the density solve; an inactive slot arms both off.

## 7. Control contract: Max Iterations, default 900 (C6)

- **Solver.** `MAX_ITERATIONS` 900, `ROUNDS` 900u (`shader_rounds_match_the_iteration_cap` holds), scalars, progress and ranges grow (7.2 KB, 3.6 KB, 14.4 KB plus chunk entries). `Stop::Converged(n)` stops on `TOLERANCE` within n; `Stop::Fixed(n)` runs n.
- **Domain.** Param `max_iterations`, label "Max Iterations", Int, default 900, range 1..=900, carried in `GpuFlipGeometry`, output `max_iterations`.
- **Step.** Input `max_iterations: ScalarF32` optional beside `solve_level`; unwired means 900. Validation: a non-finite value is the named refusal (no NaN cast); the value rounds to nearest; outside `1..=900` is the named refusal. `iterations <= 0` → `Stop::Converged(max)`; `1..=MAX_ITERATIONS` → `Stop::Fixed(n)` (a saved fixed count may exceed Max Iterations: Max Iterations controls Auto only, and the param help says so). `AUTO_PRESSURE_ITERATIONS` goes.
- **Migration.** `crates/manifold-io/src/migrations/pressure_cap_card_v1190.rs` after `solve_level_card_v1180.rs`: wire `domain.max_iterations → step.max_iterations`, binding `IntRound` default 900, card min 1 max 900 whole numbers in "Fluid" beside Solve Level; rung `1.18.0 → 1.19.0`; `CURRENT_PROJECT_VERSION` 1.19.0; the bundled `WaterDamBreakGpuFlip.json` gets the three pieces. Like its precedent it migrates the first top-level domain/step pair and reports grouped cases; this limitation is stated in the module doc, not described as universal. Tests beyond the precedent's four: authored incoming wires and existing card/binding pieces preserved; positive fixed `iterations` preserved; saved node params and overrides preserved; a partially migrated graph completed; migrating twice is a no-op.
- **Docs.** The `GPU_FLUID_SURFACE_DESIGN.md` parity row becomes matched; `GPU_FLIP_PRESSURE_SOLVE.md` loses the 64 sentence.

## 8. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| C1..C6 change no pressure bit at cap 64 against main | `pressure_module_matches_main_golden` (C0 fixture, recorded SHA, deterministic seeding) |
| Before active round k, `progress[1] == k` | the golden (records compared whole) and C5's sentinel stop tests |
| A template commits whole or leaves no trace | T5 `replay_template_*` proofs in `metal/replay_tests.rs` |
| Nothing of a failed walk executes; outputs written exactly once per copy | T5 non-idempotent proofs |
| A warm solve validates one round, not the cap | T2 counts in `pressure_module_replay_matches_direct` |
| Dead rounds in a chunk write nothing | C5 sentinel tests at, before and after chunk boundaries |
| Live and export share one cap | the step has one `read_iterations` path for both; T7 export test |

## 9. Phasing

Each phase is one commit on a lane branch; the lead reviews between. Test scope per phase: `cargo nextest run -p manifold-gpu` / `-p manifold-renderer` filtered to the touched modules, clippy on touched crates, GPU proofs through `scripts/gpu_queue.py`.

- **C0 — deterministic baseline.** Deliverable: `tests/fixtures/gpu_flip_pressure_golden.txt` recorded on main by `pressure_module_matches_main_golden` with `MANIFOLD_RECORD_GOLDEN=1`: SHA, inputs, mode (direct and replayed), and per case FNV fingerprints of the pressure, the scalars and the progress record. Unused storage is seeded with a fixed sentinel before every solve. Cases: Dam Break problems 0 and 4 and deep-pool density problem 0 at 64³, level 0, plus Dam Break problem 0 at level 1; `Fixed(n)` for n in {1, 2, 16, 24, 25, 63, 64} and `Converged(64)`. Only the legacy-sized region (128 scalars, 68 progress floats) is fingerprinted; C6 tests the new tail separately. Gate: the test passes on the recording SHA.
- **C1 — the counter at cap 64.** Section 4. Gate: the golden, `pressure_module_replay_matches_direct`, `pressure_module_converges_on_the_engine_tolerance`, the body proofs' bitwise checks.
- **C2 — transactional template API.** Section 3 without chunks, no solver change. Deliverables: `repeat_gated_template`, `GatedRecorder`, `TemplateRanges`, key and stats fields, and T5: cold record, warm replay, changed copies, undercount, overcount, zero-command failure, body `Err`, changed stride, changed length, allocation failure after a partial walk then recovery on the next visit, forbidden operations unrepresentable (compile shape) and nesting refused, ring busy, replay off (process isolation: the flag is a `OnceLock`), and exactly-once output with non-idempotent writes in every case.
- **C3 — fine and no-body rounds on the template at 64.** `BodyRound::Without` and `BodyRound::Gated` run as one template of `before` commands, `copies = iterations`, `stride = 2`. Gate: the golden; T2 counts at cap 64 (`replayed` per warm solve = prepare + arm/init/check + one round; `segments_replayed` = cap; `templates_replayed` +1 per solve; deltas only); T4 rearming (pressure-stop/density-live, pressure-live/density-stop with independent records, inactive-to-active reuse).
- **C4 — level-k bodies and the post-stop guards at 64.** Section 5. Gate: the golden; `gpu_flip_body_step_sparse_matches_all_tiles` at level 1 on the template; guard proofs that a chunk-free forced-dead round writes nothing (sentinel seeded).
- **C5 — chunk execution.** Section 3.1 and chunk entries in arm/stop. Gate: the golden; T3 sentinel stops (exact sentinel bits from scalar `2k` and progress `4+k`) at, before and after chunk boundaries via `Fixed(n)`; physical execute counts; guarded-tail behaviour.
- **C5b — the template on every path.** Section 3.2. Prerequisite: BUG-cnyc8 (smooth binding 4 mismatch). Gate: T8–T13. C6 waits for it.
- **C6 — cap 900, controls, migration.** Section 7. Gate: migration tests; a small bounded fixture proving 900 rounds advance (finite pressure, residual falling); T6 warm CPU per solve `Converged(900)` ≤ 1.25 × `Converged(64)` and ≤ 2 ms; T7 Dam Break oracle at 900 against 64 back to back, tick-bearing frames separated from display-only frames, slot counts and completed rounds recorded: paced tick p50 within 1 ms, CPU encode p50 within 10%. A systematic GPU regression is not waived as noise.

Forbidden moves, every phase: a parallel per-round path kept alive for templated rounds; an exemption list for kernels the guard audit missed; a size cap instead of chunking; a `debug_assert` standing in for a transactional rollback.

## 10. Decided — do not reopen

1. Round index from `progress[1]`.
2. One walked round per solve, transactional, restricted recorder.
3. Geometric chunks are mandatory; 32 is a starting maximum to measure.
4. Every in-round kernel is guarded on the solve-live triple; outside-round transfers are not.
5. Positive saved iterations stay `Fixed(n)`; Max Iterations is Auto's cap.
6. One cap live and export.

## 11. Deferred

- **Parallel stop zeroing** (~1,800 stores from one thread once per solve): revive if a profile shows `check_main` over 20 µs.
- **The Vulkan twin** (section 3): revive with the Vulkan backend.
- **Chunk maximum tuning past 32**: revive if T7 shows the guarded tail in the GPU p50.
