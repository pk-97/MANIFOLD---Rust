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
