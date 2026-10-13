# Roll Detector — kick and snare rolls as one rising state beside the single-hit triggers

**Status:** PROPOSED · awaiting Peter (section 15, open decisions) · 2026-10-12 · Fable
**Prerequisites:** DETECTOR_LAB_DESIGN.md P6 and DETECTOR_RUNTIME_DESIGN.md P4 (both shipped on main at df8030ca6); the roll census, section 8 (Data), says the exam needs more real roll songs before P3.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs) and section 6 (seam briefs) before starting any phase.

<!-- index: A plug-in detector pair (kick_roll, snare_roll) built in the Detector Lab and run by the generic runtime: a roll is a state (on, rate, off) not a hit; labels from MIDI note density; never touches the kick and snare models. -->

The shipped kick and snare detectors answer "did a hit land" once per 60 ms. A roll at 16ths or 32nds lands a hit
every 30 to 120 ms, so the single-hit detectors cannot follow one, and by decision they do not try: rolls are
unscored and untrained in their labels (`eval/detector_labels.py:341 roll_spans`, Peter, 2026-10-11: "remove the
rolls ... get the basics and foundations correct first"). Peter's ruling for this design, BUG-9ngk8.6 (roll detector
for kick and snare rolls): "rolls confuse the main nets, so they are now left out of kick and snare training; a
separate detector should handle them. Stage use: a roll reads better as one rising intensity (roll on, rate, density)
than as 32 separate flashes; the main triggers cannot follow a fast roll anyway (60 ms refractory)." And the training
rule: "we are not creating data to fit the detector, we are creating data for the detector to learn" (feedback,
2026-10-11).

**The governing insight: a roll is a state, not a hit, so the detector decides on a clock, not on an attack.** The
lab and the runtime are built around per-candidate decisions; the roll detector keeps every one of those parts and
changes only what a candidate is (a tick every 53 ms) and what a decision is (on with a rate, or off). It reads the
audio alone, through its own light front end, so its model, labels, run ids, release and runtime slot are disjoint
from the kick's and the snare's. "Never degrade the standard detectors" is then a set of mechanical equalities
(section 7.3), not a promise.

On stage: during a build the Kick Roll and Snare Roll cells come on a few hits into the roll and rise as the roll
speeds up; the Kick and Snare triggers keep flashing where they flash today. A performer maps the roll cell to
intensity (brightness, zoom, feedback) and the hit cells to flashes, and the build ramps with the music instead of
stuttering at the refractory. When the roll detector is wrong the cell is wrong on its own; nothing else moves.

Binding constraints (DESIGN_AUTHORING.md section 1 (the intake)): **label truth** — roll truth comes only from MIDI
note density, never from audio analysis (memory `detector-label-truth`); **fidelity** — Rust equals Python per part,
as every lab-built detector; **hot path** — the detector runs on the detector worker, allocation-free, under the D16
CPU ceiling (DETECTOR_RUNTIME_DESIGN.md D16); **persistence** — `AudioFeatureKind` is saved in projects, so new kinds
are additive; **performance surface** — two new feature cells and two trigger chips.

Companion docs:
- [DETECTOR_LAB_DESIGN.md](DETECTOR_LAB_DESIGN.md) — the parts, configs, runs, honest cutoffs and the A/B; this design adds one finder, one measure, one decision rule and two families.
- [DETECTOR_RUNTIME_DESIGN.md](DETECTOR_RUNTIME_DESIGN.md) — the model file contract and the generic runtime; this design adds parts to `runtime_parts.json` and two embedded slots.
- [AUDIO_EVAL_HARNESS_GUIDE.md](AUDIO_EVAL_HARNESS_GUIDE.md) section 0 (trained detectors) — the rules every trained detector follows (rolls out of the standard labels; oracle frontier first; three seeds).

## 1. Audit — what exists (verified 2026-10-12 on main at df8030ca6; Python paths under `tools/audio_analysis/`, Rust under `crates/manifold-audio/src/`)

Extend, don't redesign.

| Piece | Where | State |
|---|---|---|
| The roll rule, already the standard detectors' exclusion | `eval/detector_labels.py:341 roll_spans` (runs of ≥ 4 hits, each < 200 ms after the last, padded 250 ms); `:326 dense_spans` (snare: any two hits < `dense_s` .200 apart, `Family:50`) | exists; the roll truth is its complement (D2) |
| Kick labels drop roll kicks and unscore the spans | `eval/kick_labels.py:9,100` (`roll_spans` on every kick time the record knows, `song_labels :63`); `eval/kick_generated.py:42` | exists; 129 of 4733 kicks in rolls (DETECTOR_LAB_DESIGN.md P6 as built) |
| Snare labels keep roll snares as `dense_pos`, spans unscored | `eval/detector_labels.py:466`; `eval/snare_generated.py:38–46`; `eval/snare_labels.py:27 drop_kick_rolls` | exists; `detector_eval.dense_recall :248` reports per-hit recall inside them |
| Generated label sets, MIDI-only truth, one of them built for rolls | `eval/generated_sets.py:45 SETS` — Rolls set "VAR 1-4 (groove, kick and snare build-up rolls, drops) and MUSIC 1-12 (genre sections, 90-174 BPM)"; sections `rolls_var01..04`, `rolls_music01..12` in `lab/songs.json` | exists; train only (lab D8) |
| Candidate finders as a registry; audio-only finders run on a bare clip | `lab/components.py:52 FINDERS`, `:73 AUDIO_FINDERS`, `:76 audio_candidates`, `Candidates` (cand, avail, hop, sr) | exists; one entry away: a `tick` finder |
| Event measures `f(audio, sr, Candidates) -> (n, k)`, causal, never fitted, cached by content and code | `lab/components.py:172 MEASURES`, `:232 measure` | exists; one entry away: `roll24` |
| Low-band 1 ms envelope bank (8 bands, Q 2, 5 ms rectified mean, dB) | `eval/kick_goal_lowbank.py:15 CENTRES`, `:21 band_envelopes`; Rust `trained/measure/lowbank.rs:17 LowBank` | exists; the roll front end copies its shape in its own module (D5: never edit it, its code keys the kick's measures and run ids) |
| The snare finder's band and rise rule | `eval/snare_cands.py:17 BAND` (1–8 kHz), `eval/detector_cands.py:15 band_hops`, `:23 rise_candidates` | exists; the snare roll's family band |
| Trees, combine, pair cutoffs, the plan | `lab/models.py:12 plan`, `:73 fit_trees`, `:86 combine`, `:114 cutoff` | exists; used unchanged |
| Config schema, migration, `key()` for run ids | `lab/config.py:13 KEYS`, `:65 key`, `:72 migrate`, `:88 check` | exists; one key away: `decision` (D7) |
| Scorer: rows, fires, judged, honest cutoffs, one-sided `window_s` labels | `eval/detector_eval.py:67 rows`, `:89 fires`, `:104 judged`, `:208 held_out`, `:57 within` | exists; two named edits: `active`/`hard_spans` in `rows`, a decision rule in `judged` (D7) |
| Run ids and model keys hash config, labels, code and content | `lab/run.py:156–157`, `:223 model_key` | exists; the never-degrade gate reads them (section 7.3) |
| Score report sections, the coincident block | `lab/score.py:26 COINCIDENT_S`, `:27 PARTNER`, `:135 coincident_section` | exists; the roll block is shaped like it |
| A/B with the 90% paired song bootstrap | `lab/compare.py`, `eval/detector_compare.py:86 song_bootstrap` | exists; used unchanged |
| Song list with offsets and proofs; family labels files | `lab/registry.py:27 FAMILIES`, `:35 Song` (`offset_s`, `proof`) | exists; two families away |
| Audit: offset proof per project song, sidecar by labels sha | `lab/audit.py:38 project_check`, `:97 write_sidecar` | exists; the roll audit reuses the parent family's verdict (D6) |
| Release tool, contract writer, part hooks, provisional releases, `runtime_parts.json` | `lab/release.py:685 train`, `:540 train_provisional`, `:406 export`, `:56 parts`; `lab/contract.py:17 TOP`, `:19 FAMILIES`, `:21 PART_PREFIXES`; `crates/manifold-audio/tests/fixtures/detectors/runtime_parts.json` | exists; entries and prefixes to add |
| Generic runtime: spec enums, finder, measure bank, fire rule, pending FIFO, decision record | `trained/spec.rs:16 FinderKind`, `:33 Measure`, `:189 Spec`; `trained/finder/mod.rs:16 Candidate`, `:22 Finder`; `trained/measure/mod.rs:62 MeasureBank`; `trained/detector.rs:85 FireRule`, `:30 Decision`, `:402 decide` | exists; one variant each |
| Families, embedded slots, worker, events | `detectors.rs:17 EventKind`, `:40 DetectorEvent`, `:56 EMBEDDED`, `:67 slot` (exhaustive), `:168 send_detectors`, `:202 EVENT_RING` | exists; two variants and two slots |
| Fires reach the features per hop | `analysis.rs:1812 FireQueue`, `:1853 trigger`, `:2009 queue_detector_events`, `:2130`; `manifold-core/src/audio_features.rs:23 BandFeatures` (`kick :40`, `snare :43`) | exists; a level queue shaped like it, two fields |
| Feature kinds, band lock, chips, UI mirror | `manifold-core/src/audio_mod.rs:104 AudioFeatureKind`, `:136 ALL`, `:170 fixed_band`; `manifold-ui/src/types.rs:201 ALL`; `manifold-ui/src/panels/param_slider_shared/state.rs:234 TRIGGER_SOURCE_CHIPS`, `:249 trigger_source_chips`; `manifold-app/src/ui_translate.rs:122`; `manifold-core/src/project/queries.rs:386`; `manifold-playback/src/modulation.rs:943` (impulse kinds) | exists; two kinds, two chips |
| Offline and live paths build the same detectors | `manifold-app/src/audio_mod_runtime.rs:127,153`; `manifold-app/src/offline_audio_mod.rs:104,483` | exists; nothing changes there |
| Parity tests, cost test, golden list as a count match | `trained/tests.rs:400–456`, `trained/spec/tests.rs:84 refuses_each_bad_entry`, `:210 registry_matches_runtime_parts` | exists; four more sets |
| What the shipped detectors do inside rolls (simulations) | `eval/snare_roll_sim.py` (snare: candidates on 95–100% of roll hits, caught .03 at sixteenths, .14 on a rising roll); `eval/kick_buildup_sim.py` (kick: every roll hit gets a candidate; sixteenth rolls 2–15/32 caught); BUG-9ngk8.2.1 (snare detector: fast breakbeats and rolls) — dense-run training: +1.8 F1 mean, four songs −11 to −29 recall | exists; the evidence for D1 |
| Frame-wise causal drum model (TCN) | BUG-9ngk8.12 (frame-wise causal drum model) | deferred by Peter: current architecture first |
| The roll census | section 8 (Data), scripts in the session scratchpad (`roll_core.py`, `roll_proj.py`, `roll_gen.py`, `roll_cat.py`) | done 2026-10-12, read-only |

Genuinely new: the roll truth builder, the `tick` finder, the `roll24` measure and its front end module, the on/off
decision rule (lab and Rust), two families end to end, a level queue in the analyzer, two feature kinds. Everything
else is called.

## 2. Decisions

**D1. Rolls are their own family pair, `kick_roll` and `snare_roll`: lab-built plug-in detectors, released by the
same tool, run by the same runtime, disjoint from the kick and snare models.** Nothing in the kick's or snare's
labels, configs, run ids, release records, model files or Rust parts changes (section 7.3 lists the equalities).
Rejected: seating rolls in the song-history stage (`lab/stage.py`, `trained/history.rs`). The stage is the wrong
seat three times over: it re-scores one candidate at a time and its output still passes the 60 ms refractory
(`trained/detector.rs:85`), so it can only ever say "hit" per candidate, never "roll on at this rate"; its memory is
8 s of the song's confident hits, tuned to transfer a cutoff between songs, while a roll lives in the last 0.3–0.7 s;
and its training rows are the family's labels, where rolls are unscored, so teaching it rolls means retraining the
family on roll positives, which is exactly the entanglement Peter ruled out. Rejected: training the kick and snare
on their `dense_pos` instead (BUG-9ngk8.2.1's trial: +1.8 F1 mean but imitation −28.6, just_the_way −16.0,
set_me_free −15.1, burn_stems −11.1 recall), because it fails the no-song-loses-five bar and Peter's basics-first
ruling. Rejected: one combined `roll` family with the band choosing kick or snare, because the two gestures have
different front ends, different hard negatives (bass runs vs hat runs) and different data, and a single cell on
Full or High would silently read zero.

**D2. The roll truth is the standard detectors' exclusion rule, exactly.** Per family, take the family's MIDI notes
as its label builder defines them before roll removal (kick: `eval/kick_labels.py:63 song_labels`' every kick time
the record knows, layers merged as the builder merges them; snare: the snare and clap notes `detector_labels.song`
sees before `dense_spans`, `eval/detector_labels.py:421`), on audible tracks. **A roll is a maximal run of at least 4
notes in which every consecutive gap is under 200 ms** (`roll_spans(hits, gap_s=.200, min_hits=4)`). Its events:
**start** = the time of the run's 4th note (the first moment the rule is true); **end** = the last note plus the
run's last gap (the first expected hit that does not come); **rate** at any time inside = 1 / median of the last
three gaps. A run of 2 or 3 notes (a double, a flam, a triple) is not a roll and is a vouched negative for the start.
Rejected: a tempo-relative rule (gaps ≤ a quarter beat), because the show has no tempo and a seconds rule keeps the
roll truth the exact complement of the kick's and snare's truth — the two detectors partition the family's notes, so
no note is nobody's. Rejected: labelling the first note as the start, because no causal method can know a roll from
one note; the 4th-note label is what any method can meet, and the start's lateness against the first note (three
gaps: 90 ms at 32nds at 174 BPM, 570 ms at eighths at 150 BPM) is reported, never scored.

Consequences, stated honestly: under the 200 ms rule, eighth notes count as a roll above 150 BPM, so a drum and bass
eighth-note snare run is a roll for both detectors (unscored for the snare, positive for the snare roll); that is
today's exclusion, not a new choice. A continuous sixteenth pattern is one roll from its 4th note to its last: the
census found one (openings, a 265 s snare roll of 2047 hits at 116 BPM), so section 7.2 pools per song, never per
hit, and such a song contributes one start. The snare's own `dense_spans` unscore pairs under 200 ms, so a snare
double is in neither detector's positives — the roll detector sees it as a vouched negative, the snare detector
does not score it (open decision 5).

**D3. The output is a rate-coded level, not a trigger.** Two feature kinds, `KickRoll` (reads Low) and `SnareRoll`
(reads Mid), band-locked like Kick and Snare (`audio_mod.rs:170 fixed_band`). The cell's value is 0 while no roll is
on; while a roll is on it is `v = 0.25 + 0.75 · clamp(log2(rate_hz / 4) / 3, 0, 1)` — a floor of 0.25 so "on" is
never 0, then a third of the range per doubling of the rate: eighths at 174 BPM (5.8 Hz) read 0.38, sixteenths 0.63,
thirty-seconds 0.89; sixteenths at 128 BPM read 0.52, thirty-seconds 0.78; a 32 Hz buzz reads 1.0. The value updates
every tick while on and drops to 0 at the end. The single-hit triggers are untouched: a Kick keeps firing inside a
kick roll at its refractory, as today. Rejected: a 1.0-then-decay trigger at the start, because the stage use is "one
rising intensity", and a modulation shape already turns a level into a pulse if a performer wants one. Rejected: a
rate in beats, because the detector has no tempo and must not guess one (BUG-9ngk8 (trained audio detectors)
overnight guard rails: causal, no grid cues).

**D4. The detector: a tick finder, a windowed roll measure, trees, an on/off rule.** Parts, all in the lab's registries:
- finder `tick:10` — a candidate at the end of every 10th hop (53.3 ms, 18.75 per second), `avail = cand`
  (deadline 0): the decision is at the tick, and the stamp is the tick.
- measure `roll24` — 2 windows × 12 numbers over the family's onset strength, section 4.2, from the measure's own
  16-band front end (`eval/roll_features.py`, new), causal, never fitted, cached like every measure.
- models: `trees` on `roll24` with the kick's tree settings (max_iter 200, learning_rate .05, max_leaf_nodes 15,
  min_samples_leaf 40, l2 1); `combine: "trees"`; `stage: null`; no net.
- decision `state` — section 4.3: on when p ≥ cutoff, off after `hangover` ticks below the off cutoff; the cutoff
  chosen on held-out exam songs for start-event F1, as every detector's.
Rejected for v1: a net on a long slice (a 600 ms slice with coarse time bins, `kick_goal_nn.py:46 TBINS`), because
it needs its own 64-band spectrum per detector (the net is most of a detector's CPU, DETECTOR_RUNTIME_DESIGN.md D16)
and the windowed measures are the proven shape for a rate question; Deferred with the trigger in section 14.
Rejected: reading the family detector's `p_combined` as a measure, because the family nets are trained to reject
hits inside rolls (snare sim: .03 caught at sixteenths) and it would key the roll's model on the family's release;
Deferred. Rejected: the song-history stage for rolls (D1).

**D5. The front end is the roll measure's own, in its own module.** Eight causal second-order band-passes (Q 2), a
5 ms rectified mean, dB on the 1 ms grid — the shape of `eval/kick_goal_lowbank.py:21 band_envelopes` and
`trained/measure/lowbank.rs` — twice: a *family* group and an *other* group (section 4.2). Written in
`eval/roll_features.py`, never by adding a centres parameter to `kick_goal_lowbank.py`: that module's source is part
of the kick's `low16` code key, so an edit moves the kick's model keys and run ids (`lab/run.py:223 model_key`;
AUDIO_EVAL_HARNESS_GUIDE.md section 0 (trained detectors): add a module instead). The constants ship in the model
file as `roll.*` entries, so Rust reads them and nothing is written twice (DETECTOR_RUNTIME_DESIGN.md D1).

**D6. Labels are built by `eval/roll_labels.py` (project songs) and `eval/roll_generated.py` (generated sets),
into `GOAL/kick_roll_labels.json`, `snare_roll_labels.json` and their `_generated` twins (`registry.FAMILIES`).** The
builders call the family builders' note extraction (`kick_labels.song_labels`'s record truth, `detector_labels.song`'s
parts) and never edit them (an edit would move the family's labels sha, which is in every run id, `lab/run.py:156`).
Schema: the lab's dict (DETECTOR_LAB_DESIGN.md D4) with `positives` = starts, `window_s = [-0.06, 0.30]` (a fire
from 60 ms before the 4th note to 300 ms after it catches the start), two new keys `active` (spans [start, end] per
roll, training positives, D7) and `hard_spans` (vouched-negative spans: every double and triple from its first note
to its last plus 200 ms; every hat run and bass-note run of ≥ 4 notes under 200 ms; section 3.2), `unscored` = the
family's unknown spans plus `[end, end + 0.5]` after every roll (the hangover region, neither hit nor false), and
`rolls` = the note times of every roll (for the rate and end metrics). The audit (`lab/audit.py`) gains
`AUDIT_PARENT = {'kick_roll': 'kick', 'snare_roll': 'snare'}`: a roll song passes iff its parent family's audit passes
for that song (the sidecar read by the parent labels' sha) and the roll labels record the same offset; the kept share
is not a roll audit's business. Rejected: writing the raw note list into the family labels files, because it changes
their sha.

**D7. Four named edits to shared lab code, each bit-identical by default** (the DETECTOR_LAB_DESIGN.md D1 pattern):
1. `detector_eval.rows`: when the labels carry `active`, candidates inside those spans are `y = 1` and train; when
   they carry `hard_spans`, candidates inside are vouched negatives (`hard = True`). Files without the keys produce the
   same (mask, y, hard) as today (test: every committed labels file, byte for byte).
2. `detector_eval.judged` / `score` / `song_result` / `held_out`: the fire rule is an argument (`decision`), default
   `fires` (today's); `held_out` reads it as the sixth element of `meta[t]`, absent today. New
   `detector_eval.state_fires(p, avail, th, sr, hop, off_logit, hangover) -> (starts, ends)` (section 4.3).
3. `lab/config.py`: key `decision`, `"fires"` or `{"state": {"off_logit": 1.0, "hangover_ticks": 3}}`; `migrate` reads a
   missing key as `"fires"`; **`key()` omits `decision` when it is `"fires"`**, so no existing run id moves (the
   `sampling` null rule, `lab/config.py:138`; test: the key of every committed config is unchanged).
4. `lab/models.song_meta(s, c, decision)`: the sixth element; `lab/score.py` passes `cfg.decision`.
Plus additive registrations: `components.FINDERS['tick']` (and `AUDIO_FINDERS`), `components.MEASURES['roll24']`,
`registry.FAMILIES` two entries, `score.py` a `roll` report section (section 7.2) and `PARTNER` entries for the roll pair
with their own window (`COINCIDENT = {'snare': .020, 'kick': .020, 'kick_roll': .150, 'snare_roll': .150}`),
`contract.py` entries and prefixes, `lab/export/finder_tick.py`, `lab/export/roll.py`. Nothing else in `eval/` or
`lab/` changes.

**D8. The state rule is one rule, committed, in both languages.** Per tick in emission order, with `th` the cutoff,
`off = expit(logit(th) − off_logit)`, `hangover` in ticks: off → on when `p ≥ th` (the start fires at this tick);
while on, `below` counts consecutive ticks with `p < off`; on → off at the tick where `below` reaches `hangover`
(the end event, value 0). `off_logit` 1.0 and `hangover` 3 are fixed in v1, not chosen on data (one knob fewer; the
cutoff is still chosen honestly). **Trigger:** if the exam's end lateness median exceeds 300 ms or rolls flicker
(more than one on edge per true roll on average), P2 ends with a sweep of both on the pair pool and the chosen
values go into the config. Rejected: the 60 ms refractory rule for rolls, because it answers per hit.

**D9. Exam = the family's audit-passing project songs, same honest cutoffs, same bootstrap; the headline is roll-start
F1.** Songs without rolls stay in the exam (they are where false rolls come from). Generated sections train only and
are reported apart (lab D8, Peter: "The held-out exam stays on real songs"). Section 7 gives the metric set and what
"not degrading" means, as equalities. **The exam is real only when it holds at least 60 roll starts in at least 8
songs** (two roll songs per group, so every held-out group has recall to measure); today it holds 31 kick starts in
3 songs and 6 snare starts in 2 (section 8), so P3 is blocked on section 8.2's route or open decision 3.

**D10. CPU budget: 10 ms per audio second per roll detector in a release build**, so the send's detectors total
kick 40 + snare-with-kick 70 (the kick runs once) + 2 × 10 = 90, under the 100 ms ceiling
(DETECTOR_RUNTIME_DESIGN.md D16; today's numbers from its section 5.4 (how the shipped kick keeps working)).
Expected: 16 biquads at 48 kHz plus a 1 ms decimation about 2 ms/s, the per-tick autocorrelation under 1 ms/s, trees
negligible. **Trigger:** a cover or release set over 10 stops the phase and goes to Peter with the per-part breakdown.

**D11. Latency is inherent and stated, not tuned away.** A roll start is knowable at its 4th note; the detector
needs that plus one tick (53 ms) plus whatever of the short window it needs to see three periods. On stage: a
32nd-note roll at 174 BPM lights the cell about 150–200 ms after its first hit; an eighth-note roll at 150 BPM about
650 ms after. The hit triggers mark the first hits either way. The lab reports start lateness against the first note
and against the 4th note separately.

**D12. The roll detector never gates or holds the single-hit triggers.** Both cells are independent; a performer
maps what they want. A roll-aware hold (suppress Kick flashes while Kick Roll is on) is a modulation-matrix feature
for later, Deferred (open decision 6).

**D13. Two detectors, one recipe, two model files.** `kick_roll` and `snare_roll` differ in the front end's band
centres and the family's labels; the config files differ only in `family`, and the finder is the same. Rust gets two
`EventKind` variants and two embedded slots (`detectors.rs:56 EMBEDDED`, 4 entries), and
`every_event_kind_has_an_embedded_model` keeps the match exhaustive.

### Consequences, stated honestly

- A tick model has about 19 candidates a second whatever the music, so the exam's negative class is huge and the
  cutoff grid (`THRESHOLDS`, 95 steps) is coarse at the top; the oracle frontier in P2 shows whether that costs
  anything before any tuning.
- The snare band is full of hats in every electronic track (the census: hat runs of 4+ under 200 ms in 19 of the 43
  project songs, 199 runs); the `other` group ratio and the hat-run hard negatives are the defence, and false-roll
  seconds per minute on real songs is the number that says whether it holds. If it does not, the net phase
  (Deferred) is the next step, not a band tweak.
- Delay-echo trains (one hit, a 1/16 feedback delay) are a roll to the ear and to the audio; MIDI cannot label them,
  so real songs carry them as unknown false fires until a generated set includes echo trains as explicit negatives
  (section 8.3).
- Two more model files add about 2 MB each of goldens to git (the dense and quiet-start clips, as every set).
- The `AudioFeatureKind::ALL` arrays grow to 11 in core and the UI mirror; every exhaustive match over the kind
  fails to compile until it names the new kinds — that is the migration, compiler-driven.
- The real exam cannot be scored for recall today (D9, section 8.2): the first honest headline waits for roll-rich
  real songs to enter the lab, which is labels work, not detector work.

## 3. The roll truth

### 3.1 Events from notes

For one family's sorted note times `h[0..n)` (export seconds, shifted by the song's proven offset like every label):

```
runs   = maximal index ranges [a, b] with h[i] - h[i-1] < 0.200 for every a < i <= b
roll   = a run with b - a + 1 >= 4
start  = h[a + 3]
end    = h[b] + (h[b] - h[b - 1])
rate(t)= 1 / median(h[i] - h[i-1] for the last three gaps at or before t), t in [start, end]
double = a run with 2 or 3 notes            -> hard span [h[a], h[b] + 0.200]
```

Hat runs and bass-note runs of ≥ 4 notes under 200 ms are hard spans the same way (hat tracks and pads as
`catalog/als_scan.py` classes them; bass tracks as `eval/note_labels.py` defines them; the census shows the part
rule classes some hat tracks as bass by their group, so a bass run that is also a hat run is one span). Notes on
muted tracks are unknown: their spans go to `unscored`, never to negatives (memory `detector-label-truth`).

### 3.2 Labels dict per song

| Key | Value |
|---|---|
| `positives` | roll starts |
| `window_s` | `[-0.06, 0.30]` |
| `active` | `[[start, end]]` per roll — training positives (D7.1) |
| `hard_spans` | doubles and triples, hat runs, bass runs (section 3.1) — vouched negatives |
| `hard_neg` | `[]` (point negatives are not used; `rows` keeps reading it) |
| `unscored` | the family's unknown spans ∪ `[end, end + 0.5]` per roll ∪ the first and last second |
| `loop_spans` | the family's |
| `rolls` | `[[h[a] .. h[b]]]` per roll, for the rate and end metrics |
| `positives_only`, `kind`, `duration_s`, `timing_shift_ms`, `sources`, `builder`, `proof` | as the family's |

Generated sections (`eval/roll_generated.py`) use `generated_sets.sections` and `drum_notes` with the set's start
rule (`eval/generated_sets.py:54,108`), kick pad 36, snare pads `{38, 39}` (`eval/snare_generated.py:15`), hat and
bass pads and tracks as the catalogue classes them; rolls cut by a section edge are unscored (the lab's rule).

## 4. The detector

### 4.1 Tick finder

`tick:H` — candidate hops `k·H` for every k with `(k·H + 1)·256 ≤ samples`, `avail = cand`, found at the end of
that hop. H = 10 (`tick.hops` in the model file). `max_candidates(samples) = samples / (H·256) + 1`; `deadline() = 0`;
`f15` zeros (only the kick finder may name `f15`, as today). Audio-only, so release goldens run it on a bare clip.

### 4.2 `roll24`

Front end (`roll.*` entries: `roll.centres` f64[16], `roll.q` 2.0, `roll.ma_ms` 5, `roll.ms` .001, `roll.floor_db`
−80): two groups of 8 log-spaced causal band envelopes, `e_g,b(t)` in dB on the 1 ms grid:

| Family | family group | other group |
|---|---|---|
| `kick_roll` | 40–320 Hz (the low bank's centres, `kick_goal_lowbank.py:15`) | 1–5 kHz |
| `snare_roll` | 150 Hz–5 kHz | 6–14 kHz (where hats live) |

Onset strength per group: `o_g(t) = Σ_b max(0, e_g,b(t) − e_g,b(t − 3 ms))` (`roll.flux_lag_ms` 3). Per window
`W ∈ {0.25, 0.70}` s (`roll.windows_s`), ending at the tick's sample, over the family group's `o` centred by its
window mean, lags `τ ∈ [25, 250]` ms (`roll.lag_ms` [25, 250]), 12 numbers in this column order:

| # | name | definition |
|---|---|---|
| 1 | `acf_peak` | `max_τ r(τ)`, `r(τ) = Σ o(t)o(t−τ) / Σ o(t)²` over the window |
| 2 | `period_ms` | the smallest τ with `r(τ) ≥ 0.7 · acf_peak` (the shortest period that explains the window; a 32nd roll never reads as 16ths) |
| 3 | `acf_half` | `r(period/2) / acf_peak` (0 when period/2 < 25 ms) |
| 4 | `onsets` | local maxima of `o` at least 20 ms apart above `median(o) + 2·MAD(o)` over the window |
| 5 | `ioi_cv` | coefficient of variation of the inter-onset intervals; 1.0 when fewer than 3 onsets |
| 6 | `ioi_med_ms` | median inter-onset interval; 0 when fewer than 2 onsets |
| 7 | `level_slope` | least-squares slope of the family group's mean dB over the window, dB/s (crescendo) |
| 8 | `tilt_slope` | slope of (mean of the top 3 bands − mean of the bottom 3) over the window, dB/s (a high-pass sweep) |
| 9 | `level_rel` | the window's mean dB minus the median of the group's mean dB over the last 4 s (`roll.ref_s` 4) |
| 10 | `peak_decay` | `o` at the last onset minus `o` at the first onset; 0 when fewer than 2 onsets (echo trains decay) |
| 11 | `o_max` | `max o` over the window |
| 12 | `other_ratio` | `log10((Σ o_other + ε) / (Σ o_family + ε))` over the window, ε 1e-6 |

Width 24: columns 1–12 for W = 0.25 s, 13–24 for W = 0.70 s. The first `W` of a stream reads a shorter window
(whatever has arrived); new goldens lead with 0.5 s of silence so the start edge never reaches a golden
(DETECTOR_RUNTIME_DESIGN.md D10). The rate the detector reports is `1000 / period_ms` of the short window when
its `acf_peak ≥ 0.3`, else of the long window (`roll.rate_min_acf` .3); the model file carries every constant.

Industry methods this builds on, named: the onset-strength envelope is the half-wave-rectified band-flux of
Bello et al. 2005 (IEEE TSAP onset tutorial) and SuperFlux (Böck and Widmer, DAFx 2013), already the live transient
detector's design (`analysis.rs:480`); the short-window autocorrelation of that envelope is the tempo-estimation front
end of Scheirer 1998 (JASA) and Ellis 2007 (J. New Music Research), used here at roll rates (4–40 Hz) instead of beat
rates, the same quantity a tremolo-rate or modulation-spectrum estimator reads; the on/off rule with a lower off
threshold and a hangover is the voice-activity-detector shape (ITU-T G.729 Annex B). Where this goes beyond proven
practice, plainly: the roll truth rule from MIDI density (no public drum dataset annotates rolls as events; ADTOF and
ENST annotate hits), the rate-coded level as a performance signal, and the start-event exam with a state rule. The
published state of the art for dense drum events is a sequence model over onset strength (Vogl et al., ISMIR 2017,
CRNN; TCNs since), which Peter deferred (BUG-9ngk8.12, frame-wise causal drum model); section 14 names the trigger
that revives it here.

### 4.3 Decision `state`

`detector_eval.state_fires` and `trained/detector.rs StateRule`, the same words:

```
off_th = expit(logit(th) - off_logit)
on = false; below = 0
for each tick i (emission order):
    if not on:
        if p[i] >= th: on = true; below = 0; starts += i
    else:
        if p[i] >= off_th: below = 0
        else: below += 1
        if below >= hangover: on = false; ends += i
```

An on tick emits the level `v(rate)` (D3); the end tick emits 0. The start is the fire the scorer judges; NaN never
turns the state on.

## 5. Lab plug-in points

Configs `lab/experiments/kick_roll_v1.json` and `snare_roll_v1.json`:

```json
{"family": "kick_roll", "candidates": "tick:10",
 "models": {"trees": {"measures": ["roll24"], "max_iter": 200, "learning_rate": 0.05, "max_leaf_nodes": 15,
                      "min_samples_leaf": 40, "l2": 1.0, "templates": null}},
 "combine": "trees", "stage": null,
 "decision": {"state": {"off_logit": 1.0, "hangover_ticks": 3}},
 "sampling": {"per_song": "median", "generated": 0.5}, "train": ["project", "generated"],
 "seeds": [0, 1, 2, 3, 4], "folds": 4}
```

`per_song: median` draws per song are the lab's rule; tick rows make every song's row count large and the
class-balanced, song-equal weighting (`lab/models.py weights`) handles the imbalance as it does for the kick.
`config.check` refuses a `state` decision with a finder other than `tick` and a `tick` finder with a net or a stage
(v1 contract; both are Deferred rows, each with its trigger).

Training, cutoffs, pair models, layers, saves, `score.py`, `compare.py`, `overnight.py`: unchanged. A tick config's
saves hold `trees__<song>` (the final layer); `score.py` judges them with `state_fires` and writes the `roll` block.

## 6. Runtime and app

### 6.1 Model file entries (contract v2, additive)

| Entry | Type | Meaning |
|---|---|---|
| `family` | text | `kick_roll` / `snare_roll` → `EventKind::KickRoll` / `SnareRoll` |
| `finder` | text | `tick` |
| `tick.hops` | i64 | 10 |
| `models` / `combine` | text | `trees` / `trees` |
| `decision` | text | `fires` (every shipped model, written explicitly from now on; a missing entry reads as `fires`) or `state` |
| `decision.off_logit`, `decision.hangover` | f64, i64 | required iff `state` |
| `roll.*` | — | `centres` f64[16], `q`, `ma_ms`, `ms`, `floor_db`, `flux_lag_ms`, `windows_s` f64[2], `lag_ms` f64[2], `ref_s`, `rate_min_acf` |
| `trees.measures` | text | `roll24`; `trees.widths` [24] |

Rust refuses (named `SpecError`s, each a `refuses_each_bad_entry` case): `state` without both parameters or with
`hangover < 1`; `roll24` without every `roll.*` entry; `tick.hops < 1`; a `tick` finder with `f15`, a net or a stage;
`decision` text unknown. `runtime_parts.json` gains `"finders": ["kick", "snare", "tick"]`, `"measures": {..., "roll24": 24}`,
`"decisions": ["fires", "state"]`; `release.py parts` and `registry_matches_runtime_parts` check both sides.

### 6.2 Committed Rust types

```rust
// trained/spec.rs
pub enum FinderKind { Kick, Snare, Tick }                 // Tick carries no state; `tick.hops` is read by the finder
pub enum Measure { F15, Tail3, Tmpl3, Prof32, Fall16, Low16, Cdom4, Note13, Kick2, Roll24 }   // width 24
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecisionRule { Fires, State { off_logit: f64, hangover: u32 } }
pub struct Spec { /* as today, plus */ pub decision: DecisionRule, pub tick_hops: Option<u64> }

// trained/finder/mod.rs
pub enum Finder { Kick(Box<kick::BaseDetector>), Snare(snare::SnareCands), Tick(tick::Tick) }
// trained/finder/tick.rs: Tick { hops: u64, next: u64 }; push appends cand = avail = k·hops at the end of that hop

// trained/measure/roll.rs — the 16-band front end and the 24 columns of section 4.2; reads `roll.*`
pub(super) struct RollBank { /* two groups of 8 Sos + RunningMean, a 1 ms grid, a ring of 4.7 s of dB envelopes and o */ }

// trained/detector.rs
pub struct Fire { pub sample: u64, pub cand_hop: u64, pub avail_hop: u64 }           // unchanged: the start
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct State { pub on: bool, pub rate_hz: f64, pub start: bool, pub end: bool }
pub struct Decision<'a> { /* as today, plus */ pub state: Option<State> }            // Some for state models
pub(crate) enum Rule { Fires(FireRule), State(StateRule) }                            // StateRule: section 4.3
impl TrainedDetector { pub fn level(rate_hz: f64) -> f32; /* D3's v(rate) */ }

// detectors.rs
pub enum EventKind { Kick, Snare, KickRoll, SnareRoll }                               // names kick_roll, snare_roll
pub struct DetectorEvent { pub kind: EventKind, pub sample: u64, pub value: f32 }     // fires carry 1.0; on ticks the level; an end 0.0
pub(crate) const EMBEDDED: [(EventKind, &[u8]); 4];                                  // + kick_roll_model.mkick, snare_roll_model.mkick
```

`EventDetector for TrainedDetector`: a fires model pushes one event per fire with value 1.0 (today's behaviour);
a state model pushes one event per on tick (value = level) and one per end (0.0). `max_events(samples)` for a tick
model = ticks in `samples` + `PENDING`. Roll detectors read no other detector, so `send_detectors` pushes them as
plain detectors (never `KickFed`). The worker, its rings and `collect` are unchanged (`EVENT_RING` 256 holds
13 s of two roll detectors' ticks).

### 6.3 From events to the features

`analysis.rs`: a `LevelQueue` beside `FireQueue` (`:1812`), one per roll family, holding up to 32 `(sample, value)`
pairs; `queue_detector_events` (`:2009`) routes `KickRoll` to the Low band's and `SnareRoll` to the Mid band's queue;
per hop (`:2130`), `level(sample_count, &mut f32)` applies every queued pair at or before the hop's end sample, in
order, and the field holds the last value (no decay) until the next pair. A full queue drops its *oldest* pair
(the opposite of `FireQueue`, which drops new fires): for a level the newest value is the truth. `BandFeatures`
gains `pub kick_roll: f32` and `pub snare_roll: f32` (`audio_features.rs:23`), documented like `kick` and `snare`.

`manifold-core/src/audio_mod.rs`: `AudioFeatureKind::KickRoll`, `SnareRoll` (serde `kickRoll`, `snareRoll`; `ALL`
11; labels "Kick Roll", "Snare Roll"; `fixed_band` Low / Mid; `extract` reads the new fields). The impulse-kind list
in `modulation.rs:943` stays as it is: a roll level is not an impulse. `manifold-ui/src/types.rs` mirrors the two
kinds; `ui_translate.rs:122` maps both ways; `queries.rs:386` labels them; `TRIGGER_SOURCE_CHIPS` (`state.rs:234`)
gains `("Kick Roll", KickRoll, Low)` after Kick and `("Snare Roll", SnareRoll, Mid)` after Snare, and the band-ignore
match at `state.rs:256` names them with Kick and Snare. The offline renderer (`offline_audio_mod.rs:104`) runs the same
analyzer, so export parity is by construction.

Persistence: a saved `kickRoll` cell loads in any build that has the kind; an older build refuses the unknown
variant the way it refuses any unknown enum text — stated, not new.

## 7. Exam

### 7.1 Songs, cutoffs, headline

Exam = `kind == 'project'`, roll-audit-passing songs of the family (D6), in 4 groups; every roll config runs the
pair models so each song's cutoff comes from models that never saw its group (lab P6). **Headline: pooled roll-start
F1 at honest cutoffs, with the 90% paired song bootstrap of `compare.py`**, songs without rolls included (they
contribute false starts only). Generated sections: the same numbers in the `generated` section, never a cutoff.
D9's bar (60 starts in 8 songs) decides whether the headline is real or provisional.

### 7.2 The `roll` report block (`lab/score.py`, per song, then averaged over the exam songs that hold rolls — never pooled per hit, so a 265 s sixteenth pattern counts once)

| Number | Definition |
|---|---|
| start recall, precision, F1 | `judged` with `state_fires` on `positives` and `window_s` (these pool as every family's) |
| start lateness | detector start − 4th note (median, p90 ms); and − first note, reported for Peter |
| end lateness | detector end tick − truth end (median, p90 ms), over caught rolls |
| rate accuracy | share of on ticks inside caught rolls with `abs(log2(rate_est / rate_true)) ≤ 0.25` |
| false-roll seconds per minute | on ticks outside `active` ∪ `unscored` ∪ `loop_spans` × 53 ms, per scored minute, over every exam song |
| flicker | on edges inside one true roll beyond the first, per caught roll |
| coincident | start recall on rolls whose partner-family roll starts within 150 ms (`PARTNER` kick_roll ↔ snare_roll) |

The oracle frontier (per-song best cutoff, picked with the answers) is printed first, as AUDIO_EVAL_HARNESS_GUIDE.md
section 0 (trained detectors) requires of any new feature family.

### 7.3 Not degrading the standard detectors — the equalities

| Gate | Command / test | Expected |
|---|---|---|
| Kick and snare run ids unchanged | `lab/run.py --plan kick_v2_final` and `--plan snare_v2_mid` before and after every phase | identical ids |
| Kick and snare labels unchanged | `sha256sum` of `kick_labels*.json`, `snare_labels*.json` before and after | identical |
| Shipped models byte-identical | `trained::tests::embedded_assets_are_the_shipped_sets`; `git diff --stat -- crates/manifold-audio/assets/kick_model.mkick crates/manifold-audio/assets/snare_model.mkick` | pass; empty |
| Kick and snare fires unchanged beside the roll detectors | new `detectors::tests::roll_detectors_leave_hit_fires_unchanged`: `send_detectors` with and without the roll slots on every kick and snare golden clip → kick and snare events equal | pass |
| Cost under the ceiling | `trained::tests::detector_cost` in release: each roll set ≤ 10 ms/s; all detectors ≤ 100 | numbers in the landing commit |

The one non-structural claim is the ceiling; everything else is an equality a diff can check.

## 8. Data — the roll census (2026-10-12, read-only, MIDI and label files only; rule of section 3.1; classes within ±10% of the beat fraction)

### 8.1 What exists

**Project songs (43; every kick and snare audit passes; muted-track rolls: none).** Rolls under the rule:

| Song | Kick rolls | Snare rolls | Doubles/triples K/S | Hat runs | BPM |
|---|---|---|---|---|---|
| back_to_you | 13 rolls, 52 hits, 6.4 s — all 4-hit sixteenths (163 ms), none accelerating, velocity flat | 0 | 5 / 0 | 6 | 92 |
| late_night | 17 rolls, 76 hits, 7.2 s — 9 × 4-hit sixteenths (139 ms), 8 × 5-hit at 104 ms (3/16 beat), 8 accelerating | 0 | 0 / 0 | 5 | 108 |
| worship | 1 roll, 7 hits, 120 ms sixteenths, accelerating | 0 | 0 / 1 | — | 134 |
| openings | — | 1 roll of 2047 hits, 264.6 s: a sixteenth pattern (129 ms) across the song, 266 of them prominent | — / 0 | 0 | 116 |
| gerrit | 0 | 5 rolls, 30 hits, 2.7 s — all 6-hit sixteenths (131 ms) | 15 / 7 | — | 112 |
| the other 38 | 0 | 0 | kick 171 and snare 194 in all, e.g. eat_sleep 59 K, weight 69 S, water_shimmer 48 S | 199 runs in 19 songs | — |

Exam totals: **kick 31 rolls (31 starts), 135 hits, 14.3 s, in 3 of 30 kick songs; snare 6 rolls, 2077 hits
(2047 of them openings), in 2 of 37 snare songs.** Gap classes: kick sixteenths 23, other 8; snare sixteenths 6.
Velocity rises in 1 of the 22 kick rolls whose MIDI is matched (the real ones are fills, not builds). Bass runs of
4+: 111 in 5 songs, most of them hat tracks the part rule classes as bass by their group. Cross-check: the kick
builder's roll spans equal the census spans in all 30 songs and every dropped kick is inside them; every snare roll
hit is inside the snare labels' `dense` in all 37.

**Generated sections (78; drum tracks audible; the builders' roll spans equal the census in all 78).**

| Set | Kick rolls | Snare rolls |
|---|---|---|
| linn_1–4 (140 BPM) | 2 (4-hit sixteenths, linn_2) | 0 (10 doubles) |
| rolls_var01–04 (140) | 10 rolls, 1124 hits, 80.6 s: two builds per section of 88–97 and 184–194 hits (6.4–13.8 s), 32nds at 54 ms, all accelerating, all rising in velocity; plus 2 four-hit sixteenth runs | 11 rolls, 1124 hits, 80.7 s: the same two builds per section (88–96, 184–192 hits), plus 3 four-hit sixteenth runs |
| rolls_music01–12 (90–174) | 0 (11 doubles) | 5 rolls, 112 hits: sixteenths at 140 and 130, 32nds at 174 (48 hits) and 150 (24), all rising in velocity |
| mod_01–58 (122–174) | 1 (mod_14: 6 eighths at 160 BPM, accelerating) | 19 rolls, 304 hits in 18 sections: 32nds 8 (20–44 hits, all accelerating), sixteenths 7 (4–5 hits), sixteenth triplets 1, other 3 (65 ms at 174); velocity rises in 16 |

Generated totals: **kick 13 rolls, 1138 hits, 82.1 s in 6 sections; snare 35 rolls, 1540 hits, 109.4 s in 26
sections; kick and snare rolling together: 0.** Hat runs 86 (4377 hits) in 41 sections; audible bass runs 5; the
Modulations set's muted Bass group holds 71 more (unknown). Gap classes: kick 32nds 8, sixteenths 4, eighths 1;
snare 32nds 18, sixteenths 13, triplets 1, other 3.

**Catalogue (54 compositions with a local Ableton set holding kick or snare MIDI: 39 label-ready, 15 blocked).**
Rolls live in 11 (kick) and 17 (snare) of them:

| Composition (set, BPM) | Kick rolls / hits | Snare rolls / hits | Hat runs |
|---|---|---|---|
| running (Mixdown BETA, 136) | 24 / 120 — sixteenths 8, 32nds 16, velocity rising | 7 / 264 | 4 |
| better_strangers = know_youre_there (134) | 21 / 234 sixteenths, velocity rising 18 | 1 / 112 | 3 |
| break (Slowdown, 144) | 14 / 66 sixteenths | 1 / 256 | 0 |
| motion (Dub Mixdown V2, 130) | 12 / 83 | 0 | 15 |
| openings_2023 (Slow Version, 116) | 7 / 28 | 1 / 2048 | kick and snare together 7 |
| dusk2026, dusk2037 (76) | 4 / 16 32nds, accelerating | 0 | 6 |
| rush (134), progression (168, eighths), dream (140) | 4 / 149; 5 / 32; 1 / 9 | —; —; 9 / 48 | 4; 2; — |
| desire (Wait V2, 138) | 0 | 39 / 712, velocity rising 17 | — |
| drive (144), mendez (125), rushing_back (140), control (126), echo (138), touch (138) | 0 | 8 / 32; 7 / 156; 6 / 34; 4 / 145; 4 / 32; 4 / 20 | 3; 7; —; —; —; 44 |
| best_of_me (102), drmendez (126), skyline (140), recognition (154) | 0 | 2 / 320; 2 / 120; 1 / 128; 1 / 48 | — |

Catalogue totals: **kick 117 rolls, 987 hits, 96.1 s (sixteenths 88, 32nds 24, eighths 5); snare 98 rolls, 4587
hits, 545.7 s (all sixteenths); kick and snare together 11.** Doubles/triples kick 373, snare 194; hat runs 188 in
30 compositions. Not countable: 29 compositions without a set, 3 with online-only sets (not opened), 23 with
audio-clip drums and no MIDI, 25 with sets but no drum MIDI and no audio; catalogue muted groups are unknown (the
mix test needs audio). These compositions are not lab songs: each needs a bounce matched to its set, a proven
export offset and the family labels pipeline before it can be examined.

### 8.2 Verdict

**Not enough to exam now; enough to begin training the snare roll, thin for the kick roll.** Recall cannot be scored
on real songs: 31 kick starts sit in 3 of 30 songs (23 of them minimal 4-hit sixteenth fills in one song at 92 BPM)
and 6 snare starts in 2 of 37, one of them a whole-song pattern — with 4 groups, at least one held-out group has no
roll of either family. Precision can: every exam song carries the false-roll material (199 hat runs, 365 doubles
and triples), so false-roll seconds per minute is honest from day one. Training: the snare has 35 generated rolls at
122–174 BPM across 32nds, sixteenths and a triplet, 29 of them builds; the kick has 13, ten of them two shapes (the
VAR builds), one eighth-note roll, no flat 32nds, and no kick-and-snare-together anywhere in the generated sets,
while 11 catalogue builds roll both at once.

**The route to a real exam is labels work on the catalogue's roll compositions, not detector work:** running,
better_strangers (or know_youre_there), break, motion, desire, dream, drive, mendez and rushing_back hold about 100
kick and 80 snare roll starts in 9 songs. Each enters the lab the standard way (chosen bounce, proven offset via
`lab/offsets.py`, `registry.py build`, the family labels, the audit) — the label-ready path the catalogue was built
for. With them and today's 5 roll songs the exam clears D9's bar (60 starts, 8 songs) for the kick and nearly for
the snare. **The generated roll set (section 8.3) is still needed**, for what no real song labels: echo-train
negatives, kick and snare rolling together, flat-velocity 32nds, eighth-note rolls above 150 BPM, triplets, and
kit variety. Order: P1 and P2 run now on what exists (the numbers are provisional, labelled so); P3 waits for the
nine compositions and the generated set, or for Peter's ruling on open decision 3.

### 8.3 What a generated roll set must contain (Peter builds it; MIDI and one-shots only, full mix plus Drums
stem, known export start — memory `detector-label-truth`)

Per family, each as its own locator section at 90, 128, 150 and 174 BPM:
- rolls at eighths, sixteenths, sixteenth triplets, thirty-seconds and thirty-second triplets, 1, 2, 4 and 8 bars long;
- accelerating builds (eighths → sixteenths → thirty-seconds over 8 bars), the classic;
- flat velocity, rising velocity, and velocity rising with a high-pass sweep on the roll bus (the live case of
  BUG-qy8q7 (drop kicks missed after a high-passed build-up roll));
- rolls over the full mix and over a breakdown bed (no kick), and kick and snare rolling together;
- hard negatives, every one labelled in MIDI: hat sixteenths and thirty-seconds under a backbeat; a bass line at
  sixteenths; doubles, flams and triples on the backbeat; and one-hit delay-echo trains (a single snare into a 1/16
  feedback delay, 4–8 repeats) on a track named for it, so the builder can mark them as vouched negatives;
- the kit variety of the V1 set (effects, compression, distortion on the drum bus), never one clean kit.

## 9. Cost and latency

CPU: D10. Memory: the `RollBank` ring holds 4.7 s of 16 envelopes and 2 onset strengths at 1 ms (about 85 k f64,
680 kB) per detector, allocated at load. Worker events: ≤ 19 per second per roll detector. Content thread: one
`LevelQueue` drain per hop, no allocation. Latency: D11.

## 10. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| The roll truth is exactly the standard exclusion | `eval/tests/test_roll_labels.py::test_rolls_are_the_family_exclusion`: for every song, the roll builder's padded spans equal the kick labels' roll part of `unscored` and the snare labels' `dense` roll part (the census already shows this equality by hand) |
| A note is nobody's only when unknown | same test: every audible family note is in the family's positives, dropped, dense_pos or a roll of the roll labels |
| Standard run ids, labels and models never move | section 7.3 gates, run at every phase; `lab/test_lab.py::test_decision_fires_keeps_run_ids` (committed configs' `key()` unchanged) |
| `rows` is bit-identical without the new keys | `lab/test_lab.py::test_rows_parity_without_active` (every committed labels file) |
| The state rule is one rule | `trained::tests::parity_<set>` compares `starts`, `ends`, `state.on`, `state.rate_hz` (section 12 P4 tolerances); `lab/test_lab.py::test_state_fires_edges` (hand-built p sequences: hysteresis, hangover, NaN) |
| Rust runs only what the file names and refuses the rest | `trained::spec::tests::refuses_each_bad_entry` with the section 6.1 cases |
| Parts declared on both sides | `registry_matches_runtime_parts`; `test_runtime_parts_exist_in_the_lab` |
| Every family has a slot and a model | `every_event_kind_has_an_embedded_model` (exhaustive `slot`) |
| Push never allocates | capacity assertion in every end-to-end parity test (existing pattern) |
| A roll cell reads 0 when no roll is on, holds its level while on | `analysis::tests::roll_level_holds_until_the_next_event_and_drops_at_end` |
| Chips and kinds agree across core, UI and app | `param_slider_shared::state::tests` chip list test extended; `ui_translate` round-trip test over `AudioFeatureKind::ALL` |
| The exam is real or says it is not | `lab/score.py` prints `exam: provisional (N starts in M songs; bar 60 in 8)` in the `roll` block whenever D9's bar is unmet; `compare.py` carries the line |
| Cost under budget | `detector_cost`, numbers recorded at P4 and P6. Enforcement: none automatic — timing tests are not gates on shared machines (the runtime design's honest-cost line) |

## 11. The plausible-wrong turns, by name

- **"Put it in the stage, it already re-scores recent hits."** No (D1): per-candidate, refractory-bound, 8 s memory, entangled rows.
- **"Train the snare on `dense_pos`, it was +1.8."** No: four songs lost 11–29 recall; Peter's basics-first ruling.
- **"Fire per hit with a 30 ms refractory for rolls."** No (D8): the roll is a state; the hit detectors already own hits.
- **"Add a `centres` parameter to `kick_goal_lowbank.band_envelopes`."** No (D5): the kick's measure code key moves. New module.
- **"Add `decision` to every config's key."** No (D7.3): every run id moves, every cached layer orphaned.
- **"Read `p_combined` of the family detector as a roll feature."** Not in v1 (D4): trained to reject roll hits; keys the roll on the family's release. Deferred.
- **"One `Roll` kind, pick the family by band."** No (D1): silent zero on Full/High; two band-locked kinds.
- **"Label the first note as the start."** No (D2).
- **"Define a roll by the tempo grid."** No (D2, guard rails): no tempo in the detector or the truth.
- **"Score rolls as per-hit recall inside `dense_pos`."** No: that is `dense_recall`, the standard detector's question.
- **"Let the roll cell hold the Kick trigger during a roll."** Not in v1 (D12).
- **"Put the generated roll sections in the exam, the real songs have too few rolls."** Not without Peter (open decision 3); until then recall on them is the `generated` section and the headline says provisional.
- **"Drop the delay-echo negatives, MIDI can't label them."** No: the generated set carries them on a named track (section 8.3); real-song echoes stay unknown.
- **"Pool the roll metrics per hit."** No (section 7.2): one 265 s pattern would be the whole exam.

## 12. Phasing

### P0 — Data (not a session of this design; the census is section 8)

**Entry state for P3:** the nine catalogue compositions of section 8.2 are lab songs with family labels and passing
audits, or Peter has ruled on open decision 3; and the generated roll set exists with its Drums stem, its timing
proven (`generated_sets.timing_proof`) and a `GenSet` row added to `eval/generated_sets.py:45`, or Peter has ruled on
open decision 4. P1 and P2 do not wait for it.

### P1 — Roll truth (Python, one session)

- **Entry state:** `git merge-base --is-ancestor df8030ca6 HEAD`; `rg "def roll_spans" tools/audio_analysis/eval/detector_labels.py` hits; `rg "def song_labels" tools/audio_analysis/eval/kick_labels.py` hits; `lab/run.py --plan kick_v2_final` and `--plan snare_v2_mid` printed and kept.
- **Read-back:** this doc sections 2–3, 7.3, 10, 11; `eval/kick_labels.py`, `eval/detector_labels.py` (`Family`, `song`, `project_parts`, `dense_spans`, `roll_spans`), `eval/snare_generated.py`, `eval/kick_generated.py`, `eval/generated_sets.py`, `lab/audit.py`, `lab/registry.py`; the census scripts (`roll_core.py`) as a worked reading of the rule. Restate D2, D6, D7.1 and the forbidden moves.
- **Deliverables:** `eval/roll_labels.py`, `eval/roll_generated.py`, the four labels files and their audits; `registry.FAMILIES` entries and `lab/registry.py build`; `AUDIT_PARENT` in `lab/audit.py`; `detector_eval.rows` named edit (`active`, `hard_spans`); `eval/tests/test_roll_labels.py` (`test_rolls_are_the_family_exclusion`, the nobody's-note test, start/end/rate on hand-built note lists, the 3-note-is-not-a-roll case); `lab/test_lab.py::test_rows_parity_without_active`; the catalogue's hat and bass classes read through `catalog/als_scan.py`, no new classifier.
- **Gate:** tests pass; `lab/audit.py kick_roll` and `snare_roll` print their tables with the parent verdicts; the counts equal section 8.1's (31 kick, 6 snare starts on the exam; 13 and 35 on the generated sections); the section 7.3 run-id and labels-sha gates unchanged; negative: `git diff --stat -- tools/audio_analysis/eval/kick_labels.py tools/audio_analysis/eval/snare_labels.py tools/audio_analysis/eval/detector_labels.py` is empty (the roll builders import, never edit).
- **Demo:** none — L1 (the audit tables are the artifact Peter reads).
- **Forbidden:** editing the family builders or `generated_sets.py` beyond a `GenSet` row; audio analysis for any label; point hard negatives at tick resolution (spans only); a tempo in the rule.
- **Test scope:** `eval/tests/test_roll_labels.py`, `lab/test_lab.py`.

### P2 — Lab parts and the first exam (Python, one session; trees only, CPU)

- **Entry state:** P1 committed; `rg "'tick'" tools/audio_analysis/lab/components.py` → 0 hits.
- **Read-back:** sections 4–5, 7, 8; `lab/components.py`, `lab/config.py`, `lab/models.py`, `lab/run.py` (`setup`, `tree_rows`), `lab/score.py`, `eval/detector_eval.py` whole, `eval/kick_goal_lowbank.py`, `eval/detector_cands.py`. Restate D4, D5, D7, D8, D9 and the forbidden moves.
- **Seam brief:** `detector_eval.judged(s, stamp, p, avail, th, sr, hop=HOP)` → `judged(..., hop=HOP, decision=fires)`; `score`, `song_result` the same; `held_out` reads `meta[t][5]` when present (re-run `rg -n "held_out\(|judged\(|song_result\(" tools/audio_analysis` and stop if the call-site list differs from: `detector_eval.py` internal, `lab/score.py:81`, `lab/models.py`, `eval/detector_compare.py`, `eval/tests/`); `models.song_meta(s, c)` → `song_meta(s, c, decision=fires)`. Compiler-driven is not available in Python: the gate's `rg` list is the checklist.
- **Deliverables:** `eval/roll_features.py` (front end, `roll24`, every constant named at the top); `components.FINDERS['tick']`, `AUDIO_FINDERS`, `MEASURES['roll24']`; `config.py` `decision` key with `key()` omission and `check` rules; `detector_eval.state_fires` and the decision argument; `lab/score.py` `roll` block with the provisional line, `COINCIDENT` per family; configs `kick_roll_v1.json`, `snare_roll_v1.json`, plus the ablations `kick_roll_short.json` / `snare_roll_short.json` (window 0.25 s only, width 12) and `kick_roll_noother.json` / `snare_roll_noother.json` (`other_ratio` zeroed); tests `test_decision_fires_keeps_run_ids`, `test_state_fires_edges`, `test_roll24_causal` (prefixing silence shifts columns by exactly the prefix), `test_tick_candidates`; runs over seeds 0–4 of the six configs and `compare.py` of each ablation against `_v1`.
- **Gate:** `lab/test_lab.py` green; the six reports exist and carry the provisional line; the oracle frontier printed; section 7.3 gates unchanged; a measured number to report: start F1 with its interval, false-roll seconds per minute on the real songs, rate accuracy and end lateness on the generated sections, per family.
- **Demo:** the reports, read by Peter — L2.
- **Forbidden:** editing `kick_goal_lowbank.py`, `kick_fusion_*`, `snare_cands.py`, `detector_cands.py`; a net; a stage; choosing `off_logit` or `hangover` on exam songs (D8's trigger uses the pair pool); float32 measures in the cache (today's dtypes); calling a provisional number a result.
- **Test scope:** `lab/test_lab.py`, `eval/tests/`. GPU: none (trees only).

### P3 — Release path (Python, one session)

- **Entry state:** P2 committed; P0's entry state; `rg "state" crates/manifold-audio/tests/fixtures/detectors/runtime_parts.json` → 0 hits.
- **Read-back:** section 6.1; `lab/release.py` (`train_provisional`, `final_record`, `export`, `reference`), `lab/contract.py`, `lab/export/*`. Restate D5, D13, section 6.1's refusals.
- **Deliverables:** `contract.py` `FAMILIES`, `TOP` (`decision`, `decision.off_logit`, `decision.hangover`), `PART_PREFIXES` (`tick.`, `roll.`), writer and `check` cases; `lab/export/finder_tick.py`, `lab/export/roll.py`; `release.reference` runs `state_fires` for state models and writes `state.on`, `state.rate_hz`, `starts`, `ends` into the goldens; `runtime_parts.json` entries; provisional releases `cover_kick_roll`, `cover_snare_roll` (`--provisional`) exported as golden sets; `release.py export` writes `decision` = `fires` for fires models from now on (the shipped kick and snare sets are *not* re-exported: a missing entry reads as `fires`, so their bytes stay); tests `test_contract_round_trip` and `test_contract_refuses_bad_entries` extended with the section 6.1 cases, `test_release_refuses_undeclared_part` for `roll24` before the parts file is edited.
- **Gate:** `lab/test_lab.py` green; `release.py parts` exits 0; both cover sets read back; section 7.3: `embedded_assets_are_the_shipped_sets` unchanged, assets diff empty.
- **Demo:** none — L1.
- **Forbidden:** re-exporting the kick or snare sets; a real (non-provisional) roll release before Peter's yes.

### P4 — Rust parts and parity (Rust, one session; lands on its own: new parts, no wiring)

- **Entry state:** P3 committed; the two cover sets on disk; `rg "Roll24|Tick" crates/manifold-audio/src/trained/spec.rs` → 0 hits.
- **Read-back:** sections 4, 6.1, 6.2, 10; `trained/spec.rs`, `trained/finder/mod.rs`, `trained/measure/{mod.rs, lowbank.rs, dsp.rs}`, `trained/detector.rs`, `trained/tests.rs`. Restate D8, D10, D13 and the tolerances below.
- **Seam brief (Rust):** `Spec` gains `decision` and `tick_hops`; `FireRule` → `Rule` enum (`Fires(FireRule)`, `State(StateRule)`), the detector's `rule` field typed `Rule`; `Decision<'a>` gains `state`; `Finder::Tick`; `Measure::Roll24`; `EventDetector for TrainedDetector` emits levels for state models. Compiler-driven: add the variants first, fix every non-exhaustive match.
- **Deliverables:** `trained/finder/tick.rs`, `trained/measure/roll.rs`, `StateRule`, the spec cases, `parity_cover_kick_roll`, `parity_cover_snare_roll`, `every_golden_set_runs` list extended (a count match), `refuses_each_bad_entry` cases, `registry_matches_runtime_parts` green, `detector_cost` printing the cover sets.
- **Gate:** `cargo nextest run -p manifold-audio trained`; `cargo clippy -p manifold-audio -- -D warnings`; tolerances: `m.roll24` abs 1e-6; `p.trees` as the existing rule; `state.on`, `starts`, `ends` exact except at most one tick per clip whose `p.final` is within 1e-3 of the cutoff or the off cutoff (the v1 rule, both edges); `state.rate_hz` exact except ticks where the two best lags' `r` are within 1e-6; cost per cover set ≤ 10 ms/s in release (D10's trigger).
- **Demo:** none — L1.
- **Forbidden:** widening a tolerance; a part not in `runtime_parts.json`; any edit to `detectors.rs` slots, `analysis.rs`, `manifold-core`, `manifold-app` (P5); `Box<dyn>` dispatch.
- **Test scope:** `-p manifold-audio trained`.

### P5 — App wiring (Rust, one session; built against the cover sets; does not land alone)

- **Entry state:** P4 landed; `rg "KickRoll" crates` → 0 hits.
- **Read-back:** section 6.2–6.3; `detectors.rs`, `analysis.rs:1800–1870, 2000–2015, 2120–2135`, `manifold-core/src/audio_features.rs`, `audio_mod.rs`, `manifold-ui/src/types.rs`, `param_slider_shared/state.rs`, `audio_trigger_section.rs`, `ui_translate.rs`, `queries.rs`, `modulation.rs:935–950`, `offline_audio_mod.rs`. Restate D3, D12, D13.
- **Seam brief:** `DetectorEvent { kind, sample }` → `{ kind, sample, value }` (re-run `rg -n "DetectorEvent \{" crates` and stop if the sites differ from `detectors.rs`, `audio_mod_runtime.rs:1148–1151`, `offline_audio_mod.rs`); `EventKind` two variants, `EMBEDDED` four slots (`kick_roll_model.mkick`, `snare_roll_model.mkick` from the cover sets until P6), `slot` exhaustive; `BandFeatures` two fields; `AudioFeatureKind` two kinds in core and the UI mirror; `LevelQueue`; the chips.
- **Deliverables:** the above; `analysis::tests::roll_level_holds_until_the_next_event_and_drops_at_end`; `detectors::tests::roll_detectors_leave_hit_fires_unchanged`; `every_event_kind_has_an_embedded_model` green with four; the chip test and the `ui_translate` round trip; a `scripts/ui-flows/trigger-roll-chips.json` flow that opens a clip trigger drawer, clicks "Snare Roll", and asserts the saved cell is `snareRoll` × Mid.
- **Gate:** `cargo nextest run -p manifold-audio`; `cargo nextest run -p manifold-core audio`; `cargo nextest run -p manifold-app audio_mod_runtime`; `cargo xtask ui-snap <scene> --script scripts/ui-flows/trigger-roll-chips.json` passes (L3); round trip: a project saved with a `kickRoll` modulation reloads and the cell modulates (the `audio_mod_runtime` test pattern at `:1104–1150`); content thread: no new per-frame work beyond one queue drain, so no render trace; section 7.3's fires gate green.
- **Performer gesture:** map Snare Roll × Mid to a generator's intensity with a 300 ms release; play a build: the slider ramps up through the roll and falls after the drop.
- **Demo:** L3 (the flow) plus a PNG of the drawer with seven chips, read by Peter.
- **Forbidden:** holding or gating the Kick/Snare fire queues from a roll event; decay on the level; a `Roll` kind without a fixed band; landing with the cover models embedded.

### P6 — Ship (data, one session per family; lands with P5)

- **Entry state:** P5 green on the branch; the exam real by D9's bar or Peter's ruling on open decision 3; Peter's yes on the P2/P3 numbers; run seeds saved for `kick_roll_v1` and `snare_roll_v1`.
- **Deliverables:** `release.py train kick_roll_v1 --seed S`, `export <id> --set kick_roll --ship`, the same for the snare; the goldens replace the cover sets; `release.json` scores quoted in the commit; the supersession sweep: DETECTOR_LAB_DESIGN.md and DETECTOR_RUNTIME_DESIGN.md Deferred rows "families beyond kick and snare" pointed here, AUDIO_EVAL_HARNESS_GUIDE.md section 0 (trained detectors) rolls bullet pointed here, memory `detector-label-truth`'s "a separate roll detector may come later" line updated, BUG-9ngk8.6 (roll detector for kick and snare rolls) closed, this doc's status line; `scripts/gen_docs_index.py`.
- **Gate:** `cargo nextest run -p manifold-audio`; cost in release (D10); `scripts/landing_gate.py --skip-gpu`; section 7.3 all five.
- **Demo:** L4, Peter, with the worktree binary path in the landing message: a build-up song on an analysed send; Kick Roll and Snare Roll chips rise through the build; Kick and Snare chips fire as before.
- **Forbidden:** any Rust edit (a parity failure is a P4 bug: escalate); a dirty-tree `train_id`; shipping with the exam below what Peter accepted.

Phase coverage: truth (P1), lab parts and exam (P2), release (P3), runtime (P4), cells, chips, offline parity and
persistence (P5), shipped models and docs (P6); the catalogue roll songs and the generated set are P0, owned by
labels work and Peter. Net, stage, `p_combined` measure, roll-aware hold, combined chip, other roll families:
Deferred.

## 13. Decided — do not reopen

1. Rolls are two lab families on the generic runtime; the kick and snare models are untouched (equalities, section 7.3).
2. The truth rule is `roll_spans`' rule: ≥ 4 notes, gaps < 200 ms; start = 4th note; end = last note + last gap; 2–3 notes are negatives.
3. The output is a rate-coded level (0.25–1.0 while on, 0 off), two band-locked kinds, never a hold on the hit triggers.
4. Tick finder, `roll24` on its own front end in its own module, trees only, `state` decision with fixed `off_logit` 1.0 and `hangover` 3 (sweep only on D8's trigger).
5. `decision` is omitted from `key()` when `fires`; the family builders and `kick_goal_lowbank.py` are never edited.
6. The exam is real songs at honest cutoffs; the headline is start F1 with the bootstrap, provisional until 60 starts in 8 songs; generated sections train only unless Peter rules otherwise (open decision 3).
7. Model constants ship in `roll.*` and `tick.*` entries; Rust refuses an incomplete file.
8. CPU ≤ 10 ms/s per roll detector; the ceiling stays 100 for the send.
9. Roll metrics are per song, never per hit.

## 14. Deferred

| What | Revive when |
|---|---|
| A net on a long slice (600 ms, `TBINS` 4) as a second model | a real exam's start F1 under 0.80, or false-roll seconds over 2 per minute on real songs |
| The family detector's `p_combined` as a `roll24` column (a song measure like `kick2`, run through `KickFed`) | the net is also short of the bar; needs a release pairing rule like `kick_train_id` |
| The song-history stage on a roll detector | cutoff transfer between songs is the measured failure (per-song oracle F1 far above the honest F1) |
| Roll-aware hold of the Kick / Snare trigger while its roll is on | Peter asks for it after playing with both cells (open decision 6) |
| One combined "Roll" chip (max of both cells) | Peter asks; a matrix-level max, no detector work |
| Hat and percussion rolls as families | the hats family (BUG-9ngk8.3, hats and percussion detector) has labels |
| Scoring snare doubles as hits in the snare detector (`dense_spans` to the 4-hit rule) | open decision 5, as its own snare A/B |
| Rate in note values | a tempo source the detector may read (never the grid) |
| Chosen `off_logit` and `hangover` | D8's trigger |
| Delay-echo negatives on real songs | an audio-derived label rule Peter accepts (today: unknown) |
| Rolls in the 23 audio-clip-drum compositions | BUG-hvaj2 (ADTOF on drum stems to fill unknown spans) lands; its labels are hits, so the rule applies to them unchanged |

## 15. Open decisions (Peter)

1. **Output value:** a rate-coded level (0.25 at eighths, rising a third per doubling, 1.0 at a buzz) while the roll is on, or a plain 1.0 on/off — recommend rate-coded; the build gesture is the point.
2. **Two cells or one:** Kick Roll and Snare Roll as separate band-locked cells and chips, or one Roll cell taking the max — recommend two; a max can be added at the matrix later without touching the detectors.
3. **Exam while real songs are roll-poor:** until the exam holds 60 starts in 8 songs, let the generated roll sections count toward the headline's recall (precision stays real-song) — recommend no; bring the nine catalogue compositions in instead (section 8.2), and read the generated recall as the provisional line it is.
4. **The generated roll set:** build it per section 8.3 before P3 — recommend yes; it is the only source of echo-train negatives, kick-and-snare-together builds and flat 32nds, none of which the 13 kick and 35 snare generated rolls carry.
5. **Snare doubles:** move the snare detector's `dense_spans` to the 4-hit rule so a double is a scored hit — recommend later, as its own snare A/B, because it changes the snare's labels sha and exam.
6. **Roll-aware hold** of the Kick / Snare trigger during a roll — recommend no in v1; map the two cells and decide with your hands.
7. **The 200 ms boundary:** keep eighth notes above 150 BPM as rolls (today's exclusion; 5 of the 117 catalogue kick rolls are eighths) rather than tightening to 150 ms — recommend keep; tightening moves the kick and snare labels too.
