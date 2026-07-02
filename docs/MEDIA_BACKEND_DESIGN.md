# Media Backend — Neutral Decode/Encode Traits

**Status: APPROVED design, not built · 2026-07-02 · Fable queue (media backend)**
**Prerequisites: none for Metal-era P1–P3. The Vulkan-era handoff (§6) pairs with
`docs/VULKAN_BACKEND_DESIGN.md` §8 — this is the biggest single port item after the GPU.**

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

```rust
pub trait MediaDecoder: Send {
    fn probe(path: &Path) -> Option<MediaInfo>;          // static, per-backend
    fn open(&mut self, path: &Path) -> Result<MediaInfo, MediaError>;
    fn seek(&mut self, to: Seconds) -> Result<(), MediaError>;
    /// Decode the next frame and deliver it as a GPU texture.
    /// The device handle is the backend-matched GpuDevice (Metal now, Vk later).
    fn next_frame(&mut self, gpu: &GpuDevice) -> Result<DecodedFrame, MediaError>;
}

pub struct DecodedFrame {
    pub texture: GpuTexture,   // RGBA (video) or BC1/BC3/BC7 (texture codecs)
    pub pts: Seconds,
}
```

`MediaError` is a real enum surfaced to the failure-reporting path from
`docs/GIG_RESILIENCE_DESIGN.md` §6 (no log-only errors on the media path).

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

- **P1 — Trait extraction.** Define traits + `DecodedFrame`; wrap the existing VT
  plugin and metal encoder as impls; scheduler/export unchanged. Zero behavior change —
  gate: full workspace sweep (media path = infrastructure) + existing export tests.
- **P2 — TextureCodec decode.** HAP (all variants) + DXV3 decode, probe routing,
  parity vs FFmpeg-decoded reference frames (value-level: exact BC payloads for
  direct variants). Peter's Resolume-world clips are the acceptance fixture.
- **P3 — Stills.** Measure, then prefetch/cache per §7. Acceptance: no visible delay
  triggering a still clip live.
- **P4 — FFmpeg backend.** Decode first (codec coverage on Mac), encode with the
  Vulkan/Windows port. LGPL build plumbing (dynamic link, dylib shipping).
- **P5 — HAP encode** (with the export/encode work, or when first needed).

## 9. Decided — do not reopen

1. Traits return `GpuTexture`; formats/planes/conversion are backend-internal.
2. Backend families: VideoToolbox (wrap existing plugin), FFmpeg (LGPL dynamic),
   TextureCodec (pure Rust, dependency-free, always on).
3. HAP + DXV route to TextureCodec, never through VT/FFmpeg — the BC passthrough is
   the point (Peter: "HAP and DXV are important").
4. Scheduler/pool/export session are neutral policy and do not move.
5. Zero-copy is a backend-internal optimization; the trait contract is identical
   with or without it.
6. Media errors surface through the gig-resilience failure path — no log-only.
7. Stills: measure first, then prefetch discipline — no speculative micro-fixes.
8. DXV encode only via FFmpeg if it ships one; otherwise HAP export + Alley. No
   reverse-engineering beyond FFmpeg's implementation.

Deferred: objc2 rewrite of the VT plugin, NDI/Syphon (multi-display §10 P6 owns
those), audio file decode, Hap7/HapH (BC7) until clips actually arrive in it.
