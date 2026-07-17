# AUDIO_ANALYSIS_ACCURACY P2 — Beat This swap — landed 2026-07-17 @ <merge SHA on push>

**Branch:** wave/audio-accuracy (`453f52a2`, Sonnet executor, Fable orchestrated + landed) · **Level reached:** L2 target; Peter's listen-list recorded as VD (below)
**Doc status line (quoted verbatim):** "**Status:** IN PROGRESS — P1+P2 SHIPPED 2026-07-17 (P1 harness core; P2 Beat This swap: madmom beat/downbeat/tempo arms deleted, sum-F1 gate passed both splits with downbeat +40pp heldout; D14 ≤5ms deferred to P5 correction seam per BUG-229; Integer octave mis-track = P4/P5 target; split veto + BUG-227 still open) · designed 2026-07-08 · Fable"

## What landed

`manifold_audio/beat_tracking.py` (new): Beat This (`beat-this==1.1.0`, MIT —
LICENSE + README license section read directly; weights MIT per the project's
own statement, cached outside git). madmom beat/downbeat/tempo arms DELETED
from `bpm.py`/`analyzer.py`/`cli.py` (`_estimate_madmom_beats`,
`_detect_madmom_downbeat_phase`, tempo-prior scoring) — not flag-gated.
Autocorrelation fallback stays, stamped `"tracker": "autocorr_fallback"`.
`dbn=False` hardcoded and documented (Beat This's dbn path lazily imports
madmom — never enabled). `onset_detection.py` untouched (P6). Verified through
the real `manifold_audio.cli` on drums-only, parallel-dispatch, and
`--bpm-only` paths.

## Gate results (executor verbatim; negative gate re-run by orchestrator)

```
rg 'madmom\.features\.(beats|downbeats|tempo)' tools/audio_analysis → zero hits  (re-verified at landing)
pytest eval/tests/ -q → 47 passed (was 42)

dev:     beat_f1 0.7113→0.7079 · downbeat_f1 0.4579→0.6083   (sum 1.1692→1.3162 PASS)
heldout: beat_f1 0.8553→0.8909 · downbeat_f1 0.4978→0.8940   (sum 1.3531→1.7849 PASS)
```

**Orchestrator ruling on F1:** the doc's gate is the sum ("beat F₁ + downbeat
F₁ ≥ baseline − noise floor"); both splits pass decisively. The dev beat-F1
dip (−0.35pp) is driven by `liveshow_integer`, where Beat This locks to
115.4 BPM against 132 truth (≈7/8 confusion, 57% over-detection) and
`liveshow_midnight_patience` (136.4 vs 132). Recorded as a named P4/P5 target:
BPM range hint 60–200 (Peter's directive) + octave disambiguation. Noise floor
0.0 (bit-identical reruns).

**Orchestrator ruling on D14 ≤5ms:** NOT met as raw beat output — median
14.4 ms, max 26.25 ms, a clean sawtooth = pure 50 fps frame quantization
(the D14-predicted category), format-stable (42/43 bit-identical across
wav/mp3/AAC). Beat This correctly refuses the non-periodic sparse click
fixture; a periodic 128 BPM fixture was added. Deferred to the D14 correction
seam (P5 wiring) per the design's own measure-then-correct architecture —
**BUG-229**, verification debt, not waived. P3 additionally measures
*fitted-grid* alignment (quantization should average out over hundreds of
beats; if ≤5 ms there, the gate's intent is already met).

## Deviations from brief

Both gate frictions above — executor flagged rather than self-resolved
(correct behavior), orchestrator ruled. **BUG-230** logged: Beat This
converges on ~142.86 BPM for 4/5 short (~13 s) isolated-stem clips;
full-length fixtures unaffected.

## Verification debt

- VD: Peter's listen-list (10 beat-click renders, gitignored under
  `tools/audio_analysis/eval/data/listen_list/`) awaiting his ears — L2 demo
  deferred per the doc's own "deferred OK as VD entry."
- VD: BUG-229 D14 frame-quantization correction at P5's seam; ≤5 ms re-check
  there (or at P3's fitted-grid measurement, whichever comes first).

## Click-script for Peter (≤2 minutes)

1. Play any file in `tools/audio_analysis/eval/data/listen_list/` — expect: clicks sit on the beat; downbeat clicks land on the "1".
2. `liveshow_integer` render is the known-bad one — expect: audible tempo confusion (the P4/P5 target, honest display of the current weakness).
