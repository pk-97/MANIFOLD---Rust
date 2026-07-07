# OFFLINE_AUDIO_REACTIVE_EXPORT P1–P3 + journey-proofs harness — landed 2026-07-07

**Branch:** `wave/offline-audio-export` (tip `f2d4cc38`; P1 `d207f94a`, P2 `bdbf50d5`,
P3 `f2d4cc38`, docs ride the landing merge) · **Level reached:** L2 / target L2
(flow driver can't reach export; L4 real-track pass = VD-016)
**Doc status line (quoted verbatim):** "Status: SHIPPED P1–P3 (2026-07-07, Fable→Sonnet
wave, branch `wave/offline-audio-export`) — L2 verified: exported video demonstrably moves
with the audio (click-track luma ratio ~6.9×), survives save→reload, and two runs are
bit-identical in extracted frames. P3 shipped as the standing `journey-proofs` cargo
feature on manifold-app (run deliberately, like gpu-proofs; needs ffmpeg/ffprobe on PATH).
L4 (Peter exports a real track and sees the pump) = VD-016. Landing report:
`docs/landings/2026-07-07-offline-audio-reactive-export.md`."

## What shipped (and what it means on stage)

Exported video no longer freezes every audio-bound parameter. The export loop now renders
the timeline audio once (`render_export_audio`), feeds it per frame through the same
`SendAnalyzer` DSP the live rig uses (`OfflineAudioModDriver`), and writes the feature
snapshot the engine already consumes — so param modulation, param triggers (§8 wave), and
transient-fired clip triggers all move in the deliverable, deterministically. The release
workflow (master in → compose → export) now carries the show's most expressive layer into
the rendered file. Capture-fed sends hear the timeline mix as the front-of-house substitute,
logged per send, never silent.

## Gate results (verbatim, re-run by the orchestrating session)

- `cargo test -p manifold-playback --lib` → `test result: ok. 171 passed; 0 failed; 2 ignored`
  (includes the P1 pre-refactor byte-identity fixture, hash `0xaa873b48d8b143e1` unchanged
  across the seam refactor)
- `cargo test -p manifold-app` (default, no feature) → `147 passed; 0 failed; 2 ignored`
  — harness invisible to the default sweep (Peter's hard constraint)
- `cargo test -p manifold-app --features journey-proofs` →
  `151 passed; 0 failed; 2 ignored; finished in 3.54s` — all four proofs:
  `audio_reactive_export_moves` · `audio_reactive_survives_save_reload` ·
  `lfo_modulation_exports` · `export_is_deterministic_in_features`
- `cargo clippy --workspace -- -D warnings` → clean (re-run post-merge at landing)
- Full workspace sweep at landing: see merge-gate output in the push.

Measured proof numbers (96 frames, 320×180@24fps, 8 beats @120bpm StarField `brightness`
bound to the send's Full band): click-frame mean luma 0.00103 vs gap 0.000149 (~6.9×);
LFO run oscillates 0.0→0.017 with 8 mean-crossings; two identical exports differ by
**0** in per-frame luma. Round-trip (save V1 → load → export) reproduces the same numbers.
Orchestrator read the extracted frames directly: post-click frames show the starfield,
gap/trough frames are black.

## Deviations from brief

1. `ExportAudio` carries `left`/`right` + `audible_in_range` beyond the doc's sketch — the
   wrapper needs the stereo for the WAV and the original-range flag for exact `Ok(false)`
   semantics. Doc seam brief updated in this landing.
2. manifold-app is bin-only: gates run as `cargo test -p manifold-app` (no `--lib`).
3. P3 assertions are ratio-based, not absolute-delta (StarField's sparse-highlight baseline
   luma is ~1e-3); thresholds justified in comments against the measured series.
4. P3 scope extension (orchestrator-directed): the harness also proves LFO-in-export and
   save→reload→export — two of the three unverified release-journey seams from the
   priority queue. The full glb→scene→animate journey leg remains future work.

## Shortcuts confessed (rolled up from phase reports)

None in P1/P2/P3 reports. P3's one workaround: `rebind_gpu_device_pointers` re-states the
device-repoint invariant inside the harness — filed as BUG-054 (root fix: remove the
self-referential raw pointer).

## Verification debt

- **VD-016 opened** — real-track export feel (L4, Peter; the design's stated milestone).
- **VD-004 closed** — mixdown now proven on a real export (ffprobe audio stream + P1
  byte-identity).

## Found this wave

- **BUG-054** (renderer-device-ptr-dangles): renderers cache a raw `*const GpuDevice` that
  only `ContentThread::run()` repoints — latent segfault for every future headless/embedded
  consumer. Workaround in harness; root fix shape in the backlog entry.

## Click-script for Peter (≤2 minutes)

1. Open a project with an audio layer + any generator param audio-bound to a send fed by
   that layer (or bind one: send → layer feed, param → Audio mod → that send).
2. Export a few bars (any resolution; SDR is fine).
3. Open the .mp4 — expect: the bound visual pumps with the track (it froze before), and
   the audio is in the file.
4. Optional: export the same range twice — the motion is identical both times (something
   live capture could never give you).
