# Audio Eval Harness — how to run it, read it, and grade against it

**Status: NORMATIVE working guide · 2026-07-06 · Fable.** The operating manual for
`mod_harness`, the offline grading loop every audio-analysis change (salience,
tracker, presence, transients) must pass through before touching the live path.
Design context: [AUDIO_OBJECT_TRACKING_DESIGN.md](AUDIO_OBJECT_TRACKING_DESIGN.md).
Written so a session with NO prior context can run, read, and judge results.

**2026-10-09 audit note:** the active pack now has 66 corrected visual kick
estimates (14/15/15/12/10); `attack_review.csv` preserves the one duplicate,
four clip-boundary events, and two ambiguous ending hits as provenance and
excluded-event registry. Primary scoring must exclude [0, 0.250] seconds for
`clip_boundary` and [estimated_attack_s - 0.100, EOF] for `needs_listening`,
reporting those categories separately. `bad_guy` has a mix reconstructed from
its unchanged stems and matching label timebases; all previous scores and
per-track calibration are stale. No auditory validation is claimed. See
`tests/fixtures/audio_labels/README.md` and BUG-qtd.

Reproduce the corrected five-full-mix Kick baseline from a worktree:

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo build -p manifold-audio --example mod_harness
python3 tools/audio_analysis/eval/live_kick_baseline.py \
  --harness target/debug/examples/mod_harness \
  --audio-root '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio' \
  --out-dir /tmp/live-kick-baseline \
  --report /tmp/live-kick-baseline.json
```

This uses the unchanged live `StreamingSendAnalyzer`, default settings, and
native-rate full mixes. It scores one-to-one matches at ±35/50/70 ms (50 ms
primary) and records uncertain/boundary-region triggers separately. Raw
availability is `(kick_hop + 1) * hop_samples / sample_rate`, not the old
zero-based plot time and not a latency-corrected timestamp. The separate
-35/+200 ms association diagnostic measures late nearby triggers and possible
duplicates without improving the tight accuracy score. It does not establish
which sound caused a trigger or measure capture/UI/display delay. Results:
`tools/audio_analysis/eval/scoreboard/live_kick_2026-10-09.json`.

**2026-10-09 attack trial — experimental, not enabled in the app:**
`kick_attack_probe` processes PCM causally through 45–140, 140–400 and
400–2000 Hz filters, 3/80 ms power followers, an attack/rearm check, a
35 ms low-band confirmation timeout and a 60 ms minimum firing interval. This is
the fifth mechanism variant tried on these same clips, with one fixed setting
across songs. These are development results, not held-out validation.

| Full mix | Live → trial matches within ±50 ms | Live → trial extra triggers, allowing -35/+200 ms association |
|---|---:|---:|
| Apricots | 0 → 14 / 14 | 5 → 0 |
| Bad Guy | 3 → 15 / 15 | 55 → 17 |
| Feel the Vibration | 2 → 13 / 15 | 27 → 2 |
| Inhale Exhale | 1 → 11 / 12 | 4 → 14 |
| Tears | 2 → 10 / 10 | 34 → 7 |
| Total | 8 → 63 / 66 | 125 → 40 |

At the strict ±50 ms tolerance, unmatched triggers fall from 179 to 42;
that includes late kicks, so it is not a pure false-positive count. The wider
association finds 65/66 labels versus 62/66 live; median availability delay
falls from 68.17 to 15.67 ms (p90 116.33 to 43 ms). The Rust event hops match
the SciPy reference exactly on all five files. The standalone synthetic probe
reports silence 0, four falling-pitch kicks 4, stationary bass pulses 3 and
slow bass swell 0 events. Bass-note confusion and the Inhale regression block
live replacement. Next work belongs to BUG-5to: discriminate non-kick attacks
without losing this timing improvement, then validate on fresh material.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo build -p manifold-audio --example kick_attack_probe
target/debug/examples/kick_attack_probe --selftest
python3 tools/audio_analysis/eval/live_kick_baseline.py \
  --detector attack-probe --harness target/debug/examples/kick_attack_probe \
  --audio-root '/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio' \
  --out-dir /tmp/kick-attack-trial --report /tmp/kick-attack-trial.json
```

Recorded evidence: `tools/audio_analysis/eval/scoreboard/kick_attack_trial_2026-10-09.json`.
The report includes working-source and binary hashes; its base revision alone
does not identify uncommitted prototype source.

**Non-kick rejection trial — rejected; original prototype unchanged.**
Offline stem subtraction identifies bass-dependent events in 13/14 scored
Inhale extras: removing bass eliminates a firing within ±30 ms of each original
event. Bad Guy has 16/17 drum-dependent extras and one dependent on bass/others.
These are detector counterfactuals, not proof of instrument identity; resampling
the stems is an offline diagnostic and is not part of the proposed live path.

The tested rule requires low/body power ratio to grow fourfold within 35 ms of
a prototype candidate. It emits at the confirming hop, with no backdating.
Wider-association extras fall from 40 to 9, including Inhale 14→2 and Bad Guy
17→2. However, associated Apricots labels fall from 14 to 7, and Feel's median
availability slips to 58.33 ms. Overall ±50 ms matches fall from 63 to 41/66.
A mandatory low-band descent therefore fails this development pack under the
current provisional visual labels. It must not become a live veto. No thresholds
were swept to repair these results.

Reproduce the diagnosis and fixed-rule comparison with
`tools/audio_analysis/eval/kick_attack_rejection.py --audio-root <audio>
--harness target/debug/examples/kick_attack_probe --out-dir /tmp/kick-rejection
--report /tmp/kick-rejection.json`. Recorded results are in
`tools/audio_analysis/eval/scoreboard/kick_rejection_trial_2026-10-09.json`.

Independent Astra checks also reject this gate: all seven Apricots misses fail
on drums alone at the same candidate times, and similar drum waveforms receive
opposite decisions. No evidence justified removing those labels. Two past-only
48 ms spectral-innovation gates were also rejected: low-band share retained
49/66 timely matches with 36 extras; tonal-concentration rejection retained
59/66 with 34 extras, losing four previously timely kicks. Both retain all
17 Bad Guy extras. Their retained timestamps are unchanged; this is evidence
loss, not an added-delay result. Scratch scripts, JSON and Apricots plots are
preserved locally under
`~/.cache/manifold/audio-rejection-research-2026-10-09/`.

The parallel non-kick code audit found that Snare/Hats/Bass chips map to
Transients × Mid/High/Low, not instrument classifiers. Next evaluations need
separate reviewed snare/hat labels (including coincident hits), and raw-versus-
shaped measurements of synthetic wobble/growl/note changes plus real passages.
Default modulation release is 120 ms; pitch holds during dropout. These need
musical-response checks, not just feature jitter measurements. A separate
static UI/runtime mismatch needs reproduction: Step/Random Kick/Transient
actions bypass Attack/Release while the drawer still offers those controls.

## 1. Running it

**Additional source pack, 2026-10-09:**
`tools/audio_analysis/eval/additional_stem_sources.json` records Peter's Dropbox
source paths, complete-file hashes, formats, supplied BPMs and alignment checks.
Late Night (108 BPM) and Midnight Patience (132 BPM) are development material;
Miracle (148 BPM) and Heavy On Mind (158 BPM), including alternate versions,
are reserved for validation. Only metadata/hash checks have run on the latter.
There are 29 named stems, two alternate mixes and four mastered references.
All stems are 48 kHz / 24-bit stereo; Miracle/Heavy masters are 44.1 kHz.

Stems propose candidate events; score the actual mastered mixes after reviewing
those events. Peter confirms stem sums and masters differ, so exact waveform
reconstruction is not required. Midnight's master has 6,546 leading silent
frames: map candidate stem times by **+0.136375 seconds**, supported by sampled
native-rate alignment with no detected drift. This is source alignment, not
detector-latency compensation. Late Night's alternate NO VOX reference aligns
with its master in three sampled passages; kick-envelope peaks shift with
mastering, so no automatic onset offset has been applied to its individual
stems. No originals were warped. The bounded development review below now
provides provisional labels for four master passages.


**Master passage comparison, 2026-10-09:** both unchanged Rust harnesses ran
chronologically over the complete Late Night and Midnight Patience masters.
Two 12-second passages per track were selected using stem activity, then
reviewed from master waveforms/spectrograms without detector results. The labels
in `tests/fixtures/audio_labels/master_passages_2026-10-09.json` are provisional:
initial grid-assisted point timings were revoked before scoring and replaced
with visual onset intervals (20–40 ms wide). They are not independent listening
truth or precise latency ground truth. Ambiguous fills and their possible
responses are excluded; 64 accepted kicks remain scored.

| Detector | Within 50 ms | Extra at 50 ms | Within 70 ms | Extra at 70 ms |
|---|---:|---:|---:|---:|
| Unchanged live | 37/64 | 77 | 64/64 | 50 |
| Fixed v5 prototype | 40/64 | 8 | 40/64 | 8 |

The prototype's 24 misses persist in the wider -35/+200 ms diagnostic. Its
bass-only passage fires are 5 versus live's 16 on Late Night, and 0 versus 12
on Midnight. These results reject v5 as a general replacement despite its
lower extra-fire count. Matching uses reviewed margins to avoid boundary
artifacts; no detector reset, timestamp correction, or per-song tuning occurs.

The causal Python reference exactly reproduces both complete native Rust v5
event lists. All 24 missed onsets have no eligible candidate in the reviewed
-20/+70 ms neighborhood; 13 pass the individual conditions at different hops
but never together. The fixed temporal, tonal and PCEN probes below now test
three proposed responses. None justifies integrating v5 into the live path.

Reproduce scoring with `PYTHONPATH=tools/audio_analysis python3 -m
 eval.master_kick_comparison --runs
 tools/audio_analysis/eval/scoreboard/master_kick_runs_2026-10-09.json --labels
 tests/fixtures/audio_labels/master_passages_2026-10-09.json --out /tmp/master-score.json`
(join the command onto one line). Raw runs include exact commands, input and
binary hashes, and build provenance. Results and miss diagnosis are the
`master_kick_comparison_2026-10-09.json` and
`master_kick_miss_diagnosis_2026-10-09.json` scoreboard files. Review plots,
scripts and complete logs are preserved in
`~/.cache/manifold/master-kick-comparison-2026-10-09/`.


**Three isolated DSP probes, 2026-10-09:** fixed first settings were tested
without per-track tuning, models, GPU work or live integration. Five original
mix excerpts plus four master passages provide 130 scored visual labels.
All seven reference Python event lists exactly reproduce native Rust v5.

| Detector | Matched within 50 ms | Missed | Extra fires |
|---|---:|---:|---:|
| Unchanged live (reused baseline) | 45 | 85 | 256 |
| Fixed v5 reference | 103 | 27 | 50 |
| 20 ms temporal eligibility | 108 | 22 | 130 |
| SuperFlux-style confirmation | 85 | 45 | 40 |
| PCEN-style candidate gate | 114 | 16 | 386 |

These implementations fail the proposed upgrade criterion. Temporal evidence
recovers some kicks but also joins ongoing bass fluctuations. The tonal mask
eliminates fires in both kick-free master passages yet rejects strong kicks.
PCEN applied to the existing 3 ms band-power envelopes fires 48 times after
startup on a stationary harmonic bass control. This exposes sensitivity to
within-cycle energy variation, not a conclusion that PCEN is generally poor.
The first versions remain separate; no combined detector was tuned.

Controls also run full kick-only, bass-only, kick+bass, kick+quieter-bass and
kick+other-drums stem combinations for both development tracks. Counts use
source-clock passages, not master-label accuracy. On Midnight, v5 fires 25
times in the kick-active kick-only passage, 19 with bass, and 24 with bass
reduced by 12 dB; those counts diagnose interference, not which hits were correct.
Viewed source/master detail panels corroborate selected recovered and rejected
reference events. Labels were not changed to improve scores.

Long-master Python process CPU time was 0.15–0.18% of audio duration for v5,
temporal and PCEN, and 0.46–0.47% for tonal. This is batch CPU throughput,
not native callback or end-to-end latency proof. No GPU was used. Twelve
focused tests pass, including causality, evidence expiry, silent inputs,
filterbank coverage and FFT batch invariance. No Rust source changed.

Run each variant (`baseline`, `temporal`, `tonal`, `pcen`) using
`PYTHONPATH=tools/audio_analysis python3 -m eval.run_kick_dsp_experiments`
with `--variant`, `--cache`, and `--audio-root`. Run source/synthetic controls
with `python3 -m eval.kick_dsp_controls --out PATH` under the same PYTHONPATH.
The reproducible summary, frozen parameters, per-passage errors, controls and
source hashes are in `scoreboard/kick_dsp_experiments_2026-10-09.json` relative
to `tools/audio_analysis/eval/`. Raw reports and feature caches are under
`~/.cache/manifold/kick-dsp-experiments-2026-10-09/`.
The follow-up below tests steadier energy representations. Miracle and Heavy
On Mind remain untouched by detector evaluation.

**Hybrid DSP follow-up, 2026-10-09 — no successful replacement:** causal
frequency-dependent RMS and quadrature power were combined with fast raw
attack evidence. Initial PCEN controls lost rapid kicks. An attack-led rule
then retained the original low-energy rise requirement; a final diagnostic
removed that requirement while retaining energy presence. These were sequential
development experiments, with unchanged labels and no per-song settings.

| Rule | Matched within 50 ms | Missed | Extra fires |
|---|---:|---:|---:|
| Fixed v5 reference | 103 | 27 | 50 |
| RMS with low rise | 94 | 36 | 80 |
| Quadrature with low rise | 107 | 23 | 162 |
| RMS with low presence | 111 | 19 | 192 |
| Quadrature with low presence | 113 | 17 | 190 |

Removing the rise requirement recovered all eight rapid synthetic kicks at
48 kHz, but both presence variants fired 49 times in Late Night's reviewed
kick-free bass passage. Steadier energy alone does not distinguish kicks from
bass renewal. Quadrature presence also misses three of four isolated synthetic
kicks; this implementation has unresolved filter/confirmation timing behaviour.
Strict extras include late associated kicks; the kick-free passages establish
actual unwanted firing. No live integration is justified by these results.

Sixteen focused tests pass, covering causal prefixes, envelope calibration,
ripple, silence and detector decisions. All 110 synthetic event lists replay
after adding CLI rule selection; each mix run exactly reproduces all seven
native v5 references. Python batch CPU/audio ratios were 0.26–0.30% for RMS
and 0.38–0.42% for quadrature, not native callback deadline measurements.

Reproduce using `PYTHONPATH=tools/audio_analysis python3 -m
eval.kick_hybrid_experiment --rule presence --phase controls --out PATH`.
Rules are `pcen`, `rise`, and `presence`; `--phase mixes` additionally requires
`--audio-root PATH`. PCEN was evaluated on controls only. Results, per-passage
counts, limitations and source hashes are in
`tools/audio_analysis/eval/scoreboard/kick_hybrid_2026-10-09.json`; raw reports
remain under `~/.cache/manifold/kick-hybrid-2026-10-09/`.

**Failure audit, 2026-10-09:** `eval.kick_failure_casebook` compares 24 selected
cases across six recordings without changing the detector. It separates late
hits, masked/rolling kicks, persistent-bass ghost fires and abrupt non-kick
attacks. Six simple scalar measurements overlap across the full set. Spectral
continuity distinguishes the four sampled kick-free master fires at the 50 ms
window size, but not all abrupt non-kick attacks; timing uncertainty weakens the
shorter-window distinction. This is descriptive evidence, not a fitted rule or
validation result. Positive windows end after provisional labels, negative
windows at recorded fires, so acoustic onsets are not assumed aligned. See
`tools/audio_analysis/eval/scoreboard/kick_failure_casebook_2026-10-09.json` for
case identities, evidence, endpoint sensitivity and reproduction instructions.

Generalisation is required: case studies identify mechanisms, not song-specific
thresholds. The seven previously evaluated recordings are development material.
Use one shared configuration and report every track's misses, extras and timing;
pooled gains must not conceal regressions. Freeze the candidate and evaluation
criteria before scoring untouched Miracle and Heavy On Mind. Split by whole
recording, not random windows of the same song. If held-out results influence
another revision, those recordings become development data and fresh material
is needed for an independent generalisation claim.

**Complex-domain audit, 2026-10-09 — measurement only:** a fixed causal
45–2000 Hz predictor uses previous magnitude and extrapolated phase, with a
2048-at-48k trailing Hann window. Its normalized error does not provide a safe
universal low-score veto: retaining all existing full-history tight hits leaves
all 48 wider-unmatched baseline events and all 60 secondary hybrid kick-free
events. The latter overlap baseline events and must not be added to that count.
Midnight's ghost separation does not transfer to Late Night; Bad Guy's unwanted
attacks often have stronger novelty than its kicks. Evidence near missed kicks
uses label-guided windows and is not recovered detector recall. Nine focused
tests pass and all seven native v5 event lists replay exactly. No trigger rule
or app code changed. See
`tools/audio_analysis/eval/scoreboard/kick_complex_audit_2026-10-09.json` for
per-track results, timing sensitivity, limitations and reproduction instructions.

**Timbre-template trial, 2026-10-09 — rejected:** fixed 80-element fingerprints
combine four trailing spectra in 20 frequency bands. Cosine matching uses
reviewed mix/stem references, excluding the evaluated song and all its stems
as one group. A fixed zero-margin veto reduces 50 ms matches from 103 to 81
and extras from 50 to 42. No threshold was tuned. Label-guided timing probes
and secondary kick-free tests expose strong timing/context dependence; they
are not recovered trigger recall. Nine focused tests pass, and all 868
original-fire classifications replay exactly after adding those diagnostics.
No live change or held-out evaluation was made. See
`tools/audio_analysis/eval/scoreboard/kick_timbre_trial_2026-10-09.json` for
per-track results, reference provenance, limitations and reproduction commands.

**Sustained-bass follow-up, 2026-10-09 — development candidate:** Peter's listening review
identified Late Night false kicks during sustained bass. At all five reviewed
bass-passage fires, the body attack ratio exceeds 2 for one isolated hop;
cycle averaging reduces combined fast/slow rise from 1.81–2.21 to 1.01–1.27.
Requiring the existing body attack on two consecutive completed hops removes
all five fires, but also loses two Late Night kicks and delays a Feel the
Vibration kick to 51 ms. Overall 50 ms matches/extras change from 103/50 to
100/40. A recovery variant preserves the original v5 decision and validates its
output using two-hop attack evidence within the preceding or following 35 ms.
It recovers both lost Late Night kicks while still removing all five bass-passage
fires. All 105 original wider-associated labels remain associated; unmatched fires
fall from 48 to 40. Strict 50 ms scores are 102 matched /28 missed /43 extra:
one Feel hit moves from 45.667 to 51 ms. Actual validation time is used, never
backdated. Added delay reaches 26.667 ms on reviewed kicks. Eight focused tests,
seven native baseline replays and exact exploratory-report comparisons pass.
This remains offline: unequal waits shorten some output gaps below 60 ms outside
reviewed passages, and held-out evaluation/native latency are unverified. See
`tools/audio_analysis/eval/scoreboard/kick_sustain_audit_2026-10-09.json`.

**Evidence retention and bass continuity, 2026-10-09 — neither adopted:**
Keeping balance/growth evidence inside one body-attack episode recovers the
Inhale Exhale 12.620 s label, but wider unmatched fires increase from 40 to 52.
A fixed past-trained AR(8) predictor on 45–400 Hz audio rejects 81 of the
105 associated kicks: predictable waveform continuation does not identify bass.
Strict 50 ms scores are 105/25/53 for episode retention and 24/106/8 for the
predictor, versus the improved reference's 102/28/43 (matched/missed/extra).
Eight focused tests and both sets of seven reference replays pass. The prior
sustained-bass improvement remains frozen; no held-out or live change was made.
See `tools/audio_analysis/eval/scoreboard/kick_continuity_trials_2026-10-09.json`.

**Frozen held-out evaluation, 2026-10-09 — failed generalisation:** detector-blind
master/stem visual review labelled 44 kicks in two 12 s cores, plus two 12 s
bass-only controls. Cores were selected by isolated-stem RMS before detection;
labels and implementation hashes were frozen before running full native-rate
masters. Miracle remains 9/12 caught, with extras reduced 4→3. Heavy On Mind
falls from 4/32 to 3/32 caught, with no extras. Both bass-only controls remain
at zero fires. These counts agree at 35/50/70 ms and wider association.
Persistence rejects the real 209.240 s Heavy On Mind kick; it therefore fails
the requirement to retain every original associated kick. In a read-only
[-15,+70] ms probe, all 31 original misses lack simultaneous eligibility;
30 nevertheless pass each individual condition at some point. Brief body-rise
evidence is not specific to false bass triggers. No threshold was changed.
Twelve focused scorer/persistence tests pass, and both baseline sequences replay
exactly. This is limited provisional visual truth, not audited listening or app
latency evidence. See `tools/audio_analysis/eval/scoreboard/kick_heldout_2026-10-09.json`
and `tests/fixtures/audio_labels/heldout_passages_2026-10-09.json`. These tracks
must be treated as development material if this diagnosis informs later changes.

**Background-relative balance, 2026-10-09 — rejected:** at the missed Heavy On
Mind 200.130 s kick, body rise and combined rise pass, but body/low balance is
0.221, below the fixed 1/3 threshold. At the caught 201.265 s kick it is 0.427.
One trial replaces only that balance with positive `fast - slow` power in each
band, retaining all thresholds, timing and other v5 rules. It recovers the
examined kick and raises Heavy On Mind from 4/32 to 6/32. Across nine recordings,
strict 50 ms matches/misses/extras change from 116/58/54 to 119/55/60; wider
association changes from 118/56/52 to 122/52/57 without losing associated labels.
Late Night bass-only extras rise 5→6 and Heavy On Mind bass-only extras 0→1.
Thus background-relative balance exposes a real bottleneck but fails the
false-fire requirement. No persistence veto was added. Four focused tests pass;
all nine frozen v5 sequences replay exactly and synthetic controls are unchanged.
Miracle and Heavy On Mind are now development material, not independent holdouts.
See `tools/audio_analysis/eval/scoreboard/kick_excess_balance_2026-10-09.json`.

**Two-component spectral mixture, 2026-10-09 — rejected:** exact nonnegative
least-squares fits a fixed kick spectrum plus a prior-only 80 ms background
spectrum. Additional explained power drives a fixed 3/80 ms onset follower;
there is no v5 candidate gate. Each foreign reference averages other songs'
isolated-stem templates, excluding the evaluated song. Heavy On Mind reaches
32/32 within 50 ms, but has 4 extra fires in its kick core and 113 in its
12 s bass-only core. Across nine recordings, strict matches/misses/extras are
160/14/794 versus v5's 116/58/54. The privileged same-song diagnostic is also
poor: 19/32 within 50 ms, 31 extras in the kick core and 126 in the bass core.
High recall at these firing rates does not establish discrimination. Cold-start
controls leave silence quiet but double-fire on isolated kicks and repeatedly
fire on stationary bass even after the initial 250 ms. Kick references are
precomputed, not learned from incoming music; only the background adapts.
Six numerical/causality tests pass. Offline CPU processing takes 0.16–0.18% of
audio duration; native callback latency remains untested. No tuning or live
change. See `tools/audio_analysis/eval/scoreboard/kick_mixture_2026-10-09.json`.

**Upper/low cue diagnostic, 2026-10-09 — mixed evidence, not adopted:** native
causal envelopes measure 1–2, 2–4 and 4–8 kHz attacks, a fresh 45–140 Hz rise
within -10/+35 ms, and upper half-power decay at a 50 ms deadline. Fixed
label-centred ±50 ms queries find the complete cue near 14/28 previously missed
Heavy On Mind kicks, but also 15 cue edges in its 12 s kick-free core. None of
the five known Late Night bass false fires has the cue nearby, yet nine other
cue edges occur in that same core. Across all recordings, coverage is 91/174
kick labels and 17/52 original unmatched fires; these are diagnostic coverage
counts, not detector recall/precision. Synthetic hats over stationary bass and
hats with new bass notes pass 4/4 at both sample rates; hats alone fail linkage.
Thus cross-band evidence may contribute to a future confidence score but cannot
identify a kick by itself. No fusion weights were fitted. Seven timing/causality
tests pass; no live change. See
`tools/audio_analysis/eval/scoreboard/kick_upper_cue_2026-10-09.json`.

**DSP feature fusion, 2026-10-09 — fixed-cutoff trial not adopted:** nine causal
features feed a regularised linear logistic score, with each evaluated song
excluded from fitting and normalisation. The 40 ms evidence horizon rounds up
to a hop boundary; actual availability is scored. At ±50 ms, matches rise 116→139/174
but unmatched triggers rise 54→260. Heavy On Mind rises 4→25/32 (31 at ±70 ms).
Across four reviewed kick-free cores, extras rise 5→34. All original widely
associated labels are retained. This first trial uses equal class weighting and
a fixed 0.5 cutoff, not calibrated probabilities; threshold calibration and
additional bandwise features remain untested. Ten focused tests pass. All nine
songs are development material; no untouched validation or live change. See
`tools/audio_analysis/eval/scoreboard/kick_fusion_2026-10-09.json`.

**Nested calibration and bandwise fusion, 2026-10-09 — neither replaces the
prototype:** thresholds are selected using inner song exclusions, with the outer
song absent from every fit and threshold choice. At ±70 ms, calibrated nine-feature
fusion gives 86/174 matches and 44 unmatched triggers; adding six bandwise
flux/centroid features improves this to 98/174 and 41. The prototype gives 118/174
and 52. At ±50 ms the same comparisons are 65/65, 66/73 and 116/54 (matches/extras),
so the bandwise benefit depends on the accepted timing tolerance. Both calibrated
variants have zero extras across four reviewed kick-free cores, but lose many
real kicks. Candidates and evidence horizon are unchanged. Twelve new tests pass;
the nine-feature fixed-0.5 replay exactly matches all prior event sequences.
The initial reporting-schema failure was repaired and regression-tested before
repeating the unchanged experiment. These remain exploratory development results;
no live change. See
`tools/audio_analysis/eval/scoreboard/kick_fusion_parallel_2026-10-09.json`.

**Threshold refinement and error audit, 2026-10-09:** adding 51 inner-only
cutoffs inside each selected coarse interval changes the bandwise ±70 ms result
from 98 matches/41 extras to 102/45; ±50 ms changes 66/73 to 68/79. All four
kick-free cores remain clean. This is a small tradeoff, not a prototype replacement.
Stored-model replay identifies 16 drum-dependent Bad Guy extras ranked above most
matched kicks. Existing measurements show broader attacks, higher body/low balance,
less low buildup and slower upper decay, but the learned score rewards their
centroid rise and attack strength. This is descriptive evidence on one song,
not cross-song separability or identified drum species. Midnight extras comprise
seven bass-dominated events and four kick-stem-dominated events (two duplicate
decisions and two following excluded labels). Counts and labels remain frozen.
Twelve new tests pass; the audit's nearest-label context was repaired and tested
without repeating DSP. See
`tools/audio_analysis/eval/scoreboard/kick_fusion_refinement_audit_2026-10-09.json`.

**Temporal trajectories and class subspaces, 2026-10-09 — no upgrade:** one
shared immutable cache retains the frozen 15 features plus eight ordered power
observations in each of low/body/upper bands, relative to pre-candidate background.
The candidate grid and evidence deadline are unchanged. Linear scoring on these
39 values gives 94/174 matches and 41 extras at ±70 ms; separate rank-2 weighted
SVD class subspaces give 82/174 and 67. Both use the same nested calibration and
one threshold refinement. Sixteen focused tests pass; stored base output replays
exactly. SVD finds 20 labels missed by linear scoring, while linear finds 32 missed
by SVD, but their independently matched sets cover only 114 labels. This does
not establish a successful ensemble. See
`tools/audio_analysis/eval/scoreboard/kick_trajectory_2026-10-09.json`.

The local corpus inventory now locks Waypoints and Know You're There, including
all versions/stems, as final-validation reserves. Their audio content has not
been analysed for this work. Broader development starts with additional sections
of the four isolated-kick/master pairs, then Pattern, All In For You and Integer.
Existing liveshow labels are visual clip placements with detector-affinity class
tags, not independently observed acoustic onsets: Pattern duplicates 30 kick
timestamps across two layers, and All In For You has no kick-tagged visual events.
Do not import those labels as acoustic ground truth or apply their show timebase
to different masters. Inventory and source evidence:
`tools/audio_analysis/eval/local_corpus_inventory_2026-10-09.json`.

**Failure diagnosis and target correction, 2026-10-09:** all 174 labels have
temporally available raw candidates; this does not establish their acoustic
cause. Frozen-model misses primarily come from score rejection, with one
uncertainty-boundary artifact. Scores shift between songs, and hard false
triggers can outrank kicks within a song. Rank-2 reconstruction also loses
discrimination present in the same features. Three candidates associated with
excluded Midnight labels incorrectly remained positive training targets; they
are now ignored, not reassigned negative. Cache-only replay gives 103/174 with
45 extras for linear15, 91/174 with 40 for linear39, and 86/174 with 66 for SVD
at 70 ms. All four reviewed kick-free cores remain clear. Eight fusion tests
and two runner integration tests pass. Old results and labels remain recorded;
the repair and diagnostic provenance are appended to the trajectory scoreboard.
Twenty additional fixed 12-second cores are prepared, but their 370 source
proposals contain decay-tail crossings and weak noise excursions. They are
review material, not additional accepted labels or expanded validation.

**Expanded review and compact shape test, 2026-10-09 — stop this experiment series:**
raw master/source review now preserves all 20 fixed cores, with two Late Night
cores unscorable and explicit bounded ambiguities elsewhere. The new provisional
manifest adds 207 scored kicks to the unchanged 174, including weak attacks and
rolls missed by the source proposer. Five additional complete kick-free cores
increase reviewed kick-free coverage to 1.8 minutes. These remain the same nine
development recordings, not untouched validation tracks.

One fixed representation retains the original 15 values and adds nine normalised
temporal moments, cross-band time differences and shape concordances. Candidates,
evidence deadlines, linear fitting and nested song exclusions stay fixed. On the
expanded pack, shape24 gives 209/381 matches plus 85 extras at 70 ms; the existing
linear15 coarse comparison gives 211 plus 84, and its refined comparison gives
223 plus 89. Shape also regresses on the original pack: 115/174 plus 74 versus
v5's 118 plus 52. All nine complete kick-free cores remain clear. Of shape's 172
misses, 166 are score rejections; only two lack a timely raw candidate. Added
relationships do not resolve source confusion or transfer between songs.

Eleven focused tests pass, cached output replays exactly, and no audio features
were re-extracted. The third materially different hypothesis has not improved
the best recall/error tradeoff; the agreed research stop condition is reached.
The 95% targets are not met, no candidate is promoted, and reserved audio remains
untouched. Labels and per-track results:
`tests/fixtures/audio_labels/expanded_passages_2026-10-09.json` and
`tools/audio_analysis/eval/scoreboard/kick_shape_2026-10-09.json`.

**Bounded hard-negative weighting, 2026-10-09 — retain the unweighted scorer:**
source-assisted review accepted 114 of 126 potential hard negatives; eight kick
tails and four ambiguous candidates were excluded from emphasis. The fixed rule
selects reviewed negatives scoring at least 0.90 under each training fold's own
unweighted model. Exactly 1×, 2× and 4× were compared, preserving song/class mass,
positive-event weights, baseline fold normalisation and the calibration protocol.
At 70 ms they give respectively 223/381 plus 89 extras, 226 plus 93, and 222 plus
86. Neither meets the 223-hit/70-extra milestone. All nine kick-free cores stay
clear; the original 174-label pack gives 121+76, 123+77 and 121+69.

Stronger weighting reduces mean positive/negative score margins on every held-out
song and worsens candidate ranking on six of nine. Lower calibrated cutoffs
recover some hits, while 4× loses three Apricots, two Inhale and four Heavy matches
net. This does not establish a missing DSP measurement or a physical limit; stop
this weighting route. Eight focused tests, all 81 saved event replays and 243
nested-model checks pass. No live changes or reserved material were used. Review
truth remains provisional; Inhale's pitched drum near 9.735 s is still ambiguous.
Reproduction and per-track precision, recall, timing, duplicates and coefficients:
`tools/audio_analysis/eval/run_kick_hard_negative_trial.py`,
`tests/fixtures/audio_labels/hard_negative_review_2026-10-09.json`, and
`tools/audio_analysis/eval/scoreboard/kick_hard_negative_2026-10-09.json`.

**Bounded nonlinear research, evening 2026-10-09 — no candidate promoted:**
the unchanged 15-feature linear reference remains **223/381 matches + 89 extras**
at ±70 ms. The first six hypotheses, three configurations each, used all nine
development recordings with complete-song exclusion in fitting, normalisation
and nested threshold selection. Existing 174-label and added 207-label truth
were unchanged. All labels remain provisional. No neural network, external
dataset, GPU inference, live integration or landing was involved.

| Hypothesis and declared configurations | Matches + extras at ±70 ms | Finding |
|---|---|---|
| H1: 64 boosted trees, depths 1 / 2 / 3 | 218+90 / 230+99 / **274+85** | Conditional boundaries help; additive stumps do not. Depth 3 loses previously caught events and fires in three kick-free cores. |
| H2: class covariance, diagonal shrinkage .1 / .5 / .9 | 133+88 / 101+82 / 83+84 | Better within-song candidate ordering at stronger shrinkage does not produce transferable cutoffs. |
| H3: fixed tree-logit blend .25 / .5 / .75 | 239+89 / 263+91 / 276+93 | Restores some original-track recall, but also retains false fires and swaps which real kicks are caught. |
| H4: paired-stem augmentation mass .1 / .25 / .5 | 268+89 / 269+91 / 262+94 | Sixteen controlled contexts do not improve the unaugmented tree on actual masters. |
| H5: timely event-window supervision, top 1 / top 2 / soft | 204+92 / 234+91 / 210+91 | Selecting stronger observations during training does not resolve source confusion. |
| H6: zero inner kick-free fires, linear / tree / .75 blend | 222+88 / 204+46 / **244+62** | Enforcing the actual negative-core target reduces errors but loses real kicks; one held-out Miracle bass transition still fires. |

None passes the complete intermediate target: the aggregate count tradeoff,
zero fires in all nine reviewed kick-free cores, and at most one *previously
matched event* lost per song. Net counts can conceal different missed events.
H2 initially reported net retention; that reporting error was corrected and
its comparisons and dependent H5 replayed with identical models, cutoffs, scores
and emissions. Original reports and the correction receipt are preserved.

Depth 3 is the best balanced research candidate, at 71.9% recall and 76.3%
precision. It recovers 64 baseline misses while losing 13 baseline hits: 2
Apricots, 4 Inhale, 2 Tears, 3 Late Night, 1 Midnight and 1 Miracle. The three
kick-free fires occur at Midnight 152.032 s, Miracle 220.57494 s and Heavy
26.47347 s. A focused native master/kick/bass review corroborates bass continuation
or transitions at all three; it demonstrates no missing kick or offset error.
Silent named kick stems alone do not prove the differing masters contain no weak
coincident kick. Raising the threshold is not a demonstrated general solution.

| Song | Labels | Linear matches / extras | Depth-3 matches / extras |
|---|---:|---:|---:|
| Apricots | 14 | 9 / 0 | 7 / 0 |
| Bad Guy | 15 | 15 / 45 | 15 / 34 |
| Feel the Vibration | 15 | 15 / 0 | 15 / 1 |
| Inhale Exhale | 12 | 11 / 6 | 7 / 3 |
| Tears | 10 | 10 / 11 | 8 / 4 |
| Late Night | 77 | 53 / 0 | 64 / 10 |
| Midnight Patience | 88 | 44 / 20 | 48 / 21 |
| Miracle | 32 | 21 / 0 | 27 / 3 |
| Heavy on Mind | 118 | 45 / 7 | 83 / 9 |

Heavy accounts for 38 of the 51 net recovered kicks. Equal-song mean recall is
75.4% for linear and 75.6% for depth 3; pooled recall alone overstates how broadly
the improvement transfers. On the original 174 labels, linear gives **121+76**
and depth 3 **135+55**; on the added 207, they give **102+13** and **139+30**.
The zero-core blend gives 129+49 and 115+13 respectively, with better pooled
precision but failed retention and negative-core safeguards.

A second timing audit moves all 315 bracketed master labels to their recorded
starts or ends, leaving the 66 original-five labels unchanged. Starts give
linear/tree **199+113 / 237+122**; ends give **225+87 / 276+83**. Both cases have
384 effective labels because three net Midnight labels enter the unchanged
uncertainty exclusions' scoreable domain. The official 381 midpoint labels stay
unchanged. The recall advantage survives these two coordinated checks; the small
reduction in extras does not. Neither check bounds every independent timing
choice, and neither repairs the three negative-core fires or original-track losses.

Eight additional 12-second master cores were frozen before source review or
detector inspection. Two Late Night cores remain wholly unscorable: source
absence does not establish that a master-only attack is bass rather than kick.
The other six supply 73 provisional visual labels and three opening negative
cores. All four comparison models and thresholds were frozen before scoring:

| Additional development passages | Linear | Depth 3 | .75 blend | Zero-core blend |
|---|---:|---:|---:|---:|
| Matches / 73 | 42 | 48 | 48 | 43 |
| Extras | 18 | 9 | 13 | 6 |
| Fires in the three opening negative cores | 6 | 2 | 5 | 1 |

Depth 3 improves each newly reviewed family: Heavy 11→14/20, Midnight
23→24/43, Miracle 8→10/10, while reducing extras. This is additional development
evidence, not reserved-song confirmation. Moving every onset to its review
bracket's start changes linear/tree to 30+30 / 33+24; using every bracket end
gives 43+17 / 49+8. Neither changes the accepted midpoint labels. These two
coordinated endpoint checks expose timing sensitivity; they are not exhaustive
bounds over independent annotation choices. The independent audit verifies all
matches, exclusions, source identities and family exclusions. Startup emissions
are counted at sample zero; only explicit ambiguous audio excludes a response.

**Mechanisms and transferable lessons.** Controlled mixtures hold the kick
waveform fixed, vary accompaniment gain 0/1/2, and use one common headroom scalar
for paired kick-present/removed signals. Timely candidates remain in all 16
contexts, but detections emitted after the source anchor and within ±70 ms of
the reviewed master label fall **16→12→5**. Accompaniment
raises the slow body-band energy more than the fast response, suppressing the
relative-rise evidence. Under an approximate additive-power model, a sustained
background B changes F/S to (F+B)/(S+B), pushing the ratio toward one despite an
unchanged transient. Actual waveforms also contain phase-dependent cross terms;
the paired signal comparison, not this approximation, establishes the effect.
The mixtures diagnose masking; they do not reproduce the finished masters.
Their fixed-anchor measurements also coincide with saved nearby natural-candidate
measurements in only 15/16, 9/16 and 7/16 contexts as accompaniment rises. That
sampling mismatch limits what the failed H4 augmentation says about augmentation
in general; H7 below tests the corrected sampling scheme.

Exact tree paths show useful conjunctions of body rise, low-band growth and
low-band rise. Heavy's recovered events have a median +1.018 logit improvement
despite a +0.200-logit *harder* cutoff, so its gain is not simply a relaxed
threshold. The same positive conjunction also admits a reviewed bass-only fire.
Conversely, the same negative partition removes a Tears extra and loses a real
Tears kick. All 13 tree losses have timely candidates but no passing tree score;
63 of 64 recoveries have no timely linear score crossing. This isolates score
discrimination from proposer or refractory failure. Branch contributions explain
decisions, not physical source identity or independent causal feature effects.

Cold-start, sustained/wobbling bass, hats, renewed bass attacks, rapid kicks and
changing-material controls expose further limits. The tree rejects all four
isolated synthetic kicks across all nine frozen models at both 44.1/48 kHz,
despite timely candidates; linear catches all four. This control is a clickless
220→55 Hz sweep over 220 ms, still about 167 Hz at 44 ms and reaching 140 Hz
around 72 ms. Its body/low balance is above the 99.89th weighted percentile of
positive training windows. It exposes a rare-shape learned rejection, not a
physical DSP limit. Wobbling bass can satisfy positive rise/growth branches
without a descending centroid. Synthetic scores never selected a model or cutoff.

For future snare, hat, bass and synth detectors, retain the methodology:
separate candidate availability, acoustic score, calibration and emitted-event
timing; verify conditional cues with source-addition/removal pairs; distinguish
attack from ongoing energy; split whole source families; and retain explicit
unknown labels. These kick bands and learned constants do not transfer as
validated settings. More features or a larger classifier do not automatically
repair shared-source ambiguity, scarce shape support or score shifts between
songs. Another threshold sweep or repetition of the failed weighting/SVD routes
has no demonstrated justification from this run.

**Timing and CPU.** A bounded streaming reference replays all nine complete
recordings (1,082.64 s, 43,585 candidates). It produces identical candidate hops
and emitted linear/tree events; maximum feature error is 3.38e-14. Observation
is 42.630–42.667 ms, with no backdating. Associated emitted-event median/p90 is
45.73/56.33 ms for linear and 44.50/56.85 ms for depth 3. Depth 3 has two broad
associations later than 70 ms, the latest 183.33 ms; these are not timely matches
and may represent another sound. At ±35/50 ms it gives 61/191 matches, versus
36/155 for linear. Possible duplicate counts are 4 versus 3 (zero-core blend 2).
The broad association rule is −35/+200 ms, not a superset of the ±70 ms rule;
all matching remains one-to-one.

Feature extraction plus tree decisions costs **3.3–4.1% of one CPU core** in the
Python reference on this Mac. The retained feature arrays occupy 43.5–44.1 kB
and tree arrays 36.8–38.6 kB, excluding object overhead; each candidate needs at
most 192 tree comparisons. Python/SciPy still allocate bounded per-hop temporary
arrays. Wall-hop p99 reaches about 1.25 ms, but observed outliers reach 74.70 ms in
the verification loop, which includes both scorers and parity checks. This is a
throughput/bounded-state proof, **not** a hard audio-callback deadline guarantee
or a measurement of device/display latency. No native integration was attempted.

Forty-one focused tests cover the six methods, mixture construction, streaming
and boundary scoring. Independent audits cover Gaussian algebra, event weights,
nested exclusions, JSON/scalar-tree parity, float32 split boundaries, complete
streaming replay and additional-passage matching. Current results, hypotheses,
per-track timing, precision, recall, safeguards and provenance are in
`tools/audio_analysis/eval/scoreboard/kick_evening_2026-10-09.json`; the nine frozen
linear/tree models and comparison cutoffs are preserved in
`tools/audio_analysis/eval/scoreboard/kick_evening_models_2026-10-09.json`.
They are leave-one-song-out research models, not a globally fitted shipping model.
Repeated method selection on these development families can make their reported
performance optimistic; the additional passages do not remove that limitation.
New labels are in `tests/fixtures/audio_labels/evening_validation_passages_2026-10-09.json`.
Waypoints and Know You're There remain untouched: no candidate warranted consuming
the final reserve. The scoreboard records the eventual stop condition and usage.

Local detailed reports, predeclared rules, correction receipts and listening
pairs are under `~/.cache/manifold/kick-research-2026-10-09-evening/`.
`listening/manifest.json` indexes nine 8-second A/B pairs: identical original
stereo mix at 75%, with a short high click at each actual emission. A is linear15;
B is depth 3. Clicks indicate detector output, not ground truth. The fixed set
includes regressions as well as gains; no human listening verdict is claimed.
`preserve_results.py` reproduces the compact artifacts from the retained reports.
Run the matching `run_kick_{boosted,covariance,anchored,stem_augmented,event_window,zero_core}_trial`
module for a deliberate repeat, supplying its cached reports/rule paths via
`--help`. Set `OPENBLAS_NUM_THREADS=1 VECLIB_MAXIMUM_THREADS=1 OMP_NUM_THREADS=1`.
Never substitute the stale slot copy of Bad Guy for the main fixture audio root.

**Continuation under the revised usage budget.** The user removed the time and
hypothesis-count ceilings and lowered the allowance reserve to 5%. The original
three-configuration limit, local-only data, whole-family exclusions, causal
inference, unchanged 381-label comparison and strict retention safeguard remain.
Failure of these methods does not establish a physical limit.

| Hypothesis and fixed configurations | Matches + extras on 381 | Finding |
|---|---|---|
| H7: natural-candidate stem augmentation, mass .1 / .25 / .5 | 268+90 / 272+90 / 272+96 | Correct sampling improves controlled mixture decisions but does not transfer enough to actual masters. |
| H8: tree39 / base15 + background6 / tree39 + background6 | 269+91 / 253+86 / 273+93 | Prior variability and excess attack power do not prevent source confusion; all fail negative-core and retention safeguards. |
| H9: smooth pair interactions, L2 .001 / .01 / .1 | 242+96 / 240+89 / 200+95 | All nine negative cores remain clear, but this initial representation loses the tree recall gain. |
| H10: add 73 reviewed training labels, linear / tree / interaction | 232+89 / 274+84 / **250+87** | Interaction gains ten hits without losing any of its prior hits; seven original-linear hits remain lost. |
| H11: raw / residual / both spectral shapes | 282+95 / 268+92 / 276+101 | Spectral power shares, concentration and width alter ranking, but introduce false fires and real-kick losses. |
| H12: past-only logit correction, strength .25 / .5 / 1 | 227+91 / 224+89 / 225+81 | Global score-centering does not solve the within-song overlap or transfer problem. |
| H13: tree + signed timing / + concordance / + concentration | 274+92 / 272+89 / 268+92 | Compact timing suppresses some bass fires, but also penalises legitimate Apricots and Inhale shapes. |
| H14: covered interaction/linear blend, interaction weight .25 / .5 / .75 | 238+90 / 249+91 / 253+93 | All nine negative cores remain clear. Blending repairs several old losses, but retains the shared Bad Guy false-trigger problem. |
| H15: add the next 32 reviewed labels, linear / tree / interaction | 228+89 / 278+93 / 245+88 | More contexts are not a monotonic improvement. The unchanged scorers still fail the full target. |
| H16: RBF support-vector margin, gamma 1/60 / 1/30 / 1/15 | 234+92 / 264+94 / **285+90** | Local nonlinear relationships recover more kicks. The strongest setting still has one negative-core fire and loses five old baseline events. |
| H17: kernel/covered-linear blend, kernel weight .25 / .5 / .75 | 239+93 / **251+91** / 262+93 | All retain the baseline within the per-song safeguard; .25/.5 keep nine cores clear, but extras remain above target. |
| H18: interaction weight .10 / .25 / .50 into H17's 75/25 blend | 261+92 / **262+89** / 260+92 | The middle setting meets counts and clears all nine negative cores, but loses two baseline Inhale events; the full safeguard still fails. |

H9 is a weighted logistic score over the original 15 fold-standardised values
and 120 bounded pair products. It tests smooth conditional relationships rather
than class reconstruction. H10 adds passages only to the training families: the
complete outer and inner test families remain absent. Original 381-label
calibration and evaluation objects are unchanged. The 73 reused labels are now
development training evidence, not fresh validation. H10 interaction loses two
Apricots, three Inhale and two Heavy events caught by original linear15. Its
original-174 result is 130+69; the added-207 result is 120+18.

The H12 diagnostic preceding calibration tested 102 fixed cutoffs retrospectively
on each song. Such label-informed, per-song choices give linear 301+79, tree
293+76 and interaction 306+75. These are diagnostic oracle results, not valid
detectors or exhaustive bounds. They show that some failures concern threshold
transfer, while Bad Guy and Midnight still have poor precision at 90% recall.
H12 instead uses only the preceding eight seconds of candidate logits, a
training-family reference and a capped offset; it creates no new candidates or
extra observation delay. An initial missing reporting method was repaired;
every first-configuration model, cutoff, event and score remains identical, with
only measured prediction CPU times changed. Independent replay verifies the
causal history and every family exclusion.

D2 adds eight chronologically selected 12-second cores from the four paired
source/master families. All 25 raw waveform and spectral sheets were reviewed
before acceptance. Thirty-four raw core proposals become 32 scored labels under
the existing uncertainty exclusions; three margin proposals remain available to
the matcher. Four complete sustained-bass cores are provisionally negative.
Two Late Night roll discontinuities and two Midnight intervals remain unknown.
Neither source silence nor a source-energy threshold alone defines these labels.

Frozen original linear/tree models score **19+0 / 27+7** on those 32 new labels;
frozen H10 linear/tree/interaction score **20+0 / 27+5 / 26+0**. The two tree
versions each fire twice in the new Late Night sustained-bass core; both smooth
scorers keep all four new negative cores clear. These are new passages of known
development families, not reserved recordings. H15 subsequently reuses D2 only
inside training families and discloses this reuse. Labels and review provenance:
`tests/fixtures/audio_labels/evening_extension_passages_2026-10-09.json`.

These extensions sharpen the working diagnosis: interaction learning and broader
examples can help, but neither extra features nor more labels guarantee improved
transfer. Ratio cues are weakened by sustained accompaniment; static spectral
shape and timing relations are not exclusive to kicks. A score can reject a bass
transition by moving an acoustic boundary and simultaneously reject a real kick.
Compare event identities, negative passages and emitted timing, not just pooled
matches. The original reference and all rejected candidates remain preserved.

H10 interaction scalar and batch predictions agree across all 43,585 cached
candidates, reproducing emitted events exactly. Scalar decisions take about
10.8–11.4 microseconds per candidate; model numeric arrays occupy 1,320 bytes.
Three 15–16-second sample-zero streaming probes (Bad Guy, Late Night, Heavy)
reproduce native-hop features and events with maximum feature error below
3.1e-14. Combined feature and decision CPU costs are 3.06%, 3.16% and 3.26% of
one core. Retained feature arrays occupy about 44–45 KB. These Python throughput
probes do not establish an allocation-free native callback or device latency.
Receipt: `h10/smooth_benchmark.json` under the evening cache root.

H16 uses the same 15 measurements, fold standardisation and H10 training
coverage. It is a classical, non-neural RBF SVM with fixed C=100, three declared
kernel widths and unchanged nested cutoff selection. The sigmoid-transformed
margin is a ranking score, not a calibrated probability. All 135 fits converged
within the 240 CPU-second protected budget (189 CPU seconds including export
checks). Gamma 1/15 reaches 146+66 on the original 174 labels and 139+24 on the
expanded 207. It recovers 67 original-linear misses while losing five previously
caught events: three Inhale, one Tears and one Miracle. Only Miracle's previously
reviewed bass transition fires in the nine original negative cores.

Kernel and interaction errors overlap only partially: kernel catches 39 labels
missed by interaction; interaction catches four kernel misses. Both lose two
baseline Inhale events. The kernel's Miracle bass fire has logit margin +0.050;
interaction assigns the same candidate margin -0.686. Complementary errors
justify a bounded blend experiment but do not establish that a blend will work.
The kernel uses 0.84–1.47 MB of retained arrays across the measured outer models
and about 0.12–0.21 ms per candidate in short decision probes. This adds cost
without approaching GPU inference; native callback timing remains unverified.
Full cached scalar scoring subsequently reproduced all nine emitted-event lists.
Combined sample-zero probes used 3.35%, 3.55% and 3.76% of one core on Bad Guy,
Late Night and Heavy, with maximum feature error below 3.1e-14. Their retained
feature/model arrays total 0.88–1.19 MB. These short Python probes establish
throughput and replay parity, not a hard callback deadline.

D3 fixes three 12-second Pattern cores before inspection. The named drums could
not be aligned reliably, so their timing never enters the truth. Independent
review of 16 master-only sheets accepts 39 provisional core onsets, one margin
onset and two explicit unknown intervals. Nine-family model fits and cutoff
selection were frozen before lead label inspection: linear gives 23+0, tree
14+3 and covered interaction 26+0. All three keep the scored opening clear.
Linear and interaction each emit a raw 48 ms startup trigger inside the
unchanged uncertain-response region; this is disclosed rather than called a
verified correct rejection. Whole-family exclusions, cutoff trials, feature
parity and scores passed independent replay. Pattern remains additional
development material; neither reserved family was accessed.

All three already-frozen H16 settings were then evaluated on D2 and Pattern,
with that follow-up explicitly declared after seeing the other methods' results.
No new kernel configuration or Pattern/D2 training was allowed. On D2 the three
settings give 25+0 / 27+0 / 28+1 of 32; on Pattern they give 28+0 / 28+0 / 30+2
of 39. This supports some transfer of the nonlinear gain, but is not untouched
validation or success under the full safeguards. The accepted Pattern evidence
is `tests/fixtures/audio_labels/evening_pattern_passages_2026-10-09.json`.
Independent audit verified all model, normalisation, cutoff and event records.
Gamma 1/15's D2 extra occurs in the Late Night sustained-bass negative core.
All three kernels have no raw opening emissions on Pattern.

`listening/continuation_manifest.json` preserves the same nine eight-second
excerpts used earlier. A is original linear15, C is H10 interaction, and D is
H16 gamma 1/15; every short high click marks the actual emission time. Source
identity, PCM quantisation, clipping and click alignment were checked. These
include Inhale regressions as well as Heavy and Late improvements. No human
listening verdict has been claimed.

H17 uses unchanged gamma 1/15 kernel and H10 covered-linear models, with no new
fits. At equal weights it loses only the baseline Inhale 6.62-second event and
gives 132+73 on the original 174 labels; all nine original negative cores stay
clear. At kernel weight .75 the earlier Miracle bass candidate falls below the
cutoff, but a later candidate at 220.602 seconds fires instead. Inspect the full
candidate/refractory sequence: suppressing one previously observed false event
does not establish that its acoustic passage is safe. The three variants lose
51, 38 and 27 kernel-caught labels, respectively, while repairing baseline
retention. This is a measured tradeoff, not a completed detector target.

A fixed analytic RBF-gradient diagnostic covers 380 actual event rows across
all nine songs: 67 recovered labels, 218 shared matches, five lost baseline
events and 90 extras. Twelve deterministic rows pass finite-difference checks
across all 15 coordinates (maximum absolute error 3.02e-10). In song-balanced
summaries, body-band log rise is among the top five sensitivities in all nine
songs and has positive mean sensitivity in all nine. Low-band rise and low
energy evolution also have positive song means throughout. Flux, peak lag and
centroid cues change sign across contexts; even low-band centroid drop has a
negative mean derivative in eight songs. This argues against treating a falling
centroid as universal kick evidence.

These are local derivatives of the learned score in fold-standardised feature
coordinates. They do not establish acoustic causation, source separation or an
event-recovery guarantee: real measurements are correlated, clipping matters,
refractory selection is held fixed, and jointly missed events are absent from
this particular diagnostic. The reusable lesson for other detectors is to
measure growth, spectral change and temporal relations together, then test their
conditional meaning across whole songs and explicit negative passages.

**H18 comparison at the end of exploration.** Counts below are matches/extras
at ±70 ms on the same provisional 381 labels. The original linear model remains
unchanged. H18 .25 is the closest candidate to all safeguards, not a promoted
replacement; H16 gamma 1/15 retains the highest recall among these comparisons.

| Track (labels) | Original linear15 | H16 kernel | H18 .25 blend |
|---|---:|---:|---:|
| Apricots (14) | 9 / 0 | 13 / 0 | 10 / 0 |
| Bad Guy (15) | 15 / 45 | 15 / 42 | 15 / 42 |
| Feel the Vibration (15) | 15 / 0 | 15 / 0 | 15 / 0 |
| Inhale Exhale (12) | 11 / 6 | 9 / 4 | 9 / 4 |
| Tears (10) | 10 / 11 | 9 / 4 | 10 / 8 |
| Late Night (77) | 53 / 0 | 71 / 2 | 67 / 0 |
| Midnight Patience (88) | 44 / 20 | 56 / 34 | 49 / 29 |
| Miracle (32) | 21 / 0 | 25 / 2 | 26 / 0 |
| Heavy on Mind (118) | 45 / 7 | 72 / 2 | 61 / 6 |
| **Total (381)** | **223 / 89** | **285 / 90** | **262 / 89** |

H18 .25 has 68.8% recall and 74.6% precision, versus 58.5% and 71.5% for
original linear15. It recovers 41 baseline misses while losing Inhale's 5.38-
and 6.62-second labels. Both have available candidates below the cutoff. Its
original-174 result is 138+68 (baseline 121+76), and expanded-207 result is
124+21 (baseline 102+13). Four possible duplicates remain, versus three for the
baseline. These are proximity diagnostics, not confirmed acoustic duplicates.

The candidate observes 42.63–42.67 ms of audio and emits at the completed
availability hop; it never backdates. Wider temporal associations have median
45.33 ms and p90 55.70 ms. Three Midnight associations exceed 70 ms; those late
associations do not become timely matches merely because their likely source is
nearby. Counts at ±35/50 ms are 42/186, respectively. Neither offline throughput
nor these annotation-relative delays measure hardware or display latency.

H18's Miracle improvement is a score/calibration interaction: at the later bass
candidate, adding the interaction scorer raises the raw score, but the nested
cutoff rises further. Full replay has zero fires in the core. Calling this a
simple acoustic veto would misdescribe the result. Bad Guy and Midnight still
account for 71 of its 89 extras. No result demonstrates a physical DSP limit,
and the 95% accuracy target remains unmet.

The final frozen H18 .25 evaluation scores 26/32+0 on D2 and 26/39+0 on
Pattern. All three predeclared H18 settings have those same counts. The strongest
kernel scores 28/32+1 and 30/39+2, respectively: the blend buys rejection by
giving up some recovery. These are further development comparisons, not reserved
confirmation. D2's four negative cores stay silent. Pattern has a raw 47.96 ms
startup emission inside its unchanged uncertainty exclusion; zero scored opening
extras must not be presented as verified cold-start rejection. One missing
covered-linear global component was fitted with the original 73-label coverage
only, after an explicit recorded amendment. Every global model and cutoff was
frozen before Pattern prediction. No Pattern or D2 labels entered that fit.

H18 scalar inference reproduces all 43,585 cached candidate scores to 2.73e-15
and all nine emission sequences exactly. Three 15–16 second cold-start probes
with feature extraction use 3.56–3.85% of one CPU core, retaining 0.89–1.20 MB
of numeric state; scalar decisions cost 0.121–0.162 ms per candidate. Observed
hop p99 is 0.544–0.641 ms and maximum 1.685 ms. These are short Python reference
measurements on this Mac, excluding audio decoding, not a hard native callback
deadline guarantee. Python still allocates bounded temporaries. The scalar
benchmark lives in the research cache; the frozen experiment source was restored
byte-for-byte after an appended runtime helper was moved out. Both snapshots and
the relocation receipt are preserved.

Listening files now include `E_threeway` beside the unchanged A/C/D renders in
`kick-research-2026-10-09-evening/listening/h18_manifest.json`. All nine fixed
excerpts use the same mix level and clicks at actual emitted times; the files
are verified renders, not a human listening verdict. In particular, compare the
Heavy on Mind recovery and the Inhale regression, rather than judging only a
favourable excerpt.

Research stopped when the reported Codex allowance reached 95% used (5%
remaining), after 18 hypotheses and 54 predeclared configurations. The time
ceiling was explicitly removed. No candidate passed every intermediate safeguard;
reserved recordings remain untouched and no detector was promoted. Local cache
receipts, frozen models, focused tests and source snapshots preserve the result.

The reusable finding is that band measurements need conditional interpretation.
Positive body-energy growth consistently helps the kernel's recovered kicks;
flux and centroid changes depend on the surrounding feature values. Merely
adding measurements, reducing scores during sustained bass, or demanding one
universal kick shape loses legitimate attacks. Kernel curvature recovers useful
cases that the linear score misses, while conservative blending reduces some
false fires. Its remaining shared misses and concentrated Bad Guy/Midnight
extras require diagnosis of failure mechanisms across tracks and verified labels before more
complexity. No rhythm-only predictions, neural models or external datasets were
used. This is a useful mechanism to test for other detectors, not evidence that
the same fitted weights classify snares, hats or synths.

**Night research, 2026-10-10 — limit demonstrated, no candidate promoted.**
Same 381 labels, nine songs, nested whole-song exclusion, 60 ms refractory and
coarse-plus-refined cutoffs. Frozen H18 component logits for every outer and
inner fold are exported once (`kick_night_dump.py`); `kick_night_replay.py`
reproduces baseline 223+89, H16 285+90 and H18 262+89 exactly. Results:
`tools/audio_analysis/eval/scoreboard/kick_night_2026-10-10.json`; cache
`~/.cache/manifold/kick-research-2026-10-10-night/` (`NIGHT_STATE.md` indexes it).
Waypoints and Know You're There remain untouched.

Where H18's 119 misses come from: 50 are cutoff transfer (a per-song cutoff
catches them with no more extras), 64 within-song ranking (Midnight 38, Heavy 17),
3 refractory, 2 no candidate. Within-song ranking is strong (AUC ≥ 0.97) in seven
songs; Bad Guy 0.94 and Midnight 0.84. No label-free song statistic predicts
the best cutoff (best spread 0.74→0.52 logit), which is why H12's median
re-centring failed: ~40 candidates/s are mostly non-kicks.

**The limit, proved on the final scores** (`kick_night_oracle_frontier.py`,
`kick_night_oracle_labels.py`; diagnostic only, thresholds picked per song with
the labels). An exact search over per-song thresholds finds no setting of
baseline, H16, H18, H21 or H22 that reaches 95% recall and precision pooled, with
or without Bad Guy. Best balanced points: baseline 302+80, H16 315+67, H18 311+71,
H21 308+73, H22 305+74 (about 80% both ways; without Bad Guy H22 296+71 of 366).
At 95% precision the best is 56–68% recall. The 90%-per-track floor alone sinks
precision to about 33%: Midnight needs 575–654 extras to catch 80 of 88, Heavy
47–68. Under H22, the 82 labels that force extras on the way to 90% split into
41 visible in the mix low band but scored below non-kicks (a feature gap: Bad
Guy 12, Heavy 13, Midnight 11), 18 masked in the mix (kick-stem attack, mix low
band rises < 3 dB) and 23 without a kick-stem attack (21 in Midnight: a label
question). The extras that come in are bass-line onsets (76), backbeat
claps/snares (20, low-band share below every labelled kick's), kick-stem attacks
(29, late duplicates or unlabelled) and, from Midnight's forced threshold, 609
other low-band onsets. In Bad Guy the backbeat claps outscore the kicks, so the
label-picked best for that song alone is to fire nothing.

What the remaining errors are, from stems only:
- Bad Guy extras are bass-line fires. At matched kicks the drum stem's 45–140 Hz
  band rises ~60 dB; at extras it rises 0.8 dB while bass/others rise 5–6 dB;
  bass is the loudest low stem at 29 of 42. Unlabelled drum-stem onsets there are
  body/high-band hits (claps, snares); the labels hold.
- Midnight: H18 misses 9 of 47 labels with a fresh kick-stem attack but 30 of 51
  without one (27 carry a drums-stem low attack, 20 no stem low attack, 4 bass
  only). The kick stem is loud but not rising there: retriggers over sustain or
  another layer. **Whether these are kicks needs Peter's listening verdict**
  (BUG-7rngq (Midnight kick-stem label decision); the Bad Guy definition is
  BUG-sa3n3 (kick lane and Bad Guy bass-line fires)).
- Heavy: 55 of 115 clean kick-stem attacks are missed; masking plus cutoff
  transfer, not label source.

Label timing: stem-to-master lags are found from whole-song low-band envelope
correlation without labels (Late Night 8 ms, Midnight 136 ms = the known
6546-sample prefix, Miracle 3 ms, Heavy −2 ms). Visual midpoints sit within
~10 ms of kick-stem onsets. At 70 ms every scorer is stable across midpoint,
physical-onset and perceived-attack labels (baseline 223+89 on all three; H18
261–263). At 50 ms counts swing ~30; 35 ms is unreachable by construction, since
the 42.6 ms evidence window sets the earliest emission.

| Hypothesis (three predeclared configs) | 381 at 70 ms | Finding |
|---|---|---|
| H19 causal pitch glide (30–250 Hz zero-crossing slope, drop, pre-jump) stacked on linear / H18 / H16 | 187+88 / 225+90 / 231+91, 1–10 core fires | Glide flips sign by production; one transferred weight learns "rising = kick". |
| H20 past-only upper-anchor cutoff (p90 / p99 / block-max median, 8 s) on H18 | 146+39 / 88+46 / 184+71, 14–20 core fires | Scores alone cannot tell masked kicks from absent kicks. |
| H21 kernel gated by its own support act/(act+5), fallback linear / linear / H18 | 239+94 / **240+97** / 258+96 | Kernel-lost kicks sit in low support (median activity 0.7 vs 13.8 for recoveries); gating restores retention but re-admits low-support extras. |
| H22 H21 gate with linear+glide fallback fitted on low-support candidates / same with H16 / control fitted on all | **256+90** / 250+86 / 229+86 | Inside low support glide points the physical way in every song; main config: zero core fires, ≤1 baseline loss per track, one extra over the count rule. Control fails, so the conditioning matters. |
| H23 early path: the 15 features over 15 / 20 / 25 ms fire confident kicks early, H18 decides the rest, one shared refractory | 262+89 on all three; 35 ms 47 / 54 / 65 (H18 42) | Safe: 70 ms unchanged, cores clear. Only 3–13% of fires qualify early (~16 ms sooner); fails the declared +20 at 50 ms or −10 ms median bar. Second feature pass costs 0.8% of a core in Python. |

Stem cue in the mix (`kick_night_lowshare_mix.py`): the share of first-40 ms
energy below 140 Hz separates kicks from every impostor on the drum stem (AUC
1.0) but not reliably in the mix. It holds for Bad Guy's claps (1.0, though the
frozen body/low balance feature already scores 1.0 there, so those clap fires
are a transfer failure, not missing information) and for bass-line onsets in
Late Night and Heavy (0.92/0.91, where the frozen balance feature gets ~0.4).
It collapses for the real false fires in Tears and Inhale (0.45/0.64) and
reverses in Midnight (0.17–0.34: its impostors are sub-heavier than its kicks).
A kick sits between claps (less low end) and sub swells (more); the kick's own
share runs 0.14–0.81 across songs, so no fixed cut on it transfers.

Without Bad Guy (366 labels): baseline 208+44, H18 247+47, H21 240 config
225+51, H22 main 241+52, H23 247+47.

H22 main versus H18: original 174 135+72 vs 138+68; added 207 121+18 vs
124+21; D2 28/32+2 with one fire in the Late Night sustained-bass core vs 26/32+0;
additional 73 47+19 vs 47+16 (song-excluded fires, development passages). Delay
p50/p90 45.2/56.8 ms, with one 183 ms late association. A first H22 run leaked
an in-place training-mask edit across folds and passed spuriously; it is void
and archived. Glide costs 0.34% of one core in batch Python; native callback
cost is unmeasured. Listening: `listening/manifest.json` (F = H22 beside the
evening's A and E renders).

Transferable lessons: separate candidate, score, cutoff and refractory losses
before changing anything; ask where a learner has training support before
trusting it, and route rare shapes to evidence that is physical for them; check
a cue's sign inside each support zone, not pooled; and audit labels against
isolated stems with a label-free lag before arguing about timing. Run the
per-song oracle frontier first, on day one: it separates threshold problems from
information problems in minutes. The kick detector stays kick-only (a joint
kick/snare/bass detector is rejected); its open gain is the 41 visible-but-ranked-low
kicks, where it must learn what a clap or bass-note onset looks like and reject it.

**Kick 90/90 goal, 2026-10-10 — limit demonstrated near 84/84; best nested 78/74.**
Target: pooled recall and precision ≥ 0.90 at ±70 ms, every song ≥ 0.80, zero
tail and kick-free fires, delay ≤ 70 ms, kick-only and causal. Truth v2
(`kick_goal_labels.py`, `labels_v2.json`, audited visually with
`kick_goal_view.py` and a Fable pass): fresh attacks in isolated kick stems on an
exact 1 ms analytic envelope; drum-bus kick-shaped onsets are uncertain regions;
strict mode counts kick rolls over a ringing tail as non-kicks (BUG-gh7sj
(rolls over a ringing tail) is Peter's call; loose mode restores them). Thirteen
songs: the nine development tracks plus four stemmed songs labelled from their
kick stems (Pattern, Back to You, Burn, Cold remix; Cold holds 642 of 1313 labels).
Harness: `kick_goal_eval.py` (whole-song nested cutoffs, tail and kick-free fire
counts); `run_kick_goal_preds.py` saves every nested prediction so cutoff rules and
stacked stages run exactly nested without refits.

Per-song oracle cutoffs (picked with the answers; `run_kick_goal_frontier.py`,
`run_kick_goal_frontier_saved.py`) bound every cutoff rule. All songs, best
balanced: base 15 features 0.815; + tail and kick-template cues 0.832 with the
16-band rise profile; + a causal 40–320 Hz filter bank 0.841; + a sidechain-fall
profile 0.846; the song-relative stage's own scores 0.835. Every family moves the
ceiling by about one point, so ranking, not cutoff choice, is the limit.

| Hypothesis | Result |
|---|---|
| More training songs (C1–C3) | Dev recall up, precision flat; template cues carry cross-song transfer (new songs 0.61/0.62 vs 0.46/0.49 without). |
| Per-song feature normalisation against all candidates (N1–N3) | New-song F1 +0.07; fails its bar. |
| 16-band rise profile (S1–S3) | Burn and Pattern extras fall by a third; fails its bar. |
| 50 ms evidence window (from 40) | Ceiling 0.818 vs 0.832: later emission loses more to the 70 ms match window than the evidence gains. |
| Low-band dip before re-firing | Tail fires 102 → 88; F1 unchanged. |
| **Song-relative stage (R1–R3)**: each candidate's score, shape and low rise against a p⁸-weighted average of the song's own recent candidates (8 s), stacked on base predictions | **All 13 songs R 0.784 P 0.738** (control 0.578/0.593; plain cutoff 0.657/0.652); new songs 0.748/0.747; Cold 165 → 420 of 642. It fixes cross-song cutoff transfer, not ranking. Wider variants (all relative levels, 4 s window, raw fallbacks) do worse. |

Limiting cases (rendered in `views/extras`): soft long sub kicks inside a constant
bass bed (Cold; Heavy's breakdown at 67–69 s), where the stem attacks but the
mix low band barely moves; non-kick drum hits with low end (Burn's toms, Heavy's
busy percussion, Back to You's claps), which rank above weak kicks; bass-note
plucks and section impacts (Back to You, Pattern); and Midnight's rolls over a
ringing tail under strict truth. Loose scoring does not lift the ceiling (0.837
with strict-trained models; a fair loose test needs loose training truth).
Lessons: run the oracle frontier on every new feature family before a nested
run; the frozen fusion feature sources key the night feature cache, so never
edit them (add a module instead); and a stacked song-relative stage is the cheap
fix for cross-song transfer.

```
# All nine synthetic scenarios, one PNG each + numeric gate lines on stdout:
cargo run -p manifold-audio --example mod_harness -- --selftest --out /tmp/st.png

# A real clip (WAV/AIFF/MP3/FLAC; stereo downmixed like the live path):
cargo run -p manifold-audio --example mod_harness -- path/to/clip.wav --out /tmp/clip.png

# Flags: --csv <dir> per-hop data · --floor <dB> analysis floor (default off)
#        --bpm <f> beat/bar gridlines (auto-parsed from "<n>bpm" in the path)
#        --low/--mid crossovers · --start/--dur excerpt
```

CSV filenames embed the input path — pre-create nested dirs when batch-running files
(`mkdir -p <csvdir>/<full/input/dir>`), or fix the papercut (sanitize `label` in
`write_csv`, mod_harness.rs) if it bites again.

**Real fixtures:** `tests/fixtures/audio/<track>_<bpm>bpm/{mix,bass,drums,others,vocals}.wav`
— 5 tracks, Ableton stem splits (gitignored, never commit audio). Rendered PNGs:
`tests/fixtures/audio/renders/`. Folder BPMs are historical and are not validated
export tempos. Grade against the reviewed attacks, not an assumed beat grid.

## 2. Reading the PNG

Top to bottom: title strip (config) · spectrogram · seven feature lanes
(AMPLITUDE, BRIGHTNESS, NOISINESS, LIVELINESS, TRANSIENTS, PITCH, PRESENCE) · time
axis (seconds; bar labels `B1..` when BPM known).

- **Band colors everywhere:** magenta = Full, red-orange = Low, green = Mid,
  blue = High (matches the app scope's legend).
- **Spectrogram overlays:** dotted color traces = per-band brightness centroid;
  small white dots = raw per-hop salience peak (memoryless); solid white line =
  the TRACKER's pitch (the product signal); faint horizontal lines = Low/Mid
  crossovers; color ticks at the bottom edge = transient fires (low lowest);
  vertical faint/bold lines = beat/bar grid when BPM known.
- **In each lane:** dim wide smear = raw per-hop value (its width IS the jitter);
  bright thin line = after the default AudioModShape smoother — what a bound param
  actually receives. PITCH lane draws a band only where that band's presence ≥ 0.25;
  a blank PITCH lane with a good white spectrogram line means presence is failing,
  not tracking.
- Judging: a "connected" result = the white line rides the perceptual object, PITCH
  lane engaged, PRESENCE high where the object exists and ~0 where it doesn't,
  TRANSIENTS ticking on real hits only.

## 3. Reading the CSV

One row per hop (~5.3 ms). Columns:
`hop_index, time_s, ground_truth_f0_hz, salience_f0_hz,` then per band
(`full,low,mid,high`) the five features `amplitude, brightness, noisiness,
liveliness, transients`, then `tracked_f0_hz`, then per band `pitch, presence`.

- `ground_truth_f0_hz`: selftest scenarios only (NaN for kicks/busymix/riser gaps
  and all file inputs). `salience_f0_hz` = memoryless P1 peak; `tracked_f0_hz` =
  the D5 tracker (NaN until first acquisition). `transients` hits exactly 1.0 on a
  fire hop, then decays ~0.85/hop.
- Standard checks (python one-liners):
  octave error = `12*log2(a/b)`; jump count = adjacent `tracked_f0_hz` deltas
  > 6 st; fire rate = count(`*_transients > 0.999`)/duration; on-grid % = fires
  within ±35 ms of the 8th/16th grid at the clip's BPM; presence health = mean
  per band. Copy the scan scripts from the session digest or rewrite — they're
  ten lines.

## 4. The gates (selftest stdout)

`P2 <scenario>:` tracker trajectory gates · `P2b:` presence gates ·
`P2c notes:` note-based material gates · `P3:` transient fire-count gates.
Each line prints its own bound and PASS/FAIL. **Known-failing by design as of
2026-07-06 (post-BUG-042 (onset-settle-grab)/043 fixes):** ONE line — `P2c notes` pitch accuracy
(87.6%, gate 90) — now owned by BUG-045 (gap-ring-down-chase), the residual
mechanism after BUG-042's fix. Notes presence is green (100%). Everything else green is the entry state; a change that reddens any other
line is a regression regardless of what it improves.

## 5. Current state + the open-bug oracles (2026-07-06)

Tracker validated on synthetics (dive one smooth line, growl 0.019 st) and works in
STRETCHES on real material (apricots bass: 3 bars fully engaged, then dies). Mixes
inherit stem behavior — object selection survives polyphony. Vocals stems score the
highest presence (0.30–0.43): near-mono sustained tonal is the easy case. Presence
on real note-based basslines is effectively dark.

| Bug | One line | Oracle |
|---|---|---|
| BUG-045 gap-ring-down-chase | tracker follows the kernel ring-down 2-4 bins down in note gaps; value-trend fix direction + its knife-edge risk recorded in the entry | `notes` accuracy line (87.6/90) |
| ~~BUG-042~~ FIXED 2026-07-06 | position-anchored re-acquire window (accelerated takeover clock); see backlog Fixed entry | notes gates + tears bass are the regression guard |
| ~~BUG-044 (mix-trigger-deafness)~~ FIXED 2026-07-06 | novelty-vs-recent-max dual onset criterion; see backlog Fixed entry | `densemix` gate + feel/apricots/tears mix fire counts |
| ~~BUG-043 (deep-bass-floor-anchor)~~ FIXED 2026-07-06 | apex-masked salience comb + dominance/consistency presence factors (see backlog Fixed entry) | `sub` scenario gates are the permanent regression guard |

**Floor experiment (2026-07-06, 25 clips, off vs −28 dB):** a raised analysis floor
is a TRADE, not a win — transient sensitivity recovers on quiet stems (feel bass
1.3→7.1 fires/s; vocals ~2×), but the dead mixes barely move at −28, BUG-043 is
untouched (the floor content is loud, strengthening the ghost hypothesis), and
presence/continuity mostly WORSEN (floor removes the quiet inter-note residue that
keeps continuation alive; bad bass 1→43 octave jumps). Direction it supports:
per-source/adaptive floor as a design candidate, never one fixed global value; and
never tune the floor to fix one feature without re-running the full scan.

## 6. Protocol for future sessions

1. Never grade on the PNG alone or the CSV alone — the picture finds what the
   numbers didn't know to measure; the numbers stop the picture from lying.
2. Any analysis change: selftest gates green (minus known-failing) → full 25-clip
   scan → read at least the PNGs your change should have moved AND one it shouldn't.
3. New failure class found → new synthetic scenario that reproduces it minimally +
   a gate, THEN fix.
4. Tuning constants: bounded candidates justified by mechanism, plateau demonstrated,
   or don't ship it (the 2026-07-06 presence formula history is the worked example).
5. Bugs found and not fixed in-session go to BUG_BACKLOG with their oracle named.
