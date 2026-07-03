# Design Hardening Queue — seams that need a Fable review pass before execution

**Status: LIVING · 2026-07-03 · Owner: Peter → Fable design-review agent.**

Items where an execution worker hit a design-doc gap — the doc contradicts the shipped
code, or a refactor phase lacks the `§6` seam brief (old→new signatures) that
`DESIGN_DOC_STANDARD.md` requires — and **STOPPED clean** (zero code, zero commits) per
the escalate-don't-adapt rule, rather than improvising a shortcut.

Each entry is a **design decision, not an execution task.** Bring it to a Fable agent
for the proper options pass, then fold the answer back into the named design doc as an
addendum with committed signatures, and re-issue the blocked phase. **No item here gets
a "just make it compile" workaround** (Peter, 2026-07-03: *"I don't want hacks or
shortcuts just to get the work done. Everything needs to be done properly."*).

Per item: the gap · evidence (`file:line`) · the decision(s) needed · leans (for the
reviewer to weigh, **not** adopt) · the forbidden shortcut a rushed executor would take.

---

## 1. MEDIA_BACKEND_DESIGN §3 — `MediaDecoder` trait vs the shipped decode pipeline

**Blocks:** MEDIA_BACKEND P1 (trait extraction). Parked 2026-07-03.

**The gap.** §3 commits a synchronous, per-instance trait that returns an owned texture:
```rust
fn open(&mut self, path: &Path) -> Result<MediaInfo, MediaError>;
fn next_frame(&mut self, gpu: &GpuDevice) -> Result<DecodedFrame, MediaError>;
struct DecodedFrame { texture: GpuTexture, pts: Seconds }
```
The shipped VT pipeline is not synchronous-per-instance. It is an async job/result
protocol across a 4-worker affinity-routed thread pool, deliberately built so the
content thread never blocks.

**Evidence.**
- `decode_scheduler.rs` — content thread submits `DecodeJob::DecodeNext`; a hash-routed
  worker calls `handle.decode_next_frame()` which returns a **status only, no texture**;
  the worker sends back a raw `handle_ptr: *mut c_void` via `DecodeResultStatus::FrameReady`
  (safe only because no decode job is in-flight for that clip when the content thread drains it).
- `video_renderer.rs:471-583` (esp. `:486`) — the **content thread**, in a later
  `pre_render`, calls `VideoDecoder_CopyFrameToTexture` directly (bypassing the unused
  `DecoderPool::copy_frame_to_texture` wrapper at `decoder.rs:125`) into a destination
  `GpuTexture` it OWNS and REUSES across frames from its own `acquire_rt`/`available_rts`
  pool — zero per-frame allocation, a hot-path invariant at many-4K-layer scale.
- `decoder.rs:81-96` — the shared native `DecoderPool` (MTLDevice + `CVMetalTextureCache`
  + compute pipeline) is created once and shared `Arc<DecoderPool>` across every handle
  for the app's lifetime. The §3 `open()` has no parameter through which to inject it.

**Decisions needed.**
1. **Texture ownership:** does `next_frame` return a fresh owned `GpuTexture` (breaks the
   reuse pool → per-frame alloc), or write into a caller-provided reused texture?
2. **Where the shared `DecoderPool` lives** (the trait `open()` has no slot for it).
3. **Is `decode_scheduler.rs`'s result-channel payload in-scope to change** (carry a
   `GpuTexture` instead of `handle_ptr`), given §9.4 "scheduler/pool/export do not move"?

**Leans (weigh, don't adopt).** `next_frame(&mut self, gpu, dst: &GpuTexture) -> Result<Seconds>`
(write-into-dst, matches the shipped copy exactly), backend owns the shared pool
internally, scheduler stays a caller so its channel is untouched. Encoder side
(`metal_encoder.rs`) already wraps cleanly onto `MediaEncoder` — no issue there.

**Forbidden shortcut.** Allocating a texture per frame; collapsing the thread split into a
synchronous call on the content thread (defeats the worker pool's whole purpose); a
hidden global/static pool the doc doesn't sanction.

---

## 2. MULTI_DISPLAY_DESIGN §6.1 — per-island effect-state seam

**Blocks:** MULTI_DISPLAY P2 (island rendering). Parked 2026-07-03 (Peter: "harden the doc first").

**The gap.** §6.1 says state keys go "per island" and effects "execute once per island,
scissored," but gives **no old→new signature** for the key change — the seam brief
`DESIGN_DOC_STANDARD.md §6` requires. The re-derived inventory materially exceeds the
doc's §2/§6.1 framing.

**Evidence.**
- `rg 'StateStore|state_cache' crates/manifold-renderer/` → **127 hits across 22 files**
  (not the single StateStore-key change the doc implies).
- `state_store.rs:34` — `OwnerKey` is already an `i64`. Existing house precedent for
  mixing a discriminator into it rather than widening the type: `led_group_owner_key`
  (`layer_compositor.rs:550`) mixes a discriminator into `layer_id_owner_key` to keep
  LED temporal state separate from screen state.
- **13 of 22 files hold node-instance state directly on the primitive struct, not via
  `StateStore`:** `temporal.rs` (Feedback's `prev: Option<RenderTarget>`), `array_feedback.rs`,
  `smoothing.rs`, `envelope_decay.rs`, `envelope_follower_ar.rs`, `compressor_envelope.rs`,
  `trigger_ease_to.rs`, `sample_and_hold.rs`, `seed_particles.rs`, `inject_burst.rs`.
  "Execute once, scissored per island" over a single shared chain gives these nodes ONE
  state shared across islands — wrong for feedback/smoothing (each island needs its own
  history).
- `layer_compositor.rs:2136` `render()` is not one canvas: fixed `self.main` composite +
  `self.tonemap` + `master_effect_chain` + a fully independent LED path at a *different*
  resolution (`frame.led_composite_size`, `:1239/:2288`) with its own
  `led_master_ec`/`LED_MASTER_OWNER_KEY` chain and pre/post-tonemap tap ordering, plus a
  serial-vs-parallel per-layer CB split (`:1675/:1712`). None of this is in §2's inventory.

**Decisions needed.**
1. **Key-widen mechanism:** widen `StateStore`'s public key type to a 3-tuple, or fold
   `island` into the existing `OwnerKey` i64 via a mixing function?
2. **Per-island state for the 13 node-struct primitives:** widen each primitive's internal
   storage to a per-island map, or stand up a per-`(layer, island)` `ChainGraph`/`EffectChain`
   instance (multiplying the chain pool, needs an eviction-policy decision)?
3. **LED path scope in P2:** does LED (`led_master_ec`, `led_group_effect_chains`,
   `led_composite_size`) join the P2 atlas/per-island loop, or stay on the legacy
   single-canvas path until the LED-placements phase?

**Leans (weigh, don't adopt).** (1) Fold island into the `OwnerKey` i64 — reuses the
`led_group_owner_key` precedent, no public-API type change. (2) Per-`(layer, island)`
chain instances — reuses the existing chain-pool infra (grace frames, trim, eviction)
instead of surgically widening 13 primitives; island count is small (2–4) and the pool
already evicts. (3) LED stays legacy single-canvas in P2, joins the atlas at the
LED-placements phase — matches the doc's own phasing.

**Forbidden shortcut.** Scissor one shared chain over islands (cross-island state bleed
for feedback/smoothing); widen the 13 primitives half-way and miss one (silent bleed).

---

## Resolution protocol

1. Fable reviews the item, decides each question, and writes the answer into the named
   design doc as an addendum **with committed signatures** (per `DESIGN_DOC_STANDARD.md`
   §4/§6) — including the old→new seam brief for refactor phases.
2. Remove the item from this queue once the doc carries the decision.
3. Re-issue the blocked phase against the hardened doc.
