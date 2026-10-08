# Kick labels for the audio fixtures

**2026-10-09 review: the historical CSVs are not yet reliable attack-time
ground truth.** A visual review of all five drum stems and full mixes found
late timestamps and an Apricots duplicate. Do not treat the old scores or
per-track timing corrections as validated detector performance.

`attack_review.csv` records the review against all 73 existing rows. It is
not loaded by the evaluator and does not replace the historical track CSVs.
Estimated attacks use the full-band waveform leading edge, checked against
drum spectrograms, rather than the 30–90 Hz peak. Values are rounded to 5 ms;
this is display precision, not a guarantee of perceptual timing accuracy.
No listening review has been performed by the agent.

- Apricots: 9.385 and 9.495 label the same attack, approximately 9.310 s.
  The later row is a duplicate on the decay, not a second visible attack.
- Bad Guy at approximately 14.765 s and Inhale Exhale at 13.035 s are ending
  fills whose kick identity needs a listening decision.
- Four first events intersect the clip start. Their estimated time of zero
  does not establish the original onset or permit a clean latency measurement.
- The complete overviews revealed no additional obvious kick-shaped attacks,
  but this does not certify that no quiet or ambiguous kicks are missing.

All five mix/stem sums were rechecked at 8 kHz: zero relative lag,
correlation above 0.999999999, and gain within 0.00002 of unity. Audio was
not changed. Local evidence (alignment measurements and source hashes,
overview/detail plots, and short drum-then-mix listening clips) is in
`~/.cache/manifold/kick-label-review-2026-10-09/`.

**Remaining (BUG-qtd):** settle the listening cases and review perceptual
timing, then promote the accepted attacks into the per-track CSVs and
invalidate the old onset calibration before the full-mix baseline rerun.
Phase 1 remains incomplete until that review is settled.

## Historical labels

One CSV per track in `tests/fixtures/audio/` (the audio itself is gitignored;
these labels are ours and committed). Columns: `mix_time_s` (grade mix
detection against this), `drums_time_s` (grade drums-stem detection against
this). Onset = walk-back to 25% of the sub-envelope peak; grading tolerance
±35 ms per AUDIO_EVAL_HARNESS_GUIDE.md.

**Historical provenance claim (2026-07-07; the review above supersedes its
accuracy claims):** extracted by `scripts/kick_label_extract.py` and reported
as verified by eye on drums-stem + mix spectrograms. A kick = a local
peak of the 30–90 Hz envelope of the ISOLATED drums stem above 0.5× the
track's p99 sub level. Absolute sub strength, not sub/body dominance —
dominance mislabels kicks that land together with a snare. The split is
bimodal on all five tracks (snares/snaps ≤0.43×, kicks ≥0.97×; apricots'
snare-coincident kicks 0.62–0.67× are kicks, confirmed visually).

Counts: apricots 16 · bad_guy 17 · feel 16 · inhale_exhale 14 · tears 10 (73).

**bad_guy repair (2026-10-09):** the old mix lasted 13.241396 s, while all
four stems lasted 15.000023 s. The replacement mix is the unit-gain sum of
those unchanged stems, at their original 44,100 Hz and 661,501 frames,
encoded as 24-bit PCM. It is a reconstructed mix, not a time-stretched copy
of the old master. Both label columns now use the historical stem timestamps;
the duration-ratio scaling and onset snapping have been removed.

`bad_guy_128bpm.repair.json` records source and output hashes. Reproduce with
`python3 scripts/repair_audio_fixture.py --fixture-dir tests/fixtures/audio/bad_guy_128bpm
--backup-dir /path/to/preserved-original --apply` (omit `--apply` to inspect).
The original mix and repair provenance are preserved locally in
`~/.cache/manifold/audio-fixture-backups/bad_guy-2026-10-09/`.

The other four mixes were checked against their summed stems: zero lag and
correlation >0.999999 at 8 kHz, with gain within 0.00002 of unity. Folder BPMs
are historical names, not proof of the exported tempo. Published song BPMs
must not override measured audio timing.

**Baseline status:** historical detector scores and the old bad_guy timing
calibration do not apply to the reconstructed mix. The 17 stem labels retain
the original sub-envelope onset convention; alignment repair does not certify
their perceptual onset accuracy or constitute a fresh detector benchmark.
The broader label/scoring review remains tracked by BUG-qtd.

These labels are the grading target for the BUG-046 successor (ridge-motion
kick sweep-event detector) and replace the circular "drums-stem detector
fires as ground truth" from the 2026-07-06 session. Musically ambiguous
material (layered 808s etc.) is NOT in this set — Peter's hand-labeled corpus
still owns those calls when it lands.
