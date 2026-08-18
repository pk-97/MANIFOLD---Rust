# UI Responsiveness Under Load — keep the instrument playable when the render craters

**Status:** APPROVED design, not built · 2026-08-19 · k3 (lead)
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) and section 6 (Seam briefs) before starting any phase.

The UI thread already paints at display cadence on its own CVDisplayLink and never locks on
`Project`. What degrades the feel under heavy GPU load (RT scene at ~10fps) is two specific
couplings, not the architecture: (1) the content thread applies edit commands promptly but only
*publishes* the confirming `ContentState` snapshot after a full render tick, so the tree/params
confirm 100ms+ late; (2) each frame's GPU work is two monolithic command buffers, so the UI's own
tiny render + present can't interleave and its drawable pool stalls. Both fixes are small,
API-backed, and Vulkan-safe. Peter's directive, verbatim: "Anything we do for this should be API
backed and easy and safe for the future Vulkan port and cross platform support." And on
verification: build both, then verify — "no need to measure if it's always a net positive on
paper" refers to up-front measurement gates; post-build verification with the existing profiler
segments still happens.

Companion docs: `docs/VSYNC_AND_FRAME_PACING.md` (pacing model; Peter 2026-08-19: "quite old so
it is potentially not all that correct" — its lessons are prior evidence, not gospel).
`docs/MANIFOLD_GPU_ARCHITECTURE.md` (GPU invariants). `docs/RT_QUALITY_SETTINGS_DESIGN.md`
(adaptive resolution — the separate, bigger lever; explicitly out of scope here).

## 1. Audit — what exists (verified 2026-08-19)

| Piece | Where | State |
|---|---|---|
| UI render gate (CVDisplayLink) | `crates/manifold-app/src/app.rs:3062` (`vsync_ready()` → `tick_and_render` :3068) | UI paints at display cadence, independent of content rate. Extend, don't redesign. |
| UI drawable acquire | `crates/manifold-gpu/src/metal/surface.rs:192` (`allowsNextDrawableTimeout`), `:224` (`next_drawable` → `Option`); skip-on-None at `crates/manifold-app/src/frame/present.rs:721` | Never blocks forever, but can stall the main thread up to the ~1s timeout when the pool starves. |
| Monolithic frame encoders | `crates/manifold-app/src/content_pipeline.rs:2039` ("Generators", commit :2192/:2195), `:2214` ("Compositor", commit :3042/:3045) | Two command buffers per frame hold all generator + compositor work. This is the chunking target. |
| Generator encode loop | `crates/manifold-app/src/content_pipeline.rs:2073` (`gen_renderer.render_all(&mut gpu_gen, …)`) | All layers' generators encode into the one "Generators" buffer via the `GpuEncoder` wrapper (`crates/manifold-renderer/src/gpu_encoder.rs:7`). |
| Command drain under load | `crates/manifold-app/src/content_thread.rs:478` (`wait_for_surface_draining_commands`), main drain :371–431 | Commands are already processed promptly while the GPU is behind. Latency is in *publish*, not apply. |
| Snapshot publish | `crates/manifold-app/src/content_thread.rs:1112` (`data_version` check), build :1241, `state_tx.send` :1403 — all inside `tick_frame` (:589) | Publish is welded to rendering. `last_data_version` field already exists (:80). |
| Rendering-paused mode | `crates/manifold-app/src/content_thread.rs:85` (`rendering_paused`), skip at :449–453 | While paused, mutations never publish — the ready-made test harness for I3. |
| Commit API | `crates/manifold-gpu/src/metal/encoder.rs:2299` (`commit`, consumes self), :2306/:2322 (wait variants); encoder created at `crates/manifold-gpu/src/metal/device.rs:1111` | No commit-and-continue exists. |
| Dispatch profiling | `crates/manifold-app/src/content_pipeline.rs:2044` (`enable_dispatch_profiling` on the Generators encoder when `--profile`) | Per-buffer timestamps; incompatible with mid-encode chunk splits (D5). |
| Profiler segments for verification | `crates/manifold-app/src/ui_frame_profile.rs:54` (`present.next_drawable`); offscreen GPU-time handler `crates/manifold-app/src/ui_frame.rs:806` | Already measures exactly the starvation this design attacks. |

## 2. Decisions

- **D1 — Decouple snapshot publish from rendering.** Extract the `ContentState` build+send
  (`content_thread.rs:1112–1403`) into `publish_snapshot_if_dirty(&mut self, state_tx)`, gated on
  `editing_service.data_version() != last_data_version`. Call it from `tick_frame` (as today) and
  at the end of every command-drain pass (main loop drain and each batch inside
  `wait_for_surface_draining_commands`). One publish per drain pass, never per command.
  Rationale: commands are already applied promptly; only the confirmation lags.
- **D2 — Add `commit_and_continue` to `manifold_gpu::GpuEncoder`.** Signature:
  `pub fn commit_and_continue(&mut self, device: &GpuDevice)` — commits the current command
  buffer, then replaces it with a fresh one from the same queue (mirroring
  `device.rs:1111–1125`), clearing `state`, both bind caches, and `scopes` (moved into the fault
  handler at commit, as `commit` does). Never blocks. Same-queue commit order + Metal's automatic
  hazard tracking preserve execution order and cross-chunk resource dependencies.
  Rationale: Metal exposes no command-queue priority; smaller submissions are the only interleave
  mechanism. Rejected: a second "high-priority" queue for UI work — Metal has no priorities, and
  the April 2026 decoupled-presenter failure (`VSYNC_AND_FRAME_PACING.md`) showed a second queue
  makes contention worse, not better.
- **D3 — Vulkan mapping is the backend's problem, named now.** `commit_and_continue` maps to
  split `vkQueueSubmit`s on one queue with a conservative pipeline barrier at the start of each
  continuation buffer (Vulkan does not carry memory dependencies across submits). The API name
  and semantics are backend-neutral; the barrier lives inside the future Vulkan backend.
  Enforcement until then: none — the Vulkan backend doesn't exist (`docs/VULKAN_BACKEND_DESIGN.md`).
- **D4 — Checkpoint placement is fixed, not adaptive.** Call sites: between layers inside
  `GeneratorRenderer::render_all` (via a `checkpoint()` method on the renderer-side `GpuEncoder`
  wrapper, which owns both the native encoder and the device ref) and between compositor passes
  in `content_pipeline.rs:2214–3042`. Rejected: GPU-time-budget-driven adaptive chunking —
  needs completion-timestamp feedback, tunable only with data we don't have yet. Deferred with a
  revival trigger (section 8, Deferred).
- **D5 — Profiling frames stay monolithic.** When dispatch profiling is enabled on an encoder
  (`content_pipeline.rs:2044`), chunk call sites are skipped for that frame (a
  `chunking_enabled` flag on the pipeline, `false` while `profiling_enabled`). Profiled runs
  measure the unchunked worst case — acceptable for a dev tool. Additionally
  `commit_and_continue` carries `debug_assert!(self.profile.is_none())` so a missed call site
  fails loudly in dev.
- **D6 — No fences, no waits between chunks.** Chunks are submitted back-to-back and the content
  thread moves on. Any synchronization between chunks reintroduces the bubble this design exists
  to remove.

Plausible-wrong architectures, named: you will want a **separate UI command queue** — no, D2.
You will want to **publish a snapshot inside `handle_command` per command** — no, coalesce per
drain pass (D1); the dirty check is one `u64` compare. You will want to **wait for chunk N to
schedule before submitting N+1** — no, D6.

## 3. Design body — P1: snapshot publish decoupling

Old → new:

```
// tick_frame today: build+send inline (:1112–1403)
// new:
fn publish_snapshot_if_dirty(&mut self, state_tx: &Sender<ContentState>) {
    let version = self.editing_service.data_version();
    if version == self.last_data_version { return; }
    // existing build+send body, unchanged
}
```

Call sites: `tick_frame` (replaces the inline code), end of the main drain pass
(`content_thread.rs:431`), and each drained batch in `wait_for_surface_draining_commands`
(:478). While `rendering_paused`, the drain still runs, so paused-mode edits now publish — this
is intended and is the I3 test harness.

Precedent: `data_version` dirty-checking is the established hot-path pattern
(`last_data_version` already at :80). No new channels, no new shared state; the UI already
drains `ContentState` to latest.

Honest cost: an extra snapshot build per mutating drain pass under load. The build is
`Arc<Project>` clone + cached `Arc<str>` strings (`content_thread.rs:169` — refcount bumps, zero
alloc); modulation data rides its own snapshot (:1119) and is unaffected. Sub-millisecond.

## 4. Design body — P2: commit-and-continue + checkpoints

New API (committed):

```rust
impl GpuEncoder { // crates/manifold-gpu/src/metal/encoder.rs
    /// Commit the current command buffer and continue encoding into a fresh
    /// one from the same queue. Submission order is preserved; Metal hazard
    /// tracking covers cross-chunk resource dependencies. Never blocks.
    /// Backend-neutral: the Vulkan backend maps this to split queue submits
    /// with a barrier at each continuation (D3).
    pub fn commit_and_continue(&mut self, device: &GpuDevice);
}
```

Renderer wrapper (committed):

```rust
impl GpuEncoder<'_> { // crates/manifold-renderer/src/gpu_encoder.rs
    /// Split the underlying submission if the pipeline has chunking enabled
    /// this frame (D5: skipped under dispatch profiling). No-op otherwise.
    pub fn checkpoint(&mut self);
}
```

The wrapper gains a `chunking_enabled: bool` (set at frame start from the pipeline flag; default
`false` in `new`/`with_pool` so all other callers are untouched).

Checkpoint call sites (re-derive at execution; the count is the gate):

- `GeneratorRenderer::render_all` — one `checkpoint()` between layer cards.
  Re-derive: `rg -n 'fn render_all' crates/manifold-renderer/src/generator_renderer.rs`.
- Compositor phase (`content_pipeline.rs:2214–3042`) — between per-layer/per-pass encode groups.
  Re-derive: `rg -n 'create_encoder' crates/manifold-app/src/content_pipeline.rs` → expect 4
  sites (Generators, Compositor, PQ Encode, Still Readback); only the first two chunk.

Not chunked: PQ Encode, Still Readback, preview captures (`content_pipeline.rs:2090` stays in
whatever chunk precedes it — it reads the generator output, which Metal orders automatically).

Intra-node chunking (splitting one RT node's bounce loop) is NOT in this phase — see section 8
(Deferred).

Honest cost: per-commit CPU overhead per chunk (tens of microseconds) and weaker overlap at
boundaries; expected <1–2% at layer granularity. The failure mode to watch is not cost but
*no benefit* — if Metal's scheduler doesn't interleave UI work between chunks, the change is a
no-op. That's what the post-build profiler check decides; it is not a landing gate (Peter:
build both, verify after).

## 5. Invariants & enforcement

- **I1 — Chunking never changes render output.** Same queue, commit order preserved.
  Enforcement: `scripts/gpu_proofs_gate.py` green (fused/unfused proofs cover the generator
  encode path) and `scripts/rt_noise_gate.py` within committed ceilings (RT accumulation is
  inside the chunked region).
- **I2 — `commit_and_continue` never blocks.** Enforcement: negative gate —
  `rg -A25 'fn commit_and_continue' crates/manifold-gpu/src/metal/encoder.rs | rg 'wait'`
  must return zero hits; signature returns `()`.
- **I3 — A mutation publishes a snapshot even when no frame renders.** Enforcement: test
  `paused_mutation_publishes_snapshot` (manifold-app): content thread with
  `rendering_paused = true`, send a mutating `ContentCommand`, assert a `ContentState` with the
  bumped `data_version` arrives on the receiver within 500ms.
- **I4 — No new shared state, no new threads/channels.** Enforcement: negative gate —
  `rg 'Arc<Mutex|Arc<RwLock' crates/manifold-app/src/content_thread.rs crates/manifold-renderer/src/gpu_encoder.rs crates/manifold-gpu/src/metal/encoder.rs`
  shows only pre-existing hits (diff against main at landing).

## 6. Phasing

### P1 — snapshot publish decoupling (one session)

- **Entry state:** audit anchors at `content_thread.rs:589,1112,1241,1403` re-verified
  (`rg -n 'fn tick_frame|last_data_version|state_tx.send' crates/manifold-app/src/content_thread.rs`).
- **Read-back:** D1, the plausible-wrong list, `content_thread.rs:371–465` (loop) and
  `:1100–1410` (snapshot build). Restate: publish is coalesced per drain pass, gated on
  `data_version`; no per-command publishes; no new channels.
- **Deliverables:** `publish_snapshot_if_dirty` extracted; three call sites; test
  `paused_mutation_publishes_snapshot` (I3).
- **Gate:** positive — `cargo nextest run -p manifold-app` green including the new test.
  Negative — I4 `rg` gate. Content-thread work gate: `MANIFOLD_RENDER_TRACE=1` run, no frame
  >20ms attributable to the publish path.
- **Demo:** none — L1. Landing click-script (L4, Peter): open the RT-heavy project, drag a clip
  while output runs ~10fps; the tree/timeline confirm the drag immediately.
- **Performer gesture:** mid-set, grab and move a clip while a heavy scene plays — the edit
  confirms in the UI within a frame of the mouse, not a render frame later.
- **Forbidden moves:** publishing from inside `handle_command` per command · adding a
  "snapshot requested" flag channel · touching the ModulationSnapshot path · "while here"
  refactors of the ContentState fields.
- **Test scope:** `-p manifold-app` only.

### P2 — commit-and-continue + checkpoints (one session)

- **Entry state:** P1 landed. Anchors re-verified:
  `rg -n 'pub fn commit' crates/manifold-gpu/src/metal/encoder.rs`,
  `rg -n 'fn create_encoder' crates/manifold-gpu/src/metal/device.rs`,
  `rg -n 'fn render_all' crates/manifold-renderer/src/generator_renderer.rs`. A moved anchor is
  an escalation.
- **Read-back:** D2–D6, `encoder.rs:92–108` (struct) and `:2298–2354` (commit family),
  `device.rs:1111–1126` (what a fresh encoder needs), `gpu_encoder.rs:1–60` (wrapper).
  Restate: never blocks; profiling frames stay monolithic; no fences between chunks.
- **Deliverables:** `commit_and_continue` + `debug_assert!(profile.is_none())`; wrapper
  `checkpoint()` + `chunking_enabled` flag; checkpoint call sites per section 4
  (commit-and-continue + checkpoints); unit test
  `commit_and_continue_preserves_order` (manifold-gpu): three chunks write ordered values to one
  buffer, readback proves commit order; completed-handler count == 3.
- **Gate:** positive — `cargo nextest run -p manifold-gpu -p manifold-app` green;
  `scripts/gpu_proofs_gate.py` green; `scripts/rt_noise_gate.py` within ceilings. Negative —
  I2 `rg` gate; I4 `rg` gate.
- **Post-build verification (not a gate):** RT-heavy project, capture `present.next_drawable`
  p95 and the offscreen GPU-time handler before/after; report both numbers in the landing
  report whether or not they improved.
- **Demo:** L2 — the profiler numbers; click-script (L4, Peter): same RT project, scrub the
  timeline; UI follows the mouse while output holds ~10fps.
- **Performer gesture:** timeline scrub under an RT scene at single-digit fps.
- **Forbidden moves:** waiting/scheduling checks between chunks · adaptive budget logic ·
  chunking PQ/readback paths · a second command queue · touching `TexturePool` or the uniform
  arena to "make them chunk-safe" (they are frame-scoped; if evidence says otherwise, escalate).
- **Test scope:** `-p manifold-gpu -p manifold-app` + the two GPU scripts above (GPU path
  touched — mandatory per CLAUDE.md).

## 7. Decided — do not reopen

1. Publish is coalesced per drain pass, gated on `data_version` (D1).
2. `commit_and_continue(&mut self, device: &GpuDevice)` is the whole API; same queue, never
   blocks (D2, D6).
3. Vulkan barrier handling lives in the future Vulkan backend, not in call sites (D3).
4. Fixed checkpoint placement at layer/pass granularity; no adaptive budgets (D4).
5. Dispatch-profiled frames run monolithic (D5).
6. No second command queue for UI work, ever (D2 rejected alternative).

## 8. Deferred

- **Intra-node chunking** (splitting one RT node's bounce/sample loop across commits). Revival
  trigger: post-P2 profiler capture shows a single chunk still exceeding ~16ms GPU.
- **Adaptive chunk sizing** from completion-timestamp feedback. Revival trigger: fixed
  chunking measurably helps but boundary overhead shows in `MANIFOLD_RENDER_TRACE`.
- **Adaptive resolution / quality scaling** — owned by `RT_QUALITY_SETTINGS_DESIGN.md`, the
  bigger lever for the show itself; this design does not touch it.
- **Vulkan mapping of `commit_and_continue`** — lands with the Vulkan backend
  (`docs/VULKAN_BACKEND_DESIGN.md`), not before.
