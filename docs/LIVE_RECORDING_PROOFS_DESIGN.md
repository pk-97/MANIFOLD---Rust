# Live Recording Proofs — headless end-to-end tests for the show recorder

**Status:** SHIPPED (P1+P2) 2026-07-10 — the recorder proof suite is built and on main. P1 @ `ef12c14b` (clock/audio injection seams; Tier-1 proof harness: tests 1–4,6, ffprobe oracle, 26-block pattern). P2 @ `091290e3` (`recording-soak` bin unpaced+realtime with a decoded-index PASS gate; kill-durability test 5; `docs/DEVELOPMENT_REFERENCE.md` runbook). **P3 (in-app record smoke) DEFERRED 2026-07-10 (Peter):** its intended vehicle does not exist — `cargo xtask ui-snap` renders the UI tree only, with no live content thread or compositor (a scripted record click emits `ContentCommand::StartLiveRecording` into a channel `ui_snapshot/script.rs:19` holds and never drains), so it cannot exercise the compositor-frame capture block. Building a real headless record smoke is a new content-thread+compositor integration harness (BUG-054-adjacent), not the "one scripted flow" the phase assumed; see §8 Deferred for the revival trigger. The button→command→capture-block glue is L4-verified by live use every show (VD-023). Two other debts carried: full-scale 4K60 20-min soak is Peter's pre-gig ritual (VD-022a); BUG-086 silent audio-drop fix (VD-022b, show severity LOW after `--realtime` gave full audio). Release-gating per STRUCTURAL_AUDIT_VERDICTS (owns BUG-053) · design 2026-07-07 · Fable · approved 2026-07-09 Peter
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, 2026-07-07: *"The last time I used it it failed multiple times and we eventually
moved to a fix that work, but I want a proper end to end headless test or something for
the future"* — *"so I don't spend hours re-recording shows because the recordings
failed."*

The instrument stakes are unusually direct here: live recording is how a show becomes
an artifact. Every failure class this system has actually produced cost a full take —
a re-staged, re-performed show. And all three historical failures were **long-duration,
cumulative-state failures inside or below AVAssetWriter** that no unit test and no
10-second smoke test would ever touch:

1. **Silent writer death at ~2:20** — AVAssetWriter transitioned to a non-Writing
   state, 17 minutes of frames dropped silently, file unrecoverable (no moov atom).
   Fixed by fragmented MOV + loud first-failure logging + resilient finalize
   (`2b621085`).
2. **HEVC hardware encoder malfunction at ~3 GB cumulative bitstream** under sustained
   4K60 (kVTVideoEncoderMalfunctionErr, upstream of AVAssetWriter). Fixed by moving
   SDR capture to ProRes 422 Proxy — a separate hardware encoder path (`b2e26a09`).
3. **-16364 duplicate PTS at ~7500 frames** — timescale 600 rounded two jittery
   content-thread frames to the same PTS; AVAssetWriter goes permanently Failed on a
   non-increasing PTS. Fixed by timescale 90000 + a monotonic clamp (`fbee1ed2`).

The design conclusion from that history: **the test must drive the real
`LiveRecordingSession` API into the real native AVAssetWriter encoder, at realistic
frame counts, with adversarial timing — and then interrogate the output file with an
independent tool.** "stop() returned Ok" was true during two of the three failures.

Two tiers fall out:

- **Tier 1 — proof harness** (`cargo test -p manifold-recording --features
  recording-proofs`): fast, deterministic, headless. Regression fence for the failure
  classes we've already paid for. Runs whenever recording code is touched.
- **Tier 2 — pre-gig soak** (`recording-soak` bin): the actual show configuration
  (4K60 ProRes + audio), a full take's worth of frames, verified file at the end.
  This is the **soundcheck for the recorder** — run on the rig before a gig, exactly
  like line-checking the LEDs. It is the only tier that can catch class-2 failures
  (real hardware encoder, real cumulative gigabytes).

Binding constraints (per DESIGN_AUTHORING §1): the capture block is per-frame on the
content thread ([content_pipeline.rs:2543-2572](../crates/manifold-app/src/content_pipeline.rs)),
so the injection seams this design adds must be zero-cost to the production path — they
are (one delegating call, one enum match at construction). Thread residency is settled
(recording thread owns all encoding; the harness replicates the content-thread role).
Time model: recording is wall-clock `Seconds` domain at the edge — correct per the
invariant, unchanged. No persistence. The performance surface is the record button
itself; its failure mode *is* the stage failure this design exists to prevent.

---

## 1. Audit — what exists (verified 2026-07-07)

| Piece | Where | State |
|---|---|---|
| `LiveRecordingSession` — app-facing API: `new` / `acquire_texture` / `pool_texture` / `encode_format_conversion` / `submit_frame` / `stop` | [session.rs](../crates/manifold-recording/src/session.rs) | Shipped, live-proven post-fixes. **Zero tests in the crate.** |
| `submit_frame` stamps `Instant::now()` internally | [session.rs:214](../crates/manifold-recording/src/session.rs#L214) | The reason adversarial timing is currently untestable — seam target |
| Recording thread: drains frames + audio ring, waits `GpuFence`, computes elapsed from `wall_timestamp - start_time`, calls FFI | [recording_thread.rs:119-126](../crates/manifold-recording/src/recording_thread.rs#L119) | Shipped |
| `GpuFence` condvar (signal from GPU completion handler) | [recording_thread.rs:23-59](../crates/manifold-recording/src/recording_thread.rs#L23) | Shipped |
| Native encoder: AVAssetWriter, ProRes 422 Proxy SDR / HEVC Main10 HDR, timescale-90000 + monotonic PTS clamp, fragmented MOV (5s), resilient finalize | [LiveRecordingPlugin.m](../crates/manifold-recording/native/LiveRecordingPlugin.m) (clamp at 455-484, finalize at 655-749) | Shipped — the three historical fixes live here |
| `TextureRingPool` (8 slots, `Bgra8Unorm`, non-blocking acquire, drop on exhaustion) | [texture_pool.rs](../crates/manifold-recording/src/texture_pool.rs) | Shipped |
| `FormatConverter` (Rgba16Float → sRGB Bgra8Unorm compute, WGSL in-crate) | [format_converter.rs](../crates/manifold-recording/src/format_converter.rs), [shaders/linear_to_srgb.wgsl](../crates/manifold-recording/src/shaders/linear_to_srgb.wgsl) | Shipped — precedent for the harness's pattern shader |
| App integration: acquire → convert → submit in the compositor command buffer; fence signaled in `add_completed_handler` | [content_pipeline.rs:2547-2621](../crates/manifold-app/src/content_pipeline.rs#L2547) | Shipped — the exact sequence the harness replicates |
| Start/stop command surface | [content_commands.rs:823-861](../crates/manifold-app/src/content_commands.rs#L823), record button at [app_render.rs:1257-1260](../crates/manifold-app/src/app_render.rs#L1257) | Shipped |
| `AudioConsumer = ringbuf::HeapCons<f32>` — a plain heap ring-buffer consumer | [capture/mod.rs:34](../crates/manifold-audio/src/capture/mod.rs#L34) | Trivially injectable; session only constructs it from a device name today ([session.rs:66-91](../crates/manifold-recording/src/session.rs#L66)) |
| Feature-gated GPU test precedent (`gpu-proofs`: gated `test_device()`, `[[test]] required-features`) | [manifold-renderer/Cargo.toml:56-69](../crates/manifold-renderer/Cargo.toml#L56) | The pattern Tier 1 mirrors |
| UI flow driver (L3): resolve widget by name, click, assert | `scripts/ui-flows/select-and-inspect.json` + `cargo xtask ui-snap` | Shipped (UI_AUTOMATION P1–P2) — P3's vehicle |
| ffprobe / ffmpeg | `/opt/homebrew/bin/` | Present on the rig |

Classification: the recording pipeline **exists and is live-proven**; the injection
seams are **one wire away from existing** (a parameter added to a call, an enum at a
constructor); genuinely new are only the pattern shader, the ffprobe oracle, the test
bodies, and the soak bin. Extend, don't redesign.

**Audit finding — HDR live recording is structurally broken (statically derived, not
yet observed).** The pool is unconditionally `Bgra8Unorm`
([session.rs:60](../crates/manifold-recording/src/session.rs#L60)) while the native HDR
path wraps its CVPixelBuffer as `RGBA16Float` and blits pool → buffer
([LiveRecordingPlugin.m:378](../crates/manifold-recording/native/LiveRecordingPlugin.m#L378));
Metal forbids format-mismatched blits (4-byte vs 8-byte texel). Additionally nothing in
the pipeline performs PQ encoding, which the HDR writer config declares. The UI never
sets `hdr: true` ([app_render.rs:1257](../crates/manifold-app/src/app_render.rs#L1257)
uses `default_to_desktop()`, hdr=false), so shows are unaffected today. Logged as
**BUG-053**; see D7.

## 2. Decisions

**D1 — Clock injection: `submit_frame_at(…, elapsed: Duration)`; the frame carries
`Duration`, not `Instant`.** `RecordingFrame.wall_timestamp: Instant` becomes
`elapsed: Duration`, computed at submit time. `submit_frame` keeps its exact signature
and delegates with `self.start_time.elapsed()` — the app call site
([content_pipeline.rs:2561](../crates/manifold-app/src/content_pipeline.rs#L2561)) is
untouched. The harness calls `submit_frame_at` with fabricated values. This makes the
-16364 class reproducible in milliseconds instead of at minute 2:10 of a real take.
Rejected: leaving `Instant::now()` in place and pacing tests in real time — a
two-minute bug then needs two minutes per repro, and duplicate-PTS can't be forced at
all. Rejected: a test-only `#[cfg]` fork of `submit_frame` — parallel paths are the
forbidden move; production and test go through the same `submit_frame_at`.

**D2 — Audio injection: an `AudioFeed` enum on a new constructor; `LiveRecordingConfig`
unchanged.** `AudioConsumer` is neither `Clone` nor `Debug`, and the config crosses
`ContentCommand`, so the feed cannot live in the config. `new()` maps
`config.audio_device` to `AudioFeed::Device`/`None` and delegates to
`new_with_audio_feed`. The harness passes `AudioFeed::Injected` with a ring buffer it
holds the producer for — headless, no device, no CoreAudio dependency in tests.
Rejected: requiring a real input device (BlackHole) in tests — non-deterministic,
absent on CI-shaped machines, and it tests the device layer, not the recorder.

**D3 — The verification oracle is ffprobe/ffmpeg as a subprocess, hard-required.**
The referee must be independent of our code and must represent the ecosystem the takes
are edited in (Resolve, FCP, QuickTime all agree with ffprobe far more than with any
in-house reader). The harness errors loudly if ffprobe is missing (with the brew
install hint) — no silent skip, per `feedback_no_silent_fallbacks_or_interim_stopgaps`.
Rejected: `manifold-media`'s decoder as the oracle — a same-vendor blind spot; our
decoder tolerating our encoder proves nothing about what an NLE will accept.

**D4 — Frame identity is proven by a block-code pattern run through the real
conversion shader.** Each frame's index is baked into its pixels (spec in §4), the
frame is written into an `Rgba16Float` source texture, and the harness then runs the
**production** `encode_format_conversion` into the pool texture — so the real WGSL
conversion, the real pool, the real fence path, and the real FFI encode are all in the
loop. Decoding the file and reading indices back detects drops, duplicates, and
reordering exactly — a green "N frames encoded" counter cannot. Rejected: comparing
decoded pixels against rendered PNGs — ProRes is lossy and this tests codec fidelity,
which is not the failure class; identity bits survive, pixel equality doesn't.

**D5 — Own cargo feature `recording-proofs`, mirroring `gpu-proofs`.** Gates a
`proofs` support module in-crate (pattern shader + oracle, the `test_device()`
precedent), the `tests/recording_proofs.rs` integration target
(`required-features`), and the soak bin. The default workspace sweep stays free of
GPU/AVFoundation work. `serde_json` (ffprobe JSON) enters as an optional dependency
tied to the feature. Rejected: reusing the `gpu-proofs` feature name — different
crate, and a distinct name keeps "run the recording proofs" a deliberate, documented
act like the GPU suite.

**D6 — The soak is a manual pre-gig ritual, not a scheduled job.** A bin target, run
by Peter (or an orchestrating session on request) on the rig before a show. It writes
~17.5 GB and wants the machine otherwise idle — cron'ing that is cost without a
trigger; the value of a soak is *proximity to the gig*, not nightliness. Rejected:
scheduled/overnight automation, until there's a dedicated rig-check machine (revival
trigger in Deferred).

**D7 — HDR live recording: deferred behind BUG-053; the harness refuses it loudly.**
The SDR path is the show path (UI can't reach HDR). Fixing HDR is real feature work
(pool format must follow `config.hdr`, PQ encode must exist somewhere), not harness
work — widening this design to include it would be the cascade-redesign move. The
harness's HDR variant is written *as the acceptance test* for that future fix but the
soak's `--hdr` flag exits with an explicit "blocked by BUG-053" message until then.
Rejected: silently testing SDR when asked for HDR.

**D8 — Tests pace by pool availability, not wall clock.** The harness submits as fast
as slots free up (spin on `acquire_texture` with a 200µs sleep — fine in a test, banned
on the content thread). Timestamps are fabricated on a perfect 60fps grid regardless of
real submission speed. Consequence, stated honestly: unpaced submission exercises the
encoder *harder* than a show does (no idle time between frames), which is the right
direction for a stress fence, but it means Tier 1 does not measure real-time keep-up —
that's the soak's `--realtime` flag and the render-trace gate's territory.

## 3. Seam brief — the two injection points (per DESIGN_DOC_STANDARD §6)

### 3.1 Clock seam

Old ([session.rs:200-236](../crates/manifold-recording/src/session.rs#L200),
[recording_thread.rs:62-71,119-126](../crates/manifold-recording/src/recording_thread.rs#L62)):

```rust
pub fn submit_frame(&mut self, pool_slot: PoolSlot, fence: Arc<GpuFence>)
// RecordingFrame { pool_slot, wall_timestamp: Instant::now(), gpu_complete }
// thread: elapsed = frame.wall_timestamp.duration_since(start_time).as_secs_f64()
```

New:

```rust
pub fn submit_frame(&mut self, pool_slot: PoolSlot, fence: Arc<GpuFence>) {
    let elapsed = self.start_time.elapsed();
    self.submit_frame_at(pool_slot, fence, elapsed)
}
/// Test/harness entry: submit with an explicit elapsed-since-start timestamp.
pub fn submit_frame_at(&mut self, pool_slot: PoolSlot, fence: Arc<GpuFence>, elapsed: Duration)
// RecordingFrame { pool_slot, elapsed: Duration, gpu_complete }
// thread: let elapsed = frame.elapsed.as_secs_f64();
```

`recording_thread::run` loses its `start_time` parameter (it only existed to compute
elapsed; `drain_audio`'s `_start_time` is already dead — delete both). The session
keeps its own `start_time` for `stop()`'s duration report. Call-site inventory
(re-derive with `rg -n 'submit_frame|wall_timestamp|start_time' crates/manifold-recording/src crates/manifold-app/src`):
exactly one production caller of `submit_frame`
([content_pipeline.rs:2561](../crates/manifold-app/src/content_pipeline.rs#L2561),
unchanged), one constructor of `RecordingFrame` (session.rs), one consumer
(recording_thread.rs). If the count differs at execution time, stop and list the new
sites first.

### 3.2 Audio seam

```rust
/// Where the recording session gets its audio.
pub enum AudioFeed {
    /// Open a capture device by name (the production path — today's config.audio_device).
    Device(String),
    /// Pre-built ring-buffer consumer (harness / future routing). Session does not own a device.
    Injected { consumer: manifold_audio::capture::AudioConsumer, sample_rate: u32, channels: u16 },
    /// Video only.
    None,
}

pub fn new(config, device, w, h, fps) -> Result<Self, String>            // unchanged signature; maps
                                                                          // config.audio_device → Device/None, delegates
pub fn new_with_audio_feed(config, device, w, h, fps, feed: AudioFeed) -> Result<Self, String>
```

The device-open block currently inlined in `new`
([session.rs:66-91](../crates/manifold-recording/src/session.rs#L66)) moves verbatim
into the `Device` arm. Behavior identical for the app. Deletion gate: no second
device-open path remains (`rg 'AudioCaptureDevice::new' crates/manifold-recording/` →
exactly 1 hit).

## 4. The proof harness (Tier 1)

Lives in `crates/manifold-recording`: a feature-gated `src/proofs.rs` support module
(pattern writer + oracle; shape like `test_device()` gating at
[manifold-renderer/src/lib.rs:77](../crates/manifold-renderer/src/lib.rs#L77)) and the
integration test `tests/recording_proofs.rs` (`required-features =
["recording-proofs"]`, shape like the renderer's `gpu_proofs` target at
[manifold-renderer/Cargo.toml:56-62](../crates/manifold-renderer/Cargo.toml#L56)).

**Harness frame loop** — the production sequence from
[content_pipeline.rs:2547-2621](../crates/manifold-app/src/content_pipeline.rs#L2547),
transcribed: acquire slot (D8 pacing) → dispatch pattern shader into an
`Rgba16Float` source texture → `encode_format_conversion(src, pool_tex)` (the real
shader) → `add_completed_handler(|| fence.signal())` → commit → `submit_frame_at`
with the scripted timestamp. GPU device: the crate's own headless `GpuDevice`
(⚠ VERIFY-AT-IMPL: the exact constructor — read how
`manifold-renderer/src/lib.rs test_device()` builds one and mirror it).

**Pattern spec (committed — the oracle depends on it).** Frame size 640×360. Two
sync blocks + 24 index bits, one row of 26 blocks, each ~24×64 px (block width =
width/26, full-height stripe is fine; solid luma). Block 0 = white, block 1 = black
(polarity + locator), blocks 2..26 = frame index MSB-first, white=1. Written by
`src/shaders/test_pattern.wgsl` (shape like
[linear_to_srgb.wgsl](../crates/manifold-recording/src/shaders/linear_to_srgb.wgsl)'s
dispatcher in [format_converter.rs](../crates/manifold-recording/src/format_converter.rs)).
Readback: decode with `ffmpeg -v error -i <file> -map 0:v:0 -f rawvideo -pix_fmt gray -`,
sample each block's center pixel, threshold at 128. Solid full-height luma stripes
survive ProRes 4:2:2 quantization with enormous margin.

**Oracle spec** (`src/proofs.rs`; interiors free, surface committed):

```rust
pub struct ProbeReport {
    pub codec: String, pub width: u32, pub height: u32,
    pub video_frame_count: u64, pub video_duration_s: f64,
    pub audio_duration_s: Option<f64>,
    pub pts: Vec<i64>,                 // video packet PTS, stream order
    pub frame_indices: Vec<u32>,       // decoded block-pattern indices, stream order
}
pub fn probe(path: &Path, decode_indices: bool) -> Result<ProbeReport, String>
```

Command shapes: stream metadata via `ffprobe -v error -show_streams -of json`; PTS via
`ffprobe -v error -select_streams v:0 -show_entries packet=pts -of csv`; indices via
the ffmpeg rawvideo decode above.

**Test inventory** (names committed; all in `tests/recording_proofs.rs`):

1. `nominal_video_only` — 600 frames on a perfect 60fps grid (elapsed_n = n·16 666 667 ns),
   640×360 SDR, no audio. Gates: `stop()` reports 600 recorded / 0 dropped; probe:
   codec `prores`, 600 frames, PTS strictly increasing, `video_duration_s` within
   ±50 ms of 10.0, `frame_indices == [0..600)` exactly (no gap, dupe, or reorder).
2. `nominal_with_audio` — same, plus `AudioFeed::Injected` at 48kHz stereo; the test
   pushes a 440 Hz sine in ~10 ms chunks interleaved with frame submissions, exactly
   960 000 frames' worth (10.0 s). Gates: everything in (1) plus
   `audio_duration_s` within ±50 ms of 10.0 and within ±100 ms of `video_duration_s`.
3. `adversarial_pts_survives` — the regression fence for failure class 3. 600-frame
   grid with scripted injections: n=100..110 duplicate n=99's timestamp exactly;
   n=200 jumps backwards 50 ms; n=300..302 use +5 µs deltas (below the 11 µs
   timescale resolution); n=400 stalls +2 s then resumes the grid. Gates: `stop()`
   reports 600 recorded (writer never left Writing — pre-`fbee1ed2` code dies at the
   first duplicate and bleeds frames, failing this); probe: 600 frames, PTS strictly
   increasing, and the n=400 gap is preserved in PTS (≈2 s, ±100 ms) — the clamp must
   never flatten real gaps, or audio sync dies (wall-clock-fidelity contract from
   `fbee1ed2`).
4. `pool_accounting_consistent` — submit 200 frames while artificially holding 4 of
   the 8 slots un-released for the first 100 (simulated slow encoder via a gated
   fence-signal); gate: `frames_recorded + frames_dropped == frames_submitted_total`,
   no panic, file valid per probe with `frame_indices` strictly increasing (gaps
   allowed and expected — dropped frames leave PTS gaps, never corruption).
5. `kill_mid_take_leaves_recoverable_file` — failure class 1's durability proof.
   Spawn the soak bin (`env!("CARGO_BIN_EXE_recording-soak")`) at 1280×720, unpaced,
   large frame budget, scratch output; poll until the file exceeds 30 MB (timeout
   60 s), SIGKILL the child, then probe: ffprobe opens the file, ≥1 video frame
   readable, PTS of what's there strictly increasing. (Fragment flush cadence is
   media-time driven — derived, not observed; the gate is deliberately only
   "readable content survives a hard kill".)
6. `hdr_blocked_by_bug_053` — constructs an HDR config and asserts session creation
   (or first encode) fails **loudly**. When BUG-053 is fixed, this test is replaced
   by an HDR twin of (1) — that replacement is BUG-053's acceptance test.

Expected wall time for the suite: well under a minute after build (600 tiny ProRes
frames encode in seconds; the kill test dominates at ~10–20 s).

## 5. The pre-gig soak (Tier 2)

`crates/manifold-recording/src/bin/recording_soak.rs`, `required-features =
["recording-proofs"]`.

```
recording-soak [--width 3840] [--height 2160] [--fps 60] [--minutes 20]
               [--no-audio] [--realtime] [--keep] [--output <path>] [--hdr]
```

Defaults are the show configuration: 4K60 SDR ProRes, 20 media-minutes
(72 000 frames, ≈17.5 GB — past every historically observed failure threshold),
synthetic 48k stereo audio via `AudioFeed::Injected`, unpaced (D8; encodes a 20-minute
take in however long the hardware takes, stressing the encoder at 100% duty),
timestamps on the perfect grid. `--realtime` paces submissions to wall clock for a
true dress rehearsal. Output defaults to a temp path and is deleted on PASS unless
`--keep`. Pre-flight: check free disk ≥ 1.5× the estimated file size; abort loudly if
not. `--hdr` exits immediately with `blocked by BUG-053` (D7).

End of run: full probe (§4 oracle, including index decode) and exactly one line —

```
SOAK PASS: 72000 frames, 0 dropped, PTS monotonic, gap-free indices, 17.4 GB, audio 1200.0s
SOAK FAIL: <first failed check, with numbers>
```

exit code 0/1. Gate for the default (unpaced) mode: 0 drops, full index sequence.
`--realtime` reports drops but gates file validity only (keep-up on a loaded rig is
the render-trace gate's job, not the soak's).

**The ritual, stated for the record:** the day before a gig, on the rig, run
`cargo run --release -p manifold-recording --features recording-proofs --bin
recording-soak`. PASS means the recorder survives a full take on this machine, this
OS build, this disk. macOS updates have already changed encoder behavior once
(failure class 2 appeared without a MANIFOLD change being involved in the threshold);
this is the instrument-check that catches the next one before it costs a show.

## 6. Phasing

### P1 — Seams + oracle + proof suite (one session) — ✅ SHIPPED 2026-07-10 @ `ef12c14b`

- **Entry state:** clean main; `cargo test -p manifold-recording` passes (trivially —
  zero tests); ffprobe present (`which ffprobe`). Re-verify anchors:
  [session.rs:214](../crates/manifold-recording/src/session.rs#L214) still stamps
  `Instant::now()`; the §3.1 call-site inventory count still holds.
- **Read-back:** this doc §2–§4 whole; [format_converter.rs](../crates/manifold-recording/src/format_converter.rs)
  end-to-end; [content_pipeline.rs:2543-2621](../crates/manifold-app/src/content_pipeline.rs#L2543);
  the `gpu-proofs` wiring in [manifold-renderer/Cargo.toml:56-69](../crates/manifold-renderer/Cargo.toml#L56).
  Restate the binding decisions (D1–D5, D8), the forbidden moves, and the entry-check
  results before writing code.
- **Deliverables:** §3 seams exactly as committed (`submit_frame_at`, `AudioFeed`,
  `new_with_audio_feed`, `RecordingFrame.elapsed`); `recording-proofs` feature +
  optional `serde_json`; `src/proofs.rs` (pattern writer + `probe`);
  `src/shaders/test_pattern.wgsl`; `tests/recording_proofs.rs` with tests 1–4 and 6
  from §4 (test 5 is P2's); fix the stale "MP4" doc comment on `RecordingResult`
  ([config.rs:49-51](../crates/manifold-recording/src/config.rs#L49)) — listed here so
  the scope fence licenses it; BUG-053 backlog entry cross-linked to test 6.
- **Gate (positive):** `cargo test -p manifold-recording --features recording-proofs`
  — all green, run twice consecutively (flake check); `cargo clippy --workspace -- -D
  warnings`; `cargo test -p manifold-app --lib` (the one production caller compiles
  and its tests pass).
- **Gate (negative):** `rg 'Instant::now' crates/manifold-recording/src/recording_thread.rs`
  → 0 hits (clock fully injected at the session boundary);
  `rg 'Arc<Mutex|Arc<RwLock' crates/manifold-recording/src/` → no new hits vs. main
  (GpuFence's existing pair is the only one);
  `rg 'AudioCaptureDevice::new' crates/manifold-recording/` → exactly 1;
  `cargo test -p manifold-recording` (no features) → compiles, runs zero proof tests.
- **Acceptance demo (L2):** run the suite; paste the `nominal_with_audio` probe
  summary (frame count, PTS-monotonic verdict, durations) and `ffprobe <kept file>`
  output into the phase report; keep one output .mov for the landing session to open.
- **Forbidden moves:** a `#[cfg(test)]` fork of `submit_frame` (D1 — one path);
  skipping ffprobe when absent (D3 — hard error); asserting pixel equality instead of
  index bits (D4); pacing tests by `sleep(16ms)` wall clock (D8); touching
  `content_pipeline.rs` beyond zero lines (the seams keep the app call site
  byte-identical — if it needs an edit, the seam is wrong: escalate).
- **Test scope:** focused (`-p manifold-recording` with and without the feature +
  `-p manifold-app --lib`); no workspace sweep (blast radius is one crate + one
  signature-stable caller).

### P2 — Kill test + soak bin + runbook (one session) — ✅ SHIPPED 2026-07-10 @ `091290e3`

- **Entry state:** P1 landed; its gates re-run green.
- **Read-back:** this doc §4 item 5 + §5; P1's phase report.
- **Deliverables:** `src/bin/recording_soak.rs` per §5; test 5
  (`kill_mid_take_leaves_recoverable_file`); a "Recorder soundcheck" section in
  `docs/DEVELOPMENT_REFERENCE.md` with the §5 ritual command verbatim.
- **Gate (positive):** full proof suite green twice; one short soak executed by the
  landing session — `recording-soak --width 1920 --height 1080 --minutes 2 --keep` —
  exits 0, `SOAK PASS` line pasted verbatim, and the landing session opens the kept
  .mov in QuickTime (or `ffplay`) and confirms the block pattern is visibly
  advancing. **The full 4K60 20-minute soak is Peter's first pre-gig run, not a
  landing gate** — it needs the rig, and its first execution is deliberately the
  ritual itself (VD entry at landing for "full-scale soak unexecuted").
- **Gate (negative):** `rg 'unwrap\(\)' crates/manifold-recording/src/bin/` → 0 hits
  (a soak that panics instead of printing `SOAK FAIL` is a broken instrument check);
  soak with `--hdr` exits non-zero mentioning BUG-053.
- **Acceptance demo (L2):** the short-soak PASS line + the opened .mov.
- **Forbidden moves:** deleting the output on FAIL (the failed file is the evidence —
  keep it and print the path); a progress bar or any per-frame stdout in the hot
  submit loop; gating `--realtime` on drops (D8's consequence — file validity only).
- **Test scope:** focused, as P1; plus the single end-of-phase
  `cargo clippy --workspace -- -D warnings`.

### P3 — In-app record smoke, L3 (one session) — ⏸ DEFERRED 2026-07-10 (Peter)

> **Pre-flight result 2026-07-10 (orchestrator, before briefing any worker):** entry-state
> check (a) FAILED. `cargo xtask ui-snap` (dispatched via `manifold-app/src/ui_snapshot/`)
> renders the real UI *tree* to a PNG but runs **no** content thread, `ContentPipeline`, or
> compositor frame — `ui_snapshot/mod.rs` builds a `UIRoot` + fixture `Project`/`ContentState`
> and paints it; the `graph`/`editor` scenes even document "no content thread or running chain
> is needed." The script driver (`ui_snapshot/script.rs:19,152`) holds a `content_tx` whose
> receiver "it holds and never drains — `ContentCommand::send` only logs." So a scripted record
> click produces `ContentCommand::StartLiveRecording` that goes nowhere: nothing records, the
> compositor-frame capture block at `content_pipeline.rs:2547` never runs. **The phase's vehicle
> does not exist.** Per this phase's own entry-state instruction, escalated to Peter, who chose
> to defer (drop from the 2026-07-10 wave). Not built. See §8 Deferred for the revival trigger;
> the residual coverage gap is logged as VD-023.

The tiers above start at the `LiveRecordingSession` API; the remaining unexercised
glue is the capture block inside the real compositor frame and the start/stop command
path. The design assumed one scripted `ui-snap` flow could close it — the pre-flight above
found that assumption false (ui-snap has no live compositor). The brief below is retained for
the record and as the starting point for whoever revives this per §8.

- **Entry state:** P2 landed. ⚠ VERIFY-AT-IMPL, both before briefing: (a) `cargo
  xtask ui-snap` scenes run the real content thread + compositor frame (read the
  xtask entry point — if ui-snap renders UI panels without a live compositor, this
  phase's vehicle doesn't exist: **escalate to Peter**, options being an app-level
  smoke script or dropping P3); (b) the record control at
  [app_render.rs:1257](../crates/manifold-app/src/app_render.rs#L1257) is addressable
  by the flow driver's name/text resolution (if unnamed, adding the name is in scope —
  UI_AUTOMATION P1's name storage is the precedent).
- **Read-back:** `scripts/ui-flows/select-and-inspect.json` + the flow-driver docs;
  this doc §1's app-integration anchors.
- **Deliverables:** `scripts/ui-flows/record-smoke.json` — click record, let ~120
  frames pass, click stop; a wrapper (xtask step or script) that then runs the §4
  `probe` on the produced file and asserts ≥100 frames, PTS monotonic, prores codec.
- **Gate:** the flow passes from a clean checkout twice consecutively; probe
  assertions green; the produced .mov path printed.
- **Acceptance demo (L3):** the flow run itself; report carries the probe summary.
- **Forbidden moves:** driving `ContentCommand::StartLiveRecording` directly from the
  harness (that's L1 dressed as L3 — the point is the real input path through the real
  button); asserting only "file exists".
- **Test scope:** the flow + focused `-p manifold-recording --features
  recording-proofs`; final phase of the design → one full
  `cargo clippy --workspace -- -D warnings` + default `cargo test --workspace`.

Phasing-completeness check (standard §5): every §4 test and the §5 bin appear in
exactly one phase; the §5 ritual doc lands in P2; the HDR twin of test 1 is Deferred
(BUG-053), not a phase; the full-scale soak's first execution is explicitly Peter's
ritual with a VD entry — no body-committed affordance is unowned.

## 7. Decided — do not reopen

1. Timestamps are injected at the session boundary (`submit_frame_at`); no test-only
   forks of the production path (D1).
2. Audio is injected as an `AudioFeed` enum on the constructor; the config type and
   the app call sites do not change (D2).
3. ffprobe/ffmpeg is the oracle, hard-required, no silent skip (D3).
4. Frame identity via the 26-block luma code through the real conversion shader;
   never pixel-equality against a reference render (D4).
5. Feature name is `recording-proofs`, mirroring the `gpu-proofs` wiring (D5).
6. The soak is a manual pre-gig ritual bin, not a cron job (D6 — Peter delegated the
   call 2026-07-07; revive per Deferred trigger only).
7. HDR live recording is BUG-053, deferred; the harness refuses `--hdr` loudly (D7).
8. Tier-1 pacing is pool-availability, never wall clock; nominal tests gate on zero
   drops and a gap-free index sequence (D8).
9. The adversarial test must assert the n=400 stall's PTS **gap is preserved** — the
   monotonic clamp exists to kill duplicates, not to flatten real time (wall-clock
   fidelity is what keeps audio in sync; `fbee1ed2`'s contract).

## 8. Deferred

- **P3 — in-app record smoke (L3)** — deferred 2026-07-10 (Peter's call) because its assumed
  vehicle (`ui-snap`) has no live compositor (see the P3 phase note). Reviving it means building
  a genuine headless integration harness that boots a real content thread + `ContentPipeline` +
  compositor, drives `StartLiveRecording` through the real command channel, runs enough frames
  that the `content_pipeline.rs:2547` capture block submits to the recorder, stops, and probes
  the file with the §4 oracle. **Revival trigger:** either such a harness already exists (the
  `run_export`/`journey_proof` headless path is the closest precedent, but it drives the export
  render path, not the live capture block), or a regression in the record-button→command→capture
  wiring is observed that P1/P2's API-level tests can't catch. Any revival MUST handle BUG-054
  (renderer device-pointer repoint required when constructing a `ContentThread` outside
  `Application::resumed`). Until then the glue is covered at L4 by live stage use (VD-023).
- **HDR proof twin of `nominal_video_only`** — revived by BUG-053's fix; test 6 is
  its placeholder and the fix's acceptance test.
- **Deep A/V alignment (click-track vs. flash-frame correlation in the decoded
  file)** — durations-match is the v1 gate; revive the first time Peter observes
  audible drift in a real take.
- **Scheduled soak automation** — revive if a dedicated rig-check machine exists or
  a macOS CI runner with Metal + AVFoundation appears; same trigger revives running
  Tier 1 in CI.
- **Backpressure/starvation shaping beyond test 4** (e.g. scripted encoder-stall
  injection via the FFI) — revive if a real take ever fails in a way tests 1–5 and
  the soak all miss; the escape analysis (standard §10) would name the missing
  stage.
