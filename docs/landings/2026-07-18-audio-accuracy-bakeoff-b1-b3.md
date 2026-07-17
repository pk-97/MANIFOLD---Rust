# ADTOF bake-off B1–B3 — Stage-1 challenger built, measured, verdict called — landed 2026-07-18 @ <merge SHA on push>

**Branch:** wave/audio-accuracy (`d55cb2f3` tip; Sonnet executed, Fable judged both rounds and called the verdict) · **Level reached:** L1 (scoreboards; verdict is a measurement outcome)
**Doc status line:** design doc carries the B3 verdict section (end of file) — ADTOF stays, gap documented, Stage 2 pending Peter's approval.

## What landed

- `manifold_audio/stage1_dsp_detection.py`: license-clean Stage-1 drum detector — multi-band onset front-end (ported live-kick logic, non-causal), per-track KMeans clustering (silhouette-selected k), centroid-signature labeling; dev-fitted class profiles (`eval/fit_stage1_profiles.py` + committed profile JSON).
- E-GMD integration: license CC BY 4.0 verified from the archive's own LICENSE via Range request; selective Range-addressable fetcher pulls 63 stratified performances (38.7 MB of a 96.4 GB archive); dev/heldout via the dataset's own split column.
- Three real `spectral.py` bug fixes (np.convolve mode="same" kernel-length trap; backtrack fed flux instead of energy → ~50 ms early bias; frame-start vs frame-center → ~25 ms early bias) plus a bake-off scorer input bug (manifold_own fed mix.wav instead of drums.wav — apricots kick 0.0→0.645 corrected). New `edm_kit_128bpm` self-render fixture (non-colliding timbres).
- Scoreboards: `bakeoff_b1_stage1.json` + B2 re-run. 121 pytest green.

## The verdict numbers (dev dense truth)

Electronic (the gate's domain): kick 0.311 vs ADTOF 0.702 · snare 0.250 vs 0.653 (n=1) · hat 0.592 vs 0.426 (n=2) · perc 0.000/0.000 (n=1). Acoustic: kick 0.490/0.815 · snare 0.492/0.774 · hat 0.270/0.529 · perc 0.256/0.553. Kick fixed in-domain (0.925 on edm_kit; kick_hat fixture proven structurally unsolvable per-onset — kick and hat physically superimposed). Round-3 bar (kick ≥0.5 electronic) not met → verdict called, heldout not consumed.

## Rulings

ADTOF stays (BUG-069 trigger unchanged). Gap documented per class — this is the "prove it or measure the shortfall" Peter asked for, delivered as the shortfall. Stage 2 (small CRNN, permissive data, demucs-separated-render training) now has a quantified case and **awaits Peter's explicit approval** — no training without it. P6 targets ADTOF as keeper. Stage-1 front-end, clustering, fixtures, and fetch infra carry forward into Stage 2.

## Deviations / disclosures

Lever-2 fitted profiles regressed the synthetic fixture's non-kick label accuracy (91%→~35% on matched) — small-n clap/tom profiles and acoustic-dominated snare profile; disclosed in test assertions, part of the Stage-2 case. Slakh trickle-fetch still running (2 complete tracks used; ~35 h Zenodo throttle).

## Verification debt

Carried: P2 listen-list, BUG-229 (P5). New: none — negative verdicts don't ship code paths.

## Click-script for Peter (≤2 minutes)

1. Read the B3 verdict section at the end of `docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md` — the decision you owe is one line: approve Stage-2 training (compute + dataset build) or park it.
2. `eval/scoreboard/bakeoff_b1_stage1.json` — the side-by-side, per class.
