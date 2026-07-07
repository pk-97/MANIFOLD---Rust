# Kick ground-truth labels for the audio fixtures

One CSV per track in `tests/fixtures/audio/` (the audio itself is gitignored;
these labels are ours and committed). Columns: `mix_time_s` (grade mix
detection against this), `drums_time_s` (grade drums-stem detection against
this). Onset = walk-back to 25% of the sub-envelope peak; grading tolerance
±35 ms per AUDIO_EVAL_HARNESS_GUIDE.md.

**Provenance (2026-07-07, extracted by `scripts/kick_label_extract.py`, every
event verified by eye on drums-stem + mix spectrograms):** a kick = a local
peak of the 30–90 Hz envelope of the ISOLATED drums stem above 0.5× the
track's p99 sub level. Absolute sub strength, not sub/body dominance —
dominance mislabels kicks that land together with a snare. The split is
bimodal on all five tracks (snares/snaps ≤0.43×, kicks ≥0.97×; apricots'
snare-coincident kicks 0.62–0.67× are kicks, confirmed visually).

Counts: apricots 16 · bad_guy 17 · feel 16 · inhale_exhale 14 · tears 10 (73).

**bad_guy caveat:** its stems are unwarped (15.0 s, native 128) while its mix
is tempo-warped (13.241 s). `mix_time_s` was linearly scaled (×0.8828) and
snapped to the nearest mix low-band onset (±60 ms window). The other four
tracks' stems and mix share a time base exactly. Re-exporting bad_guy stems
warped would remove the caveat.

These labels are the grading target for the BUG-046 successor (ridge-motion
kick sweep-event detector) and replace the circular "drums-stem detector
fires as ground truth" from the 2026-07-06 session. Musically ambiguous
material (layered 808s etc.) is NOT in this set — Peter's hand-labeled corpus
still owns those calls when it lands.
