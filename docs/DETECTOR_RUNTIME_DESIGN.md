# Detector Runtime — one app runtime that runs any lab-built detector from its model file

**Status:** SHIPPED · kick-2026-10-12-60984540f and snare-2026-10-12-b5ccd28c9 (the snare reads the kick, section 5.5) · owed: Peter's live test of the v2 pair
**Prerequisites:** Detector Lab P6 (landed on main, `5461d2fee`); the snare port (`feat/snare-port`) merged into this work's branch (section 9, P0).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs) and section 6 (seam briefs) before starting any phase.

<!-- index: How lab-built detectors (kick, snare, any family) ship: one release tool, a self-describing model file (contract v2), one generic Rust runtime on the detector worker, per-part parity. -->

The lab builds a detector from named parts: a candidate finder, measures, a net, trees, a combine, an optional
song-history stage, a cutoff. The app runs two detectors, each hand-wired to one recipe (`KickDetector`, and
`SnareDetector` on `feat/snare-port`), so every recipe the lab picks costs Rust work. **The governing insight: the
lab's parts list is already a contract; write it into the model file and let one Rust runtime read it.** Both shipped
models are expressible in that contract with their numbers unchanged, so the generic runtime proves itself on today's
goldens before it replaces anything, and the live kick never changes behaviour.

Peter, 2026-10-11 (DETECTOR_LAB_DESIGN.md P6): detectors are built "with component style designs and architectures",
"a tree, or nn, or both", "with all of the DSP measurements too as components ... for any general detector".

The first two releases are already chosen (DETECTOR_LAB_DESIGN.md P6, "Recipes chosen for release"):

| | finder | net | trees | combine | history stage |
|---|---|---|---|---|---|
| kick `kick_gen50_gated` | `kick` | 150 ms, no extra channels | f15 tail3 prof32 low16 | mean_logit | 8 s; shape prof32 low16; level f15 column 0; learn project; gate 1 |
| snare `snare_gen50_hybrid_low16` | `snare:4.5` | 150 ms, no extra channels | prof32 fall16 low16 | mean_logit | 8 s; shape prof32; no level; learn all; gate 1 |

Superseded 2026-10-12 by the v2 recipes, chosen by drop-one ablation (each kept input earns its place in F1 or
false fires):

| | finder | net | trees | combine | history stage |
|---|---|---|---|---|---|
| kick `kick_v2_final` | `kick` | 150 ms, flat and perc, 1 net | f15 tail3 prof32 fall16 low16 note13 cdom4 | mean_logit | as kick_gen50_gated |
| snare `snare_v2_mid` | `snare:4.5` | 150 ms, flat and perc, 3 nets | prof32 fall16 low16 note13 cdom4 kick2 | mean_logit | 8 s; shape prof32 low16; level prof32 column 29; learn all; gate 1 |

Neither uses templates. Seed averaging (several nets, mean of their logits) gained about one F1 point on the snare,
so the file carries a net count (default 1).

On stage this means: a lab win ships by swapping one file and running one parity test. If the recipe uses a part the
app cannot run, the app's test suite refuses the file before it ships; it never degrades silently mid-set.

Binding constraints (DESIGN_AUTHORING.md section 1 (the intake)): **hot path** — detectors run per audio block on the
detector worker, allocation-free after construction; **fidelity** — Rust equals Python per part; **continuity** — the
shipped kick (Peter live-tested) and the snare must fire exactly as now through the cut-over. Thread residency is
settled (one worker per send, KICK_REALTIME_DESIGN.md section 1b (threading)); nothing here touches the content thread
or the project file.

Companion docs:
- [DETECTOR_LAB_DESIGN.md](DETECTOR_LAB_DESIGN.md) — the parts, configs and runs; its P7 (releases from component configs) is built here.
- [KICK_REALTIME_DESIGN.md](KICK_REALTIME_DESIGN.md) — the kick's consumer contract, the container format, threading; its sections 2–4 are superseded here.
- `docs/archive/SNARE_REALTIME_DESIGN.md` — the snare v1 port, superseded here. Its consumer contract still holds: `SendFeatures.bands[Mid].snare` is 1.0 on the fire hop and decays like the kick; the trigger drawer's Snare chip reads it.

## 1. Audit — what exists (verified 2026-10-11)

Slot at `1004ee2db` (feat/detector-lab-general), lab rows re-checked on main at `5461d2fee` (lab P6 landed); `feat/snare-port` at `feaac6531` (local, unlanded; built, awaiting
Peter's live test, BUG-nss46 (snare ship decision)). Extend, don't redesign. `fsp:` = path on `feat/snare-port`.

**Python** (`tools/audio_analysis/`)

| Piece | Where | State |
|---|---|---|
| Container writer/reader (named LE arrays, magic `MKICK001`) | `kick_container.py:12`, `write` :21 | exists |
| Finders, event measures, frame measures as registries | `lab/components.py` `FINDERS` :52, `MEASURES` :121, `FRAME_MEASURES` :131 | exists |
| Finder callable on a bare clip | `_snare` :28 / `_kick` :44 close over a `song` | one step: a pure `find(rule, family, x, sr)` |
| Frame measure callable on a clip | `_flat` :125 reads the float16 cache (`lab/cache.py:127`) | one step: pure builder `detector_channels.flatness` (`eval/detector_channels.py:14`) |
| Training plan, tree fit, combine, cutoff | `lab/models.py` `plan` :12, `fit_trees` :65, `combine` :78, `cutoff` :97 | exists |
| Net train / predict (f32 sigmoid) | `eval/kick_goal_nn.py` `train` :197, `predict` :261, `Net(channels)` :175 | exists |
| Stage inputs, gate, training | `lab/stage.py` `inputs` :72, `gated` :39, `train` :82 | exists; saves only gated outputs (:130, :132) — one step: save the ungated outer stage probability |
| Fitted templates (tmpl3 for any family) | `lab/templates.py` (main, merge `2882272cd`): config `trees.templates` null or `{"k": K}`, (K, span, 32) k-means templates fitted per training, three cosines appended after the measures | exists; not adopted by either chosen recipe |
| Seed averaging of nets | lab result reported by the lead (3 snare nets, about +1 F1 point); no config key | not in the config schema; a release option here |
| Kick release (v1 recipe, eval fast loop) | `kick_release.py` `train_final` :204, `reference` :315, `build_clip` :343, `export` :437 | exists; replaced |
| Snare release (through the lab, folds 0) | `snare_release.py` `train` :79, `reference` :123, `model_entries` :179 | exists; replaced |
| Export hooks: kick finder, extra measures, net, forests | `kick_export/base.py`, `extra.py`, `net.py`, `stage.py` (`forest` :32) | exist; moved into `lab/export/` |
| Shipped v1 parts | `~/.cache/manifold/kick-goal-2026-10-10/final/final.pkl`, `.../snare_final/final.pkl` | exist (train_ids match the assets) |
| Release dependants | `eval/kick_buildup_sim.py:20`, `eval/snare_roll_sim.py:14`, `lab/test_lab.py:276`, `lab/README.md:30`, `scripts/dev.py:125,128` | exist; rewired in P1 |

**Rust** (`crates/manifold-audio/src/`)

| Piece | Where | State |
|---|---|---|
| Container parse | `kick/container.rs:82` | exists |
| Recipe gate | `kick/detector.rs:17` `RECIPE_VERSION = 1`; `fsp:snare/detector.rs` its own `RECIPE_VERSION` | exists twice; replaced by `contract` |
| Resampling input (any rate → 48 kHz blocks) | `fsp:kick/detector.rs:99` `ModelInput` | exists on fsp |
| Stream driver: pending FIFO, ready times, decide in emission order | `kick/detector.rs:188`; `fsp:snare/detector.rs:162` | exists twice; merged |
| Kick finder + f15 | `kick/base/mod.rs:58` `BaseDetector`, `Candidate` :22 | exists |
| Snare finder | `fsp:snare/cands.rs:10` `SnareCands` | exists on fsp |
| tail3, tmpl3, prof32, low16 | `kick/extra.rs:53` `ExtraFeatures` — one fixed 54-column layout | one step: four selectable measures |
| fall16 | same paired spectrum as prof32 (`kick/extra/profile.rs:9`); reference `eval/kick_goal_profile.py:34` | one step |
| res16 | lowbank envelopes exist (`kick/extra/lowbank.rs:17`); reference `eval/kick_goal_residual.py:17,29` | new (running 400 ms median); no chosen recipe uses it — Deferred |
| glide3 | reference `eval/kick_glide_features.py:57` | new; no chosen recipe uses it — Deferred |
| Several nets on one spectrum | one `Cnn` per `KickNet` (`kick/net.rs:15`) | one step: N CNNs sharing the spectrum and frames |
| Net, any channel count and time bins, extra channels via `FrameSource` | `fsp:kick/net.rs:13`, `fsp:kick/net/cnn.rs:173` (main: `IN_CH = 2`, `kick/net/cnn.rs:10`) | exists on fsp |
| flat frames | `fsp:snare/flat.rs:18` `Flatness` | exists on fsp |
| Forest inference | `kick/trees.rs:20` | exists |
| Combine | `kick/stage.rs:170` `blend` (fixed mean logit) | one step |
| History stage | `kick/stage.rs:38` — one window, 48-wide shape, level always, no gate | one step: W windows, any shape, optional level, gate |
| Fire rule (cutoff, 60 ms refractory in avail hops) | `kick/stage.rs:176`; `fsp:snare/detector.rs` `decide` | exists twice |
| `EventKind::Snare`, worker publishing per detector, `send_detectors` | `fsp:detectors.rs:19,86` | exists on fsp |
| Snare on the Mid band, `FireQueue`, `queue_detector_events` | `fsp:analysis.rs:1812,2009` | exists on fsp |
| `AudioFeatureKind::Snare` (saved `"snare"`), Snare trigger chip | `fsp:manifold-core/src/audio_mod.rs:124,173` | exists on fsp |
| Contract parsing and refusal | — (searched `rg "contract\|Spec" crates/manifold-audio/src`) | new |

Genuinely new: the contract (both sides), the release tool's composition, the generalised stage with its gate, net
averaging, `TrainedDetector`. Everything else moves or gains a parameter. Both chosen recipes need only parts that
exist or are one step away.

## 2. Decisions

**D1. One data-driven runtime.** A model file names its parts; `TrainedDetector` builds exactly those parts and nothing
else. A recipe that uses only supported parts ships with no Rust change. Rejected: a hand-written detector per recipe
(today), because every lab win would be Rust work and the copies drift. Rejected: an ML library (ONNX, tract), because
KICK_REALTIME_DESIGN.md section 1 (what ships) settled hand-written inference and the DSP measures are not graph ops.
Rejected: the lab config JSON parsed in Rust, because it adds a dependency to `manifold-audio` and splits one model
into two files.

**D2. Contract v2 rides the existing container.** Same bytes, same magic `MKICK001`, same readers. A v2 file carries
`contract` = 2 and no `recipe_version`. Rejected: a new container format, because both readers already agree byte for
byte and nothing in the format is missing.

**D3. The shipped v1 models are converted, not kept** (done; the converter retired at P3 with the v1 files). `release.py convert-v1` builds a v2 release record from each
shipped `final.pkl` (same `train_id`, same numbers). The generic runtime must reproduce the v1 goldens' fires exactly;
then `KickDetector`, `SnareDetector` and both `RECIPE_VERSION`s are deleted in the same landing. Rejected: dispatching
v1 files to the old detectors, because that keeps two runtimes alive forever.

**D4. One release tool, `lab/release.py`, built only from the lab's own part functions.** The reference pipeline that
writes goldens calls `components` finders and measures, `kick_goal_nn` net and `predict`, the fitted trees, `models.combine`,
`stage.inputs` and `stage.gated`, `detector_eval.fires` — the functions the run trained and scored with. It replaces
`kick_release.py` and `snare_release.py`; `kick_export/*` and `snare_release.model_entries` move to `lab/export/` as
per-part hooks. Rejected: a reference re-implemented in the release tool, because that is a third copy of the recipe.

**D5. What a release fits, and where its cutoffs come from.** Given config C and seed S whose lab run is saved:
- Final models train on every song the plan trains on (`models.plan` training with `without=()`), clips as the config
  says, `random_state = S` (net k: `S + k`).
- The final stage trains on stage inputs built from each song's **outer** final-layer prediction (the run's
  `combined__t` or `<model>__t`: models that never saw the song's group), on the songs `learn` allows, `random_state = S`.
- `cut_detector` = `models.cutoff` on the outer final-layer predictions pooled over every exam song.
  `cut_stage` = `models.cutoff` on the outer stages' **ungated** probabilities (named edit: `stage.train` also saves
  `stageraw__<song>`). The release `cutoff` = `models.cutoff` on `stage.gated(st, x_outer, stageraw, (cut_detector, cut_stage))`.
  Without a stage, `cutoff = cut_detector`; with gate 0, the cuts are unused and not written.
- With N nets (D17) the run's seeds S..S+N-1 must all be saved. The outer net layer is the seeds' `net__t` averaged
  by D17's rule, the outer trees layer is seed S's `trees__t`, and the outer final layer is recomputed from them with
  `models.combine`; everything after uses that layer. N = 1 reads the run's own layers unchanged.
- A config with `folds` 0 releases the same way (outer = one net per song); the lab already refuses a stage there.
This is the kick v1 rule (`kick_release.py:276–281`) made generic. Rejected: cutoffs on the final models' own
predictions, because those are in-sample.

**D6. The app's parts are declared in one file.** `crates/manifold-audio/tests/fixtures/detectors/runtime_parts.json`
lists the finders, event measures with widths, frame channels, combines and stage limits the Rust runtime implements.
`release.py` refuses a config using an undeclared part **before training**; a Rust test asserts its registry equals the
file; a Python test asserts every declared part exists in the lab registries with the same width. A Rust phase that
adds a part edits this file.

**D7. Enum dispatch, sized at load.** Parts are enums (`Measure`, `Channel`, `FinderKind`, `Combine`), matched
exhaustively; every buffer is allocated in `from_container` from the spec; `push` never allocates when the caller
reserved `max_fires`. Rejected: `Box<dyn Measure>` per part from a name map, because it hides a missing match arm
until runtime and buys nothing at ~40 candidates a second.

**D8. Refusal is loud and early.** Any unknown, missing or inconsistent entry → `TrainedDetector::from_container`
errors; that family is not built, the error is logged once, its feature stays 0 (the v1 behaviour,
`kick/detector.rs:113`). The embedded assets are parsed by a test, so a refused file never reaches a build Peter runs.

**D9. Each net's probability is the lab's: sigmoid of the f32 logit in f32, widened to f64** (`kick_goal_nn.predict`).
The snare v1 port took the sigmoid in f64; the difference is under 1e-7 and its fires are unaffected (P1 gate).

**D10. Golden sets per model; every new clip starts with 0.5 s of digital silence.** A set is a directory
`tests/fixtures/detectors/<set>/` holding `model.mkick` and `clip_*.mkick`. Converted v1 sets keep their v1 clips so
the P1 proof compares like with like. New sets lead with silence (the snare precedent, `snare_release.py:62`), so no
part's stream-start edge is ever in a golden.

**D11. Fitted measures are data.** A measure fitted at release (templates) is a lab registry entry with
`fit(train inputs) -> {key: array}` and `apply(params, x, sr, candidates) -> (n, k)`; its params go in the file as
`fit.<measure>.<key>`. The converted kick v1 needs `tmpl3` (its trees read it), and it runs on the existing Rust
templates code. Lab-fitted templates (`trees.templates` `{"k": K}`) were not adopted, so the release refuses a config
that sets them until the Deferred row is picked up; the contract already carries them.

**D12. Families are a small closed set in Rust.** The file's `family` text maps to `EventKind` (`kick`, `snare`);
band routing and the trigger chip stay app code. A new family is one enum variant plus its routing, never new
detector code. Unknown family → refused.

**D13. Model grid inherited.** 48 kHz mono, hop 256 for every part; input of any rate is resampled first
(`ModelInput`). Rust refuses any other `rate` or `hop`.

**D14. Fires are stamped at their decision sample** (input clock), as both v1 detectors do, so a fire lands on the
same hop whatever the frame size.

**D15. Rust is built against the chosen recipes before their final training.** `release.py train --provisional`
produces a structurally complete release from any config in minutes on the CPU (numbers throwaway). The two coverage
sets are provisional releases of the chosen configs themselves, with two nets each and the extra options the runtime
supports (section 9, P1), so P2 builds exactly what P4 ships.

**D16. CPU ceiling, defaulted.** All of a send's detectors together stay under 100 ms CPU per audio second in a
release build: 1.7 ms per 60 fps frame, inside the content thread's 2 ms wait
(KICK_REALTIME_DESIGN.md section 1b (threading)). Today kick 25 + snare 18 (fsp commit `2465c8a8a`); the net is most
of each, so three snare nets add roughly 30. **Trigger:** a release or coverage set over the ceiling stops the phase
and goes to Peter with the per-part breakdown.

**D17. Several nets, averaged.** `net.count` = N ≥ 1 (written 1 when there is one). All N nets share one input
setting (spectrum, extra channels, slice), so the spectrum and frames are computed once and each net runs its own
forward pass. N = 1: `p_net` is that net's probability (D9), no round trip. N > 1:
`p_net = expit(mean_k logit(clip(p_k, combine.clip)))` over the nets' D9 probabilities, then combine. The release
chooses its cutoffs on outer predictions averaged by the same rule (D5), so the runtime and the cutoff see one formula.
Rejected: averaging probabilities, because the lab's result was measured on mean logits. Rejected: averaging raw
logits, because the lab's saves hold probabilities and the cutoff could not be chosen on the runtime's quantity.

### Consequences, stated honestly

- Generic code carries spec-sized `Vec`s and enum matches where v1 had fixed arrays. P2 measures it against
  `KickDetector` interleaved; over 10% slower stops the phase.
- `kick_release.py` goes: the shipped kick stays runnable (converted) but cannot be retrained on its old recipe. Future
  kicks come from lab configs, whose truth differs (DETECTOR_LAB_DESIGN.md P6 states it is not comparable with
  0.889 / 0.894). Open decision 2.
- Release cutoffs come from models trained on about three quarters of the songs, then applied to a final model trained
  on all of them (the v1 rule). The shipped detector is a little more confident than the one its cutoff was chosen on.
- MPS training is not reproducible, so re-running a release gives different nets. The `train_id` names the shipped
  ones and the release record keeps their weights.
- Each extra averaged net is one more forward pass per candidate (roughly 10 ms CPU per audio second for the snare)
  and one more full training at release; N = 3 also needs three saved run seeds.
- Nets train on float16-cached spectra and run on f32 ones (inherited from v1, `kick_release.py:321`).
- Golden sets for the two coverage configs add about 12 MB to git; the v1 sets are replaced, not duplicated.
- res16 and glide3 stay lab-only until a release config needs them; such a config is refused at release, which
  costs one Rust session at that point (Deferred). When ported, res16's reference reads up to 200 ms ahead in the
  first 200 ms of a stream (scipy `mode='nearest'`); Rust will use a causal start that goldens never exercise (D10).
- v2 keeps v1's part prefixes (`base.`, `extra.`, `cand.`) so the moves stay mechanical; the names are inherited, not tidy.

## 3. The release path (Python)

```
lab/release.py train CONFIG [--seed S] [--nets N]   # run seeds S..S+N-1 must be saved; writes a release record
lab/release.py train CONFIG --provisional [--nets N] # CPU, 1 epoch, 3 songs, trees max_iter 5, cuts and cutoff from quantiles of its coverage clips (fires on every clip, unequal cuts); numbers throwaway
lab/release.py export TRAIN_ID --set NAME [--ship]
        # writes crates/manifold-audio/tests/fixtures/detectors/NAME/{model,clip_*}.mkick;
        # --ship also writes crates/manifold-audio/assets/<family>_model.mkick (byte-identical to the set's model)
lab/release.py parts                          # checks runtime_parts.json against the lab registries
```

`scripts/dev.py` verb `detector-release` replaces `kick-release` and `snare-release`.

**Release record** (`~/.cache/manifold/detector-lab/releases/<train_id>/final.pkl`, plus `release.json` holding
everything but the fitted objects):

```python
dict(
    contract=2, family=str, train_id=str,      # '<family>-<date>-<git>' | v1's id | 'provisional-<config>-<time>'
    config=dict, run_id=str, seed=int,          # run_id 'v1' for converted records
    finder=str,                                 # 'kick' | 'snare:<dB>'
    net=dict(state_dicts=list, past_ms, ahead_ms, channels=list[str]) | None,   # len(state_dicts) = N >= 1
    trees=dict(model=HistGradientBoostingClassifier, measures=list[str]) | None,
    fitted={measure: {key: ndarray}},          # e.g. {'tmpl3': {'templates': (K, span, 32)}}
    combine=str,                                # 'net' | 'trees' | 'mean_logit'
    stage=dict(model, windows_s, shape, level, gate, power, cuts=(cut_detector, cut_stage) | None) | None,
    cutoff=float, refractory_s=float,
    songs=list[str],                            # what the final models trained on
    score=dict,                                 # pooled P / R / F1 at the cutoff on the outer predictions, per layer
    clips=str,                                  # 'v2' | 'cover' (provisional: dense at 12 s only)
)
```

`train` refuses: a dirty `tools/audio_analysis` tree unless `--provisional`; a missing run seed; any exam song without
an outer prediction; a config part not in `runtime_parts.json`; `trees.templates` set (D11).

**Export**: builds each clip from its spec (v2: `dense` = 0.5 s silence + tears_140bpm + apricots_128bpm cut in a
candidate-free gap at `max(20, 2.5 × longest window)` s; `quiet_start` = 0.75 s silence + bad_guy_128bpm with a 2 s
fade-in, 12 s; coverage sets cut `dense` at 12 s only), runs the reference pipeline on the clip alone (stream starts
at its first sample, f32 spectra and frames, never the float16 caches), writes the golden and the model, reads both
back and refuses non-finite or non-round-tripping arrays (the `kick_release.py:468` check).

**Part hooks** (`lab/export/`, each `entries(record) -> {name: array}` with its prefix enforced, as
`kick_release.py:413 hook_entries` does): `finder_kick.py` (from `kick_export/base.py`), `finder_snare.py` (the
`cand.*` half of `snare_release.model_entries`), `measures.py` (from `kick_export/extra.py`, with `fit.tmpl3.*`
split out), `frames.py` (`flat.*`), `net.py` (from `kick_export/net.py`, any `past_ms` and channels), `trees.py`
(`forest` from `kick_export/stage.py`), `stage.py`.

## 4. The model file contract v2

Container as KICK_REALTIME_DESIGN.md section 2 (the model file). Text entries are UTF-8 `u8` arrays; lists inside
text are comma-separated names with no spaces. Every entry below marked required must exist; any entry name the
runtime does not know under a known prefix is an error (no silent ignores).

| Entry | Type | Meaning |
|---|---|---|
| `contract` | i32 scalar | 2 (required) |
| `family` | text | `kick` / `snare` → `EventKind` |
| `train_id` | text | release id; goldens must match it |
| `config` | text | lab config JSON, provenance only — Rust never parses it |
| `run_id` | text | lab run the cutoffs came from, `v1` for converted |
| `rate`, `hop` | i64 | 48000, 256 (D13) |
| `finder` | text | `kick` or `snare:<dB>`; must equal `cand.rise_db` for snare |
| `models` | text | `net`, `trees` or `net,trees` |
| `combine` | text | `net` / `trees` / `mean_logit`; must agree with `models` (lab `config.COMBINES`) |
| `combine.clip` | f64 | 1e-6, logit clip of the mean-logit combine |
| `cutoff`, `refractory_s` | f64 | the fire rule |
| `base.*` | — | kick finder constants (v1 names, `kick_export/base.py`) |
| `cand.*` | — | snare finder constants (v1 names, `snare_release.py:201–207`) |
| `extra.*` | — | tail, band spectrum, low bank and template constants (v1 names, minus `extra.templates_*`); `extra.cdom_*` only with cdom4 and `extra.note_*` only with note13 (`lab/export/measures.py` `names_for`) |
| `res.*`, `glide.*` | — | reserved for res16 and glide3 (Deferred) |
| `kick2.*` | — | `window_s` [before, after], `none_dt_ms`, `kick_train_id` (text): the kick verdict and the kick model it was trained against (section 5.5) |
| `fit.<measure>.<key>` | — | fitted measure params; `fit.tmpl3.all`, `.low`, `.high` (pre-normalised, as v1), `fit.tmpl3.span` |
| `net.count` | i64 scalar | N ≥ 1 nets averaged by D17 (required when `models` has `net`) |
| `net.*` input entries | — | v1 names (`net.sos`, `net.k`, `net.frame_s`, `net.frame_hop`, `net.slice`, `net.pre_s`, `net.span`, `net.ahead_s`, `net.hop`, `net.sample_rate`) plus `net.channels` (count), `net.tbins` (fsp names); `net.extra` text = frame channels in order (`flat`, `perc`, or empty); shared by all N nets |
| `net.<k>.<param>` | f32 | net k's weights, k = 0..N-1, PyTorch names (`net.0.conv.0.weight` ... `net.0.head.2.bias`); every net has the same shapes |
| `flat.*` | — | flat frame constants (fsp names) |
| `perc.*` | i64 | `sustain`, `neighbours`: the percussive-share channel's median windows (`eval/kick_goal_nn.py` `percussive`) |
| `trees.measures` | text | event measures in column order |
| `trees.widths` | i64[n] | their widths (cross-check with `runtime_parts.json`) |
| `trees.*` | — | forest arrays (`kick_export/stage.py:32` layout), `n_features` = sum of widths |
| `stage` | i32 scalar | 0 / 1 (required) |
| `stage.windows_s` | f64[W] | memory windows, file order; window 0 feeds the gate; 1 ≤ W ≤ 4 |
| `stage.shape` | text | shape measures, hstacked |
| `stage.level` | text | `<measure>:<column>` or empty |
| `stage.gate` | f64 | 0 = off |
| `stage.cut_detector`, `stage.cut_stage` | f64 | required iff gate > 0 |
| `stage.power`, `stage.lp_clip`, `stage.q_clip` | f64 | 8, 1e-6, 1e-6 |
| `stage.*` forest | — | `n_features` = 1 + W × (3 + has_level) |

Moved from v1: the net weights `net.<param>` become `net.0.<param>`. Dropped from v1: `recipe_version`, `stage.cutoff`, `stage.refractory_s`, `stage.window_s`, `stage.blend_clip`,
`stage.blend_weights`, `stage.sample_rate`, `stage.hop`, `extra.templates_*`, the snare's top-level `sample_rate`.

Rust refuses (each a named `SpecError`): contract not 2 or missing; unknown family, finder kind, measure, channel or
combine; `f15` with a finder other than `kick`; `combine` not matching `models`; a measure width or forest
`n_features` mismatch; `rate`/`hop` not 48000/256 or any part's own `*.hop`/`*.sample_rate` disagreeing; W outside
1..=4; a level column outside its measure; gate > 0 without both cuts; a fitted measure without its `fit.*` entries;
`net.extra` names not matching `net.channels - 2`; `net.count` < 1, a missing `net.<k>.*` weight for k < N, or
weights for k ≥ N; nets whose weight shapes differ.

## 5. The app runtime (Rust)

### 5.1 Modules

New module `crates/manifold-audio/src/trained/`; `kick/` and `snare/` are folded into it and deleted at P3.

| Path | From | Role |
|---|---|---|
| `trained/mod.rs` | `kick/mod.rs` | re-exports; module doc cites this design |
| `trained/container.rs` | `kick/container.rs` (unchanged) | container |
| `trained/spec.rs` | new | `Spec`, `SpecError`, the part enums |
| `trained/input.rs` | `fsp:kick/detector.rs:99` | `ModelInput` |
| `trained/finder/{mod.rs, kick/, snare.rs}` | `kick/base/`, `fsp:snare/cands.rs` | `Finder` enum |
| `trained/measure/{mod.rs, dsp.rs, spec.rs, tail.rs, templates.rs, profile.rs, lowbank.rs}` | `kick/extra/*` | `MeasureBank`; profile gains fall16 |
| `trained/measure/{cdom.rs, note.rs, verdict.rs}` | new | cdom4 (`eval/kick_complex_onset.py`), note13 (`eval/note_pitch.py`), kick2 (`lab/kick_verdict.py`) |
| `trained/frames/{mod.rs, flat.rs, perc.rs}` | `fsp:snare/flat.rs`; perc new | `FrameBank` (net extra channels) |
| `trained/net/{mod.rs, cnn.rs, spectrum.rs}` | `fsp:kick/net*` | `Net`: one `Spectrum`, one input window, `Vec<Cnn>` of N nets (D17) |
| `trained/trees.rs` | `kick/trees.rs` | `Forest` |
| `trained/history.rs` | `kick/stage.rs` | generalised stage |
| `trained/detector.rs` | `kick/detector.rs` + `fsp:snare/detector.rs` | `TrainedDetector` |
| `detectors.rs` | fsp | `EventKind`, worker, `send_detectors` from the embedded list, `KickFed` (section 5.5) |

Dependency direction unchanged: `trained` depends on nothing outside `manifold-audio` except `log`.

### 5.2 Committed types

```rust
// trained/spec.rs
pub const CONTRACT: i32 = 2;
pub const MODEL_RATE: u32 = 48_000;
pub const MODEL_HOP: u64 = 256;
pub const MAX_WINDOWS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinderKind { Kick, Snare }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Measure { F15, Tail3, Tmpl3, Prof32, Fall16, Low16, Cdom4, Note13, Kick2 }   // res16, glide3, pitch6: Deferred
impl Measure {
    pub const ALL: [Measure; 9];
    pub const fn name(self) -> &'static str;   // the lab's names: "f15", "tail3", ...
    pub const fn width(self) -> usize;         // 15, 3, 3, 32, 16, 16, 4, 13, 2
    pub fn parse(s: &str) -> Option<Self>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel { Flat, Perc }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combine { Net, Trees, MeanLogit }

#[derive(Debug, Clone, PartialEq)]
pub struct NetSpec {
    pub count: usize,                          // N >= 1 nets averaged (D17)
    pub channels: Vec<Channel>,                // extra channels in order
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageSpec {
    pub windows_s: Vec<f64>,
    pub shape: Vec<Measure>,
    pub level: Option<(Measure, usize)>,
    pub gate: f64,
    pub cuts: Option<(f64, f64)>,              // (cut_detector, cut_stage); Some iff gate > 0
    pub power: f64,
    pub lp_clip: f64,
    pub q_clip: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub family: EventKind,
    pub train_id: String,
    pub finder: FinderKind,
    pub trees: Option<Vec<Measure>>,           // column order
    pub net: Option<NetSpec>,
    pub combine: Combine,
    pub combine_clip: f64,
    pub stage: Option<StageSpec>,
    pub cutoff: f64,
    pub refractory_s: f64,
}
impl Spec {
    pub fn parse(c: &Container) -> Result<Self, SpecError>;
    /// Every measure the model reads (trees, stage shape, level), first use order.
    pub fn measures(&self) -> Vec<Measure>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpecError {
    Container(ContainerError),
    Contract(Option<i32>),
    Unknown { what: &'static str, name: String },
    Mismatch { what: &'static str, detail: String },
}

// trained/detector.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fire { pub sample: u64, pub cand_hop: u64, pub avail_hop: u64 }

/// One candidate's decision, every intermediate exposed for parity tests and diagnostics.
pub struct Decision<'a> {
    pub cand_hop: u64,
    pub avail_hop: u64,
    pub measures: &'a [f64],                   // Spec::measures() hstacked
    pub net_logits: &'a [f32],                 // one per net; empty when the model has no net
    pub p_net: f64,                            // averaged per D17; NaN when no net
    pub p_trees: f64,                          // NaN when no trees
    pub p_combined: f64,
    pub stage_x: &'a [f64],                    // empty when no stage
    pub p_stage: f64,                          // ungated; NaN when no stage
    pub p_final: f64,
    pub fire: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DetectorError { Spec(SpecError), BadRate(u32) }

pub struct TrainedDetector { /* private */ }
impl TrainedDetector {
    pub fn from_container(c: &Container, input_rate: u32) -> Result<Self, DetectorError>;
    pub fn kind(&self) -> EventKind;
    pub fn train_id(&self) -> &str;
    /// Seconds from a candidate's emission to its decision, from the parts' ready rules.
    pub fn decision_lag_s(&self) -> f64;
    pub fn push_with(&mut self, samples: &[f32], on_fire: impl FnMut(Fire));
    pub fn push_traced(&mut self, samples: &[f32], on_decision: impl FnMut(&Decision<'_>, Option<Fire>));
    pub fn max_fires(&self, samples: usize) -> usize;
    pub fn decided(&self) -> u64;
    pub fn dropped(&self) -> u64;              // pending FIFO and history overflows; 0 on every golden
}

// detectors.rs
pub enum EventKind { Kick, Snare }
impl EventKind { pub fn parse(family: &str) -> Option<Self>; pub const fn name(self) -> &'static str; }
impl EventDetector for TrainedDetector { /* kind from the spec */ }
const EMBEDDED: [(EventKind, &[u8]); 2] = [
    (EventKind::Kick, include_bytes!("../assets/kick_model.mkick")),
    (EventKind::Snare, include_bytes!("../assets/snare_model.mkick")),
];
pub fn send_detectors(rate: u32) -> Vec<Box<dyn EventDetector>>;   // one per embedded model that loads
```

The file's `family` must equal its `EMBEDDED` slot. Each embedded file is parsed once per process (`OnceLock`, the
`kick/detector.rs:26` precedent); a refusal logs once per family (`fsp:detectors.rs` `or_log`). At load the runtime
logs `"[Audio] <family> detector <train_id>: decides <lag> ms after emission"`.

### 5.3 Per block and per candidate

Per input block (≤ 512 samples, `ModelInput::STEP`): resample to 48 kHz; push the block to the finder, every
instantiated measure stream (only those `Spec::measures()` needs: band spectrum for prof32/fall16/tmpl3, tail, low
bank for low16), the net spectrum and frame channels. Found candidates enter a 64-slot FIFO in
finder order with `ready = max(finder.ready, each measure's ready, net.ready)`; the head is decided once
`ready <= samples48`. Decisions therefore run in emission order (`debug_assert!` that `avail_hop` never decreases).

Per decided candidate, exactly the lab's maths:
1. Measures into one row (`Spec::measures()` order); `f15` is the kick candidate's own 15 features.
2. `p_trees` = forest proba of the `trees.measures` columns. The net input window (shape, new, frame channels) is
   built once; each of the N nets gives its f32 logit and its D9 probability; `p_net` by D17.
3. `p_combined`: the one model, or `expit((logit(clip(p_net)) + logit(clip(p_trees))) / 2)`, clip = `combine.clip`.
4. Stage (when present), `lab/stage.py inputs` and `eval/kick_goal_selfsim.py:29 self_features` per window in file
   order: `lp = logit(clip(p_combined, lp_clip))`; descriptor `d` = hstacked shape measures clipped at 0 and divided by
   (L2 norm + 1e-9); `level` = the named column or 0; over earlier candidates with `emit_s >= emit_s(now) - w`:
   `sw`, `Σ w·d`, `Σ w·lp`, `Σ w·level` with `w = clip(p,0,1)^power`. `stage_x = [lp] ++ per window [sim, lp_rel,
   (lvl_rel when level), ln_1p(sw)]`, the relative terms 0 when `sw <= 1e-9`. Sums are recomputed over the ring each
   call, never kept running (`kick/stage.rs:138`: no drift over a long set). `p_stage` = stage forest proba.
5. Gate (`lab/stage.py:39 gated`), gate > 0: `g = min(1, expm1(stage_x[evidence of window 0]) / gate)`,
   `det = stage_x[0] - logit(cut_detector)`, `stg = logit(clip(p_stage, q_clip)) - logit(cut_stage)`,
   `p_final = expit(logit(cut_detector) + det + g·(stg - det))`. Gate 0: `p_final = p_stage`. No stage:
   `p_final = p_combined`.
6. Fire iff `p_final >= cutoff` and the last fire's `avail_hop` is at least `refractory_s - 1e-12` earlier
   (`detector_eval.py:89`). NaN never fires. The candidate then enters the history ring (fired or not).

History ring capacity = `ceil(max window × 192) + 1` entries (`kick/stage.rs:14` rate), descriptors in one flat
`Vec<f64>` of capacity × shape width, allocated at load.

### 5.4 How the shipped kick keeps working

- Until P3 lands, main runs `KickDetector` and nothing in this design touches it there.
- P1 converts the shipped kick to a v2 release record; its v2 file holds the same numbers under the v2 names and the
  same `train_id`. Python proves the generic reference reproduces every v1 golden array exactly (P1 gate).
- P2 proves `TrainedDetector` on that set: fires exact, same tolerances as v1.
- P3 swaps `assets/kick_model.mkick` for the converted file, wires `send_detectors` to `TrainedDetector` and deletes
  `KickDetector`, in one landing. P2 and P3 share a branch; P2 never lands alone.
- The lab-trained `kick_gen50_gated` replaces the converted one only through P4, with Peter's yes.

The snare follows the same path from `fsp`'s snare v1.

As built: P3 and P4 land as one change on Peter's yes to the lab-trained detectors, so the converted v1 files never
reach main. Release-build cost on the shipped sets' dense clips: kick 39–42 ms CPU per audio second, and the snare
with the kick it reads 70 (D16 ceiling 100 for all detectors together; the kick runs once, inside the pair).

### 5.5 Models that read the kick (kick2)

A snare fires on bright kicks, so the snare reads the kick detector's verdict: at each snare candidate, the most
confident kick decision whose onset is within 40 ms before to 20 ms after the snare's onset and that is out by the
snare's emission, and that kick's onset offset in ms (60 with none). A kick and snare together still count as a snare:
the snare keeps its own upper-band evidence.

- Training: `lab/kick_verdict.py build` stores the shipped kick's final probability per song (held out on the kick's
  own training songs, the release itself elsewhere); the measure reads that store. The release records
  `kick_train_id`; a kick2 model is trained against exactly one kick.
- Runtime: `send_detectors` pairs each kick2 model with the kick in a `KickFed`: the kick runs on the block, hands
  every decision (fired or not) to the reader in emission order, then the reader runs on the same block, in
  512-sample steps whatever the push size (the reader keeps the last 256 kick decisions). One thread, no shared
  state. The reader refuses any kick but its `kick_train_id` (`DetectorError::KickMismatch`, logged, the
  reader disabled, the kick still runs). The reader waits for the kick's own decision lag on top of its finder.
- Goldens: the release reference runs the kick release on each clip; the kick's net shape must be the reader's
  (`release.use` exits otherwise). Parity compares kick2 at 1e-4 (the kick's own p.final tolerance) and leaves out
  candidates whose kick window opens inside the kick's stream-start edge.

## 6. Parity

Two levels, every golden set, no GPU (all tests are CPU `cargo nextest`):

- **Part parity** — each part fed the golden's own inputs: trees on golden `m.*` → `p.trees`; history on golden
  `p.combined` and `m.*` → `stage.x`, `p.stage`, `p.final`; fire rule on golden `p.final` → `fires`.
- **End to end** — audio only, fed in seeded random chunks of 256–1024 samples, output capacity reserved and asserted
  unchanged (`kick/detector.rs:281–284` precedent); `dropped() == 0`; every fire stamped at or after its emission and
  at most `decision_lag_s()` after it.

**Golden schema** (written by `release.py export`, read by both sides):
`train_id`, `contract`, `rate`, `hop`, `audio_i16`, `cand_hop`, `avail_hop`, `onset_s`, `emit_s`, `m.<measure>`
(n, width) for each of `Spec::measures()`, `frames.spec` and `frames.<channel>` (T, 64, f32), `net.logit` (n, N),
`p.net` (averaged),
`p.trees`, `p.combined`, `stage.x`, `p.stage`, `p.final`, `fires` (candidate indices). An entry is present exactly
when the model has that part.

| What | Level | Tolerance |
|---|---|---|
| `cand_hop`, `avail_hop` | end to end | exact |
| `m.*` | end to end | abs 1e-6 |
| `frames.spec`, `frames.flat` | end to end | abs 1e-3 dB |
| `net.logit` (each net) / `p.net` | end to end | 1e-3 / 1e-4 |
| `p.trees` (as raw logit) | part 1e-6; end to end 1e-4 on `p` |
| `stage.x` | part | abs 1e-12 (`kick/stage.rs:278`) |
| `p.stage`, `p.final`, `p.combined` | part 1e-12; end to end 1e-4 |
| `fires` | part exact; end to end exact, except at most one per clip whose `p.final` is within 1e-3 of the cutoff (v1 rule) |

Tests: `trained::tests::parity_<set>` per set; `every_golden_set_runs` asserts the sets found on disk equal the literal
list in the test (`kick`, `snare`, `cover_kick`, `cover_snare`, and later shipped sets) — a count match, so no set is
silently skipped; `embedded_assets_are_the_shipped_sets` asserts each `assets/<family>_model.mkick` is byte-identical
to `tests/fixtures/detectors/<family>/model.mkick`; `goldens_match_their_model` asserts every clip's `train_id`.

A new release's goldens are held-out input for the Rust code: no Rust phase develops against them.

## 7. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| The app never runs a model it cannot run fully | `trained::spec::tests::refuses_each_bad_entry` (one mutated container per `SpecError` case in section 4) |
| Rust equals Python on every part of every shipped and coverage model | `trained::tests::parity_<set>`, `every_golden_set_runs` |
| The embedded models are the tested ones | `embedded_assets_are_the_shipped_sets` (byte equality) |
| Every family has an embedded model | `detectors::tests::every_event_kind_has_an_embedded_model` (exhaustive match over `EventKind`) |
| Rust's parts equal the declared parts; the lab has them with the same widths | `trained::spec::tests::registry_matches_runtime_parts`; `test_lab.py::test_runtime_parts_exist_in_the_lab` |
| A release uses only parts the app runs | `release.py train` refuses; `test_lab.py::test_release_refuses_undeclared_part` |
| Release cutoffs never come from in-sample predictions | `test_lab.py::test_release_cutoffs_from_outer` (synthetic run: in-sample predictions would choose a different cutoff) |
| The release reference is the lab's own functions | `test_lab.py::test_shipped_sets_are_their_release` (a fresh export is the committed set; the v1 reproduction test retired with the v1 files at P3); gate `rg "kick_fusion_bandwise|kick_goal_profile|kick_goal_lowbank|kick_goal_tail|snare_cands|detector_channels" tools/audio_analysis/lab/release.py` → 0 |
| Push never allocates | capacity assertion in every end-to-end parity test |
| One runtime | after P3: `rg "KickDetector|SnareDetector|RECIPE_VERSION|recipe_version" crates/manifold-audio crates/manifold-app` → 0 |
| Decisions run in emission order | `debug_assert!` in `TrainedDetector` on `avail_hop` |
| The runtime and the release cutoff average nets by one rule | `parity_cover_kick`, `parity_cover_snare` (N = 2); `test_lab.py::test_release_averages_like_runtime` |
| Detector cost stays under the ceiling | `trained::tests::detector_cost` prints per set; release-build numbers recorded at P2, P3, P4 (D16 trigger). Enforcement: none automatic — timing tests are not gates on shared machines |

## 8. The plausible-wrong turns, by name

- **"Keep `KickDetector` for v1 files, use the new runtime for v2."** No. Convert v1 (D3); one runtime.
- **"Parse the config JSON in Rust, it's all there."** No. The contract's own entries (D1); `config` is provenance.
- **"Write the reference pipeline in release.py, it's only a few calls."** No. Call the registries (D4); the rg gate catches it.
- **"Choose the cutoff on the final model's predictions."** No. Outer predictions only (D5).
- **"Skip the unknown entry and carry on."** No. Refuse (D8); the feature at 0 with a logged error is the designed failure.
- **"`Box<dyn Measure>` from a name map."** No. Enums (D7).
- **"Keep running sums in the history, it's faster."** No. Re-sum the ring (`kick/stage.rs:138`).
- **"Widen a tolerance so a new part passes."** No. A failing part is a port bug; escalate with the worst rows.
- **"Store spectra as float16 in the goldens to match training."** No. Goldens are f32 end to end (v1 rule).
- **"Load model files from disk at runtime so Peter can swap them live."** Not now (Deferred).
- **"Land the generic runtime unwired first."** No. P2 and P3 land together.
- **"Average the nets' probabilities, or their raw logits."** No. Mean of clipped logits of each net's probability (D17).
- **"Build res16 and glide3 now, the lab has them."** No. Neither chosen recipe uses them; the release refuses a config that does (Deferred).

## 9. Phasing

### P0 — Bases (not a session of this design; entry state for P1 and P2)

The lab's P6 is on main (`5461d2fee`). The snare port lands on main after Peter's live test
(BUG-nss46 (snare ship decision)). **Default:** if Peter has not answered by P2's start, P2 merges `feat/snare-port`
into `feat/detector-runtime`, and P3's landing is **blocked** on his yes (open decision 1).

### P1 — Release tool, contract writer, v1 conversion (Python, one session)

- **Entry state:** `feat/detector-runtime` cut from main at or after `5461d2fee`
  (`git merge-base --is-ancestor 5461d2fee HEAD`); both `final.pkl` files exist and their `train_id`s equal the
  assets' (`release.py convert-v1` checks); `rg "def gated" tools/audio_analysis/lab/stage.py` and
  `rg "def features" tools/audio_analysis/lab/templates.py` hit.
- **Read-back:** this doc sections 2–4, 6, 8; `lab/components.py`, `lab/models.py`, `lab/stage.py`, `lab/run.py`
  (`setup`, `build`, `tree_rows`, `train_model`); `kick_release.py` whole; `snare_release.py` whole; `kick_export/*`.
  Restate D3–D6, D9–D11, D15, D17 and the forbidden moves.
- **Seam brief (Python):** old → new: `kick_release.features/reference` and `snare_release.reference` →
  `release.reference(record, x)`; `kick_export/<piece>.model_entries(final)` → `lab/export/<part>.entries(record)`;
  `components._snare/_kick` closures → `components.audio_candidates(rule, family, x, sr) -> Candidates` over the same
  `FINDERS` closures (built: `candidates()` keeps calling them, so no trained layer's code key moves); `FRAME_MEASURES['flat']`
  gains a pure builder (`components.FRAME_BUILDERS`, the function the cache wraps); `stage.train` additionally saves `stageraw__<song>`.
  Call sites (re-run `rg -n "kick_release|snare_release|kick_export" tools scripts docs crates`; stop if the list
  differs from section 1's dependants row): `eval/kick_buildup_sim.py` and `eval/snare_roll_sim.py` call
  `release.reference` (or are deleted, named in the commit); `test_lab.py:268–288` and `:553` compare against the
  converted kick set's arrays; `lab/README.md` "Ship the snare" becomes `detector-release`; Rust doc comments citing
  the deleted scripts point here (comment-only).
- **Deliverables:** `lab/release.py`, `lab/contract.py` (entry writer and validator, section 4 rules),
  `lab/export/*`, the named edits above, a fitted-measure registry holding `tmpl3` for the converted kick (apply =
  `lab/templates.py features`; verified 2026-10-11: it calls `kick_goal_templates.template_features`, and the v1
  `tmpl3` goldens reproduce with no difference), `runtime_parts.json` (finders kick and snare; measures f15 tail3
  tmpl3 prof32 fall16 low16; channel flat; three combines; nets N ≥ 1; stage W ≤ 4, level optional, gate), configs
  `lab/experiments/cover_kick.json` (`kick_gen50_gated` with fall16 added to its trees and stage windows [2, 4]) and
  `cover_snare.json` (`snare_gen50_hybrid_low16` with a 190 ms net on the flat channel and tail3 added to its trees,
  stage window [4]), each released `--provisional --nets 2`; goldens sets `kick` and `snare` (converted) and
  `cover_kick`, `cover_snare`; `dev.py` verb `detector-release`, `scripts/TOOLS.md` regenerated;
  `kick_release.py`, `snare_release.py`, `kick_export/` deleted; tests `test_reference_reproduces_v1_goldens`,
  `test_contract_round_trip`, `test_contract_refuses_bad_entries`, `test_release_cutoffs_from_outer`,
  `test_release_averages_like_runtime`, `test_release_refuses_undeclared_part`, `test_runtime_parts_exist_in_the_lab`.
- **Gate:** `lab/test_lab.py` passes, including: converted kick set vs `tests/fixtures/kick/clip_*.mkick` max abs diff
  0 on every shared array and equal fires; converted snare set: candidates, `net_spec`, `flat`, `net_logit` exact,
  `p.net` within 1e-7, fires equal. Any nonzero kick difference stops the phase (no tolerance). `release.py parts`
  exits 0. Negative: `rg "kick_release|snare_release|kick_export" tools scripts` → 0; the section 7 reference rg → 0.
- **Demo:** none — L1.
- **Forbidden:** re-implementing a part; float16 caches in goldens; in-sample cutoffs; editing `eval/kick_fusion_*`;
  touching the shipped assets or `tests/fixtures/{kick,snare}` (P3 replaces them).
- **Test scope:** `lab/test_lab.py` only. No GPU, no lab training (provisional releases run on the CPU).

### P2 — The generic runtime on the v1 parts (Rust, one session; does not land alone)

- **Entry state:** P1 committed on `feat/detector-runtime`; `fsp` merged into it (`git log | rg feaac6531`); the four
  sets exist; `rg "pub trait FrameSource" crates/manifold-audio/src/kick/net.rs` hits.
- **Read-back:** sections 4–8; `kick/detector.rs`, `kick/stage.rs`, `kick/extra.rs`, `fsp:snare/detector.rs`,
  `fsp:kick/net.rs`. Restate D2, D7–D9, D12–D14, D17 and the forbidden moves.
- **Seam brief (Rust):** move every file per section 5.1 (`git mv`, then edit); `ExtraFeatures` (fixed 54 columns)
  → `MeasureBank::new(c, &[Measure])` + `ready_sample(cand)` + `write(m, cand, out)`; `Stage::decide(tree_p, net_p,
  descriptor, level, emit_hop, avail_hop)` → `History::decide(p_combined, shape_row, level, emit_hop) -> (x, p_stage,
  p_final)` with the fire rule moved to the detector; `KickNet`/`SnareNet` → `Net` (the fsp API with `Vec<Cnn>` over
  `net.<k>.*`, `SnareNet` deleted).
  Compiler-driven: delete the old type, fix every error. `KickDetector` and `SnareDetector` remain only as thin
  constructors over `TrainedDetector` until P3 deletes them.
- **Deliverables:** section 5.1 modules; fall16; N averaged nets; section 5.2 types; tests from section 6 and 7
  (`refuses_each_bad_entry`, `registry_matches_runtime_parts`, `parity_kick`, `parity_snare`, `parity_cover_kick`,
  `parity_cover_snare`, `every_golden_set_runs`, `detector_cost`).
- **Gate:** `cargo nextest run -p manifold-audio trained detectors`; `cargo clippy -p manifold-audio -- -D warnings`;
  cost A/B in release, interleaved (`TrainedDetector` on the kick set vs the old `KickDetector` from P1's base,
  per memory `feedback_frame_timing_method`): within 10%, else stop and report.
- **Demo:** none — L1.
- **Forbidden:** wiring `send_detectors` (P3); changing a tolerance; a part not in `runtime_parts.json`; any edit to
  `analysis.rs` or `manifold-app`.
- **Test scope:** `-p manifold-audio`, filters above.

### P3 — Cut-over and landing (one session)

- **Entry state:** P2 green; Peter's yes on the snare (BUG-nss46 (snare ship decision)) if `fsp` is not on main.
- **Read-back:** sections 5.4, 7, 8; `fsp:detectors.rs`, `fsp:manifold-app/src/offline_audio_mod.rs`,
  `fsp:manifold-app/src/audio_mod_runtime.rs` tests at 1104–1150.
- **Seam brief:** call sites (re-run `rg -n "KickDetector|SnareDetector|kick_detector\(|snare_detector\(|manifold_audio::(kick|snare)" crates`;
  on fsp today: `audio_mod_runtime.rs:1104,1112,1138,1139,1147`, `examples/mod_harness.rs:346`, `detectors.rs:13–91`;
  stop if different): each builds from `send_detectors` or `TrainedDetector::from_container`.
- **Deliverables:** `assets/kick_model.mkick` and `assets/snare_model.mkick` replaced by the converted files
  (`release.py export <id> --set kick --ship`, same for snare); `EMBEDDED` and `send_detectors`; `kick/`, `snare/`,
  `tests/fixtures/kick/`, `tests/fixtures/snare/` deleted; `embedded_assets_are_the_shipped_sets`,
  `every_event_kind_has_an_embedded_model`; supersession sweep: KICK_REALTIME_DESIGN.md header points here for the
  model file, parity and retrain sections, `SNARE_REALTIME_DESIGN.md` archived or folded per
  `design_status.py --lifecycle-check`; this doc's status updated; `scripts/gen_docs_index.py`.
- **Gate:** `cargo nextest run -p manifold-audio`; `cargo nextest run -p manifold-app audio_mod_runtime` (worker kick
  and snare fires land on their hops, the fsp tests); `scripts/landing_gate.py --skip-gpu`; negative: the section 7
  one-runtime rg → 0. No content-thread work is added (the worker and its 2 ms wait are unchanged), so no render trace.
- **Demo:** L4, Peter: the worktree release binary (exact path in the landing message), a project with a kick-heavy
  and a snare-heavy song on an analysed send, Kick and Snare trigger chips firing as before.
- **Forbidden:** a fallback to the old detectors; keeping the old goldens; editing any number in the converted files.

### P4 — The chosen recipes ship (data only, one session per family)

- **Entry state:** P3 landed; run seeds saved for `kick_gen50_gated` (seed S) and `snare_gen50_hybrid_low16`
  (seeds S..S+N-1, N from open decision 3; `lab/run.py <config> --seeds ...`); `release.py parts` passes for both.
- **Deliverables:** per family `release.py train <config> --seed S [--nets N]`, then
  `export <id> --set <family> --ship`; the set's goldens replace the converted ones; `release.json` score (pooled
  F1 / P / R on outer predictions) quoted in the commit.
- **Gate:** `cargo nextest run -p manifold-audio`; cost in release (D16); `scripts/landing_gate.py --skip-gpu`.
- **Demo:** L4, Peter's live test with the worktree binary path; ships only on his yes. Gesture: a song with a drop
  after a high-passed build — the first kick and snare after the gap fire (what the gate exists for).
- **Forbidden:** any Rust edit (a parity failure is a P2 bug: escalate); a dirty-tree `train_id`; choosing N or the
  seed by comparing released models' scores.

Phase coverage: release tool (P1), contract both sides (P1, P2), v1 continuity (P1–P3), snare joins via the shared
runtime (P3), net averaging (P1, P2), the chosen recipes (P4). res16, glide3, lab-fitted templates, hot-swap loading
and other families: Deferred.

## 10. Decided — do not reopen

1. One runtime, data-driven; parts are enums; no ML library; no JSON in Rust.
2. Contract v2 in the existing container; `contract` = 2; v1 files are converted, never run.
3. One release tool calling the lab's own part functions; the two release scripts and `kick_export/` go.
4. Release cutoffs from outer predictions pooled over exam songs; final models on every trained song.
5. Unknown or inconsistent file → refused, logged, feature 0; tests parse the embedded assets.
6. Each net's probability = f32 sigmoid widened; N > 1 nets average by mean clipped logit; fires stamped at the decision sample.
7. Parity per part and end to end, tolerances as section 6; goldens per model set; new clips lead with silence.
8. Parts the app runs are declared in `runtime_parts.json`, checked from both sides.
9. 48 kHz, hop 256 model grid; any input rate resampled.

## 11. Deferred

| What | Revive when |
|---|---|
| Loading model files from disk at runtime (live A/B without rebuild) | Peter asks to audition candidates live |
| Frame channels other than flat and perc (side) | a release config names one |
| Families beyond kick and snare | the lab releases one (adds an `EventKind` variant and its routing) |
| res16, glide3 and pitch6 in Rust (none earned its place in the 2026-10-12 drop-one ablations; `measure/residual.rs`: per band a fixed-capacity sorted window over the last 400 ms of the f32-rounded 1 ms envelope, the order statistic at `size // 2` ⚠ VERIFY-AT-IMPL in scipy `_rank_filter`; `measure/glide.rs`) | a release config names either; until then `release.py` refuses it |
| Lab-fitted templates at release (`trees.templates` `{"k": K}`: fit with `lab/templates.py` on the release's training songs, export `fit.tmpl3.*`, Rust `tmpl3` exists) | a release config sets `trees.templates` |
| Model rates other than 48 kHz | a part's reference changes rate |

## 12. Open decisions (Peter)

1. **Land the snare port on its own first?** Answered 2026-10-11: no; the snare goes live with the runtime landing (Peter's yes to the lab-trained detectors). Recommendation was: yes, after your live test (BUG-nss46 (snare ship
   decision)); the runtime then replaces it with the same numbers. If you'd rather wait, the snare goes live with the
   P3 landing and P3 waits for your yes.
2. **Retire `kick_release.py`.** Answered 2026-10-11: yes (done in P1). The shipped kick keeps running (converted, identical fires), but its old recipe can no
   longer be retrained; every future kick comes from a lab config. Recommendation: yes — the lab's kick configs
   already reproduce its parts, and two release paths would drift. P4 replaces the converted kick anyway.
3. **How many snare nets?** Answered 2026-10-11: N = 3 for the snare, N = 1 for the kick. Three averaged nets gained about one F1 point; each costs one more forward pass (snare
   detector roughly 18 → 40 ms CPU per audio second, all detectors still under the 100 ms ceiling) and needs three
   saved run seeds. Recommendation: N = 3 for the snare, N = 1 for the kick until a kick averaging A/B says otherwise.
