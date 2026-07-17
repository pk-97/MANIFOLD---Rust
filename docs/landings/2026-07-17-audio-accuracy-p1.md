# AUDIO_ANALYSIS_ACCURACY P1 — harness core + noise floor — landed 2026-07-17 @ <merge SHA on push>

**Branch:** wave/audio-accuracy (`edac881a`, Sonnet executor, Fable orchestrated + landed) · **Level reached:** L1 per doc (scoreboard JSON + worst-track dump; no UI surface)
**Doc status line (quoted verbatim):** "**Status:** IN PROGRESS — P1 SHIPPED 2026-07-17 (harness core, D7/D10/D11/D14, live-show fixture + provisional split awaiting Peter's veto; metamorphic invariance failure = BUG-227, a pipeline finding, not loosened) · designed 2026-07-08 · Fable"

## What landed

`tools/audio_analysis/eval/` — the full P1 harness: `metrics.py` (D10 frozen),
`metamorphic.py`, `bundles.py` (D7), `sweep.py` (dev-only, D9 guard),
`run.py`, `click_track.py` + measured `decoder_alignment.json` applied at the
`audio_io.py` seam (D14), `noise_floor.py` (D11), `liveshow_extract.py` (the
2026-07-17 addendum's `.manifold` label extractor: grid / onset / section truth
from the Liveschool corpus), `fixtures.toml` with per-fixture `domain` tags
(Peter's EDM-target directive) and a provisional dev/heldout split, fetch
scripts for babyslakh_16k / Harmonix / MUSDB18, 42 pytest tests. Mid-flight
scope additions folded in: the five historical `<track>_<bpm>bpm` stem packs
(kick-onset truth per their README; dev-only because they tuned past
thresholds) and per-domain scoreboard aggregates.

## Gate results (verbatim from executor, spot-reverified by orchestrator)

```
pytest eval/tests/ -q                       → 42 passed, 1 warning in 1.2s
python -m eval.run --set dev --report …     → scoreboard JSON written (incl. by_domain)
git ls-files | rg '\.(wav|flac|mp3|npz)$'   → zero hits   (no audio committed)
rg 'heldout' eval/sweep.py                  → zero hits   (D9 leakage guard)
```

Noise floor (D11, N=3): stdev 0.0 this environment (deterministic run;
re-measure under MPS noted). D14 decode-stage offsets: −0.011 ms across
wav/mp3/AAC — decode introduces no skew; per-detector corrections belong to
P2/P6 by design.

## Deviations from brief

- Metamorphic gate not fully green: silence-floor passes; gain-invariance and
  time-stretch-invariance FAIL on real babyslakh mixes with the current
  (pre-P2/P6) madmom onset arm. Two harness bugs were found and fixed first;
  the residual is a genuine pipeline property → **BUG-227**, per the doc's own
  "violations are bugs, not tuning targets." Orchestrator ruling: the
  instrument works — the failure is a measurement, and blocking P1 on it would
  confuse the two. Landed with the finding recorded.
- MUSDB18 fetch (~4.7 GB) still downloading unattended at landing time;
  babyslakh (the gate minimum) fetched + md5-verified, Harmonix annotations
  fetched (912 tracks, 129 Dance/Electronic).

## Shortcuts confessed

Detector-stage D14 alignment is diagnostic-only in P1 (soft click bursts
under-trigger the outgoing madmom CNN; the P2/P6 replacements gate on these
same fixtures). Harmonix per-track audio matching deferred to P3 per the doc.

## Verification debt

None opened beyond the doc's own deferrals. Awaiting Peter (morning): the
dev/heldout split veto — proposal holds out BASALT + STAGNATE (both
electronic, one drum-dense, one varied; rationale in `fixtures.toml`), and
flags a 7-markers-vs-6-group-layers song-boundary discrepancy for his
one-glance resolution.

## Click-script for Peter (≤2 minutes)

1. `tools/audio_analysis/BundledRuntime/macOS/python/bin/python3 -m eval.run --set dev` (from `tools/audio_analysis/`) — expect: scoreboard JSON path printed, per-domain aggregates inside.
2. Open `tools/audio_analysis/eval/fixtures.toml` — expect: your five stem tracks + seven show songs tagged, split proposal + rationale in comments.
