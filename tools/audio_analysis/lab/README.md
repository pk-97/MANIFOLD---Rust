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
`--plan` prints the run id. A net trains on the GPU (MPS); a child that would land on the CPU exits non-zero with one
line saying why, unless the command carries `--cpu` (a queue step passes it in `"args"`). Trees-only runs are exempt.

**Score and compare.** `lab/score.py RUN_ID [--layer net|trees|combined]` scores in seconds from the saves.
`lab/compare.py A B` trains whatever is missing, A and B interleaved, then prints the A/B on the real songs and,
apart, on the generated songs. The headline is the 90% interval over songs; seed spread is the second line. It
refuses a pair whose exam songs or labels differ, and one-net-per-song configs. Snare and kick reports also carry
`coincident`: recall on the labels within 20 ms of a label of the other family (snare against kick, kick against
snare), per song and pooled, beside overall recall; `compare.py` prints it under the exam A/B.
`--exam-of RUN_ID` (on `compare.py` and `score.py`) adds the same numbers restricted to the exam songs of that earlier
run's `meta.json`, headed by which subset and how many songs; `compare.py` prefixes those lines `old exam | `. It reads
saved per-song results only, so run ids, cached layers and the full-exam numbers do not move. Use it when the exam has
grown and you need a number comparable with a detector scored on the old exam.

**Overnight.** `lab/overnight.py PLAN.json [--dry-run]` (`scripts/dev.py lab-queue`) runs a JSON list of run and
compare steps one at a time under the GPU lock, writes a log per step and a summary, and stops at the first failure.

**Ship a detector.** `lab/release.py` (`scripts/dev.py detector-release`; rules in
`docs/DETECTOR_RUNTIME_DESIGN.md`). Save the config's run seeds (`lab/run.py CONFIG --seeds S[,S+1..]`), then
`release.py train CONFIG --seed S [--nets N]` (final models on every trained song, cutoffs from the run's outer
predictions) and `release.py export TRAIN_ID --set FAMILY --ship` (the model file, contract v2, and the Rust parity
goldens). A config using a part the app does not run (`runtime_parts.json`) is refused before anything trains.
`release.py train CONFIG --provisional` makes a throwaway release in minutes on the CPU, for building Rust against it.

**Test.** `lab/test_lab.py` (scorer and input parity against saves of the retired runners, kick labels and parts
against the kick records, the release tool against the shipped v1 goldens and the model file contract, plus the
guards; about 25 s).
