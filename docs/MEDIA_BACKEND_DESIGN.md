# Media Backend — Neutral Decode/Encode Traits

**Status: APPROVED design · §3a hardening addendum resolved 2026-07-06 (decoder trait re-committed against the shipped async protocol; the original §3 decoder trait is superseded) — P1 is RE-ISSUABLE. · 2026-07-02 · Fable queue (media backend)**
**Prerequisites: none for Metal-era P1–P3. The Vulkan-era handoff (§6) pairs with
`docs/VULKAN_BACKEND_DESIGN.md` §8 — this is the biggest single port item after the GPU.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase.**

Peter's directive: "move all of that stuff behind a proper GPU API like everything
else." Plus (2026-07-02): **"HAP and DXV are important"** and stills "seem to have a
delay or something so can probably improve that further too."

---

## 1. What exists (audited 2026-07-02)

- **Decode is already behind a C FFI** — `decoder_ffi.rs` (`VideoDecoder_*`): native
  plugin owns AVFoundation/VideoToolbox, a shared pool with `MTLDevice` + compute
  pipeline + `CVMetalTextureCache` (`VideoDecoder_CreatePool`, decoder_ffi.rs:12).
  Path: decode → CVPixelBuffer → zero-copy Metal texture via cache → YUV→RGBA
  dispatch. The backend split half-exists; this design formalizes it.
- **Scheduler is pure Rust policy** — `decode_scheduler.rs` (371 lines): lookahead,
  frame timing, seek. Backend-neutral already; unchanged by this design.
- **Encode same pattern** — `metal_ffi.rs` / `metal_encoder.rs`: VideoToolbox hardware
  encode behind FFI. `export_session.rs` + `audio_muxer.rs` orchestrate (neutral).
- **Stills** — `image_renderer.rs` (532 lines), separate path; user-visible delay
  reported (§7).

## 2. Decisions

- **D1 — Two traits, cfg-gated like `manifold-gpu`:** `MediaDecoder` (open / probe /
  seek / next_frame) and `MediaEncoder` (session / submit_frame / finish). Scheduler,
  phantom-frame policy, export session stay neutral Rust above the trait.
- **D2 — The trait returns a `GpuTexture`, nothing rawer.** Pixel formats, planes,
  color conversion are backend-internal. Video backends deliver RGBA (VT keeps today's
  NV12→RGBA dispatch); texture-codec backends deliver BC-compressed textures directly.
  The graph samples either identically. This is the "proper GPU API" boundary.
- **D3 — Three backend families:**
  - **VideoToolbox** (macOS): today's plugin wrapped as the trait impl. Least churn;
    an objc2 rewrite is allowed later but is not this design.
  - **FFmpeg** (all platforms, cfg-gated): codec coverage + the Windows/Linux story.
    **LGPL dynamic-link build** (closed-source app), hardware encoders (NVENC/QSV/AMF)
    for export off-Mac.
  - **TextureCodec** (pure Rust, always compiled): HAP + DXV (§4). No system
    dependencies → works unchanged on the Vulkan port.
- **D4 — Routing by probe:** container/codec probe → HAP/DXV → TextureCodec; else
  VideoToolbox where available; else FFmpeg. One decoder instance per clip as today
  (pool semantics preserved).
- **D5 — Stills join the same prefetch discipline** (§7); root-cause the reported
  delay before optimizing (measure decode vs upload vs schedule).
- **D6 — Audio file decode is out of scope.** No backend problem exists there.

## 3. Trait shape

> **SUPERSEDED for the decoder side by §3a (2026-07-06).** The decoder trait below
> was committed before auditing the shipped decode protocol and cannot wrap it: the
> shipped pipeline is an async job/result split across 4 affinity-routed workers
> (`decode_scheduler.rs`), with the GPU delivery step running separately on the
> content thread into a **reused** texture from its own pool
> (`video_renderer.rs:456-495`) — zero per-frame allocation at many-4K-layer scale.
> A synchronous per-instance `next_frame` returning an **owned** `GpuTexture` breaks
> both shipped invariants at once. §3a is the committed decoder shape. The
> **encoder** side of D1 (`MediaEncoder`: session / submit_frame / finish) is
> unaffected — `metal_encoder.rs` already has exactly that session shape.

```rust
// SUPERSEDED — see §3a. Kept for the record of what was wrong and why.
pub trait MediaDecoder: Send {
    fn probe(path: &Path) -> Option<MediaInfo>;          // static, per-backend
    fn open(&mut self, path: &Path) -> Result<MediaInfo, MediaError>;
    fn seek(&mut self, to: Seconds) -> Result<(), MediaError>;
    fn next_frame(&mut self, gpu: &GpuDevice) -> Result<DecodedFrame, MediaError>;
}
pub struct DecodedFrame { pub texture: GpuTexture, pub pts: Seconds }
```

`MediaError` is a real enum surfaced to the failure-reporting path from
`docs/GIG_RESILIENCE_DESIGN.md` §6 (no log-only errors on the media path).
`⚠ VERIFY-AT-IMPL`: that surfacing path is itself unbuilt (gig-resilience P1). If it
hasn't landed yet, route every `MediaError` through ONE central reporting function
(log-only body for now) so gig P1 has a single site to wire — never scatter
`log::error` calls the later phase would have to hunt.

## 3a. Hardening addendum (2026-07-06) — decoder trait matches the shipped async protocol

Resolves `DESIGN_HARDENING_QUEUE.md` item 1 (parked 2026-07-03). Evidence re-verified
against the tree at `482f554a`: workers return **status only** and send a raw
`handle_ptr: *mut c_void` back (`decode_scheduler.rs:81-95`); the content thread later
calls `VideoDecoder_CopyFrameToTexture` directly (`video_renderer.rs:486`, bypassing
the unused wrapper at `decoder.rs:125`) into a texture it owns and **reuses** via
`acquire_rt`/`available_rts` (`video_renderer.rs:456-468`); the shared `DecoderPool`
(`decoder.rs:86`) is created once at `video_renderer.rs:131` and shared
`Arc<DecoderPool>` through the scheduler for the app's lifetime.

### The three decisions

**A1 — Texture ownership: the caller's, always.** `next_frame` returning an owned
`GpuTexture` is dead — it forces a per-frame allocation or a per-backend recycle
protocol, and the shipped RT pool already solves this. Delivery **writes into a
caller-provided reused texture**, exactly the shipped `copy_frame_to_rt` semantics.
`MediaInfo` gains `delivered_format` so the caller's pool can allocate destinations in
the format the backend actually produces (VT: `Rgba16Float` via today's NV12 dispatch;
TextureCodec: BC1/BC3 direct, `Rgba8Unorm` for Hap Q post-dispatch — a P2 concern, P1
is `Rgba16Float` everywhere as today).

**A2 — The shared pool lives inside the backend layer.** A `MediaBackends` registry
owns each family's shared context (for VT: the `Arc<DecoderPool>`). It is constructed
exactly where `video_renderer.rs:131` constructs the pool today and handed
`Arc<MediaBackends>` to `DecodeScheduler::new`. Decoder instances receive their family
context at construction (`open_decoder`), so the trait needs no pool or device
parameter at all. This registry is also D4's probe-routing home. No global, no static,
no hidden singleton.

**A3 — The scheduler's result payload is in scope; the copy stays on the content
thread.** §9.4 ("scheduler/pool/export do not move") means the *policy* — lookahead,
affinity routing, non-blocking drain — none of which changes. The payload
`handle_ptr: *mut c_void` was never neutral: it is a VideoToolbox pointer that no
other backend family can produce, so it cannot survive D3's multi-backend world. It
becomes a typed, backend-neutral `FrameLease`. What is **forbidden by name**: moving
the copy dispatch onto the workers (single-call `decode+copy`). It looks cleaner —
one trait method — but it changes GPU submission from content-thread-serialized to
four concurrent worker queues, changes when the frame lands relative to compositing,
and violates P1's zero-behavior-change gate. The shipped copy-on-content-thread is
the reference semantics (§5) and stays.

### Committed shapes (new module `crates/manifold-media/src/backend.rs`)

```rust
pub struct MediaInfo {
    pub duration: Seconds,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f32,
    /// Format this backend writes in `FrameLease::deliver` — the caller
    /// allocates its reused destination textures in this format.
    pub delivered_format: TextureFormat,
}

pub enum DecodeProgress {
    FrameReady(Seconds),   // pts of the decoded frame
    EndOfFile,
}

/// One instance per open media file. Owned exclusively by ONE decode worker
/// (affinity-routed, as today) — `Send`, never `Sync`, never shared.
/// Mirrors the shipped job protocol one-to-one:
/// Open (construction, via `MediaBackends::open_decoder`) · Prepare · Seek ·
/// DecodeNext · Close (drop).
pub trait MediaDecoder: Send {
    /// Reader + first frame (today's `VideoDecoder_Prepare`).
    fn prepare(&mut self) -> Result<(), MediaError>;
    /// Seek and decode one frame; returns the actual landed pts.
    fn seek(&mut self, to: Seconds) -> Result<Seconds, MediaError>;
    fn decode_next(&mut self) -> Result<DecodeProgress, MediaError>;
    /// Deliverable for the current decoded frame; `None` until one exists.
    fn frame_lease(&self) -> Option<FrameLease>;
}

/// Cross-thread deliverable for one decoded frame, sent worker → content
/// thread inside `DecodeResultStatus`. The content thread writes the frame
/// into a caller-owned, REUSED texture. Send-safety contract is identical
/// to today's `handle_ptr`: no decode job may be in-flight for the same
/// clip while a lease is delivered (the `decode_pending` flag, unchanged).
/// Closed enum over the D3 backend families — closed set → enum, per house
/// convention (no per-frame `Box<dyn>`).
pub enum FrameLease {
    /// VideoToolbox: deliver = `VideoDecoder_CopyFrameToTexture`.
    /// `Arc` clone per frame is a refcount bump, not an allocation.
    Native { pool: Arc<DecoderPool>, handle: *mut c_void },
    /// CPU payload (TextureCodec BC data now; FFmpeg staging later).
    /// `data` is a backend-owned reusable buffer — no per-frame alloc.
    /// `FrameBytes` (payload bytes + row layout) is committed at P2 with
    /// the TextureCodec backend — P1 constructs only `Native` (§4-sketch
    /// allowance: no P1 phase touches it).
    Bytes { data: Arc<FrameBytes>, format: TextureFormat },
}
impl FrameLease {
    /// Content thread only. `dst` comes from the caller's RT pool.
    pub fn deliver(&self, dst: &GpuTexture) -> Result<(), MediaError>;
}
unsafe impl Send for FrameLease {}   // same justification as DecodeResult today

/// Owns every backend family's shared context; created once, where
/// `video_renderer.rs:131` creates the DecoderPool today. D4's probe
/// routing lives here (TextureCodec sniff → VT → FFmpeg).
pub struct MediaBackends {
    vt_pool: Arc<DecoderPool>,   // macOS; cfg-gated families join later
}
impl MediaBackends {
    pub fn new() -> Result<Self, MediaError>;
    pub fn probe(&self, path: &Path) -> Option<MediaInfo>;
    /// Today's `DecodeJob::Open` semantics: fast metadata, no reader yet.
    pub fn open_decoder(&self, path: &Path)
        -> Result<(Box<dyn MediaDecoder>, MediaInfo), MediaError>;
}
```

`MediaError` unchanged from §3's prose (real enum, one central reporting site);
`DecoderError` maps into it inside the VT backend.

### Seam brief — P1 (old → new, per DESIGN_DOC_STANDARD §6)

| Old | New |
|---|---|
| Worker maps `AHashMap<String, DecoderHandle>` (`decode_scheduler.rs:230-231`, active + warm) | `AHashMap<String, Box<dyn MediaDecoder>>` |
| `DecodeResultStatus::{Prepared, FrameReady, Seeked} { handle_ptr: *mut c_void, .. }` (`decode_scheduler.rs:81-95`) | same variants, `lease: FrameLease` (pts fields unchanged) |
| `pool.open(&path)` in workers (`decode_scheduler.rs:235`, `:348`) | `backends.open_decoder(path)` |
| `copy_frame_to_rt(&pool, handle_ptr, &clip.render_target)` (`video_renderer.rs:478-504`, FFI call at `:486`) | `lease.deliver(&clip.render_target.texture)`; `copy_frame_to_rt` deleted |
| `Arc::new(DecoderPool::new()…)` + `DecodeScheduler::new(decoder_pool)` (`video_renderer.rs:131-132`) | `Arc::new(MediaBackends::new()?)` + `DecodeScheduler::new(backends)` |
| `DecodeScheduler::pool()` accessor (`decode_scheduler.rs:199`) | deleted (no caller needs raw pool access) |
| `DecoderPool::copy_frame_to_texture` unused wrapper (`decoder.rs:125`) | deleted; its body becomes `FrameLease::deliver`'s VT arm |

Call-site inventory (counted 2026-07-06): `handle_ptr` — 13 hits in
`decode_scheduler.rs`, 10 in `video_renderer.rs`; `DecoderHandle` — 7 in
`decode_scheduler.rs`. All mechanical rewrites under the table above; no misfit
sites found. **Re-derivation command:** `rg -n 'handle_ptr|DecoderHandle|\.pool\(\)'
crates/manifold-media/` — re-run at execution time; if counts differ, stop and list
the new sites first. Compiler-driven migration: change the `DecodeResultStatus`
variants first; red is the checklist. **Deletion gates (negative):**
`rg -c 'handle_ptr' crates/manifold-media/` = 0, plus P1's existing gate
(`rg -n 'VideoDecoder_|MetalEncoder_'` outside backend modules = 0).

### Consequences, stated honestly

- The four-worker split, affinity routing, `decode_pending` discipline, RT pool, and
  copy-on-content-thread timing are all byte-identically preserved — P1 stays a pure
  type-level refactor, which is what its gate demands.
- `FrameLease::Bytes` commits TextureCodec (P2) to backend-owned reusable payload
  buffers. Double-buffering across the in-flight window is the backend's business;
  the no-per-frame-alloc contract is the trait's.
- The §3 trait's `probe` as a trait method could never work through `dyn` anyway
  (no receiver); routing was always going to need a registry. `MediaBackends` is
  that registry, named.

## 4. TextureCodec backend — HAP and DXV

These are GPU texture codecs: each frame is S3TC/BC texture data, compressed with a
fast LZ. Decode = demux MOV → LZ decompress (CPU, cheap) → upload as a
BC-format `GpuTexture`. No pixel conversion, no hardware decoder session, near-zero
GPU cost — which is why the VJ world standardized on them for many simultaneous 4K
layers.

| Codec | Payload | Chunk LZ | Delivered texture |
|---|---|---|---|
| Hap | BC1 (DXT1) | Snappy | BC1 `GpuTexture`, direct |
| Hap Alpha | BC3 (DXT5) | Snappy | BC3, direct |
| Hap Q | YCoCg-DXT5 | Snappy | BC3 + one YCoCg→RGB dispatch → RGBA8 |
| DXV3 (Resolume) | DXT1 / DXT5 | LZF / raw | BC1/BC3, direct |

- Pure Rust: minimal MOV atom demux + `snap` + LZF; BC formats are supported by Metal
  on all Macs and by every desktop Vulkan device.
- **FFmpeg's `dxv` decoder is the reference implementation** for DXV chunk layout —
  mirror it, don't link it (keeps TextureCodec dependency-free).
- Hap Q is the one variant needing a (single, tiny) conversion dispatch; it is not a
  monolith — it's the same one-dispatch shape as the VT path's NV12→RGBA.
- **Encode:** HAP export = BC encode (CPU crate or GPU compute) + snappy + MOV mux —
  deferred to the encode phase, worth having (hand clips to other VJs). DXV encode:
  verify FFmpeg's `dxv` encoder status at implementation; if absent, export HAP and
  let Resolume Alley (free) convert. Never reverse-engineer DXV encode beyond what
  FFmpeg ships.

## 5. Zero-copy handoff per backend

The judgment-dense core: `next_frame` must be zero-copy where the platform allows it,
and the *trait must not know which*.

- **VideoToolbox / Metal (today):** CVPixelBuffer is IOSurface-backed →
  `CVMetalTextureCache` wraps it as MTLTexture without copying → conversion dispatch
  writes the pool RGBA texture. Already shipped; becomes the reference semantics.
- **FFmpeg software decode (any GPU):** staging-buffer upload. Not zero-copy, fine —
  software decode is the fallback tier by definition.
- **FFmpeg hardware decode / Vulkan (later):** VAAPI/D3D11/VideoToolbox hw frames →
  `VkImage` external-memory import where the interop exists; staging upload where it
  doesn't. Import is an optimization *inside* the backend — behavior identical either
  way. (Pairs with VULKAN_BACKEND_DESIGN §8; do not build ahead of the Vk port.)
- **TextureCodec (any GPU):** CPU decompress → BC staging upload. BC1 at 4K is 4MB —
  upload cost is trivial; that's the codec's whole point.

## 6. Encode / export

- `MediaEncoder` impls: VideoToolbox (H.264/HEVC/ProRes — today's `metal_encoder.rs`
  wrapped), FFmpeg (hw encoders off-Mac), HAP (deferred, §4).
- Export session, audio mixdown/mux, still export stay neutral above the trait.
- Encoder errors surface like decoder errors (D6 gig-resilience alignment).

## 7. Stills latency (reported)

Peter reports a visible delay on stills. Likely candidates, in order: synchronous
decode at first-display time (no prefetch), decode on a shared thread behind video
work, full-size CGImage decode + upload stall on the content tick. **Implementation
starts with measurement** (instrument `image_renderer.rs` open→first-texture), then:

- Stills join the scheduler's lookahead: decode + upload before the clip's start beat,
  same policy video already gets.
- Decoded-still cache keyed by asset (stills are small; cache whole).
- If upload is the stall: staging upload off the content tick (pattern exists in the
  decode pool).

Root-fix rule applies: whatever the measurement shows, fix the class (prefetch
discipline), not the symptom.

## 8. Phasing (Sonnet-executable)

Entry state, every phase: re-verify the §1 anchors (`decoder_ffi.rs:12`,
`decode_scheduler.rs`, `metal_encoder.rs`, `image_renderer.rs` — audited 2026-07-02).

- **P1 — Trait extraction (re-issued against §3a).** Define the §3a shapes
  (`MediaDecoder`, `FrameLease`, `MediaBackends`, `MediaInfo`, `DecodeProgress`) in
  `backend.rs`; wrap the existing VT plugin (decoder side) and metal encoder as
  impls; scheduler policy and export unchanged. Read-back: §3a whole, including its
  seam table and re-derivation command. Seam: §3a's table governs; additionally
  enumerate the FFI call sites — `rg -n 'VideoDecoder_|MetalEncoder_'
  crates/manifold-media/` — every one moves behind a backend module; the negative
  gates are that `rg` scoped outside the backend modules returning zero AND
  `rg -c 'handle_ptr' crates/manifold-media/` = 0. Forbidden: scheduler/policy
  "improvements" while wrapping (zero behavior change is the gate); moving the copy
  dispatch onto workers (§3a A3, forbidden by name); per-frame texture allocation
  (A1); pixel-format or plane logic above the trait (D2 boundary); the objc2 rewrite
  (explicitly not this design). Gate: full workspace sweep (media path =
  infrastructure) + existing export tests, byte-identical outputs.
- **P2 — TextureCodec decode.** HAP (all variants) + DXV3 decode, probe routing,
  parity vs FFmpeg-decoded reference frames (value-level: exact BC payloads for
  direct variants). Forbidden: linking FFmpeg into TextureCodec (mirror its `dxv`
  source as reference, dependency-free rule §4); more than the single Hap Q
  conversion dispatch. Negative gate: existing non-HAP clips still probe-route to VT
  (regression test on routing). Peter's Resolume-world clips are the acceptance
  fixture.
- **P3 — Stills.** Measure FIRST — instrument `image_renderer.rs` open→first-texture
  and record the numbers in the session before touching anything (D5/§7; speculative
  fixes are the named forbidden move). Then prefetch/cache per §7. Acceptance: no
  visible delay triggering a still clip live, and the measurement diff proving which
  slice shrank.
- **P4 — FFmpeg backend.** Decode first (codec coverage on Mac), encode with the
  Vulkan/Windows port. LGPL build plumbing (dynamic link, dylib shipping — verify the
  license posture: no static FFmpeg anywhere).
- **P5 — HAP encode** (with the export/encode work, or when first needed). DXV encode
  per §9.8 — check FFmpeg's encoder status at that time, never reverse-engineer.

## 9. Decided — do not reopen

1. The trait boundary is a `GpuTexture`; formats/planes/conversion are
   backend-internal. (§3a mechanism: delivery *writes into* a caller-owned reused
   `GpuTexture` rather than returning an owned one — the D2 boundary is unchanged,
   the ownership direction is.)
2. Backend families: VideoToolbox (wrap existing plugin), FFmpeg (LGPL dynamic),
   TextureCodec (pure Rust, dependency-free, always on).
3. HAP + DXV route to TextureCodec, never through VT/FFmpeg — the BC passthrough is
   the point (Peter: "HAP and DXV are important").
4. Scheduler/pool/export session are neutral policy and do not move. (Clarified by
   §3a: "do not move" binds the *policy* — lookahead, affinity routing, non-blocking
   drain. The result-channel payload was VT-specific (`handle_ptr`) and becomes the
   neutral `FrameLease`; the copy stays on the content thread.)
5. Zero-copy is a backend-internal optimization; the trait contract is identical
   with or without it.
6. Media errors surface through the gig-resilience failure path — no log-only.
7. Stills: measure first, then prefetch discipline — no speculative micro-fixes.
8. DXV encode only via FFmpeg if it ships one; otherwise HAP export + Alley. No
   reverse-engineering beyond FFmpeg's implementation.

Deferred: objc2 rewrite of the VT plugin, NDI/Syphon (multi-display §10 P6 owns
those), audio file decode, Hap7/HapH (BC7) until clips actually arrive in it.
