# Realtime Kick Detector — the trained kick model as the app's only kick detector

**Status:** APPROVED design, not built · 2026-10-10 · Opus 5.5 lead, Fable review
**Prerequisites:** the research recipe on `feat/kick-realtime` (`tools/audio_analysis/eval/`, `docs/AUDIO_EVAL_HARNESS_GUIDE.md`).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) before starting a phase.

## 1. What ships

The trained kick model replaces the old kick detector (`KickTrack`/`KickRidges` in
`crates/manifold-audio/src/analysis.rs`), which is deleted. Consumers keep their
contract: `SendFeatures.bands[Low].kick` is 1.0 on the fire hop and decays after.

Pipeline, all causal, 48 kHz mono:
1. Candidates and 15 base features (`kick_fusion_features.py`).
2. 54 more features: tail (3), templates (3), rise profile (32), low filter bank (16) — `kick_goal_featsets.py` `build(g, 'f69')`.
3. Trees (sklearn HistGradientBoosting) on the 69 features.
4. One small CNN (`kick_goal_nn.py`) on 64 causal band envelopes, slice ending 40 ms after the candidate's emission.
5. Blend = mean logit of trees and net. Stage: 5 song-relative features over the previous 8 s (`kick_goal_selfsim.py`) into a second tree model.
6. Cutoff and 60 ms refractory (`kick_goal_eval.py` `fires`). A fire leaves at emission + 40 ms (about 80 ms after the kick).

Decisions: one net (not an ensemble). Input of any rate is resampled to 48 kHz
before the detector; the model exists at 48 kHz only. Hand-written Rust inference,
no ML library. The detector runs on its own worker thread per analysed send (CPU:
about 38 candidates/s typical, 86 peak; one net ≈ 10 M multiply-adds each).

## 1b. Threading (Peter, 2026-10-10) — obsolete when audio analysis leaves the content thread

Trained detectors (kick now; snare, clap, hats and others later) run on a
detector worker thread, one per analysed send, behind one interface: mono audio
in, events stamped with their input sample index out. Today each send's audio is
mixed on the content thread (capture mono plus audio-layer taps, summed) and
analysed once per frame in `AudioModRuntime::update`, right before
`engine.tick` reads the features. So, per frame:
1. `update` hands every send's new mono samples to its worker (no allocation, no lock on the audio path);
2. it waits for that worker's events with a hard cap of about 2 ms in total;
3. the analyzer marks each event on the hop that contains its sample, or on the first hop after it if the event arrived late.

Detector work is about 1 ms per frame, so fires normally land in the same frame. A
late detector costs one frame (about 17 ms) and never stalls rendering past the
cap. Offline paths (export, harnesses) run the same detectors inline, without
the worker.

This hand-off exists only because mixing happens on the content thread. If the
audio path is re-architected so sends are mixed off the content thread, the
workers should read that mix directly and publish events continuously; then
the per-frame hand-off, the wait cap and the one-frame fallback all go away.
Nothing else in the detectors depends on the frame.

## 2. The model file

One file, `crates/manifold-audio/assets/kick_model.mkick`, built into the app with
`include_bytes!`. It holds every learned or tuned number and every filter
coefficient; Rust derives nothing from first principles.

Container (little-endian), shared with the parity goldens:
- magic `MKICK001` (8 bytes), then `u32` entry count;
- per entry: `u16` name length, UTF-8 name, `u8` dtype (0 f64, 1 f32, 2 i32, 3 i16, 4 i64, 5 u8), `u8` ndim, `u64` per dim, then the raw data.

Readers: `tools/audio_analysis/kick_container.py`, `crates/manifold-audio/src/kick/container.rs`.

Required entries: `recipe_version` (i32 scalar), `train_id` (u8 text). The
recipe version names the pipeline shape (feature list, band count, slice
length, stage inputs). A retrain with the same recipe keeps the version and only
changes numbers: swap the file, run the parity test, ship. A recipe change bumps
the version and needs Rust work. Rust refuses a file whose version or shapes it
does not know; then the kick stays 0 and an error is logged.

## 3. Parity

The exporter writes goldens (`crates/manifold-audio/tests/fixtures/kick/*.mkick`)
for short clips cut from `tests/fixtures/audio`, audio stored as i16 so both
sides see identical samples. Each golden holds the audio, each stage's inputs
and outputs, and the model's `train_id`; a test with a different `train_id`
fails. Rust feeds the audio in random 256–1024-sample chunks. Tolerances:
features abs 1e-6, tree logit 1e-6, net probability 1e-4, stage probability
1e-4, fires exact (one mismatch allowed only within 1e-3 of the cutoff).

## 4. Retrain → ship

`tools/audio_analysis/kick_release.py` (dev.py verb): regenerate features from
audio, grouped run for the stage's training rows and the cutoff, fit final trees,
net and stage on every song, export the model and goldens, run
`cargo nextest run -p manifold-audio kick`.

## 5. Build order

P1 container + exporter + final model (Python, lead). P2 four parallel Rust lanes
against the goldens: base features + candidates; the 54 extra features; the net;
trees + stage + fires. P3 wire-in on a worker thread, delete the old detector,
archive `KICK_SWEEP_EVENT_DESIGN.md`. P4 parity, CPU measurement, Peter's live test, land.
