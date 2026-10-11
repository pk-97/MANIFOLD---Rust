# Detector Lab

One place to label, train and judge the trained audio detectors. Rules and reasons: `docs/DETECTOR_LAB_DESIGN.md`.
Every script applies the pinned recipe itself; never set `KICK_GOAL_*` or `SNARE_*` env by hand.

**Add a song.** Build its labels with the family's builder (`eval/snare_labels.py` / `eval/kick_labels.py` for
project songs, `eval/snare_generated.py` for Peter's generated sets), then `lab/registry.py build` and `lab/registry.py verify SONG`
(the audio the lab reads matches the legacy loader byte for byte).

**Audit the labels.** `lab/audit.py FAMILY`. A project song fails without a proven export offset; prove one with
`lab/offsets.py SONG`. Training refuses labels whose audit is missing or older than the labels file.

**Build a detector.** A config in `lab/experiments/` names its parts (every key required): a candidate finder
(`kick`, `snare:DB`, a band rule), models (`net` with frame measures as extra channels, `trees` on event measures,
or both), how they combine (`net`, `trees`, `mean_logit`) and an optional song-history `stage` (`lab/stage.py`: the
song's recent confident hits re-score each candidate; needs song groups). Parts live in `lab/components.py`; any
family may use any measure. A new measure is one function there, cached by song audio, finder and the function's code.
Trees may add sound templates (`"templates": {"k": K}`, `lab/templates.py`): fitted inside each training on its own
songs' hits, never on the songs it predicts.

**Run an experiment.** `lab/run.py CONFIG` trains the missing seeds, one process each, and saves held-out
predictions per layer under a run id that changes whenever the config, the labels or the code changes. With song
groups it also trains the pair models that choose each song's cutoff without ever seeing it. A model another config
already trained is reused, so changing only the trees retrains no nets. `--warm` builds the cached inputs first;
`--plan` prints the run id.

**Score and compare.** `lab/score.py RUN_ID [--layer net|trees|combined]` scores in seconds from the saves.
`lab/compare.py A B` trains whatever is missing, A and B interleaved, then prints the A/B on the real songs and,
apart, on the generated songs. The headline is the 90% interval over songs; seed spread is the second line. It
refuses a pair whose exam songs or labels differ, and one-net-per-song configs.

**Ship the snare.** `lab/run.py snare_release --seeds 0`, then `snare_release.py train` and
`snare_release.py export --out DIR`.

**Test.** `lab/test_lab.py` (scorer and input parity against saves of the retired runners, kick labels and parts
against the kick records and `kick_release`, plus the guards; about 10 s).
