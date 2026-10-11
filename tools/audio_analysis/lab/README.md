# Detector Lab

One place to label, train and judge the trained audio detectors. Rules and reasons: `docs/DETECTOR_LAB_DESIGN.md`.
Every script applies the pinned recipe itself; never set `KICK_GOAL_*` or `SNARE_*` env by hand.

**Add a song.** Build its labels with the family's builder (`eval/snare_labels.py` for project songs,
`eval/snare_generated.py` for Peter's generated sets), then `lab/registry.py build` and `lab/registry.py verify SONG`
(the audio the lab reads matches the legacy loader byte for byte).

**Audit the labels.** `lab/audit.py snare`. A project song fails without a proven export offset; prove one with
`lab/offsets.py SONG`. Training refuses labels whose audit is missing or older than the labels file.

**Run an experiment.** A config in `lab/experiments/` (every key required). `lab/run.py CONFIG` trains the missing
seeds, one process each, and saves held-out predictions under a run id that changes whenever the config, the labels
or the code changes. `--warm` builds the cached inputs first; `--plan` prints the run id.

**Score and compare.** `lab/score.py RUN_ID` scores in seconds from the saves. `lab/compare.py A B` trains whatever is
missing, A and B interleaved, then prints the A/B on the real songs and, apart, on the generated songs. It refuses a
pair whose exam songs or labels differ. Seed noise is 1-3 points: read the ± before calling a win.

**Ship the snare.** `lab/run.py snare_release --seeds 0`, then `snare_release.py train` and
`snare_release.py export --out DIR`.

**Test.** `lab/test_lab.py` (scorer and input parity against a save of the retired snare runner, plus the guards).
