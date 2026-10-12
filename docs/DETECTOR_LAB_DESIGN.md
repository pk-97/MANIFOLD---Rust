# Detector Lab — one place to label, train and judge MANIFOLD's audio detectors

**Status:** SHIPPED · P1–P7 shipped (bass and synth parked by Peter); v2 kick and snare released 2026-10-12 (docs/DETECTOR_RUNTIME_DESIGN.md) · owed: Peter's live test
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs) and section 6 (seam briefs) before starting any phase.

The live rig fires visuals from detectors trained offline: kick today, snare next, then bass, synth, and later
modulation and envelope followers. Every one is judged the same way: held-out songs, a firing cutoff chosen on other
songs, A against B over several seeds. That loop is the instrument's quality gate, and today it is rebuilt per
detector out of environment switches and scratch scripts. On 2026-10-11 alone it produced a cutoff picked on generated
songs, a 25-minute re-score that rebuilt every song's audio and candidates, stale caches reused under a recycled song
name, and run-time estimates off by 70%. Peter: "we need a generalised system that you use that is as fast, accurate,
stable, and reliable. We will use this a lot for training our detectors for onsets, triggers, modulation, envelopes,
etc."

**The governing insight: the lab is mostly wiring.** Labels, candidates, the net, the scoring rules, the A/B yardstick
and even a content-addressed cache already exist (section 1 below). What is missing is one owner for four things:
which songs exist and where their audio comes from, whether their labels are true, what an experiment is, and how a
result is scored. The lab owns those four and calls everything else.

The binding constraint is **label truth**. Peter, 2026-10-11: "we can't claim anything about results unless we are
100% confident in our base data and labels." None of the runtime constraints bind: nothing here reaches the show
without a release step.

Companion docs:
- [AUDIO_EVAL_HARNESS_GUIDE.md](AUDIO_EVAL_HARNESS_GUIDE.md) section 0 (trained detectors) — the rules every trained detector follows; the lab enforces them.
- [DETECTOR_RUNTIME_DESIGN.md](DETECTOR_RUNTIME_DESIGN.md) section 3 (The release path) — the lab feeds the release path, never replaces it.

## 1. Audit — what exists (verified 2026-10-11, branch feat/detector-labels-v2 at e775c7068)

Extend, don't redesign. Paths are under `tools/audio_analysis/`.

| Piece | Where | State |
|---|---|---|
| Project reader, read-only, hash-checked; tempo map, clips, notes, locators | `eval/als_extract.py (extract)` | exists |
| Content-addressed cache: key = sha256 of source audio bytes + stage stamps; a stamp mismatch is a miss | `eval/bundles.py (content_hash_for_audio line 103, BundleStamp line 37, load_bundle line 120)` | exists; **the lab's cache precedent** |
| Snare label rules, known times only (MIDI on playing tracks, clip starts, stem-export mutes tested) | `eval/detector_labels.py (song, project_parts, export_groups)` | exists; `kind` is `'project'` (line 390) |
| Bass and synth note labels | `eval/note_labels.py (song)` | exists; never calls `export_groups`; `kind` `'project'` (line 216) |
| Generated label sets (V1, Rolls) | `eval/snare_generated.py` | exists, snare only, `kind` `'generated'`, no prominence filter (lines 63–66) |
| Labels schema (positives, unscored, loop_spans, hard_neg, positives_only, dense_pos, dropped, kind, timing_shift_ms) | `eval/detector_eval.py` module doc, `rows` (line 38) | exists; the lab's schema |
| Song set and audio for real songs | `eval/kick_goal_eval.py (Goal, line 156)` — which songs exist depends on `KICK_GOAL_MORE/TRIGGER/WIP/RECALL` (lines 118–135, 210–213); `eval/detector_songs.py (audio, spec)` | exists; **song identity hides in env vars** |
| The pinned recipe env | `snare_release.py (RECIPE_ENV, lines 24–26)` | exists; the only place the env is pinned |
| Name-keyed caches | `kick_goal_nn.spectrum_cache` (`nnspec_{t}`, line 80), `extra_caches` (`nnperc_`, `nnside_`, lines 96–120), `detector_songs.spec` (`nnspec_wip_`), `detector_channels.flat_cache` (`snare_flat_`, float16), `snare_wip/{t}.npy`, `kick_goal_eval` `features_{name}.npz` (line 217) | exist; all keyed by song name, spectra and flatness stored float16 |
| Candidates | `eval/detector_cands.py`, `eval/snare_cands.py`, `eval/detector_net.py (family_candidates, line 41)` | exist; recomputed every run |
| Net | `eval/kick_goal_nn.py (train line 193, predict line 231)`; settings read from env at import (lines 33–56; `EPOCHS` fixed at line 56) | exists; draws per song = median positives; synthetic clips (`synth=`) together get as many draws as the real songs |
| Held-out nets | `eval/detector_net.py (groups line 87, held_out_nets line 96)`: groups deal all sorted names round-robin; net k seeds `1000*k+seed` | exists |
| Scoring | `eval/detector_eval.py (fires, choose, held_out line 132, song_result)`; cutoffs skip generated and recall-only songs (line 140) | exists |
| A/B yardstick | `eval/detector_compare.py` | exists; hardcodes `RECALL_ONLY` (line 19) instead of reading labels |
| Runners | `eval/snare_net.py` (saves predictions; `--rescore` rebuilds audio and candidates, lines 55–63), `eval/detector_net.py (main)`, `eval/run_kick_goal_fast.py`, `eval/note_clean.py` | four runners, env switches, env-var-tag file names |
| snare_net dependants | `snare_release.py` (imports `snare_net.AHEAD`, reads `snare_net_s0_snare_past_ms190_snare_flat1.npz`, lines 58–61), `eval/snare_combo.py` (27, 32), `eval/snare_stack.py` (68–70) | exist |
| Tests | `eval/tests/` (unittest, imports as `eval.*` from `tools/audio_analysis`), e.g. `test_detector_eval.py`, `test_bundles.py` | exist; not run by any gate |
| GPU lock | `scripts/gpu_queue.py (hold)` | exists |

Not ours: `eval/fixtures.toml` and `additional_stem_sources.json` belong to the MIR fixture harness; the lab neither
reads nor replaces them.

Genuinely new: the song list with resolved sources, a raw-truth audit, experiment configs, a run store with cached
candidates, the guards. Everything else is called.

## 2. Decisions

**D1. The lab is a Python package, `tools/audio_analysis/lab/`, that calls `eval/` modules.** Three named edits to
`eval/` are approved, each keeping today's behaviour bit-identical by default, plus one speed-up with identical
output (`detector_eval.held_out` scores each song once per threshold, then sums: 124 s to 4 s on 44 songs, proven by
`test_scorer_parity`): `kick_goal_nn.train` gains
`epochs=EPOCHS` and `synth_share=0.5` parameters (D9); `detector_net.held_out_nets` passes `train_kw` to `train`,
records what each net trained on (`record`), and with one net per song gives the untrained songs one shared net
(`net_groups`); `detector_compare` reads recall-only songs from the report instead of `RECALL_ONLY` and exposes the
A/B as `compare()`. P6 adds, each named: `held_out` takes a cutoff `pool` (the pair models' predictions);
`inside` sorts spans once (identical output); `rows` counts a candidate exactly at the 35 ms edge as inside (`EDGE`,
1e-12, the legacy kick rule; a behaviour change at that float edge only); `detector_compare.compare` headlines a
paired song bootstrap; `kick_goal_eval.add_whole_song_truth` keeps its truth on the record (`train_source`);
`kick_goal_nn.Net` takes its channel count; `kick_goal_nn.train` takes `swa_epochs`, `batch`, `lr` and `precision`
(defaults reproduce today's training) and cuts slices on the song's device (bit-identical);
`detector_eval.song_result` adds the recall of hits after a gap (`after_gap`) and `detector_compare` prints it.
`held_out_nets` is retired: it could hold out one group with nets only,
and the lab's loop over the plan (`lab/run.py train_model`) calls `train` and `predict` for every model and pair
training. Nothing else in `eval/` changes. Rejected: re-implementing `train`, `held_out_nets` or the
scorer inside the lab, because a copy is a second truth that drifts. Rejected: a Rust harness, because the research
loop is torch and numpy.

**D2. The recipe env is pinned, and the song list records what it resolved to.** `lab/recipe.py` holds `RECIPE_ENV`
(moved from `snare_release.py`, which imports it) and applies it before any `eval/` import, exiting on a conflicting
value, exactly as `snare_release.py` does today. `lab/registry.py build` writes `lab/songs.json`: per song `id`,
`kind` (`project` | `generated`), `source` (resolved audio path or paths, how they combine, section bounds for
generated songs), `source_sha256` (of each source file's bytes), `offset` and `proof` (D5), and `audit_override`
(preserved across rebuilds, set only by Peter). `lab/registry.py verify` asserts each song still resolves to the
recorded paths and hashes. Rejected: no registry at all, because song identity would stay hidden in five env vars.

**D3. Caches extend `eval/bundles.py`'s keying: source-file sha256 plus a stamp.** The stamp names the loader
version and every setting that changes the artifact (spectrum bands, candidate rule and dB, channels). Artifacts are
stored as today's dtypes (spectra and flatness float16), so net inputs are byte-identical to the legacy path.
Generated songs key on the export's sha plus section bounds. Hashing reads source bytes once per run (seconds), never
the decoded mix. The lab never calls the name-keyed helpers (`spectrum_cache`, `extra_caches`, `detector_songs.spec`,
`flat_cache`, `pitch_cache`); it calls the pure builders (`spectrum`, `flatness`) through its own cache. Rejected:
hashing the decoded mix, because it forces a full decode every run.

**D4. The labels schema stays the existing dict; `kind` keeps its values.** `kind` is `project` or `generated`
(today's values); the exam is `kind == 'project'` and not `positives_only`. Builders add `sources` (positives per
source: `midi`, `clip_start`, `generated`) and `builder` (module and content hash). Audit results never go into the
labels file: they live in a sidecar `<labels>.audit.json` keyed by the labels file's sha (D5), so auditing never
changes the labels sha.

**D5. The audit checks offsets and note truth, not loudness.** Per project song it records known times found,
kept, dropped as not prominent, the fitted timing shift, and the offset proof: `wip:<status>` (detector_wip),
`lab:<status>` (`lab/offsets.py`, the same kick-and-snare grid fit run on a goal song's own mix, counted only when it
lands within 1 ms of the offset the labels use), `goal:trigger ...`, `goal:kick stem ...`, or `none`. A project song
**fails** when its proof is not proven or its timing shift sits at the ±30 ms search edge. The kept share is reported,
never failed: quiet fills and rolls are dropped by design. Peter, 2026-10-11: "we should focus on the snares on the
main off beats, not all of the little fills and percusions." A generated song fails when a label is not a snare or
clap note of its section, or such a note has no label. Failing songs are excluded and listed in every report until
Peter sets `audit_override`. **Default with trigger:** if more than 3 project songs fail, stop and show Peter the
table before excluding any. (First run, 2026-10-11: the trigger fired on four songs whose offsets had never been
recorded as proven; `lab/offsets.py` proved all four at their existing offsets.) Rejected: an attack-alignment test,
because snare positives already passed a 9 dB attack test (`prominent`) and would pass by construction, while certain
quiet MIDI notes (DnB ghost snares) would fail it.

**D6. An experiment is a JSON config file; A/B is two configs.** `lab/experiments/<name>.json`: `family` (its labels
files are `registry.FAMILIES[family]`), `candidates` (`snare:4.5` or a `detector_net.family_candidates` rule), `net` (`past_ms`, `ahead_ms`, `flat`,
`epochs`), `sampling` (D9), `train` (`["project"]` or `["project", "generated"]`), `seeds`, `folds` (0 = one net per
song). Each (config, seed) trains in its own subprocess: `lab/run.py` sets the `KICK_GOAL_NN_*` env from the config,
imports `eval/`, and asserts the module constants equal the config before training. The run id hashes the config, the
labels sha, and the contents of every `tools/audio_analysis` module the subprocess imported (from `sys.modules`).
Uncommitted edits change it. Edits to modules it never imports don't.

**D7. Runs save predictions and candidates; scoring reads only saves.** A run writes
`~/.cache/manifold/detector-lab/runs/<run_id>/` with `s<seed>.npz` (held-out probabilities per song), `meta.json`
(config, song order, groups, the songs each net trained on, timings), and per-song candidate arrays (onset, emit,
avail) in the D3 cache. `lab/score.py RUN_ID` and `lab/compare.py` read only those plus the labels: seconds, no audio,
no GPU. The legacy 25-minute re-score rebuilt audio and candidates; cached candidates remove exactly that.

**D8. Scoring rules are code, not config.** Exam = `kind == 'project'`, audit-passing songs. Cutoffs are chosen per
held-out song on the other exam songs (`detector_eval.held_out`, line 140). Generated songs train only and are
reported in a separate section. Peter, 2026-10-11, on the generated sets: "Train only. The held-out exam stays on real
songs."

**D9. Sampling is explicit.** `"sampling": {"per_song": "median", "generated": "songs"}` reproduces the legacy
`SNARE_GEN=train` runs: generated songs train as ordinary songs, each drawing the median like a project song.
`"generated": 0.35` passes them to `train` as `synth=` clips that together get that share of all draws (0.5: as many
as the project songs; D1's named edit). `generated` is set exactly when `train` holds `"generated"`. `per_song` stays `median` in v1; label-proportional draws are the
label-rich sampling bead's job (`bd show BUG-9ngk8.5`). The policy is printed in every report. `DETECTOR_DRAW_CAP`
goes when bass and synth move (P5).

**D10. Every run states its finish time from a warm seed.** The runner times the first seed after caches are warm and
prints the projected finish; the report records actual times. Training holds `gpu_queue.hold('detector-lab')`.

**D11. Migration is proven by input parity and scorer parity, not training parity.** MPS kernels promise no
bit reproducibility (though seed 0 of `snare_base` did reproduce the legacy save byte for byte on 2026-10-11). So: (a) **scorer parity** — `lab/score.py` on a legacy
`snare_net_s*.npz` reproduces the legacy results JSON exactly (deterministic, CPU); (b) **input parity** — for the same
config the lab builds `held_out_nets` arguments equal to the legacy runner's: same song order, same groups (generated
songs included where legacy includes them), same seeds, byte-identical `Song` tensors. Equal inputs into the same
function is the proof. Training-level agreement is reported, not gated.

**D12. v1 is event detectors.** Curves (modulation depth, envelopes) need Ableton automation, which `als_extract` does
not read. Deferred with a trigger (section 8 below).

### Consequences, stated honestly

The song list pins absolute Dropbox paths: it is Peter's machine's list. The lab's cache duplicates part of the 7.8 GB
legacy cache until the kick migrates. The audit's thresholds (0.25 kept share, shift at the search edge) are first
guesses; the trigger in D5 makes the first run a review with Peter, not a silent exclusion. Kick-goal songs keep their
legacy audio loader (master or stem sum) behind the recipe env; the lab proves it reads the same bytes, it does not
re-derive them.

## 3. Data model

```python
# lab/recipe.py
RECIPE_ENV: dict[str, str]          # moved from snare_release.py; applied before eval/ imports
def apply() -> None: ...            # sets env, exits on a conflicting value

# lab/registry.py
@dataclass(frozen=True)
class Song:
    id: str
    kind: str                        # 'project' | 'generated'
    source: dict                     # {'paths': [...], 'how': 'goal'|'wip'|'section', 'start_s'?, 'end_s'?}
    source_sha256: tuple[str, ...]
    offset_s: float | None
    proof: str                       # 'wip:...' | 'goal:...' | 'generated' | 'none'
    audit_override: str | None = None

def load() -> dict[str, Song]: ...
def mix(song: Song) -> np.ndarray: ...   # the legacy loader's output for 'goal'/'wip'; a section cut for 'generated'

# lab/cache.py  (keying shaped like eval/bundles.py)
def artifact(song: Song, stamp: dict, build: Callable[[], np.ndarray]) -> np.ndarray: ...

# lab/config.py
@dataclass(frozen=True)
class Experiment:
    name: str; family: str; candidates: str
    net: dict; sampling: dict; train: tuple[str, ...]; seeds: tuple[int, ...]; folds: int
```

Report JSON: the `write_results` layout (`results.net.per_song`) plus `exam`, `generated`, `excluded`, `recall_only`,
`sampling`, `timings`, `run_id`, one file per seed (`report_s<seed>.json`). Exam songs sit under `results.net.per_song`;
generated songs under `generated`. `detector_compare` reads `recall_only` from it.

Commands (`scripts/dev.py` verbs `lab-songs`, `lab-audit`, `lab-offsets`, `lab-test`, `lab-run`, `lab-score`, `lab-compare`):

```
lab/registry.py build | show [SONG] | verify
lab/audit.py FAMILY                     # writes <labels>.audit.json, prints the table and failures
lab/run.py CONFIG [--seeds 0,1]         # one subprocess per seed; saves predictions; prints ETA (--plan, --warm)
lab/score.py RUN_ID | --legacy NPZ      # seconds; writes report.json
lab/compare.py CONFIG_A CONFIG_B        # runs what is missing (interleaved seeds), scores both, prints the A/B
lab/test_lab.py                         # the lab's tests (unittest, run directly)
```

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Cutoffs are chosen only on exam songs | `test_lab.py::test_cutoff_ignores_generated_and_recall_only` |
| The exam holds only project, audit-passing songs | `run.py` asserts before training; `test_lab.py::test_exam_rejects_generated` |
| No net predicts a song it trained on | `run.py` asserts train ∩ group = ∅ per net, recorded in `meta.json`; `test_lab.py::test_held_out_disjoint` |
| A/B compares the same exam songs and labels sha | `compare.py` refuses otherwise; `test_lab.py::test_compare_refuses_mismatch` |
| Caches key on content, never names | `test_lab.py::test_recycled_name_misses_cache`; gate `rg "spectrum_cache\|extra_caches\|flat_cache\|pitch_cache\|detector_songs.spec\|snare_wip/" tools/audio_analysis/lab` → 0 |
| The lab scores like the legacy runner | `test_lab.py::test_scorer_parity` (legacy npz → identical per-song results) |
| The lab feeds training what the legacy runner did | `test_lab.py::test_input_parity` (song order, groups, seeds, tensor bytes) |
| The recipe env is pinned | `recipe.apply` exits on conflict; `test_lab.py::test_recipe_conflict_exits` |
| Auditing never changes the labels sha | sidecar file; `test_lab.py::test_audit_keeps_labels_sha` |
| A song's copies (gain, mix variant) share one song's draws and weight; copy-less songs train unchanged | `kick_goal_nn.train` versions, `models.weights`; `test_lab.py::test_versions_share_one_draw_budget`, `test_versions_share_weight`, `test_train_sampling_matches_head` |
| A mix variant trains only on a proven offset, its labels moved by it | `lab/offsets.py --variants`, `registry.variants_of`; `test_lab.py::test_mix_variants` |
| The release holdout is never in a dev exam or cutoff pool | `run.setup`, `score.without_holdout`; `test_lab.py::test_release_holdout` |

## 5. The plausible-wrong turns, by name

- **"Add one more env var to `snare_net.py`."** No. Config fields; the legacy runner is deleted in P4.
- **"Gate on training parity."** No. MPS is not reproducible; gate on inputs and on the scorer (D11).
- **"Call `spectrum_cache` or `flat_cache`, they already cache."** By name. Call the builders through the lab cache.
- **"Store spectra as float32 in the new cache."** No. Net inputs would change; keep today's dtypes.
- **"Check labels by attack strength."** No. That measures loudness; snare positives pass by construction (D5).
- **"Lower the audit threshold so a song passes."** No. `audit_override` with Peter's reason.
- **"Re-implement `train` or the scorer in the lab."** No. Call them; D1's named edits are the only ones. The loop
  over trainings is the lab's since P6 (D1).
- **"Write the audit into the labels file."** No. Sidecar (D4).

## 6. Phasing

### P1 — Recipe, song list, content cache, audit (one session)

- **Entry state:** `git -C <slot> log --oneline | rg e775c7068` hits; `rg "def locators" tools/audio_analysis/eval/als_extract.py` hits; `rg "kind'\) != 'generated'" tools/audio_analysis/eval/detector_eval.py` hits.
- **Read-back:** this doc sections 2–5; `eval/bundles.py`; `snare_release.py` lines 1–40; `eval/kick_goal_eval.py`
  lines 100–220; `eval/detector_songs.py`; `eval/detector_labels.py (song)`; `eval/snare_generated.py`. Restate D2–D5
  and the forbidden moves.
- **Deliverables:** `lab/__init__.py`, `lab/recipe.py` (and `snare_release.py` importing it), `lab/registry.py`,
  `lab/cache.py`, `lab/audit.py`, `lab/songs.json`, `lab/test_lab.py` with `test_recycled_name_misses_cache`,
  `test_recipe_conflict_exits`, `test_audit_keeps_labels_sha`; `detector_labels.song`, `snare_generated` write
  `sources` and `builder`; `lab/offsets.py`, `lab/offset_proofs.json`; `dev.py` verbs `lab-songs`, `lab-audit`, `lab-offsets`, `lab-test`; `scripts/TOOLS.md` regenerated.
- **Gate:** `registry.py build` lists 24 project and 20 generated snare songs; `registry.py verify` prints, per song,
  max abs diff 0 between `registry.mix` and the legacy loader under the recipe env; `audit.py snare` prints the table;
  `test_lab.py` passes; the section-4 cache rg gate → 0.
- **Demo:** the audit table, L1. If the D5 trigger fires, the phase ends with the table sent to Peter.
- **Forbidden:** name-keyed caches; float32 artifacts; editing `kick_fusion_*`; touching kick labels.

### P2 — Run store, scorer, guards, parity (one session)

- **Entry state:** P1 committed; `lab/test_lab.py` green.
- **Read-back:** `eval/snare_net.py`, `eval/detector_net.py (groups, held_out_nets)`, `eval/kick_goal_nn.py (train,
  predict)`, `eval/detector_eval.py`, `eval/detector_compare.py`. Restate D6–D11.
- **Deliverables:** `lab/config.py`, `lab/run.py`, `lab/score.py`, `lab/compare.py`; D1's two named edits;
  `lab/experiments/snare_base.json` (`snare:4.5`, past 150, ahead 40, flat off, folds 4, median, train project),
  `snare_generated.json` (same, train project and generated), `snare_release.json` (past 190, flat on, folds 0: the
  shipped recipe); tests `test_cutoff_ignores_generated_and_recall_only`, `test_exam_rejects_generated`,
  `test_held_out_disjoint`, `test_compare_refuses_mismatch`, `test_scorer_parity`, `test_input_parity`.
- **Gate:** `test_lab.py` passes (parity tests run on CPU against the committed legacy files
  `snare_net_s0_snare_gen*_snare_folds4.npz` copied into `lab/tests/golden/`); one seed of `snare_base` runs end to
  end and `score.py` on it prints its time, under 30 s.
- **Demo:** none — L1.
- **Forbidden:** scoring during training; training-parity gates; adapters around `snare_net.py`.

### P3 — The generated-data A/B on the lab (one session, GPU)

- **Entry state:** P2 committed.
- **Deliverables:** `compare.py snare_base snare_generated` over seeds 0–4; three sampling variants of
  `snare_generated` (`sampling.generated` 0.2, 0.35 and 0.5) compared against `snare_base`; the report files.
- **Gate:** the reports exist; ETA within 20% of actual for warm seeds.
- **Demo:** the reports, read by Peter, L2.

### P4 — Snare legacy deleted, how-to (one session)

- **Entry state:** P3 committed.
- **Deliverables:** `snare_release.py` trains from `snare_release.json` through the lab and reads its cutoff from the
  lab run; `eval/snare_combo.py` and `eval/snare_stack.py` read lab runs or are deleted (each named in the commit);
  `eval/snare_net.py` deleted; AUDIO_EVAL_HARNESS_GUIDE.md section 0 (trained detectors) points to the lab;
  `lab/README.md` (add a song, build labels, audit, run, compare).
- **Gate:** `rg "snare_net|SNARE_GEN|SNARE_FOLDS" tools/ scripts/` → 0; `snare_release.py train --help` runs.
- **Demo:** none — L1.

### P5 — Bass and synth (`bd show BUG-ymy4z.1`: drop the 300 cap, train on generated sets)

- **Entry state:** P4 committed.
- **Deliverables:** generated bass and synth labels from V1 and Rolls (tracks 11–18 bass, 2–9 synth);
  `note_labels.song` calls `export_groups` like the snare; the D5 audit for note families (kept share uses
  `note_labels`' presence filter, not attacks); `detector_net.py main` and `DETECTOR_DRAW_CAP` deleted;
  `lab/experiments/{bass,synth}_base.json` and `_generated.json`.
- **Gate:** `rg "DETECTOR_DRAW_CAP" tools/audio_analysis` → 0; bass and synth A/B over seeds 0–4 reported.

### P6 — Components: any detector built from parts, the kick first

Peter, 2026-10-11: detectors are built "with component style designs and architectures", "a tree, or nn, or both",
"with all of the DSP measurements too as components ... for any general detector". Kick-stem attacks count as known
times ("the data for kicks is good and it works well in app already").

**Parts.** A config names one part per slot; every part is a registered function in `lab/components.py`.

1. *Candidate finder* — where a hit might be. `kick` (`kick_fusion_bandwise.fusion_features`, its own hop),
   `snare:DB`, `rise:DB` / `change:DB` / `union:R:C` in the family band. Gives candidate hops, emission hops and the
   hop size; the scorer stamps with that hop, never a hard-coded 256.
2. *Measures* — numbers per candidate from the audio and the candidates only, causal (audio up to the emission plus
   the net's look-ahead), never fitted: `f15` (the kick finder's 15 columns), `tail3`, `prof32`, `low16`, `fall16`,
   `res16`, `glide3`, and per-frame `flat` (noisiness, a net channel). Each is `f(audio, sr, candidates) -> (n, k)`,
   cached by song content, finder and the measure's code. Any family may use any measure. A measure that needs
   fitting is a model: the kick's sound templates (`tmpl3`) are a trees option fitted inside each training
   (`lab/templates.py`).
3. *Models* — `net` (kick_goal_nn's CNN on the spectrum, measure channels added) and `trees` (HistGradientBoosting
   on a list of measures, the kick fast loop's settings, `random_state` = seed). One or both.
4. *Combine* — the one model, or `mean_logit` over both (the kick's blend).
5. *Stage* (optional) — the song-history stage (`lab/stage.py`), any family: `kick_goal_selfsim` features of the
   combined probability over each of the song's last `windows_s` (shape measures and an optional level column named
   in the config), then trees. It holds no timing: nothing predicts when the next hit is due.
6. *Decision* — cutoff chosen on held-out exam songs, 60 ms refractory. Rules, not config (D8).

Configs: `net` moves under `models`; `combine` and `stage` are new keys. The three snare configs and the bass and
synth configs migrate mechanically (`combine: "net"`, `stage: null`).

**Training, per seed.** Song groups as now. For group k every model trains on the other groups and predicts k.
The stage of group k trains on combined predictions of each other group j from models that saw neither k nor j
(4 groups: 4 + 6 trainings per model, seconds each). Group k's cutoff comes from stages learnt without groups k and
j, each judging group j on its pair (k, j) inputs (`pair{k}__staged`). A stage needs `folds` >= 2; with one net per
song the pairs run to hundreds, so the config is refused. This is stricter than the legacy fast loop, whose nets had
seen the scored song when they built its stage inputs. Trees and the stage train on the net's rows
(`detector_eval.rows`) with the kick's weighting: song-equal, class-balanced, vouched negatives `HARD_W` times as
likely (the shipped kick's stage fit had no vouched-negative weighting).

**Saves.** `s<seed>.npz` keys are `<layer>/<song>`: each model, `combined`, `staged`. `score.py` scores the last
layer by default and any layer by name, so "what did the stage add" needs no retraining. `meta.json` records every
training's songs, and `run.py` refuses a plan where any layer of a song came from a model that saw it.

**Kick labels in the lab format.** `eval/kick_labels.py` writes `GOAL/kick_labels.json` for the kick goal set (the
songs `kick_release` trains on): positives are the song's whole kick truth (kick-stem notes, fresh attacks and
pitch restarts; trigger kicks; reviewed labels where there is no stem); unscored are the first and last second, the
uncertain and doubtful spans, and kick rolls (`detector_labels.roll_spans`, Peter's rolls ruling); vouched
negatives are melodic note starts far from kicks (`kick_goal_melodic`); recall songs are positives-only. One truth
per song for training and scoring: the legacy loop scored dev songs on reviewed passages but trained on whole songs.
Project songs outside the goal set (the catalogue's label-ready songs, `detector_wip.PAIRS`) have no record: `kick_labels.py
project` writes `GOAL/kick_labels_project.json` by the shared labeller's rules with the kick as the family (`own_kicks`:
kicks are never negatives, negatives keep 105 ms from every kick note; attacks judged in 100-400 Hz, where the sub and
bass do not hide them).

**Consequence, stated plainly.** Lab kick numbers use a different truth (whole songs, rolls out) and the shared
scorer (detector_eval), so they are not comparable with the shipped 0.889 / 0.894. Proof of the move is part
parity, not score parity: the lab's kick candidates and measures equal `kick_release.features()` byte for byte on
three songs, and the lab's trees on those rows equal a direct sklearn fit. The shipped kick model is untouched
until Peter's yes on a lab-trained one (P7).

**Fable review, 2026-10-11, all taken.**
- *Kick labels* come from the `Goal(mode='v3')` records after `add_whole_song_truth`, never re-derived: WIP section
  songs, drum-bus and other-stem doubt, reviewed passages, song lag and one-per-refractory all ride along. Gate: per
  song, `detector_eval.rows(labels, legacy candidates)` equals the record's `(train_mask, train_y)` before rolls are
  subtracted. Vouched negatives are melodic notes more than 105 ms from any kick (the legacy rule's 70 ms kick
  distance plus the 35 ms row window), so `rows` needs no kick rule.
- *Proof kind `stem`:* positives measured on an isolated stem need no project offset; the audit accepts it.
- *Cutoffs from models that never saw the group:* every lab run with groups also trains the pair models (without
  groups k and j) and chooses group k's cutoff on their predictions for the other groups. A one-net-per-song run
  cannot, so `compare.py` refuses it; its score is labelled optimistic.
- *Folds:* kick A/Bs without a stage run on 4 groups. The stage chooses its own folds when it is built; on 8
  groups its training models would see 6/8 of the songs and applied ones 7/8.
- *Cache keys* cover a builder's closure: its source, the constants it reads and the helpers it calls in
  `tools/audio_analysis`.
- *Two measure kinds:* `frame` measures (T, k on the finder's hop grid; `flat`, later envelopes) feed the net as
  channels; `event` measures (n, k at candidates) feed trees. Hop and the candidate-to-emission wait are finder
  properties. `kick_goal_nn.Net` takes its channel count (named edit).
- *Comparisons* headline a paired bootstrap over songs (90% interval of the pooled F1 difference); seed spread is
  the second line.
- *Kick scorer semantics:* the shared scorer counts a label caught by any fire within the window; the legacy kick
  matcher paired fires and labels one to one. Stated next to the 0.889 / 0.894 caveat.
- *Saves:* npz keys `layer__song`; `meta.json` carries a schema version and old metas migrate on read; a song in two
  labels files of one family is an error.
- *Scope:* first pass is finder, `f15` `tail3` `prof32` `low16` `flat`, trees, net, combine and the pair cutoffs.
  The stage comes once `kick_blend` matches a direct fit; `fall16`, `res16`, `glide3` and the templates after.

- **Entry state:** P5 committed.
- **Deliverables:** `lab/components.py` (finders, measures, models, combine, stage registries); config schema v2
  and migrated configs; layered saves and `score.py --layer`; `eval/kick_labels.py` and its audit; configs
  `kick_net`, `kick_trees`, `kick_blend` (trees + net) and `kick_staged` (+ stage); tests for measure parity, tree
  parity, nesting refusal, layer scoring and config migration.
- **Gate:** `test_lab.py` green; `compare.py kick_blend kick_staged` over seeds 0–4 reported; a Fable review of the
  whole lab before Peter's next data set lands (Peter, 2026-10-11: "have Fable review it so we're working from a
  strong foundation").
- **Forbidden:** fitted measures outside the fold; a second scorer for the kick; touching the shipped kick model.

**As built (first pass, 2026-10-11).** Finder, measures `f15` `tail3` `prof32` `low16` (event) and `flat` (frame),
net, trees, `mean_logit`, pair cutoffs, layered saves, kick labels. Owed: the stage, `fall16` `res16` `glide3`, the
Fable review.
- Kick labels: 30 songs, 4733 kicks, 129 in rolls left out; rows() equals the records on every song. got_it, renew
  and overwhelming_force fail the audit (unproven project offsets) and stay out, as Peter ruled for bass and synth.
- A trained model's predictions are stored under a key of everything that decides them (`run.py model_key`), so a
  config that needs the same model reuses it: an A/B that changes only the trees retrains no nets.
- Cost per kick seed on 4 groups (10 trainings): an outer net trains in 10 s, so nets take under 2 min; trees
  were 50 s for 36 fits on 4 threads; scoring 3 s. Snare seed on 4 groups (10 nets): 20 s; scoring 2 s.
- Net training options, each a config key with today's training as the default (a schema 2 net without them
  migrates to the defaults): `swa_epochs` (equal-weight average of the last N epochs' weights; the net has no
  BatchNorm, so nothing to recompute), `batch` (256), `lr` (0.002), `precision` (`float32` or `mixed`: float16
  autocast with a loss scaler). Slices are cut on the device holding the song; the trained net is bit-identical.
  Against `snare_gen_share50` over seeds 0-4: `snare_gen50_swa` +0.16 F1 points (90% -0.54 to +0.90);
  `snare_gen50_fast` (batch 1024, lr 0.004, mixed) +0.01 (-2.34 to +2.23) on real songs but -12 on generated ones
  and only 10% faster. Neither is adopted.
- Training i of the plan uses random state 1000 i + seed (outer nets keep the retired runner's seeds).
- On a snare save the pair cutoffs and the old leave-one-out cutoffs scored 0.806 and 0.802: the rule is sound and
  costs nothing measurable.
- Fable review (2026-10-11), taken: sources re-hashed at setup and audio content in run ids and model keys; a model
  keyed on the labels of the songs it trains on and on its training code's closure; code_id follows container
  constants and cache-wrapped helpers, artifacts carry the loader's code; scored minutes merge spans and false fires
  per minute leave recall-only songs out; scoring refuses saves that do not line up with the candidates.
- Generated sets are descriptors (`eval/generated_sets.py`): project, full-mix and Drums exports, explicit start,
  sections. Each set's timing is proven on its Drums stem before labels are written (isolated drum hits: V1 0 ms,
  Rolls 0 ms, Modulations +1 ms median). Hits cut by a section's edge are unscored. `eval/kick_generated.py` labels
  generated kicks; the Modulations set adds 58 sections (kick and snare).
- Sound templates (`trees.templates`: null or `{"k": K}`, `lab/templates.py`), any family: each training fits K
  k-means templates on mix onset patches at its own trained songs' and clips' hits (200 per song at most, songs
  weighed like tree rows), then the shipped kick's three similarities join the trees' inputs. Null reproduced
  `kick_gen50_gated`'s saves byte for byte over seeds 0-4. Against it, real songs: K 8 -0.11 F1 points (90% -0.47
  to +0.18), K 16 -0.61 (-0.93 to -0.25), K 32 -0.18 (-0.34 to -0.03). On the snare hybrid: K 8 -0.68 (-1.42 to
  +0.23), K 16 -0.41 (-1.31 to +0.47), both with more false fires. Not adopted: the net already hears the sound.
- Song-history stage (`lab/stage.py`), any family, with the gate (history's change measured against the detector's
  and the stage's own cutoffs, scaled by memory evidence). Ungated, history cost drop kicks (first kicks after a
  2 s gap 94 to 89 percent) because the staged cutoff sat far above an untouched kick's score. Gated, against no
  history: kick +0.80 F1 points (90% +0.18 to +1.52, learning on real songs only; first hits after a gap 93 vs 94
  percent), snare +1.32 (+0.46 to +2.35, learning on all songs).
- Snare hybrid (`snare_gen50_hybrid_low16`: trees on prof32 fall16 low16, mean logit, history): +2.45 F1 points
  over the net with history (+0.20 to +5.23), recall 0.84 to 0.90; history on the hybrid +0.45 (0.00 to +0.94).
- Recipes chosen for release (P7): kick `kick_gen50_gated` (net, trees, history learnt on real songs), snare
  `snare_gen50_hybrid_low16` (net, trees, history learnt on all songs). got_it, renew and overwhelming_force joined
  the kick exam on Peter's ruling (songs.json audit_override, with the label timing evidence): 30 kick songs.
  Superseded 2026-10-12 by `kick_v2_final` and `snare_v2_mid` (docs/DETECTOR_RUNTIME_DESIGN.md section 5.5 and its
  recipe table): kick +1.97 F1 points over `kick_v2_base` (90% +1.01 to +3.14), snare +9.35 over `snare_v2_base`
  (+5.75 to +12.76) on the 37-song whole-song exam.

### P7 — Releases from component configs

`snare_release.py` and `kick_release.py` fit their final parts from one lab config each and take the cutoff from
that config's run. Ships only with Peter's yes.

## 7. Decided — do not reopen

1. `lab/` calls `eval/`; the named edits in D1 (P1–P6), nothing else.
2. The recipe env is pinned in one module; the song list records resolved sources and their hashes.
3. Caches key on source bytes plus a stamp (`bundles.py` keying), today's dtypes.
4. The labels dict is the schema; `kind` stays `project` | `generated`; audit goes in a sidecar.
5. The audit checks raw known times, kept share, shift and proof; overrides are Peter's.
6. Experiments are config files; each seed trains in its own subprocess; scoring reads saves.
7. Exam = project, audit-passing songs; cutoffs on exam songs; generated data trains only.
8. Parity = inputs and scorer, never training.

## 8. Deferred

| What | Revive when |
|---|---|
| Curves (modulation, envelopes) as labels and a curve scorer | `als_extract` reads automation, or Peter asks for an envelope detector |
| Label-proportional draws (`per_song: "labels"`) | the label-rich sampling bead (`bd show BUG-9ngk8.5`) is picked up |
| Running `eval/tests/` in a gate | a lab test or eval test breaks unnoticed |
| Retiring the 7.8 GB legacy cache | P7 committed |
