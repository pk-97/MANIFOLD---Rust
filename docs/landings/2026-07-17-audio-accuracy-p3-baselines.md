# AUDIO_ANALYSIS_ACCURACY P3 — fixture pack + full baselines — landed 2026-07-17 @ <merge SHA on push>

**Branch:** wave/audio-accuracy (`e9087ae7` tip; Sonnet executor, Fable orchestrated + landed) · **Level reached:** L1 (baseline report; measurement-only phase by design)
**Doc status line:** updated to P1+P2+P3 SHIPPED (see doc header — carries the BUG-235/232 blockers verbatim).

## What landed

Fixture pack now 19 entries: Slakh test split (streaming fetch left running —
Zenodo throttles to ~830 KB/s, ~35 h total; registered with fetch state),
MAESTRO v3 selection (20 tracks, MIDI + synth-rendered audio — real audio not
individually fetchable, gap documented), Harmonix electronic slice audio
107/129 matched via yt-dlp (22 unavailable, logged), self-render v1 (3
fixtures with exact MIDI truth), MUSDB18 complete (150/150). Baseline
scoreboards committed: `p3_baseline_babyslakh.json`, `p3_full_pack_baseline.json`,
`p3_beat_baseline.json`. 67 pytest green; all negative gates re-run green.

## The reference numbers (bake-off targets)

ADTOF on babyslakh (MIDI-aligned truth, dev/other): **kick 0.818 · snare
0.621 · hat 0.233 · perc 0.349** — hats/perc weakness confirms the doc's
audit. basic_pitch: bass 0.645 (stem, production condition), melodic 0.823.
Beat/downbeat reproduces P2 exactly.

## Findings that gate what comes next

- **BUG-235:** ADTOF kicks are systematically EARLY (tight −20…−125 ms
  clusters, per-track constant-ish) against the manifold_own hand-labeled
  convention (25%-envelope walk-back). NOT a scorer bug — columns verified
  identical; the current column choice already optimal. The electronic-domain
  drum baseline is therefore BLOCKED on a one-time per-detector
  onset-convention calibration (D14-mechanism extension) before any bake-off
  comparison on these fixtures is meaningful. Applying it was tuning → out of
  P3's measurement-only scope, correctly deferred.
- **BUG-232:** Harmonix YouTube-matched audio carries 2.9–7.3 s constant
  offsets (wrong-edit/lead-in matches). Offset estimator built and proven
  (beat F1 0.112 → 0.654 after correction); productizing it at match time is
  the fix. Until then Harmonix audio is QUARANTINED from tuning/acceptance.
- **BUG-229 follow-up (honest negative):** fitted-grid alignment does NOT
  rescue the ≤5 ms target on real inference (128 BPM: 14.16 ms fitted vs
  14.38 raw; 174 BPM: fitted worse). Synthetic case proves the averaging
  hypothesis; real output carries more than quantization noise. Stays open
  for P5's correction seam.
- **BUG-231:** no BPM-range/tempo-prior API exists in Beat This's
  license-clean path — the Integer octave fix must be an in-house windowed
  tempo-referee heuristic (P4/P5), shape validated by the median-IBI
  diagnostic (130.4 vs raw 230.8 on true-132 UKG material).
- BUG-069 status updated: madmom beat/downbeat/tempo arms now gone (P2);
  onsets (P6) and ADTOF (bake-off) remain.

## Deviations from brief

Slakh incomplete by throttle, not by choice; MAESTRO synth-rendered; vocal
scoring is a mix-vs-stem agreement measure (0.461), not the full D13 recipe;
yt-dlp added to the gitignored bundled runtime as an eval-only dep.

## Verification debt

Carried: P2 listen-list (Peter's ears), BUG-229 correction at P5.
Opened: none beyond the logged bugs above.

## Click-script for Peter (≤2 minutes)

1. Open `tools/audio_analysis/eval/scoreboard/p3_baseline_babyslakh.json` — expect: per-class ADTOF F1 matching the table above.
2. `rg 'BUG-23[125]' docs/BUG_BACKLOG.md` — expect: the three new measurement-integrity bugs with diagnostics inline.
