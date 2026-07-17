# Audio event classifier P1–P3 + heldout exam — landed 2026-07-18

**Branch:** lane/audio-event-classifier (Sonnet executed P1/P2/P3 rounds, Fable orchestrated + judged + ran the exam) · **Level reached:** L1 (scoreboards; the exam is a measurement outcome)
**Doc status line (verbatim):** "IN PROGRESS — P1+P2+P3 executed 2026-07-18 (same day); heldout exam run, verdict SHORT OF BAR (see §8): dev tuning plateaued … P4/P5 blocked on Peter's call: expand training data (D6 dial + more labeled shows / license-verified Splice) or park. ADTOF stays meanwhile; BUG-069 unchanged."

## What landed
- `train/` package: sources.toml license allowlist + dataset pipeline (25,708 patches, 6 classes; vocal deferred — all vocal truth is heldout-only), trainer (seeded, MPS, reproducible to the last decimal), export (.aec format + parity fixtures), classifier-labeling mode in stage1_dsp_detection (default path byte-identical, proven by monkeypatch tests).
- `manifold_audio/mel_patch.py`: single-source patch geometry (64 mels × 16 frames, 100ms).
- Scoreboard artifact `bakeoff_b1_stage1_classifier.json`; 151 eval tests green throughout; license + heldout rg gates clean at every commit.
- P3 rounds: R1 rebalance ACCEPTED (kick 0.476→0.547); R2 mels-96 REVERTED (net-negative); R3 span-150ms REVERTED (net-negative, hypothesis falsified by the windowed gate).

## The verdict numbers
Dev (round-1 model): kick 0.547 / snare 0.505 / hat 0.266 / perc 0.531 vs ADTOF 0.796/0.749/0.510/0.547 — beats the signature labeler it replaces on snare/hat/perc.
Heldout (one-shot, D8): liveshow kick 0.414 vs 0.909 · snare 0.218 vs 0.502 · hat 0.014 vs 0.619; E-GMD snare **0.723 vs 0.639 (only SHIP-grade line)**, kick 0.487/0.792, hat 0.194/0.496, perc 0.517/0.639; drums-filed-as-other 15.5% (bar <10%) FAIL. Full table + reading: design doc §5b.

## Rulings
Short of bar → ADTOF stays, BUG-069 trigger unchanged. Dev→heldout collapse = data starvation (three dev shows of production variety), not architecture failure. P4 (Rust inference) and P5 (integration) BLOCKED on Peter: open the D6 data dial (more labeled shows, license-verified Splice composites, expanded E-GMD) or park. Exam consumed the liveshow+E-GMD heldout for this candidate.

## Deviations / disclosures
Rounds 2–3 overwrote the round-1 weights in DATA_ROOT; caught before the exam (which would otherwise have scored the rejected round-3 model) and round-1 was retrained bit-identically first. Liveshow heldout kick rests on n=1 song (only one heldout song carries kick truth). ADTOF's liveshow-heldout arm ran without a beat grid (grid=None) — its beat-phase prior knob was inert there; ADTOF won every liveshow line regardless.

## Verification debt
None new — negative verdicts ship no code paths; classifier mode stays off by default everywhere.

## Click-script for Peter (≤2 minutes)
1. Design doc §5b — the exam table; your call is one line: open the data dial or park.
2. `eval/scoreboard/bakeoff_b1_stage1_classifier.json` — dev per-track detail.
