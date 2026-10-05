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

Contract (C2 to C5; C5b replaces it with section 3.2: the template leaves the frame span, and the failure path below no longer runs the body directly but returns `Err` before the solve encodes anything):

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

**Problem.** The template lives in the frame replay ring. Whenever a frame encodes directly, the body is unrolled on the CPU to the cap. That happens with no span, dispatch profiling, `MANIFOLD_GPU_DIAGNOSTICS=1`, `MANIFOLD_ENCODE_REPLAY=0`, array dumps, or the ring busy. Measured on the Dam Break oracle at cap 900 on a profiled (direct) frame: `node.gpu_flip_step` CPU preparation p50 is about 137 ms, against 10–15 ms at cap 64 on main. A busy ring is exactly when the GPU is already behind. This is also the open half of BUG-fwp2n (unused solver rounds still cost encode time). Recording the template in the frame ring on every path can't fix it: a busy ring has no entry to record into, so the template needs storage of its own.

**Rule.** Pressure rounds always run as GPU-gated chunked executes of the template. That holds on replayed, direct, cold, profiled and diagnostic frames, and in export. The CPU never unrolls rounds, and section 3's CPU fallback is gone. The only answer to a template that can't be built is the solve's named `Err`, returned before the solve encodes anything. The proofs' gate-off lever (`BodyRound::Plain`, test-only) keeps its per-round encode. It is the reference for the gated body product, not a fallback.

**API (manifold-gpu, `metal/template.rs`).**
```rust
pub struct GpuTemplateStore { /* slots, scratch round, stats */ }
pub struct TemplateTicket { /* slot index, execute ranges */ }
impl GpuEncoder {
    /// Walk `body` once into the store's provisional round, validate it whole,
    /// find or build the slot. Encodes nothing; Err leaves every slot as it was.
    pub fn prepare_template(&mut self, store: &mut GpuTemplateStore, at: TemplateRanges, commands: u32,
        copies: u32, body: impl FnOnce(&mut GatedRecorder) -> Result<(), String>) -> Result<TemplateTicket, String>;
    /// Run the prepared slot's chunked executes here.
    pub fn execute_template(&mut self, store: &mut GpuTemplateStore, ticket: TemplateTicket);
}
```
The solver calls `prepare_template` before its prelude (restriction, arm, init, check), so a failed prepare means the solve encodes nothing. It calls `execute_template` where the rounds go. `GatedRecorder` now only collects: it has no encoder.

**One provisional transaction.** `prepare_template` clears the store's scratch round and runs the body once. Each `dispatch_gated` appends one command to the scratch round, with:
- the pipeline, retained;
- the groups;
- the label;
- every buffer binding, as a retained buffer, its offset and its Metal slot;
- every inline binding's bytes, in one flat scratch byte vector;
- the generated sizes bytes.

The body runs once, so `FnOnce` replaces `FnMut` and no Dry pass exists. Validation then runs over the whole round: the body returned `Ok`, it took exactly `commands` dispatches, and every pipeline supports replay. Only after that is a slot looked up, built and published. Any failure, including an ICB or arena allocation failure, returns `Err` with every existing slot untouched. A new slot is built off to the side and pushed only once built. A rewritten idle slot is reset before it is written, and on failure left reset and keyless, so it is never executed stale.

**Slots and keys.**
- **Slot contents.** Each slot owns its replicated ICB (`commands × chunk` commands, bound by `MAX_TEMPLATE_COMMANDS`) and its arena. It also holds, retained, every pipeline and buffer its commands name, its key, and its outstanding command buffers.
- **Key.** Every command's pipeline identity, groups, and bindings in order: buffer identity, offset and Metal slot, or inline bytes and Metal slot, the generated sizes bytes included. Plus `commands` and `chunk`. That covers what `GateKey` and the dispatch key cover today. Identities are pointers to objects the slot retains, so an address can't be recycled while a slot names it. A `prepare` that replaces solver buffers changes the key.
- **Range identity.** The range buffer and its first offset, stride and `copies` are not in the key. They are the ticket's execute arguments, passed to `executeCommandsInBuffer` directly, so the command buffer retains them. A changed Fixed count or stop reuses the slot.

**Retirement: every outstanding user is tracked.** No submission-order contract is assumed.
- Each execute pushes the encoder's current command buffer onto the slot's user list, once per buffer, before commit. That covers `commit_and_continue` splits and profiled encoders.
- A slot is in flight while any user's status is neither Completed nor Error. Completed users are pruned on every lookup, and the list's capacity is reused.
- A slot with an equal key runs even in flight: executing an ICB only reads it, and nothing is written.
- A slot in flight is never reset, written or freed.

**Capacity: the ring grows on demand.**
- **Admission.** A changed key takes the least recently used idle slot, or appends a new slot when none is idle. There is no cap and no exhaustion state.
- **Why that is bounded.** A slot is in flight only while a command buffer that ran it is outstanding. So live slots ≤ distinct keys a command buffer runs × outstanding command buffers, and the host bounds the second factor (frames in flight).
- **Trim.** After a lookup, idle slots beyond 4 are freed, so a burst of churn doesn't stay resident.
- **Drop.** Dropping the store waits for its committed users. A user still uncommitted (a dropped encoder) can't be waited on: its slot is leaked with a warning, never freed under the GPU.

**Execute boundary** (`execute_template`, every encoder path):
1. Flush the frame span's pending stretch (`flush_replay`) and close its open gated segment, so earlier work keeps its order. The template is not part of the frame recording. The span validates on after it.
2. Get the compute encoder. When profiling, open a sampled encoder labelled "pressure rounds": one per execute at Dispatch granularity, one for all executes at Tag granularity.
3. On every newly opened encoder: a buffer barrier, then `useResources` (read and write) for every buffer the slot names, plus its arena and the range buffer.
4. Run the executes: chunk j by range entry `first + j·stride`, with a buffer barrier between executes. Every command keeps its `setBarrier`.
5. A trailing buffer barrier, then `compute_cache.clear()`. Executing an ICB leaves the encoder's bindings unspecified, as replay already handles at `metal/replay.rs` (`flush_span`).
6. Push the user command buffer, and count the executes in the stats.

**Cold frames.** The first visit or a changed key builds the slot: one walk, `commands` command writes for replica 0, and the same writes for the other `chunk − 1` replicas. Stats count walks, slot builds, command writes, hits and executes. T9 asserts those counts, not only timing.

**Profiling.**
- Profiled frames run the template too, so perf-soak CPU numbers measure the live path.
- Per-pass timing inside the rounds is gone. A `Fixed(1)` solve still runs every round command in one execute, so it can't rank passes either. Ranking passes inside a round needs the gate-off proof lever, or a profiled frame-replay-off build of the pre-C5b path. That trade-off is accepted.
- The rounds' spans carry the node's tag, so the per-tag sum is still the node's time.
- Sample exhaustion follows today's rule: no sample reserved means an unsampled encoder.

**Capture and dump.**
- `MANIFOLD_GPU_DIAGNOSTICS=1` keeps frame replay off and the template on. Each execute is wrapped in `pushDebugGroup` with the slot's label.
- An Xcode capture shows the ICB commands per execute.
- Array dumps disable frame replay only. They read at node boundaries, after the solve.
- `MANIFOLD_ENCODE_REPLAY=0` stays the frame replay kill switch and does not cover templates.

**Export, today and after.**
- **Today:** export renders through the same `ContentPipeline` and executor as live (`crates/manifold-app/src/content_export.rs`), with `encode_replay` on by default (`node_graph/execution.rs`). So export frames replay unless the ring is busy or a dump is on. How often export hits a busy ring has not been measured.
- **After:** export runs the same executes as live, whatever the ring does.

**Bindings.** With `setInheritBuffers(false)`, a binding the entry point references but the command leaves out is undefined. `prepare_template` checks `unbound_binding` on every collected command and refuses with `Err`. BUG-cnyc8 (smooth binding 4 mismatch) is fixed in this change: smooth binds `buffer(4, v.rhs)`, which it never reads on sources 0 and 2.

**Cap-900 test configuration.** The renderer feature `pressure-cap-900` sets `MAX_ITERATIONS` to 900 and substitutes the shader's `ROUNDS` at load. The source stays at 64 until C6. Proofs past 64 run under it instead of being filtered out.

**Proofs (C5b gate).**
- **T8, independent reference.** The C0 golden fixture was recorded on main by the pre-template unrolled path. Both its direct and replay lines must hold, every case, on:
  - no span; a replaying span; a busy ring (every entry held in flight by uncommitted encoders); a profiled encoder at both granularities;
  - a cold store (first visit) and a warm one;
  - a cap change between visits (Converged 64 → Fixed 16 → Converged 64 on one store), and density rearming (`pressure_module_rearms_between_solves`).
  - The body golden holds as well. Diagnostics are a process-wide OnceLock and dumps are an executor flag: both only switch frame replay off, so the no-span case covers them, and the doc says so rather than claiming a separate run.
- **T9, counts and cost.**
  - Per warm solve: walks 1, builds 0, command writes 0, executes = chunk count, on a direct and on a replayed frame.
  - A changed key builds exactly one slot with `commands × chunk` writes, and an idle slot is reused.
  - CPU encode of a warm direct solve at cap 900 is within 10% of cap 64 (feature build). The changed-key visit cost is reported.
- **T10, ranges and outputs.** The arm/stop range proof runs on a direct encoder and in a span. It checks range entries, and sentinels past the completed rounds. Pressure, `r`/`z`/`p`/scratch and the body sums must equal the replayed solve bit for bit.
- **T11, profiling.** A profiled frame's pressure, scalars and record are bit-identical to an unprofiled frame's, at Tag and at Dispatch granularity. The rounds' spans are in submission order with start ≤ end, tagged with the solve's tag, one per execute at Dispatch.
- **T12, lifetime.**
  - Each solve's outputs are copied out in its own command buffer before later solves overwrite them.
  - Covered cases: equal-key overlap (two uncommitted buffers running one slot); a key change while a slot is in flight (a new slot appended, the old untouched); `prepare` reallocation; a `commit_and_continue` split between two executes of one slot; dropping the store with committed users in flight.
- **T13, oracle.** Frame-time on Dam Break, interleaved A/B through `gpu_queue` against the main build pinned by SHA, on the matched project and arguments. Tick interval p50 and p95 on plain frames, and `node.gpu_flip_step` CPU preparation on frames stamped every 10. Cap 900 is the feature build. Pass: tick p50 within 1 ms, stamped CPU preparation within 10% of main's at 64.
- **Atomicity (manifold-gpu).** `prepare_template` returns `Err` and leaves an existing slot's ICB, key and users unchanged, then succeeds on the next visit, on:
  - a body error after several dispatches;
  - a count mismatch, over and under;
  - an unrecordable pipeline;
  - a forced allocation failure (test hook).

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
- **C5b — the template on every path.** Section 3.2, BUG-cnyc8 (smooth binding 4 mismatch) fixed inside it. Gate: T8–T12 and the atomicity proofs; T13 before C6. C6 waits for it.
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
