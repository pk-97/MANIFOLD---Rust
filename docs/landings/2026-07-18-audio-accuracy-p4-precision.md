# AUDIO_ANALYSIS_ACCURACY P4 — precision pass — landed 2026-07-18 @ <merge SHA on push>

**Branch:** wave/audio-accuracy (`d15c9aaa` tip; Sonnet executed sweeps, Fable judged every acceptance — the judge/executor split Peter mandated) · **Level reached:** L1 + CLI end-to-end verification of shipped defaults
**Doc status line:** updated (see header — carries accepted defaults and rejections verbatim).

## What landed

- **BUG-235 scorer calibration** (`eval/calibration.py` + per-fixture constants): five-fixture ADTOF kick baseline corrected 0.238 → **0.739** (feel_the_vibration 0.000 → 0.968). Applied at scoring seam only.
- **Truth-type-aware objective** (round-2 correction, orchestrator-directed): dense fixtures (MIDI/self-render/calibrated stems) score full P/R/F1; sparse-visual fixtures (Peter's placed clips) score recall + active-passage precision only. Round 1's pooled metric was structurally wrong and every round-1 aggregate is superseded.
- **Accepted production defaults** (heldout-confirmed, now live in `analyze_percussion()` — CLI-verified: `ADTOF thresholds (kick,snare,tom,hihat,cymbal)=0.138,0.182,0.140,0.090,0.090`):
  kick ×1.15 → dense F1 0.8536→**0.8577**; snare ×1.3 → 0.5788→**0.6410**; hat ×0.5 → 0.1739→**0.3028** with sparse recall 0.786→0.929 and one-shot recall 0→0.25 (dev), heldout recall +5.5pp, zero heldout regressions on any class.
- **Rejected for the detector layer** (measured, not vibes): shape gates (kick sparse recall 0.77→0.22 at floor 0.3), co-fire weights (dead for snare, harmful for kick), beat-phase priors (dead/harmful everywhere tested), median-adaptive baseline (no gain offline). PARKED as trigger-selection-layer candidates — Peter's corpus encodes *prominence* (which hits deserve visuals), and these knobs measure prominence, not existence. Two objectives, two layers — the night's key architectural insight.
- Dead knobs recorded: snare/cofire, perc/beat_phase, synth/refractory, hat/refractory, hat/median_adaptive.
- Synth NOT tuned: n=1 dev coverage (self-render arp only) — a real fixture gap, deferred with the 0.7→0.9167 single-track tease explicitly not trusted.
- Hat one-shot autopsy: of 4 lone hats, 2 have near-zero ADTOF activation (upstream suppression — unfixable by threshold), 1 recovered at ×0.5, 1 near-miss at 66% of threshold.

## Gate results

110→112 pytest green across rounds (30+ new tests incl. truth-type split structure and default-pinning). Heldout touched exactly once, on the final four-config acceptance read (`p4_heldout_acceptance.json`). Noise floor 0.0 (deterministic environment).

## Deviations from brief

Round-1 objective flaw caught by orchestrator from per-track rows; round 2 re-ran everything under the corrected metric (this is the process working, recorded for the method's sake). `p4_sweep_r1_synth.json` left in superseded round-1 format.

## Verification debt

Carried: P2 listen-list; BUG-229 (P5 seam). The parked prominence knobs need a home when the trigger-selection/mapping-grammar layer is designed.

## Click-script for Peter (≤2 minutes)

1. Run a percussion import on any electronic track — expect the progress log line `ADTOF thresholds (kick,snare,tom,hihat,cymbal)=0.138,0.182,0.140,0.090,0.090`, and noticeably more hats than before at the same sensitivity slider position.
2. `eval/scoreboard/p4_heldout_acceptance.json` — expect the four-config read, hats +5.5pp recall, nothing regressed.
