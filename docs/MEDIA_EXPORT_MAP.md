# Media & Export — Current-State Map (Decode · Playback · Video Export · Still Export)

**Status: AUTHORITATIVE current-state map.** Written 2026-07-12 (Fable) from a full read of
`manifold-media` (all 13 Rust modules + both native Objective-C plugins) plus a periphery
sweep of every call site in `manifold-app`. Sibling of `CORE_ENGINE_MAP.md` and
`FREEZE_COMPILER_MAP.md`: this describes what is *built*, not what is planned.
`MEDIA_BACKEND_DESIGN.md` (neutral decode/encode traits) and `VIDEO_IO_DESIGN.md`
(Syphon/NDI) are the forward-looking contracts layered on top of this; where they disagree
with this file about the present, this file wins.

**Scope.** The `manifold-media` crate and the app-side export path (`content_export.rs`).
**Live recording is out of scope** — it is a separate crate (`manifold-recording`, its own
AVAssetWriter plugin, texture pool, and fence machinery) mapped and proof-tested by
`LIVE_RECORDING_PROOFS_DESIGN.md`. The two never share code; recording is even skipped
per-frame while `export_mode` is set (`content_pipeline.rs` recording block is gated
`!export_mode`).

## 1. What it is, in one paragraph

`manifold-media` is macOS-native media I/O for the show: hardware video decode for timeline
playback (AVAssetReader + VideoToolbox → NV12 → a Metal compute shader that converts to the
compositor's linear Rgba16Float), hardware video encode for offline export (compositor
texture → compute copy → CVPixelBuffer → AVAssetWriter, H.264 SDR or HEVC-10 HDR), a
CPU still-frame exporter (PNG/JPEG from a GPU readback), and an ffmpeg-subprocess audio
muxer that marries the rendered mixdown WAV into the exported MP4. Only `manifold-app`
depends on it. The export frame loop itself lives in the app
(`ContentThread::run_export`), not the crate — the crate provides the session state
machine and the encoder; the app provides the engine ticks and the frames.

## 2. File map

Crate (`crates/manifold-media/src/`, ~6.9k lines; all GPU/native modules are
`#[cfg(target_os = "macos")]`):

| File | Role |
|---|---|
| `export_session.rs` | `ExportSession` state machine: frame count/progress/audio-offset math, wraps the encoder. Two near-duplicate constructors (`new`, `new_with_device` — copy-pasted range/offset logic). |
| `export_config.rs` | `ExportConfig` plain struct. No validation methods; codec choice is implicit in `hdr: bool` — there is no codec enum. |
| `metal_encoder.rs` | Safe wrapper over the encoder FFI. Tracks `frames_encoded`; `Drop` finalizes if `end_session()` was never called. `new()` checks `is_available()`/`is_hdr_available()`; **`new_with_device()` skips both checks**. |
| `audio_muxer.rs` | Blocking ffmpeg subprocess: video stream-copied, audio always re-encoded AAC 256k, `-shortest`, `+faststart`. `resolve_ffmpeg()`: `FFMPEG_PATH` env → bundled runtime paths → Homebrew/system. |
| `still_exporter.rs` | CPU PNG/JPEG encode + `linear_f16_rgba_to_srgb8` (true piecewise sRGB OETF, optional tanh highlight rolloff above 0.8 for EDR). Well unit-tested. |
| `decoder.rs` / `decoder_ffi.rs` | `DecoderPool` (shared MTLDevice + NV12→RGBA pipeline + texture cache) and per-file `DecoderHandle` (AVAssetReader; seek = reader re-create). |
| `decode_scheduler.rs` | 4 worker threads, jobs routed by clip-id hash affinity so a clip's handle lives on exactly one worker. Content thread submits jobs / drains results, never blocks (except `flush`, §3). |
| `video_renderer.rs` | `ClipRenderer` impl for video clips: per-clip render targets from a pool, decode pacing by accumulated dt vs. source fps, seek coalescing, poster/filmstrip one-shot decodes for parked clips (isolated `\u{1}poster\u{1}` key namespace). |
| `image_renderer.rs` | `ClipRenderer` impl for still images: background-thread decode + aspect-fit + premultiply, upload via `upload_texture`. Caches the full-res decode so resizes re-fit from memory. |
| `metadata.rs` | `probe_video_metadata` (fast AVAsset probe) + `SUPPORTED_EXTENSIONS` (`.mp4 .mov .webm .avi`). |
| `native/MetalEncoderPlugin.m` | AVAssetWriter + copy shader. SDR: H.264 High, BGRA8, **bakes gamma via `pow(1/2.2)`**. HDR: HEVC Main10, RGBA16Half, BT.2020/PQ metadata, straight copy (PQ already applied upstream). Bitrate: 0.6 bits/px/frame, clamped 20–400 Mbps, 1 GOP/s, no reordering. |
| `native/MetalVideoDecoderPlugin.m` | AVAssetReader → NV12 → compute convert. **Hardcodes BT.709 video-range and `pow(2.2)` linearization; nearest-neighbor sampling; aspect-fit (FitInside) baked into the shader**, transparent-black padding, alpha forced to 1 inside the image. |

App periphery (`crates/manifold-app/src/`):

| File | Role |
|---|---|
| `content_export.rs` | The whole export orchestration: `run_export` (replaces the normal frame loop), `export_one_frame`, still-export submit/poll, progress snapshots. |
| `app_lifecycle.rs` | `start_export` (save dialog → `ExportConfig` from `ProjectSettings` + timeline export range → `ContentCommand::StartExport`), `export_frame` (still), `import_video_clip` (probe on a background thread). |
| `content_thread.rs` | `StartExport` handled directly in the run loop (needs `cmd_rx`/`state_tx`); `set_device` re-pointing for both renderers after the pipeline moves; prewarm candidates → `pre_warm_from_candidates`. |
| `content_pipeline.rs` | `export_output_texture()` (= compositor output), `pq_encode_for_export()` (HDR), still-readback submit/take, clip-texture downcast chase, poster/filmstrip capture into the clip atlas. |
| `journey_proof.rs` | Headless harness that drives the real `run_export` (feature `journey-proofs`). |

## 3. Video export, end to end

1. **Start (UI thread):** menu `ExportVideo` → `start_export` → native save dialog →
   `ExportConfig` from `ProjectSettings` (`output_width/height`, `frame_rate`,
   `export_hdr`) + `Timeline::export_in/out_beat` → `ContentCommand::StartExport`.
2. **Content thread enters export mode** — `run_export` *replaces* the normal per-frame
   loop until done. UI keeps rendering; it receives `ContentState` snapshots with
   `export_progress`/`export_status` every 10 frames (the BUG-083 fields).
3. **Range & audio:** beat range falls back to the content range when unset, errors if
   start ≥ end. Audio mixdown is rendered up front to a temp WAV via
   `manifold_playback::audio_mixdown::render_export_audio` — the `audio_path` in the
   incoming config is unconditionally replaced by this internal mixdown.
4. **Session:** `ExportSession::new_with_device` shares the content pipeline's Metal
   device (avoids cross-device sync). Frame count = `round((end−start)·fps)`. Audio
   offset = audio start − range start − encoder delay.
5. **Warmup:** up to 120 engine ticks so decoders for clips at the in-point are ready.
6. **Per frame** (`export_one_frame`): tick engine at fixed `1/fps` delta →
   `flush_pending_decodes()` (**blocking** until no clip has `decode_pending`) →
   `render_content` → `flush_all_background_work()` (async effect workers) → grab the
   raw `id<MTLTexture>` pointer — SDR: compositor output; HDR:
   `pq_encode_for_export(200, 10000)` applies PQ first — → `wait_for_render_complete()`
   → `session.encode_frame(ptr)`. Same device, same process: **no readback, no
   IOSurface, no staging copy** on this path. The native side copies into a
   CVPixelBuffer-backed texture via a compute dispatch and `waitUntilCompleted` per
   frame, then appends with `CMTime(frame_index, fps_rounded_to_int)`.
7. **Cancellation:** the loop polls `cmd_rx` for `ContentCommand::CancelExport` — but
   **nothing in the UI sends it** (`content_command.rs` marks it `#[allow(dead_code)]`,
   "no UI producer yet"). A long export is uninterruptible today short of quitting.
8. **Finalize:** encoder writes the MP4 trailer (`finishWritingWithCompletionHandler`,
   30 s semaphore timeout). With audio: ffmpeg is resolved *now* (not at export start)
   and muxes WAV + video-only temp into the final path; on success the temp is deleted.
   On cancel/error the partial output and temp are removed; the mixdown WAV is always
   removed; transport state (position/playing) is restored regardless of outcome.

## 4. Still export

A genuinely different mechanism — the only CPU readback in the pipeline. `ExportFrame`
command → next tick, `submit_still_readback()` blits the compositor output into a
CPU-visible staging buffer (`manifold_renderer::gpu_readback::ReadbackRequest`) → the tick
after, `take_still_readback()` yields packed f16 pixels → a detached named thread runs
`linear_f16_rgba_to_srgb8` (true sRGB, optional EDR rolloff) + `save_still` (PNG keeps
alpha; JPEG drops to RGB at hardcoded quality 95, chosen by file extension). Two-tick
submit/poll ordering is enforced by call-site placement in `content_thread.rs`.

## 5. Video playback decode

- **Lifecycle:** `start_clip` inserts an `ActiveVideoClip` (render target from the pool)
  and submits `Open` + `Prepare`. Results drain in `pre_render` each tick.
- **Threading contract:** 4 decode workers; all jobs for one clip hash to the same
  worker, whose local map owns the `DecoderHandle`. Frame pixels never cross threads —
  the worker sends back the raw handle pointer, and the *content thread* runs the
  NV12→RGBA compute into the clip's render target. This is sound only because
  `decode_pending` guarantees no job is in flight for that clip when the result is
  consumed. That flag is the load-bearing invariant of the whole design.
- **Pacing:** `pre_render` accumulates `dt · playback_rate` and submits `DecodeNext`
  once per source-frame interval; >2 intervals behind resets the accumulator, >3
  switches to a `Seek` (decoder catch-up by reader re-create). Seeks arriving while a
  decode is pending are coalesced to the latest target.
- **EOF:** looping clips seek to 0.0 (the *file* start, not the clip in-point — the
  engine's drift correction may re-seek afterwards; unverified, see §12); non-looping
  clips stop.
- **Prewarm:** engine lookahead candidates open+prepare decoders keyed by
  `video_clip_id` into a per-worker `warm` map. Warm handles are only ever evicted at
  shutdown, and a warm-open failure is logged but sends no result.
- **Posters/filmstrips (§24 5c):** parked clips get one-shot decodes under a prefixed
  key (`\u{1}poster\u{1}<id>`) with an isolated decoder and render target, so a poster
  can never write into a live clip's texture. Filmstrips walk one decoder across bar
  times; eviction (`evict_posters`) closes decoders and returns targets to the pool.
- **Resize:** active clips get fresh right-sized targets (`has_frame` drops until the
  next decoded frame lands); posters are dropped wholesale and re-requested.

## 6. Image clips

Still image on a video layer = "one-frame video": background thread decodes the file
once (full-res cached), aspect-fits + letterboxes into a canvas-sized RGBA8 buffer,
**premultiplies alpha** (compositor blends premultiplied; decode yields straight — the
multiply is in sRGB space, exact at α∈{0,1}), then the content thread uploads into an
`Rgba8UnormSrgb` texture (sampler converts to linear on read). Stale results from before
a resize are discarded by target-size tag. Resize re-fits from the cached decode without
touching disk; the old texture stays up until the new one lands (no black flash).

## 7. Color & precision contract

The compositor works in **linear** Rgba16Float; the display surface is
ExtendedLinearSRGB (the display applies the transfer function at scanout). Every path in
and out applies its own transfer function — and they do not all agree:

| Path | Transfer function | EDR (>1.0) handling |
|---|---|---|
| Display (live show) | true sRGB, at scanout | EDR passthrough (tonemap soft-clip per display peak) |
| Still export, faithful | true piecewise sRGB (`linear_f16_rgba_to_srgb8`) | hard clip at white |
| Still export, rolloff | true sRGB after tanh shoulder above 0.8 | compressed into SDR white |
| **SDR video export** | **`pow(1/2.2)` approximation** (encoder copy shader) | hard clip (BGRA8 write) |
| HDR video export | PQ (applied by `pq_encode_for_export`, 200/10000 nits), BT.2020 metadata | carried in PQ |
| **Video decode (playback)** | **BT.709 matrix + `pow(2.2)` approximation, hardcoded** | n/a (SDR sources) |

Decode and SDR-encode are mutually consistent (2.2 both ways), but both diverge from the
true sRGB used by stills and the display — same frame, three subtly different tones
(worst in shadows). Logged as BUG-128; the decoder matrix hardcode as BUG-131.

## 8. Config surface & validation

`ProjectSettings.output_width/height` (default 1920×1080, clamped ≥1, **no upper bound**),
`frame_rate` (default 60, clamped ≥1.0, fractional accepted but rounded to integer inside
the encoder — BUG-129), `export_hdr` (undoable command). Export range:
`Timeline::export_in/out_beat`, plain serde fields, validated only at export start.
`ExportConfig.start_beat/end_beat` are raw `f32`, not `Beats` (a tolerated edge of the
newtype invariant; the app side converts through `TempoMap` immediately). Still-export
quality is hardcoded (JPEG 95).

## 9. Threads & ownership

- Content thread: owns both renderers, the `ExportSession`, all result draining, and
  every `CopyFrameToTexture` dispatch. Export replaces its loop; recording rides inside
  it; the two are mutually exclusive per frame.
- 4 decode workers: own all `DecoderHandle`s (active + warm), all AVFoundation calls.
- Ad-hoc threads: image decode (`std::thread::spawn` per request), still-export encode
  (named, detached), video import probe (background).
- Both renderers cache `device_ptr: *const GpuDevice` re-pointed by
  `ContentThread::run()` — the exact BUG-054 / FOUNDATIONAL_GAPS A6 pattern, twice.
- ffmpeg mux: blocking subprocess **on the content thread** at finalize (acceptable
  offline; export mode has already suspended live rendering).

## 10. Test surface

- Crate-local: `still_exporter` (sRGB/rolloff/format edges) and `image_renderer`
  (fit/premultiply/letterbox) unit tests — CPU-only, in the default sweep.
- `journey-proofs` (manifold-app feature): 4 tests driving the **real** `run_export`
  headlessly, verifying via ffprobe/frame extraction that audio-reactive motion,
  save/reload survival, LFO motion, and determinism hold. Needs ffmpeg/ffprobe; not in
  the default sweep.
- **Dark:** no test exercises the decode/playback path (scheduler, VideoRenderer state
  machine, poster lifecycle) at any level; `gpu-proofs` is compositor-only and touches
  neither export nor decode; recording proofs live in the other crate. The
  decode-scheduler invariant (§5) is enforced by convention only.

## 11. Boundaries with neighbouring systems

- **Recording:** `manifold-recording` crate; per-frame GPU capture inline with the live
  loop; known-broken HDR (BUG-053); see LIVE_RECORDING_PROOFS_DESIGN.md.
- **Audio for export:** rendered by `manifold_playback::audio_mixdown`, not this crate;
  this crate only muxes the resulting WAV.
- **Future shape:** MEDIA_BACKEND_DESIGN.md (neutral traits, non-macOS encoders — the
  `ExportError::UnsupportedPlatform` variant is pre-allocated for it) and
  VIDEO_IO_DESIGN.md (Syphon/NDI live interchange) — both forward-looking, neither
  describes current behaviour.

## 12. Honest edges (the bug-hunt starts here)

Backlogged with ids:

1. **BUG-127 — decode worker silently drops jobs for missing handles; export can hang.**
   Worker `Prepare`/`Seek`/`DecodeNext` on an absent clip-id sends *no result*; app-side
   `decode_pending` stays true forever; `flush_pending_decodes` (called every export
   frame) blocks on `recv_results_blocking` until it clears. Failed-open file + one seek
   = wedged export, stalled clip in live playback.
2. **BUG-128 — SDR export gamma ≠ display/still gamma** (§7).
3. **BUG-129 — fractional fps silently rounds** to integer CMTime (23.976/29.97 exports
   get wrong frame timing and drift against the muxed audio).
4. **BUG-130 — ffmpeg resolved only at finalize**: an audio export renders every frame,
   then fails at the last step if ffmpeg is missing; the `.video_only.mp4` temp is left
   behind on mux *failure* (only the success path deletes it).
5. **BUG-131 — decoder hardcodes BT.709 video-range**: BT.601 (SD) and BT.2020 sources
   decode with a wrong matrix; full-range flags unchecked (VideoToolbox likely
   normalizes to the requested video-range NV12 — unverified).
6. **BUG-132 — nearest-neighbor scaling in the decode shader**: any source whose
   resolution ≠ canvas gets unfiltered scaling (blockiness up, shimmer down).
7. **BUG-133 — `SUPPORTED_EXTENSIONS` overpromises**: `.webm`/`.avi` pass the extension
   gate but AVFoundation generally can't open them; failure surfaces as a per-clip
   decode error later instead of at import.
8. No cancel-export UI (self-documented dead `CancelExport` variant) — a multi-minute
   render is uninterruptible; UX gap, release-relevant.
9. Loop restart seeks to file time 0.0, not the clip in-point (§5) — masked if engine
   drift-correction re-seeks; needs a runtime check with a trimmed looping clip.
10. `MetalEncoder_EndSession` returns OK if the 30 s finalize semaphore *times out*
    while the writer is still finishing — a very long finalize could report success
    before the trailer is written (unobserved; low likelihood).
11. `ExportSession::new` vs `new_with_device` duplicated logic; `new_with_device` also
    skips `is_available` checks — a refactor-hazard pair, not a live defect.
12. Warm decoder handles are never evicted (per-worker `warm` map grows for the life of
    the app; bounded by distinct video clips in the project, but never shrinks).
13. Adversarial-soak gap: nothing exercises export at real project scale
    (53 layers / 2928 clips) or long durations; journey proofs are seconds-long. The
    LIVE_RECORDING_PROOFS soak pattern is the template if export soak becomes a need.
