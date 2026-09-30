# Encode Replay — record a repeat region's dispatches once, replay them every frame

**Status:** IN PROGRESS · 2026-10-01 · Opus 5.5 (worker seat, slot-4). P1a and P1b built on feat/encode-replay, P1b's FFT half on feat/fft-encode-cache; P2 open; P3 blocked on the FFT decision in section 8 (Deferred).
**Prerequisites:** feat/planner-reuse (array slots static after `pre_allocate_resources`).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

A repeat region encodes the same compute dispatches every frame: same pipelines, same buffers, same grid, often the same uniform bytes. Today every one of them pays about 3.5 µs of CPU to go through the Metal encoder again. Encode replay keeps a recording of each outermost region visit in a Metal indirect command buffer (ICB). On later frames every dispatch the nodes issue is checked against the recording at the same position. A match costs a comparison and, if the uniform bytes changed, a small copy; the GPU then runs the matched stretch from the recording in one call. A mismatch runs the validated stretch, re-records the rest and carries on, so a wrong replay can't happen: the recording is only ever a cache of what the direct path would have encoded. It lives in `manifold-gpu` behind the backend surface, and it knows nothing about SWASH.

The honest ceiling: MPSGraph FFTs can't enter an ICB, and on SWASH they are about half the CPU. Replay removes most of the dispatch half; the FFT half needs its own decision (section 8).

Companion docs: `docs/MANIFOLD_GPU_ARCHITECTURE.md` (encoder model, PHASE 10 (Indirect command buffers) is the unbuilt GPU-driven compositor ICB, a different thing); `docs/VULKAN_BACKEND_DESIGN.md` (the Vulkan twin of every piece here); `FFT_WATER_SOLVER_DESIGN.md` on origin/feat/fft-water (the P3 CPU encode lever this design answers).

## 1. Audit — what exists (verified 2026-10-01 on 93b58c4f9, measured on an Apple M4 Max)

Extend, don't redesign.

| Piece | Where | State |
|---|---|---|
| Compute dispatch choke point | `crates/manifold-gpu/src/metal/encoder.rs:512` (`dispatch_compute_grid`) | Every node dispatch lands here: debug group and signpost per dispatch (:540), set pipeline, per-slot binding cache, `setBytes` for uniforms, a naga sizes buffer by `setBytes`, `useResource` per binding (:651), dispatch |
| Compute encoder lifecycle | `encoder.rs:265` (`ensure_compute`), `:279` (`end_current`), `:259` (`raw_cmd_buf`), `:422` (`make_blit_encoder`) | One serial compute encoder (`computeCommandEncoder()`, default serial dispatch) reused until any other encoder type is needed. FFTs, blits and render passes all end it through `end_current` |
| Profiled mode | `encoder.rs:530` (`profile.is_some()` opens one encoder per dispatch) | Per-dispatch GPU timing needs per-dispatch encoders |
| Fault attribution | `crates/manifold-gpu/src/metal/gpu_fault.rs:29` reads `debugSignposts`; `device.rs:1355` sets `EncoderExecutionStatus` | The per-dispatch signposts name the faulting node |
| Compute pipeline creation | `crates/manifold-gpu/src/metal/device.rs:659` (`create_compute_pipeline_inner`): descriptor path at :746 when an archive is loaded, `newComputePipelineStateWithFunction_error` at :779 otherwise | No pipeline is created with ICB support today |
| Pipeline type | `crates/manifold-gpu/src/metal/types.rs:340` (`GpuComputePipeline`) | Holds the state object, slot map, workgroup size, `needs_sizes_buffer` |
| Command buffers | `device.rs:1351` (`new_command_buffer`, `setRetainedReferences(true)`) | A command buffer retains what it encodes directly, including an executed ICB, but not what an ICB's commands reference |
| Deferred GPU frees | `crates/manifold-gpu/src/metal/retire.rs` (module doc) | Buffers the device allocates retire behind a completion fence |
| ICB bindings | objc2-metal 0.3.2: `MTLIndirectCommandBuffer.rs` (descriptor setters, `indirectComputeCommandAtIndex`), `MTLIndirectCommandEncoder.rs:338–430` (`setComputePipelineState`, `setKernelBuffer_offset_atIndex`, `concurrentDispatchThreadgroups_threadsPerThreadgroup`, `setBarrier`), `MTLComputeCommandEncoder.rs:560,612` (`useResources_count_usage`, `executeCommandsInBuffer_withRange`), `MTLComputePipeline.rs:165` (`setSupportIndirectCommandBuffers`) | Present; the `MTLIndirectCommandBuffer` and `MTLIndirectCommandEncoder` features are not enabled in `crates/manifold-gpu/Cargo.toml:47` |
| ICB code | none (`rg -i 'MTLIndirectCommand' crates -g '*.rs'` is empty) | Nothing to reuse |
| Region execution | `crates/manifold-renderer/src/node_graph/execution/substep_region.rs:36` (`run_substep_region`); `substeps.rs:92` (`SubstepRegion`, keyed by `boundary`); `substeps.rs:74` (`MAX_REGION_DEPTH` = 2) | The boundary step runs, then per iteration: set iteration scalars, run the body steps (`run_step`, `execution.rs:1814`), run the boundary's late capture (`capture_step`, `execution.rs:2686`) |
| Executor switches | `execution.rs:150` (`Executor`), `:684` (`set_profiling`), `:726` (`set_dump_all`) | Where a replay switch and the span caches go |
| Vulkan backend | `crates/manifold-gpu/src/vulkan/mod.rs` | Phase 0 scaffold, no command buffers yet. Its design already maps `GpuBinding::Bytes` to a host-visible uniform ring (VULKAN_BACKEND_DESIGN.md D5 (Bytes)), completion to a timeline semaphore (D9 (GpuEvent)), hazards to tracking inside `GpuEncoder` (D6 (Synchronization)) |

**Where the CPU goes** (content-thread encode time per frame; split taken with one-off timers around the Metal calls, not kept; frame CPU is the executor's frame without commit or wait; ranges are two runs on a machine shared with other agents):

| | SWASH Dam Break 64³ | SWASH 128³ | WaterDamBreakMatter (bundled, 64³ default) |
|---|---|---|---|
| Frame CPU, mean | 14.1–15.5 ms | 16.5 ms | 1.3–1.8 ms |
| Compute dispatches | 1846 × 3.5–3.7 µs = 6.5–6.8 ms | 1846 × 3.95 µs = 7.3 ms | 211 × 3.4–4.0 µs = 0.7–0.85 ms |
| … debug group + signpost | 0.86–0.91 µs each | 0.97 µs | 0.8–0.9 µs |
| … `useResource` per binding | 0.63–0.66 µs each | 0.70 µs | 0.75–0.9 µs |
| MPSGraph FFTs | 280 × 22.5–25.5 µs = 6.3–7.1 ms | 280 × 27 µs = 7.6 ms | none |
| … of which the executable's own encode | 17.8–20.4 µs each | 21.6 µs | — |
| Buffer copies (one blit encoder each) | 210 × 3.0 µs = 0.6 ms | — | 4 |
| Executor and nodes outside the GPU API | about 0.7–1.5 ms | about 1.6 ms | about 0.5 ms |

**Shape of the work** (one frame's op trace):

- SWASH at 64: 8 outermost region visits per frame, 8 Krylov passes each in the harness scene (the pass count is a param). One pass encodes `2 dispatches · FFT · 3 · FFT · 6 · FFT · 3 · FFT · 12 · 3 copies`. The copies are the boundary's late capture (`krylov_basis` on origin/feat/fft-water). Regions hold 1664 of the 1846 dispatches. No indirect dispatch and no texture binding inside any region. Unbroken dispatch stretches inside regions: 320 per frame, 2 to 12 long, mean 5.2.
- WaterDamBreakMatter: one region visit per frame, 34 substeps of 5 dispatches, 182 dispatches with nothing between them: one stretch.
- The freeze compiler finds nothing to fuse in the SWASH water graph (`fuse_canonical_def_masked` returns None), so fewer dispatches through fusion is not on the table here.

## 2. Decisions

**D1 — The span is one outermost region visit.** Replay covers everything a depth-0 `run_substep_region` encodes between its boundary step and the release of its held resources, nested regions included. Each span is keyed by its boundary's `NodeInstanceId`. Top-level steps encode directly.
Rejected: the whole frame as one span, because one conditional top-level dispatch shifts every later position and throws away the tail, and the top level is where texture work lives (not recordable). Rejected: one span per iteration, because a recording that ends at every iteration seam can't merge the last stretch of one pass with the first of the next.

**D2 — Validate every visit; the recording is only a cache.** While a span is open, each recordable dispatch is compared with the recorded command at the current position: pipeline state, threadgroup grid, and per binding its Metal slot, kind, buffer identity and offset, or byte length. A match skips encoding. Uniform bytes and the sizes buffer are compared by content and patched in place when they differ. A mismatch, or running past the end, executes the validated stretch, cuts the recording at that position, and records from there. There is no invalidation API.
Rejected: skipping iteration-invariant steps in the executor (a variance analysis over `iteration_scalars`), because it needs a per-node replay-safety contract, re-derives what validation checks for free, can't cross frames without validating anyway, and about a fifth of each SWASH pass reads the pass scalars. Rejected: an invalidation hook callers must remember to fire, because a forgotten call becomes a wrong replay.

**D3 — Recordings live across frames in a small ring.** Each span cache holds up to `REPLAY_RING = 3` entries. An entry may be written (recorded or patched) only when the last command buffer that executed it has completed or errored. Selection: the most recently used entry if idle, else the next idle one (recorded lazily on first use), else the visit encodes directly and counts as `ring_busy`. Never wait.
Rejected: one entry and a CPU wait, because a live frame never waits on the GPU. Rejected: re-recording every frame, because recording costs about what direct encoding costs.

**D4 — What is recordable.** A compute dispatch whose bindings are all buffers or bytes, with a threadgroup grid (`DispatchGrid::Groups`), a pipeline created with ICB support, and not an isolated RT stage (the `node.render_scene RT` label check at the top of `dispatch_compute_grid`, `encoder.rs:512`). Everything else (texture or sampler bindings, indirect grids, `dispatch_compute_with_accel`, FFTs, blits, render passes, MPS) flushes the pending stretch and runs directly. ICB commands can't carry textures without argument buffers, can't dispatch from an indirect buffer, and can't hold MPSGraph work.

**D5 — Ordering is today's serial order.** Every recorded command gets `setBarrier`. Each execute is wrapped in `memoryBarrierWithScope(Buffers)` before and after. Anything that touches the compute encoder outside the replay path flushes first: `ensure_compute`, `end_current`, and `compute_memory_barrier_buffers` all flush the pending stretch. Any GPU output is then byte-identical to direct encoding wherever direct encoding is itself deterministic.

**D6 — The Metal store.**
- ICB chunks of 512 commands: `ConcurrentDispatch`, no inherited pipeline or buffers, 31 kernel buffer slots. Growth adds a chunk and never reallocates one. A stretch that crosses a chunk boundary is two executes. Cap: 64 chunks per entry; past it the span encodes directly.
- Bytes arenas are shared `GpuBuffer`s from the device's allocation path, so they retire through `retire.rs` if a cache is dropped mid-flight. Slots are 256-byte aligned: the constant-buffer offset rule on non-Apple GPUs, and cheap insurance on Apple ones.
- Each recorded command retains its pipeline state and buffers, so an address can't be reused while a recording names it.
- Each execute declares the stretch's buffers with one `useResources` call (read and write) from a reserved scratch list.
- Every buffers-only WGSL compute pipeline is created with `supportIndirectCommandBuffers = true` through the descriptor path, archive or not, and `GpuComputePipeline` records that (`supports_replay`). Metal refuses the flag for a function that binds a texture or sampler directly ("Compute function cannot be used with indirect command buffers"), so those pipelines are built without it. A function Metal refuses for any other reason is built without it too, with a warning naming it; replay is a speed-up, never a reason a pipeline fails.
Rejected: argument buffers for textures, because nothing recordable in the measured regions binds a texture.

**D7 — When replay is off.** Replay is off under encoder dispatch profiling, under GPU fault diagnostics (`gpu_fault::diagnostics_enabled`), under executor `dump_all`, when `MANIFOLD_ENCODE_REPLAY=0`, and on a backend whose store is not built (Vulkan today). Off means `begin_replay` hands back a span that encodes everything directly. Executor CPU step profiling keeps replay on, because it measures the real cost.

**D8 — Fault attribution per stretch.** Each execute is wrapped in one debug group and one signpost named after its first command's node label: `replay: node.x`. The string is made when that command is recorded, not per frame. A fault inside a replayed stretch names the stretch. Per-dispatch attribution comes back with `MANIFOLD_ENCODE_REPLAY=0` or diagnostics on.
Rejected: dropping debug labels everywhere to save their 0.9 µs, because fault attribution reads them.

**D9 — Buffer copies join recordings (P2).** Inside an open span, `copy_buffer_to_buffer` and `copy_buffer_range` whose offsets and size are multiples of 4 encode as a dispatch of a built-in `manifold-gpu` copy kernel (WGSL, one thread per word, through `create_compute_pipeline`), so they record like any other dispatch. Other copies, and all copies outside spans, stay blits. A word copy is exact, so output is unchanged.

**D10 — The Vulkan equivalent** (named, not built; the Vulkan backend has no command buffers yet). An entry is a secondary `VkCommandBuffer`, compute only, recorded outside any render pass with push descriptors (VULKAN_BACKEND_DESIGN.md D4 (Descriptors)) and the hazard barriers the encoder's tracker emits (D6 (Synchronization)) baked in. Bytes go to an entry-owned host-visible uniform buffer, the same way D5 (Bytes) maps them. The entry executes through `vkCmdExecuteCommands` in the frame's primary. Idle means the device timeline semaphore passed the submitting commit's value (D9 (GpuEvent)). The ring removes any need for `SIMULTANEOUS_USE`. `VK_EXT_device_generated_commands` is the GPU-driven alternative and is not needed. On Vulkan the FFT is compute (no MPSGraph), so FFTs record too.

**D11 — The FFT half is out of scope.** It is a blocking decision for P3 only (section 8 (Deferred)). P1 and P2 don't depend on it.

## 3. Design body

### 3.1 Public surface (`crates/manifold-gpu/src/replay.rs`, backend-neutral, re-exported at the crate root)

```rust
/// Recordings for one replay span, owned by the caller between spans.
/// `Default` allocates nothing; entries are created on first use.
#[derive(Default)]
pub struct GpuReplayCache { /* entries: Vec<ReplayEntry>, mru: usize, stats: GpuReplayStats */ }

impl GpuReplayCache {
    pub fn stats(&self) -> GpuReplayStats;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuReplayStats {
    /// Dispatches that matched a recording and ran from it.
    pub replayed: u64,
    /// Dispatches written into a recording (first use, a changed tail, running past the end).
    pub recorded: u64,
    /// Dispatches in a span that ran directly: not recordable, replay off, or ring busy.
    pub direct: u64,
    /// Execute calls: one per replayed stretch.
    pub executes: u64,
    /// Span visits that found no idle entry.
    pub ring_busy: u64,
    /// ICB chunks and bytes arenas created: must stay flat once the ring is warm.
    pub store_allocations: u64,
}

impl GpuEncoder {
    /// Open a span. Until `end_replay`, recordable compute dispatches
    /// validate against, or record into, one entry of `cache`.
    pub fn begin_replay(&mut self, device: &GpuDevice, cache: GpuReplayCache);
    /// Run the pending stretch and hand the cache back.
    pub fn end_replay(&mut self) -> GpuReplayCache;
}
```

The cache moves into the encoder for the span and back out, so `GpuEncoder` gets no lifetime and nothing is shared. `begin_replay` takes the device because it is the only place a store allocates: it grows the chosen entry to what its last visit wanted (chunks, and arenas from the device's retire-aware allocator). A visit that outgrows its entry encodes the rest directly and the next visit grows it. A span never nests: `begin_replay` inside an open span is a `debug_assert!` failure, and the executor opens spans at depth 0 only.

`GpuReplayCache` is backend-specific (`metal/replay.rs`, and a stats-only stub in `vulkan/replay.rs`); `GpuReplayStats`, the recorded command list, the comparison and the ring policy are the neutral part in `replay.rs`.

### 3.2 Internals (`replay.rs` neutral, `metal/replay.rs` backend store)

`replay.rs` owns the recorded command list and the comparison. Per entry: `commands: Vec<CommandRecord>` (pipeline identity, grid, a range into `bindings`, the bytes slots), `bindings: Vec<BindingRecord>` (slot, kind, identity, offset or length), and a CPU shadow of every bytes slot. `metal/replay.rs` defines `pub(crate) struct ReplayStore` (ICB chunks, bytes arenas, retained objects, the per-command signpost strings, the last executing command buffer, the `useResources` scratch). Its methods are record one command at an index, patch bytes at a slot, execute a range on a compute encoder, and report idle. `vulkan/replay.rs` has the same `ReplayStore` with `supported() -> false`. The recordable test, the comparison and the ring policy are written once, in `replay.rs`.

Hooks in `metal/encoder.rs`:
- `dispatch_compute_grid`: with a span open and the dispatch recordable, hand it to the span and return. Otherwise flush and encode as today.
- `ensure_compute`, `end_current`, `compute_memory_barrier_buffers`: flush first. The span's own execute uses a non-flushing twin of `ensure_compute`.
- `copy_buffer_to_buffer`, `copy_buffer_range`: in P2, a word-aligned copy inside a span becomes a copy-kernel dispatch.

### 3.3 Executor integration (`manifold-renderer`)

`Executor` gains `encode_replay: bool` (default true), `pub fn set_encode_replay(&mut self, on: bool)`, `replay_caches: AHashMap<NodeInstanceId, GpuReplayCache>`, and `pub fn replay_stats(&self) -> GpuReplayStats` (the sum over caches). In `run_substep_region` at depth 0, with a GPU encoder present, replay on and `dump_all` off:
1. After the boundary step, take the cache: `std::mem::take(self.replay_caches.entry(region.boundary).or_default())`.
2. Call `begin_replay`.
3. Run the iteration loop in a helper that returns its `StepFlow`, so every exit path, `Abort` included, reaches `end_replay`.
4. Put the cache back before the held resources are released.

The first visit inserts into the map; later frames never allocate. The offline host sync inside a region (`commit_wait_and_continue`) needs nothing: it goes through `end_current`, which flushes.

### 3.4 Consequences, stated honestly

- The FFT half stays. SWASH at 64 keeps 256 FFT breaks inside regions and 6.3–7.1 ms of MPSGraph encode, so each pass stays four or five stretches.
- Estimated SWASH 64 after P1, from the split in section 1:
  - The 1664 region dispatches (5.8–6.2 ms) become about 1664 validations at about 0.3 µs, plus 320 executes at about 3 µs: roughly 1.5 ms.
  - Frame CPU goes from 14–15.5 to about 10–11 ms.
  - P2 folds the 192 copies in and cuts a pass to four stretches: about 0.6 ms more.
  - 128³ lands in the same place (CPU encode doesn't scale with the grid): about 10–11 ms.
  - The bundled Dam Break goes from 1.3–1.8 to about 0.8 ms.
- Both unit costs are estimates. P1a measures them before anything else is built, and has a kill line.
- A replayed stretch loses per-dispatch signposts in GPU captures and fault reports (D8).
- A recording retains the buffers it names until that entry is re-recorded or dropped. After a resize, the old arrays live up to one more visit per entry.
- Every compute pipeline changes its creation flags, so the binary archive misses once. The first launch after landing recompiles pipelines at load.
- A replayed command costs GPU time. Measured in P1a on 64-group kernels: about 1.5 µs more per command than direct (26 dispatches: 0.142 ms direct, 0.182 ms replayed; 6: 0.036 against 0.052). Kernels that small are all overhead; on real regions P1b measured no GPU cost (below).

**Measured in P1b** (M4 Max shared with other agents' GPU suites; each figure is the mean of 12 to 48 warm frames, per run):

| | Frame CPU, replay off | Replay on | Replay on + FFT tensor data kept | GPU ms, off / on |
|---|---|---|---|---|
| SWASH Dam Break 64³ | 14.5–15.3 ms | 9.5–9.6 ms | 8.9–9.1 ms | 30.4–32.7 / 30.0–32.0 |
| SWASH 128³ | 25–27 ms (min 22) | 19–24 ms (min 11.5–12) | not run | 172 / 160–162 |
| WaterDamBreakMatter (bundled, 64³) | 1.5–2.0 ms | 1.25–1.36 ms | — | 50–54 / 47–52 |

- SWASH at 64 per frame: 1546 dispatches replayed, 14 direct, 319 executes; every visit after the first records nothing. Particles and every step's water and faces match replay off bit for bit, at 64 and at 128.
- The 128 runs swing by 5 ms frame to frame with the GPU at 160–172 ms a frame under contention; their minimums (22–23 ms off, 11.5–12 ms on) show a saving at least as large as at 64.
- The first visit records while it encodes. SWASH's first frame is 75–110 ms with replay on and 75–91 ms off, one-time setup either way, so recording is lost in that noise. The Dam Break's recording frame costs 2.0 ms against 1.5–1.9 ms direct.
- What is left at 64: 280 FFT encodes at 19 µs each, 5.3 ms, over half the frame; the 319 executes at about 3.4 µs; validation at about 0.3 µs a dispatch.

## 4. Invariants & enforcement

- **I1 — A replayed command is exactly the command direct encoding would have issued.** Enforcement: the comparison runs on every visit (D2). Tests: `replay_key_detects_every_field` (`manifold-gpu`, CPU-only: change each field of a dispatch in turn; each change must miss); `replay_matches_direct_bit_for_bit` (`manifold-gpu`, GPU); `encode_replay_parity` (renderer GPU proof; section 6 (Phasing), P1b).
- **I2 — An entry is written only while nothing pending executed it.** Enforcement: an entry is chosen only when idle (`pick_entry` over `ReplayStore::is_idle`, the status of the last command buffer that executed it), and a `debug_assert!` keeps every write in a span at positions that span has not executed yet. Test: `replay_never_writes_an_entry_in_flight` (every frame waits on an event the CPU holds back: three frames take the three entries, the fourth finds none idle and encodes directly; output matches direct).
- **I3 — Every buffer an executed stretch references is declared and alive.** Enforcement: record retains, execute declares (D6); arenas come from the retire-aware allocator and the command buffer retains an executed chunk. Tests: every P1a proof runs green under `MTL_DEBUG_LAYER=1 MTL_VALIDATION=1` with no validation errors; `replay_cache_dropped_in_flight_is_safe` drops a cache whose entry is still executing and still matches direct. Not run: the negative half (switch the declaration off and watch the layer object). The auto-mode classifier blocks switching `useResources` off, so that check is Peter's to run by hand or waive.
- **I4 — A warm ring allocates nothing.** Test: `replay_steady_state_records_nothing` (after the warm-up frames, `recorded` and `store_allocations` stay flat over 20 frames).
- **I5 — Replay is off wherever per-dispatch attribution is needed.** Test: `replay_off_under_profiling_and_dump` (encoder profiling on, or `dump_all` on: `replayed == 0`).
- **I6 — Replay knows nothing about any particular graph.** Negative gate, zero hits: `rg -i 'swash|krylov|fft_3d|matter' crates/manifold-gpu/src/replay.rs crates/manifold-gpu/src/metal/replay.rs crates/manifold-renderer/src/node_graph/execution/substep_region.rs`.
- **I7 — No invalidation API.** Negative gate, zero hits: `rg 'fn (invalidate|reset_recording|mark_dirty)' crates/manifold-gpu/src`.

## 5. Out of scope

FFT encode (section 8 (Deferred)), SWASH atoms (owned by the SWASH seat; any change goes through opus-swash-build first), fusion, and the Vulkan store.

## 6. Phasing

### P1a — Metal replay core in manifold-gpu

- **Entry state:** branch `feat/encode-replay`, based on feat/planner-reuse. Re-run the section 1 anchors for `encoder.rs`, `device.rs` and `types.rs`: `rg -n 'fn dispatch_compute_grid|fn ensure_compute|fn end_current|fn create_compute_pipeline_inner' crates/manifold-gpu/src/metal`.
- **Read-back:** this doc whole, `encoder.rs` from `raw_cmd_buf` through `compute_memory_barrier_buffers`, `device.rs` `create_compute_pipeline_inner`, `retire.rs` module doc. Restate D2 through D8 and the forbidden moves.
- **Deliverables:**
  - The Cargo features `MTLIndirectCommandBuffer` and `MTLIndirectCommandEncoder`.
  - `replay.rs`, `metal/replay.rs`, and the `vulkan/replay.rs` stub.
  - The encoder hooks from section 3.2 (not the copy kernel).
  - ICB support on every buffers-only pipeline from `create_compute_pipeline_inner`, with the flag on `GpuComputePipeline`.
  - The `MANIFOLD_ENCODE_REPLAY` switch.
  - The tests `replay_key_detects_every_field`, `replay_matches_direct_bit_for_bit` (a chain of 30 small kernels with bytes that change every frame, broken by a texture round trip, a blit and an indirect dispatch, over 10 frames; replay against direct, byte-equal readback), `replay_survives_structural_changes`, `replay_never_writes_an_entry_in_flight`, `replay_steady_state_records_nothing` and `replay_cache_dropped_in_flight_is_safe`.
  - `replay_cpu_cost_probe`, which reports direct against replayed CPU µs and GPU ms for stretches of 2, 6 and 26 dispatches.
- **Gate:**
  - Positive:
    - `cargo clippy -p manifold-gpu --all-targets --features gpu-proofs -- -D warnings` and `cargo clippy -p manifold-gpu --features vulkan -- -D warnings` are clean.
    - `cargo test -p manifold-gpu --features gpu-proofs --lib replay -- --test-threads=1` passes.
    - The same under `MTL_DEBUG_LAYER=1 MTL_VALIDATION=1` passes with no validation errors in the output.
    - The probe's numbers are reported.
  - **Kill line:** a replayed stretch of 6 dispatches must cost at most half the direct CPU. If it doesn't, stop and report the numbers; don't build P1b.
  - Negative: I6 and I7 return zero hits.
- **Demo:** none — L1. The artifact is the probe table.
- **Forbidden:**
  - A parallel dispatch path that skips the comparison.
  - Waiting on a busy entry.
  - An invalidation API.
  - `Arc<Mutex>` or any shared state.
  - Recording texture dispatches through argument buffers.
  - Touching `fft.rs`.
- **Test scope:** manifold-gpu only, `cargo test` (the device lock), not nextest.

### P1b — Executor spans, proofs, measurements

- **Entry state:** P1a committed and its gate green. Re-run the `substep_region.rs:36` and `execution.rs:150,684,726` anchors.
- **Read-back:** this doc sections 2–4, `substep_region.rs` whole, `execution.rs` `run_step` and `capture_step`. Restate D1, D7 and the forbidden moves.
- **Deliverables:**
  - The executor integration from section 3.3.
  - The renderer GPU proofs, all in `tests/gpu_proofs/encode_replay.rs`:
    - `encode_replay_parity`: replay on against off, byte-equal readback of every declared output and dump array over 30 frames, for the nested-region and copy-chain graphs `tests/gpu_proofs/substeps.rs` already builds, and for the bundled WaterDamBreakMatter at its default 64.
    - `encode_replay_survives_changes`: change a param that changes a grid, an iteration count and an array capacity mid-run; output still matches replay off, and `recorded` shows the re-record.
    - `replay_off_under_profiling_and_dump`.
  - `encode_replay_probe`: frame CPU with replay on and off, and GPU ms, for the bundled Dam Break.
  - Path (a) of the FFT decision (section 8 (Deferred)), moved here by the lead: `GpuFft::encode` stops making its tensor data, arrays and execution descriptor on every call. The FFT's CPU µs per call is reported before and after on SWASH 64, and the `manifold-gpu` fft tests stay green. As built, on feat/fft-encode-cache off feat/fft-water, where the 3D plans are: the descriptor is built once and each plan keeps tensor data for its last four buffer pairs. The command-buffer wrapper stays per call, because a kept one outlives its command buffer and MPS then encodes into the committed one (a Metal assertion, seen). SWASH 64: 23.3–24.7 µs a call before, 19.5–19.8 after; `one_plan_encodes_many_buffer_pairs` proves eight pairs through one plan match a fresh plan bit for bit.
- **Gate:**
  - Positive:
    - `cargo clippy -p manifold-gpu -p manifold-renderer -- -D warnings` is clean.
    - `cargo nextest run -p manifold-renderer` passes.
    - `cargo test -p manifold-renderer --features gpu-proofs --no-fail-fast` passes, with the freeze proofs and `substeps` nested-region proofs green.
    - Measured and reported, replay off against on: frame CPU and GPU ms for SWASH 64 and 128 (on a local merge with origin/feat/fft-water, never pushed) and for the bundled Dam Break. GPU ms is no worse than 3% beyond run-to-run spread.
    - The first-visit CPU cost (recording) is reported.
  - **Defaulted:** the Dam Break is assumed deterministic run to run. If replay off against replay off already differs (float atomics in MPM transfer), its parity gate becomes "within the off-against-off spread", and the report says so. The SWASH water scenes have no atomics (FFT_WATER_SOLVER_DESIGN.md D7 (gather-form transfers)) and must match bit for bit.
  - **As measured:** two direct runs of the Dam Break in one process sometimes differ, from frame 2 on, in the Matter frame and everything downstream, by the same few discrete amounts, so the default fired (logged as BUG-4n2g (Dam Break differs between direct runs)). A byte spread can't separate replay from that, so the Dam Break gate is what the solver conserves: the same dumped arrays at the same sizes, no non-finite point, the exact live count, and mass within 1e-4; the byte spreads are printed. The nest and copy-chain graphs and SWASH match bit for bit.
  - Negative: I6 and I7 return zero hits.
- **Demo:** none — L1 for agents. For Peter, the performer gesture: play the Dam Break at 64 live and drag Speed. The content-thread trace (`MANIFOLD_RENDER_TRACE=1` on the worktree app binary, exact command in the report) shows no frame over 20 ms.
- **Forbidden:**
  - Opening spans below depth 0.
  - Any SWASH-specific branch.
  - Editing SWASH atoms or `krylov_basis`.
  - Running matter above 64.
  - Replay under `dump_all`.
- **Test scope:** manifold-gpu and manifold-renderer, with the full GPU proof suite, because the executor path is touched.

### P2 — Word copies join recordings

- **Entry state:** P1b landed or on the branch, with its gate green.
- **Read-back:** D9, and every late capture that copies inside a region (`rg -n 'late_capture' crates/manifold-renderer/src/node_graph`).
- **Deliverables:**
  - The built-in copy kernel (fusable is not required: it is a `manifold-gpu` internal, not a graph atom).
  - The copy hooks from section 3.2.
  - `replay_copy_matches_blit` (`manifold-gpu`: random offsets and sizes that are multiples of 4; byte-equal to a blit).
- **Gate:**
  - Positive:
    - The P1b gates pass again.
    - On the SWASH 64 merge, stretches inside regions per frame drop from 320 toward 256 (read from `executes`), and frame CPU is reported.
  - Negative: no copy-kernel use outside an open span: `rg -n 'copy_kernel' crates/manifold-gpu/src/metal/encoder.rs` hits only inside the span branch.
- **Demo:** none — L1.
- **Forbidden:** converting blits outside spans, and non-word copies.
- **Test scope:** as P1b.

### P3 — FFT inside recordings

Blocked on the FFT decision in section 8 (Deferred), decider Peter via the lead. Not briefed until it is answered.

## 7. Decided — do not reopen

1. One span per outermost region visit, keyed by boundary.
2. Validation on every visit is the only correctness mechanism; no invalidation API.
3. A ring of 3 entries per span; write only idle entries; never wait.
4. Recordable means buffers and bytes, a threadgroup grid, an ICB-capable pipeline, not an RT stage.
5. Serial order: a barrier on every command, memory barriers around every execute, and every other compute access flushes.
6. ICB chunks of 512 commands, bytes arenas from the device allocator, and resources retained per command.
7. Off under dispatch profiling, fault diagnostics, `dump_all` and `MANIFOLD_ENCODE_REPLAY=0`.
8. One signpost per executed stretch.
9. Word-aligned copies inside spans become a compute copy (P2).
10. Vulkan twin: secondary command buffers, not device-generated commands.

## 8. Deferred

- **FFT encode, blocking for P3; decider Peter via the lead; logged as decision .7 under BUG-l2h3 (SWASH live-instrument epic).** MPSGraph FFT encode is about 46% of SWASH's CPU, and an ICB can't hold it. Two paths:
  - (a) Keep MPSGraph and cache the per-call wrappers in `GpuFft::encode` (`metal/fft.rs:129` makes two tensor data objects, two arrays, a command-buffer wrapper and an execution descriptor per call). That saves about 5 of the 22–27 µs per call: about 1.4 ms at 64. FFTs keep breaking stretches.
  - (b) Replace MPSGraph with `manifold-gpu` compute FFT kernels, mixed radix like MPSGraph so every even side still runs. The FFTs then record with everything else: one stretch per region visit, and SWASH's encode drops to roughly the executor's own cost. It is also what the Vulkan backend needs anyway, since MPSGraph is Metal-only. It reverses FFT_WATER_SOLVER_DESIGN.md D9 (exemption classes), which names each FFT as one MPSGraph call.
  - (a) is built in P1b (the lead's call); (b) is decided on the P1b numbers.
  - The P1b numbers (section 3.4 (Consequences)): with replay and (a), SWASH 64 is 8.9–9.1 ms of frame CPU. The FFTs are 5.3 ms of it (280 × 19 µs, all inside MPSGraph's own encode, which (a) can't touch), and each FFT also ends a stretch: 319 executes at about 3.4 µs, 1.1 ms. Path (b) removes both, leaving roughly 2.5 ms.
- **Whole-frame spans.** Revive when a graph without regions shows more than 1 ms of recordable top-level dispatch per frame.
- **Texture dispatches through argument buffers.** Revive when a region that matters spends more than 1 ms per frame on texture-bound compute.
- **Vulkan store (D10).** Built with the Vulkan backend's command-buffer phase.
