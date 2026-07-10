# Landing report — LIVE_RECORDING_PROOFS

**Design:** `docs/LIVE_RECORDING_PROOFS_DESIGN.md` · **Orchestrator:** Opus · **Date:** 2026-07-10
**Model of record:** phases built by Sonnet workers in isolated worktrees; gates re-judged and landed by the orchestrator.

Release-gating proof suite for the live show recorder (owns BUG-053; ranked #2 in
DESIGN_BUILD_ORDER §3 item 13h). Three phases: P1 seams + Tier-1 proof harness · P2 kill
test + soak bin + runbook · P3 in-app record smoke (L3).

---

## P1 — Seams + oracle + proof suite — ✅ SHIPPED @ `ef12c14b`

**Merge:** `ef12c14b` (merge of `lane/recording-proofs-p1`, worker commit `b29bb4c9` +
pre-land merge of `origin/main` `23d78699`). **Level reached: L2** (target L2 — met, no debt).

### What landed (file anchors)
- **Clock seam (D1):** `submit_frame_at(pool_slot, fence, elapsed: Duration)` added; `submit_frame`
  keeps its signature and delegates with `self.start_time.elapsed()`
  (`crates/manifold-recording/src/session.rs:243,255`). `RecordingFrame.wall_timestamp: Instant`
  → `elapsed: Duration` (`recording_thread.rs:69`); `run`/`encode_frame` lose the `start_time`
  param, dead `_start_time` on `drain_audio` deleted (`recording_thread.rs:83,102,224`).
- **Audio seam (D2):** `AudioFeed` enum (`Device`/`Injected`/`None`) on new
  `new_with_audio_feed`; `new` maps `config.audio_device` and delegates
  (`session.rs:49,69,86`). Device-open block moved verbatim into the `Device` arm
  (`AudioCaptureDevice::new` exactly 1 hit, `session.rs:109`).
- **Feature + oracle (D3/D5):** `recording-proofs` cargo feature + optional `serde_json`
  (`Cargo.toml`); `src/proofs.rs` (355 lines) — headless `test_device()` mirroring
  manifold-renderer, `PatternWriter`, `ProbeReport` + `probe()` (ffprobe JSON + PTS CSV +
  ffmpeg rawvideo `-fps_mode passthrough` index decode; ffprobe/ffmpeg hard-required with
  brew hint).
- **Pattern (D4):** `src/shaders/test_pattern.wgsl` — 26-block full-height luma code, 640×360.
- **Tests:** `tests/recording_proofs.rs` (532 lines) — `nominal_video_only`,
  `nominal_with_audio`, `adversarial_pts_survives`, `pool_accounting_consistent`,
  `hdr_blocked_by_bug_053` (test 5 correctly deferred to P2).
- **Scope-fenced fix:** stale "MP4" doc comment on `RecordingResult` corrected
  (`config.rs:50-51`).
- `content_pipeline.rs` diff vs base = **0 lines** (app call site byte-identical, verified
  `git diff 7f3fd9c3 ef12c14b -- content_pipeline.rs` → empty).

### Gate output (re-run by orchestrator in the worktree, post-merge of origin/main)
```
cargo test -p manifold-recording --features recording-proofs
  nominal_video_only ... ok      (Finalized: 600 frames, 0 PTS clamps)
  hdr_blocked_by_bug_053 ... ok  (Blit error MTLCommandBufferErrorDomain Code=3 GPU Address
                                  Fault → Finalized: 0 frames — loud block, caught cleanly)
  test result: ok. 5 passed; 0 failed; 0 ignored; finished in 1.98s
  (worker also ran it twice + a third time — 5/5 each)

cargo clippy --workspace --features manifold-recording/recording-proofs -- -D warnings
  Finished dev profile — 0 Rust warnings
  (only warnings: pre-existing ObjC `tracksWithMediaType:` deprecations in manifold-media
   native code — C-compiler warnings, not Rust lints, do not trip -D warnings)

nominal_with_audio probe: 600 video frames, PTS strictly increasing,
  video_duration_s=10.000, audio_duration_s=10.000

manifold-app: no [lib] target (bin-only crate) — brief's `--lib` command N/A; substituted
  `cargo build -p manifold-app` (Finished clean) + `cargo test -p manifold-app`
  (163 passed; 0 failed; 2 ignored, + 3 integration binaries green). Gate intent
  ("the one production caller compiles and its tests pass") satisfied.
```

### Negative gates (orchestrator-run)
```
rg 'Instant::now' recording_thread.rs                    → 0 hits ✓
rg 'Arc<Mutex|Arc<RwLock' manifold-recording/src/        → 0 hits ✓ (GpuFence uses bare Mutex<bool>)
rg 'AudioCaptureDevice::new' manifold-recording/         → exactly 1 (session.rs:109) ✓
cargo test -p manifold-recording (no features)           → compiles, 0 proof tests run ✓
```

### Acceptance demo (L2) — orchestrator opened it
Kept `.mov`s under the worktree's `target/proof-output/`. Orchestrator extracted frame 0
(near-all-black = index 0, all bits zero — correct) and frame 300 (distinct advancing
vertical white stripes encoding the index) from `nominal_video_only.mov` and read both PNGs.
Combined with the in-test assertion `frame_indices == [0..600)` exact through the real ProRes
encode, frame identity is proven. `hdr_blocked_by_bug_053.mov` is 0 bytes (correct — BUG-053
blocks it loudly).

### Bug found → logged (not a regression)
**BUG-085** (`recording-frames-recorded-overstates-async-append-drops`) — `frames_recorded`
counts the synchronous return of `LiveRecorder_EncodeVideoFrame`, but the actual
`appendPixelBuffer:` runs async on `appendQueue` and can silently drop under real
VideoToolbox backpressure (`isReadyForMoreMediaData == false`) with no counter Rust can see.
Found via `pool_accounting_consistent`'s artificial backpressure (107 counted vs 106 real
packets once). MED accounting / LOW practical likelihood. The test's Rust-vs-file cross-check
is `<=` (guards the dangerous direction — more packets in file than counted = corruption —
while tolerating the async-drop direction); the design's committed accounting identity
`frames_recorded + frames_dropped == frames_submitted_total` is still asserted with `==`.
**Implication for P2:** the soak's PASS gate must verify the decoded `frame_indices`
sequence from the file (an async drop shows as an index gap), not merely trust Rust's
`frames_recorded` counter.

### Shortcuts taken (from worker report, orchestrator-reviewed)
- `nominal_with_audio` pushes one audio chunk per video frame (800 sample-frames = exact 10.0s)
  rather than literal ~10ms chunks — exact, design gates on final durations only. Accepted.
- `probe()` adds `-fps_mode passthrough` to the ffmpeg decode (default CFR matching
  drops/dupes frames on non-nominal PTS spacing) — probe-side correctness, real gotcha for
  anyone extending the oracle. Accepted.
- `pool_accounting_consistent` uses bounded retry (25 × 200µs) not unbounded spin, to exercise
  recovery after the simulated stall lifts. Accepted.
- `hdr_blocked_by_bug_053` uses a dedicated `GpuDevice::new()` (isolates the format-mismatched
  blit's GPU fault from the shared test device). Accepted.

### Verification debt
None for P1 (target L2, reached L2). BUG-085 tracked in backlog, carried into P2's soak-gate
design note above.

### Status line after P1 (quoted)
> **Status:** IN PROGRESS 2026-07-10 — P1 SHIPPED @ `ef12c14b` (clock/audio injection seams
> `submit_frame_at`/`AudioFeed`; Tier-1 proof harness: tests 1–4,6, ffprobe oracle, 26-block
> pattern shader; found+logged BUG-085). P2 (kill test + soak bin + runbook) and P3 (in-app
> record smoke, L3) pending. Release-gating per STRUCTURAL_AUDIT_VERDICTS (owns BUG-053) ·
> design 2026-07-07 · Fable · approved 2026-07-09 Peter

---

## P2 — Kill test + soak bin + runbook — ✅ SHIPPED @ `091290e3`

**Merge:** `091290e3` (merge of `lane/recording-proofs-p2`, worker commit `3f4b38c4` + orchestrator
BUG-086 disambiguation `9b996af9` + pre-land merge of `origin/main` `38a84415`).
**Level reached: L2** (target L2 for the soak demo — met). Two verification debts carried (VD-024).

### What landed (file anchors)
- **`crates/manifold-recording/src/bin/recording_soak.rs`** (739 lines) + `[[bin]]` wiring
  (`required-features = ["recording-proofs"]`). Hand-rolled arg parser, no new deps. CLI per §5.
  PASS decision for unpaced mode is anchored to the **decoded** file via `find_first_gap()`
  (`recording_soak.rs:645`) — asserts `frame_indices == [0..N)`, so a BUG-085 async append drop
  shows as an index gap even when Rust reports 0 drops. `--realtime` gates file-validity only
  (D8). `--hdr` exits non-zero naming BUG-053 (D7). Output deleted on PASS unless `--keep`; on
  FAIL the file is kept and its path printed (`fail_tail`, `recording_soak.rs:503`). Pre-flight
  disk check ≥ 1.5× estimate.
- **Test 5 `kill_mid_take_leaves_recoverable_file`** (`tests/recording_proofs.rs`) — spawns the
  soak bin as a real child, polls to >30 MB, SIGKILLs, probes that ≥1 frame survives with
  strictly-increasing PTS (fragmented-MOV durability, failure class 1).
- **Runbook** — "Recorder soundcheck" section in `docs/DEVELOPMENT_REFERENCE.md:55` with the §5
  ritual command verbatim.

### Gate output (orchestrator-run in the worktree)
```
cargo test -p manifold-recording --features recording-proofs
  pool_accounting_consistent ok · adversarial_pts_survives ok · nominal_video_only ok ·
  kill_mid_take_leaves_recoverable_file ok · nominal_with_audio ok · hdr_blocked_by_bug_053 ok
  test result: ok. 6 passed; 0 failed; finished in 3.36s

OFFICIAL short soak (orchestrator ran the design's landing command):
  cargo run --release ... --bin recording-soak -- --width 1920 --height 1080 --minutes 2 --keep
  SOAK PASS: 7200 frames, 0 dropped, PTS monotonic, gap-free indices, 0.28 GB, audio 118.6s

--realtime disambiguation run (show-paced proxy, idle machine):
  ... --minutes 2 --realtime
  SOAK PASS: 7200 frames submitted, 0 dropped (--realtime, gated on file validity only),
  PTS monotonic, 7200 decoded frames valid, 0.28 GB, audio 120.0s   ← full audio at show pace

--hdr:  SOAK FAIL: --hdr blocked by BUG-053 (...)   exit 1
cargo clippy --workspace --features manifold-recording/recording-proofs -- -D warnings → clean
```

### Negative gates (orchestrator-run)
```
rg 'unwrap\(\)' crates/manifold-recording/src/bin/   → 0 hits ✓ (no .expect() either; only
                                                        unwrap_or/_default/_else on infallible setup)
--bin recording-soak -- --hdr                         → exit 1, message names BUG-053 ✓
```

### Acceptance demo (L2) — orchestrator opened it
Extracted frame 3600 from the kept official 1080p soak `.mov` and read the PNG: the 26-block
luma code is present and advancing (a mid-take index encoded in the stripe pattern), confirming
the soak encodes real decodable content through the full ProRes pipeline at 1080p.

### Bug found → logged + disambiguated
**BUG-086** (`recording-audio-track-under-covers-duration-on-longer-takes`) — unpaced soak audio
comes out ~1.3–3.3% short past ~60s; silent drop at the native real-time backpressure gate
(`isReadyForMoreMediaData` returns `LR_OK` on drop, no counter/log). The worker root-fixed the
gross version (audio was frame-coupled; bursting media time into wall time lost ~91%) by pacing
synthetic audio to wall clock. **Orchestrator disambiguation:** a `--realtime` run (show-paced)
gave audio 120.0s exactly — full coverage — versus 118.6s unpaced. So the residual is an
**unpaced-stress-mode artifact** (100%-duty encoder starves the audio input), not a show-path
defect; show severity is LOW. The silent-drop-with-no-counter path is still the real defect worth
removing (BUG-085's sibling). Peter's full-scale run remains the confirming data point.

### Shortcuts taken (worker report, orchestrator-reviewed)
Audio-coverage gate is a coarse 50% floor + non-gating >2% stderr warning naming BUG-086, not a
tight tolerance — consistent with the design (§5's stated soak gate is video: "0 drops, full
index sequence"; the PASS line reports audio but never gated it tightly), and honest given
BUG-086's unknown-until-now magnitude. Accepted. No hidden stubs.

### Verification debt — VD-022 (see `docs/VERIFICATION_DEBT.md`)
- **Full-scale 4K60 20-minute soak unexecuted** — by design (§6 P2): its first run is Peter's
  pre-gig ritual on the rig. The short 1080p/2-min soak is the wave's proxy. **L2 reached /
  full-scale L4 carried.**
- **BUG-086 silent audio-drop fix** — deferred; show-relevance now assessed LOW after the
  `--realtime` result, revisit-upward trigger is the full-scale run.

### Status line after P2 (quoted)
> **Status:** IN PROGRESS 2026-07-10 — P1 SHIPPED @ `ef12c14b`, P2 SHIPPED @ `091290e3`
> (`recording-soak` bin unpaced+realtime with a decoded-index PASS gate; kill-durability test 5;
> `docs/DEVELOPMENT_REFERENCE.md` runbook; found BUG-086 audio-coverage shortfall and
> disambiguated it as unpaced-stress-only via a `--realtime` run giving full audio). P3 (in-app
> record smoke, L3) pending. ...

## P3 — In-app record smoke (L3)

_Pending._

## Peter checklist (accumulated across phases)

1. **Pre-gig recorder soundcheck (the ritual, per design §5/§6 P2).** On the rig, the day before a
   gig, run:
   `cargo run --release -p manifold-recording --features recording-proofs --bin recording-soak`
   Expect a single `SOAK PASS:` line and exit 0. This is the first full-scale 4K60 20-minute soak —
   the wave deliberately did not run it (needs the rig). **Watch the `audio NNNN.Ns` figure:** it
   should be within a second or so of the take length. If it is materially short at 20 minutes,
   BUG-086 scales worse than the 2-minute proxy suggested and its severity must be revised upward
   before you rely on the recorder for a show (the `--realtime` 2-min proxy gave full audio, so
   the expectation is that it holds — this run confirms it at show scale).
