# Water sim queue — the next tick overlaps this frame

<!-- index: Design for SB of the water field wave: GPU FLIP's tick region and its publication run on the device's second (async) queue through an async lane on the frame encoder, so the GPU computes the tick while this frame draws. The manifold-gpu lane API and its Vulkan mapping, what moves and what stays, every shared resource and its ordering point, why every stamp keeps the frame clock, chunking/replay/profiling/export/host sync, storage planning, the A/B measurement and drop rule, and the jobs by file. -->

**Status:** DESIGN (SB of `WATER_FIELD_WAVE_PLAN.md` section 4). Anchors checked at 9cc2a225c with the S3 working tree. SB ships only if the measured live tick interval improves (section 7). Companions: `VULKAN_BACKEND_DESIGN.md` D5, D7, D8 and section 5; `LIVE_SIM_CLOCK_DESIGN.md`.

## 1. What changes

Today a water layer's frame is one command stream on one queue. The domain nodes run, then the tick (the substep region), then the publication into a history slot, then the render. The render reads only retired history slots, never the live state, so it does not need this frame's tick. It waits for it anyway, because one queue runs in order.

SB gives the frame encoder a second lane that records onto a second queue of the same device. The tick and its publication go there; the render stays on the main lane. The GPU runs both at once.

There are two ordering points, both on one event the executor owns:
1. **Inputs (handoff).** Before the first tick command, the main lane signals the event and commits. The async lane waits for that value. Everything written on the main lane before it (domain nodes, the boundary's once-per-frame run) is then visible to the tick.
2. **Pass end.** When the graph pass ends, the main lane waits for the async lane's last value. Work after the water's graph pass (later layers, the compositor, the frame-completion signal) runs after the tick, and so does next frame's domain work.

Both waits are on the GPU. The live app never waits on the CPU per frame, as today.

On stage: the water's next step and the picture of the last step are computed side by side. If the GPU is the limit, the Dam Break frame gets shorter, by at most the shorter of the two. What the water does is unchanged; export and every frame-exact result stay bit for bit the same.

## 2. The manifold-gpu API (decision 1)

**Decision.** The second queue lives on `GpuDevice`, made on first use. The frame encoder gets a second lane onto it. No new public type: signal and wait reuse `GpuEvent` and the encoder's `signal_event`/`wait_event`. Reason: one encoder keeps one runaway span watch, one GPU-time tap and per-buffer fault logs over both queues; a separate encoder would need each of those plumbed through node-engine and the compositor.

Surface (four calls, two proof counters):
- `GpuDevice` private `async_queue: OnceLock<Retained<ProtocolObject<dyn MTLCommandQueue>>>`, from `newCommandQueue` on first use. No `Arc<Mutex>`.
- `GpuEncoder::use_async_lane(&mut self, device: &GpuDevice, handoff: &GpuEvent) -> bool`. Encode what follows on the async queue. Switching from the main lane always hands off: the main lane signals `handoff`, commits (never blocking) and continues; the async lane waits for that value before anything after it. Already on the async lane: no-op, `true`. A profiled encoder refuses (`false`) and stays on the main lane, so a profiled frame keeps one timeline, as replay already does.
- `GpuEncoder::use_main_lane(&mut self)`. Encode on the main queue again; the async lane stays open.
- `GpuEncoder::finish_async_lane(&mut self, handoff: &GpuEvent)`. Signal `handoff` on the async lane and commit it, never blocking. Remember the value. The main lane is current afterwards and does not wait yet.
- `GpuEncoder::wait_async_lane(&mut self, handoff: &GpuEvent)`. The main lane waits for the remembered value. No-op if nothing was finished.
- `#[cfg(feature = "gpu-proofs")]` thread-local counters `GpuEncoder::async_lanes_on_this_thread()` (lanes finished) and `handoffs_on_this_thread()`, the pattern of `FrameClock::waits_on_this_thread`.

Inside the encoder:
- **A lane owns** its `cmd_buf`, its queue, `state`, `compute_cache`, `render_cache`, `scopes` and `pass_samples`. A switch swaps these seven fields with the parked lane. No encode call changes.
- **The encoder shares** `profile`, `span_buffers`, `gpu_time_tap`, `template_token` and `clear_pipelines`. So an async buffer joins the open runaway span and the GPU-time tap like any chunk, and its fault log carries its own scopes.
- `commit_and_continue` takes the next buffer from the current lane's queue, so host syncs and offline publications on the async lane stay on it.
- A replay span never crosses a lane switch (`debug_assert`). Every final commit (`commit`, `commit_and_wait_*`, `try_commit_and_wait_completed`, `commit_and_wait_profiled`) asserts no open lane and no unwaited value.
- **Deadlock rule:** every wait is encoded after the buffer carrying its signal is committed. The handoff commits the main buffer before the async buffer that waits on it exists; `finish_async_lane` commits before `wait_async_lane` encodes. Two executors (two water layers, or test threads on one device) share the async queue safely, because no async buffer is ever queued ahead of the signal it waits for.
- The residency set attaches to both queues (`residency.rs`), so the tick's allocations are resident the way the main queue's are. Without it the A/B would measure a residency cost, not the overlap.

**The event belongs to the executor, not the device.** Every signal and wait on it happens on one thread in encode order, so its value only rises on the GPU. A device-wide event shared by concurrent test threads could be signalled out of order.

**Vulkan.** No Vulkan code in SB; `vulkan/` has no queue or event API yet. P1 builds the lane on the same shape:
- `async_queue` is D5's second `VkQueue` of the one family. If the family has one queue, D5 shares it behind its mutex. The submit order (main handoff, async, main) never waits on a later submit, so a shared queue runs it in order without deadlock.
- The handoff is a timeline signal at the end of the main submit (D7); the async submit waits on it. `finish_async_lane` is the async submit with its signal; `wait_async_lane` is D7's mid-encoder wait, which splits the main submit.
- One family, so no queue-family ownership transfers. The hazard tracker's first-touch barriers stay per submit; the timeline waits order across queues, as section 5 already plans for water compute to render.
- D8's waiter thread must follow both queues' submits for completion callbacks.

## 3. What moves and what stays (decision 2)

**Moves to the async lane:** the region from its first iteration on: every body step (`gpu_flip_step`, `liquid_stats`, `whitewater_step`), the boundary's per-iteration captures (`liquid_state::late_capture`: the capture copies, the readback slot copies, the host-sync sample), the clock owner's host step in coupled frames, and the publisher, `node.liquid_frame` (the 8-pass sort and gather, the solid and whitewater field copies, the offline wait).

**Stays on the main lane:** the boundary's once-per-frame `run` (seed copy on a new epoch, identity seed, whitewater clears, recovery reseed), every node before the region (domain, fill, `mesh_solid`, `whitewater_obstacle_source`), every node after it except the publisher (surface chain, `render_scene`), the late-capture pass, later layers and the compositor.

Why the publication moves: it reads the live state the tick just wrote. On the async lane it follows the tick in queue order and needs no ordering point. On the main lane it would need a wait for the tick, which stalls the render behind it, and a second wait so the next tick cannot overwrite the state it is still copying.

Why the boundary's `run` stays: it runs before the iteration count is known. On the main lane, a frame with no tick (paused, held, between Sim Rate ticks) never opens the lane and costs nothing extra. Its writes cross at the handoff like any domain output.

Every resource both sides touch:

| Resource | Written | Read | Ordering point |
|---|---|---|---|
| Domain outputs (`bodies`, `shapes`, `atlas`, `body_rows`, `regions`, `reaction`, `forces`, `impulses`, `force_lattices`), the fill's seed, `mesh_solid.solid`, the obstacle source | main, before the region | tick, publisher | handoff |
| The boundary run's writes (`out`, `identity`, `stats`, whitewater pool, state, counts and classes) | main, boundary `run` | tick, publisher | handoff |
| Live state, step and whitewater internal state, replay entries, pressure templates | tick | tick, publisher | async queue order |
| Readback slots, the host-sync sample | tick | CPU | frame clock stamp (section 4); host sync waits on the async buffer |
| History slots (particles, metadata, solid and whitewater fields) | publisher (async) | render (main) | CPU: a slot is shown only once retired on the frame clock |
| The publisher's sort scratch | publisher (async when the region ticked, else main) | same | handoff and pass-end wait |
| Tick inputs, rewritten by next frame's main lane | main, next frame | tick, this frame | pass-end wait |
| Planned arrays the tick touches vs later main-lane storage | — | — | dedicated storage (section 6) |
| Buffers dropped while the tick may use them (resize) | — | tick | `RetireMark` frame stamp |

Scalars, transforms and lattice values are CPU values encoded as inline bytes; they need no ordering. No region or publisher node commits a side encoder on the main queue (checked by `rg`: only test helpers do).

**Which regions overlap.** The plan marks a region `overlaps` only when:
- (a) every step outside the region that reads one of the boundary's array outputs is a tick publisher (`tick_publisher()`, section 8 J2);
- (b) no region step and no publisher input is a texture wire (`Texture2D`, `Texture2DTyped`, `Texture3D`): textures recycle through runtime free lists, which order only within one queue;
- (c) no later step writes in place (`aliased_array_io`) an array the region or a publisher reads.

Otherwise the region runs in order on the main lane, as today. GPU FLIP's preset meets all three: `liquid_state`'s array outputs go only to `liquid_frame` and the body. Matter's region does not yet, because `node.matter_frame` is not a publisher until SBm. Reason: the planner can check these rules, so a graph that reads live state elsewhere stays correct, only without the overlap.

## 4. Stamps (decision 3)

**Decision.** Every stamp keeps the frame clock (`FrameClock`). Reason: the pass-end wait puts the tick ahead of the frame-completion signal (compositor, `content_pipeline.rs:3455`), so "frame N retired" still means "frame N's tick retired". One clock, and no change in the files S3.4 is about to edit.

That covers every user:
- History: writer and reader stamps, `retire`, `reclaim` (`liquid_frame.rs:197-203`, `:289-302`, `frame_history.rs:299-333`, `:412`).
- Readbacks: `late_capture` stamps (`liquid_state.rs:901`), `poll_readbacks` (`:319-379`). `retired_max_speed` and `one_step` read the same retired samples.
- The coupled reaction (`gpu_flip_domain.rs:1088-1105`, `:1233`).
- Dropped buffers and textures (`RetireMark`), the texture pool, surface readiness, CPU-written rings.

Timing: the frame signal now lands at domain + max(tick, render) + compositor instead of domain + tick + render + compositor. Retirements arrive no later than today, so the CFL sample is fresh at least as often.

`content_pipeline.rs` needs no change: the Generators encoder is already the frame encoder the executor encodes into.

## 5. Chunking, replay, profiling, export, host sync (decision 4)

- **Chunking.** The per-layer `checkpoint` (`generator_renderer.rs:1125-1127`) is unchanged and only ever sees the main lane: the lane is finished and waited before the pass returns. Each handoff adds one main split. The async lane is one buffer per region visit, plus host-sync and offline splits. Its buffers join the layer's runaway span, so a runaway tick still stops the layer. The span's last buffer is the main one, which completes after the tick.
- **Encode replay and templates.** The region's replay span opens after the lane switch and closes before the region returns, so it records and replays on the async lane. Replay entries and template slots track the command buffers that ran them by status (`replay.rs:142`, `template.rs:327`), which does not depend on the queue.
- **Profiling.** A profiled encoder refuses the lane, so `commit_and_wait_profiled` frames run in order with every dispatch on one timeline, the same rule replay follows (`replay.rs:579`). This covers the app, `freeze_profile`, `perf_soak_import` and every profiled proof with no per-site flag. A profiled frame ranks passes; it never measured the live budget anyway.
- **Offline export.** Export uses the same async lane. It stays frame-exact because every tick input crosses at the handoff and the publication follows the tick on its queue. `liquid_frame`'s offline `commit_wait_and_continue` (`:389`) commits and waits the async buffer; the render then reads a slot whose completion the CPU saw. Each queue runs the same kernels in the same order on the same data, so the words match. The old in-order path is not needed for exactness; it remains only as the profiled frame.
- **Coupled host sync.** `commit_wait_and_continue` in the region (`substep_region.rs:153`) commits and waits the async lane, which is current. The main buffer with the handoff signal was committed at the handoff, so the wait never blocks on uncommitted work. The async buffer waited for that signal, so all main work before the region is finished when the host step writes shared storage. The coupled domain's `settle_ready` wait on the previous frame (`gpu_flip_domain.rs:1088-1105`) is unchanged.

## 6. Storage planning

Planned arrays share physical storage by step order (`plan_array_allocations`). On one queue, a later step reusing an array a region step last touched is ordered by the queue. With the tick on the async lane, a later main-lane step could overwrite storage the tick still reads. So for an overlapping region, every Array resource in its steps' inputs and outputs and in its publishers' inputs joins `excluded_resources` and keeps dedicated storage. This is the same rule the planner already applies to CPU steps (`resource_allocation.rs:299-313`). Publisher outputs are excluded from the rule: they show retired slots only. Cost: the reuse those arrays had. GPU FLIP's big region outputs are already node-owned (`gpu_flip_step` and `whitewater_step` provided outputs), so the extra memory is a few small wires. Textures never reach this case (rule (b)).

## 7. The measurement (decision 5)

SB is judged only by the live tick interval of the uncoupled Dam Break at 64, in the headless `manifold frame-time` paced mode.

**Fixture.** J4 adds `frame-time --preset WaterDamBreakGpuFlip`, which builds the export demo's one-layer project (1920×1080, 120 BPM, a 128-beat clip), saves it to the temp dir and loads it through the production loader. No Dam Break project is checked in, and the old oracle (`/tmp/.../gpu_flip_dam_break.manifold`, BUG-bfal6) is gone.

**Binaries.** A: the commit with J4 and without SB. B: the SB tip. Both `cargo build --release -p manifold-app --features perf-soak`, each `target/release/manifold` copied to the scratchpad as `manifold-A` and `manifold-B`.

**Run.** Interleaved, three pairs, under one hold of the GPU lock (the app closed; `gpu_queue.py` waits for it):

```sh
S=<scratchpad>
scripts/gpu_queue.py --label sb-ab -- sh -c '
for i in 1 2 3; do
  for side in A B; do
    "'"$S"'/manifold-$side" frame-time --preset WaterDamBreakGpuFlip --frames 600 \
      --splash-frames 200 --resolution 64 --stamp-every 100000 > "'"$S"'/sb-$side$i.txt"
  done
done'
```

`--stamp-every 100000` stamps no frame: a stamped frame waits for the GPU and runs in order.

**Numbers per run**, from the calm and splash tables (`frames (N): tick interval p50 / p95 | GPU surface wait p50 / p95`): the tick interval p50 and p95, and the GPU surface wait p50. Take the median of the three runs per side. Both sides must have run the same simulation work: compare the summary's accepted ticks and its grid-frames-per-tick histogram. A shorter interval bought with fewer ticks is not a win.

**Read A first.** A calm GPU surface wait near 0 means the frame is CPU-bound and SB cannot move the interval. Run B anyway to record it.

**Rule.** SB ships if B's median calm tick-interval p50 is below A's by more than A's own spread (max minus min of A's three calm p50s), and neither B's calm nor its splash p95 median is above A's by more than the matching spread. Otherwise SB is dropped: the J1 to J3 edits are reverted (never committed), J4 stays as a tool, the six calm p50s go in one paragraph in `docs/IDEAS.md`, and this doc is deleted.

The plain GPU span column is context only. Under B the async buffers report through the Generators tap, so that span is the union of both lanes.

**What it can gain.** The last per-node measure at 64 (`GPU_FLIP_SPARSE_BLOCKS_DESIGN.md` section 7, after 2c) has a 22.1 ms whole buffer. The tick side (`gpu_flip_step` 15.6, `whitewater_step` 1.05, the rest under 1) is about 17 ms. The render side (`render_scene` 8.7, `particle_volume` 1.0, the surface chain 1.6, compositor 0.24) is about 11.5 ms. The node sums already exceed the buffer by 6.6 ms, because Metal overlaps render passes with compute on one queue. So the most SB can remove is about 5 ms of that 22 ms, down to the tick side. The live Dam Break last ran at a 30.5 ms tick interval (BUG-bfal6), above the 16.7 ms budget, so the frame was GPU-bound then. The real gain is what the A/B shows, and it may be zero if the solver already fills the GPU.

## 8. Jobs

Order: J4 first (built and committed alone, so binary A exists). Then J1, J2 and J3 are written at once (separate files) and built together; J1 and J2 make this an A job, built when every other worker has reported. Then binary B and the A/B. No worker runs cargo.

### J1 manifold-gpu: the async lane (sonnet)

Files: `crates/manifold-gpu/src/metal/{device.rs, encoder.rs, residency.rs, mod.rs}`, new `async_lane_tests.rs`.
1. `device.rs:148-197`: add `async_queue` (`OnceLock`), initialise in `new()` (`:242-259`), and add `pub(crate) fn async_queue(&self)` (get or `newCommandQueue`). `new_command_buffer` (`:1401`) takes the queue as a parameter. `create_encoder` (`:1433`) passes `&self.queue` and sets the encoder's new fields. Docs: `clone_queue` (`:411-416`) is the main queue; `retire_after` (`:418-430`) covers main-queue work, which includes async-lane work once the main lane has waited for it.
2. `encoder.rs:117-147`: add `queue` (the current lane's), `parked: Option<Lane>`, `on_async: bool`, `async_wait: Option<u64>`. Private `struct Lane` with the seven lane fields of section 2 and a `swap_lane` that swaps them.
3. `commit_and_continue` (`:3186-3207`): take the next buffer from `self.queue`. Factor the submit half (end, fault handler, GPU-time tap, commit, push into `profile`/`span_buffers`) into one private fn that `finish_async_lane` reuses.
4. The four lane calls beside `signal_event`/`wait_event` (`:2977-3007`), as section 2 specifies. The async buffer's label is `"Async lane"` (static, no per-frame string). The counters follow `retire.rs:217-219`.
5. The `debug_assert` in every final commit (`:547`, `:3175`, `:3222`, `:3238`, `:3248`, `:3307`).
6. `residency.rs:75`, `:112`: hold both queues (`device.clone_queue()` and the async queue) and add or remove the set on each at `:169`, `:178`, `:185`, `:203`.

Tests (`async_lane_tests.rs`, `mod` line in `metal/mod.rs` beside `:42`; each under 1 s; a `GpuEvent` gate set from the CPU, as `replay_tests.rs:392-412` does, makes order observable):
- `async_lane_waits_for_the_main_work_before_it`. The main lane first waits on a gate, then copies a CPU-filled source into A; `use_async_lane`; the async lane copies A into B; finish. While the gate holds, the handoff's final value does not arrive within 50 ms. Release the gate, then it does. Then `wait_async_lane`, the main lane copies B into C, and `commit_and_wait_completed`: C equals the source.
- `main_lane_waits_for_a_finished_async_lane`. After the handoff the async lane waits on a gate, then writes B; finish; `wait_async_lane`; the main lane signals a probe event and commits. The probe does not arrive within 50 ms while the gate holds. Release the gate: the probe arrives, and B is right.
- `returning_to_the_async_lane_hands_off_again`. Async work, then `use_main_lane`, main writes A2 behind a gate, then `use_async_lane` again and the async lane copies A2. The copy waits for the gate, and `handoffs_on_this_thread` rose by 2.
- `async_lane_continues_on_its_own_queue`. On the async lane, `commit_wait_and_continue`; the next buffer's `commandQueue()` is the async queue.
- `profiled_encoder_refuses_the_async_lane`. After `enable_dispatch_profiling` (`device.create_timestamp_sampler`, `device.rs:289`), `use_async_lane` returns false and neither counter moves.
- `async_lane_buffers_join_the_open_span`. With a `GpuSpanTimer` span open, a handoff and a finish leave two buffers (the main split and the async one) in `span_buffers` before `end`.

### J2 node-engine: plan and executor (sonnet)

Files: `crates/manifold-node-engine/src/{primitive.rs, exec/effect_node.rs, exec/substeps.rs, exec/execution_plan.rs, exec/resource_allocation.rs, exec/execution.rs, exec/execution/substep_region.rs, testkit/substep_nodes.rs}`.
1. `EffectNode::tick_publisher(&self) -> bool { false }` beside `substep_boundary` (`effect_node.rs:1231`). Doc: true for a node that reads a substep region's final state and copies it only into storage its outputs show once the frame clock retires it, so the executor may encode it on the region's async lane. Mirror it on `Primitive` (`primitive.rs:554`) and forward it in the blanket impl (`:982`).
2. `SubstepRegion` (`substeps.rs:119-131`): add `publishers: Vec<usize>` (steps outside the region that read the boundary's array outputs, ascending) and `overlaps: bool` (rules (a) to (c) of section 3).
3. `execution_plan.rs:1136-1158`: compute both where the region is built.
4. `resource_allocation.rs`: after the per-step exclusion loop (it ends at `:345`), each overlapping region's Array resources (section 6) join `excluded_resources`, with a comment saying why.
5. `Executor` (`execution.rs:162`): a lazily made `tick_event: Option<GpuEvent>` and per-pass lane state (open, the step after which it finishes, which regions ticked: a reused `Vec<bool>`, no per-frame allocation).
6. `execute_frame_inner` (`:1923`), step loop (`:2098-2134`):
   - Reset the lane state at pass start.
   - Before a non-region step, call `use_async_lane` if the lane is open and the step is a publisher of a region that ticked this pass; otherwise call `use_main_lane` if the lane is on async.
   - After a region that opened the lane, it finishes after its last publisher (the region's last step if it has none).
   - After the late-capture pass (`:2198-2207`) and before the abort `return` (`:2131-2133`): finish if open, `use_main_lane`, `wait_async_lane`.
7. `substep_region.rs`:
   - Move `begin_replay` (`:106-111`) into the loop. On iteration 0, after `more` (`:133-135`), an overlapping region with a GPU encoder calls `use_async_lane` (with `tick_event`, made on first use) and marks the region ticked. Then the replay span begins.
   - `end_replay` (`:203-209`) runs only if the span began.
   - The host-sync commit (`:153`) is unchanged: it commits the current lane.
   - Update the module doc (`:1-9`) and `EffectNode::substep_host_sync`'s doc (`effect_node.rs:1265-1271`, "commit the frame's encoder" becomes "commit the region's lane").
8. `testkit/substep_nodes.rs`: a `test.tick_publisher` node with `tick_publisher() = true`. Each frame it copies its `in` array into one of two node-owned buffers and provides the other one, last frame's copy, as `out`. That keeps the contract: its output is storage that already finished.

Tests:
- `substeps.rs` (CPU, compile only):
  - `substeps_publisher_lets_the_region_overlap`: state read only by `test.tick_publisher`, so `overlaps` holds and `publishers` is its step.
  - `substeps_other_live_state_reader_keeps_the_region_in_order`: a plain copy also reads the state, so the region does not overlap.
  - `substeps_texture_wire_keeps_the_region_in_order`: a body node with a texture output, so the region does not overlap.
- `resource_allocation.rs` (CPU): `overlapping_region_arrays_keep_dedicated_storage`. The overlapping graph's region and publisher-input arrays are all `Allocate`, never `Reuse`. The same graph with a plain reader still reuses one (the premise).
- `crates/manifold-nodes/tests/gpu_proofs/substeps.rs`: `region_on_the_async_lane_matches_in_order`. Seed, region, `test.tick_publisher`, sink, for 8 frames, plain against profiled (in order). The state and sink bytes are equal. Plain frames finish one lane each; profiled frames finish none.

### J3 water nodes (haiku)

File: `crates/manifold-water-surface/src/primitives/liquid_frame.rs`, `impl Primitive for LiquidFrame` (`:168`). Add `fn tick_publisher(&self) -> bool { true }`, with one line of comment: the history shows a slot only once the frame clock retires it. This is the only SB edit in the files S3.4 will touch. `liquid_state.rs`, `frame_history.rs` and `gpu_flip_domain.rs` are unchanged.

Tests:
- `crates/manifold-nodes/tests/contracts/node_graph/catalog_tests/whitewater_scene.rs`: `liquid_tick_lane_matches_in_order`. Two `Show`s of `authored_cursor_history_def(0.0)` at (96, 54), 12 frames each, one with `frame(false)` (async lane) and one with `frame(true)` (profiled, in order). Per frame, `particles_a`, `particles_b` and the readback pixels are equal. `async_lanes_on_this_thread` rises in the first `Show` and not in the second.
- `crates/manifold-app/tests/renderer_contracts/gpu_proofs/liquid_conformance.rs:2580` (`liquid_live_frames_never_wait`, I13): also count finished lanes. The GPU FLIP row finishes at least one and still waits 0 times; Matter rows finish none until SBm.

### J4 app: the measurement fixture (haiku)

Files: `crates/manifold-app/src/{frame_time.rs, headless_harness.rs, gpu_flip_export_demo.rs}`.
1. Move `project_of` (`gpu_flip_export_demo.rs:45-58`) to `headless_harness.rs` as `pub(crate) fn preset_project(preset: &str) -> Project`, under `cfg(any(feature = "perf-soak", feature = "journey-proofs"))` so no build compiles it without a caller. The demo calls it.
2. `frame_time.rs`: `--preset <id>` in place of `<project.manifold>` (`parse`, `:105-110`; usage `:86-95`; module doc `:1`). With `--preset`, `run` (`:1062-1089`) saves `preset_project(id)` with `manifold_io::saver::save_project` to `std::env::temp_dir().join("frame-time-<id>.manifold")` and passes that path to `prepare_project_edited`.

Test: `frame_time.rs` tests (`:204`): `parse_reads_a_preset_in_place_of_a_project`.

### The lead's checks

- Clippy: `cargo clippy -p manifold-gpu -p manifold-node-engine -p manifold-water-surface -p manifold-app --tests -- -D warnings`, plus `manifold-app` with `--features perf-soak`.
- New tests: the J1 to J4 tests above.
- Existing tests that must still pass, all GPU ones through `scripts/gpu_queue.py`:
  - `substeps.rs` (`:1705-1979`, the host-sync tests at `:1856`, `:1902`, `:1930`, `:1940` included).
  - `liquid_state.rs` tests from `:915`; `frame_history.rs` `:841`, `:903`.
  - `liquid_conformance.rs`: `:911`, `:1121`, `:1928` (I6), `:2254` (I7, word for word), `:2356`, `:2451`, `:2527`, `:2580` (I13), `:2614`, `:2772`, `:2805`.
  - `whitewater_scene.rs`: `:963`, `:1029`, `:1081`, `:1130`.
  - `gpu_proofs/substeps.rs`: `:206`, `:240`, `:406`, `:469`; `gpu_proofs/encode_replay.rs`: `:193`, `:333`, `:411`.
  - `gpu_flip_tile_tests.rs:659`; `gpu_flip_export_is_deterministic` and `export_is_deterministic_in_features` (journey-proofs).

**When SB ships:**
- Correct the async compute section of `.claude/skills/gpu-metal-patterns/SKILL.md:113-135` to the lane calls. Its snippet names do not exist, and its deadlock note becomes section 2's rule.
- Drop the stale parallel-layer claim at `docs/DEVELOPMENT_REFERENCE.md:93`.
- Later, fix `docs/MANIFOLD_GPU_ARCHITECTURE.md:258` (phase 9 is not done) and add the lane protocol there.
- Add the result to the plan's SB entry, list nothing new in `docs/README.md`, and delete this doc.

## 9. Not in SB

- **A trailing frame fence.** The pass-end wait makes everything after the water pass (later layers, the compositor, the preview publish) wait for the tick. Moving the main-lane wait into its own buffer just before the frame-completion signal would let them run beside it, and the preview would no longer wait. It needs the export fence, the warmup checkpoint and the harness waits to cover both queues. Worth it only if SB ships and preview latency then matters.
- A tick that runs longer than a frame (sim decoupled from the render beyond one frame), Matter on the lane (SBm), and any Vulkan code (P1).
