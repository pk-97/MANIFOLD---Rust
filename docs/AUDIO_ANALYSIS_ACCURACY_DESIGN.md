<!-- index: Offline audio analysis v2: measured accuracy instead of vibes. An eval harness over public MIR datasets (Slakh, Harmonix, MUSDB18, MAESTRO) + metamorphic tests scores every detector; Beat This (MIT) replaces madmom's NC-licensed beat/downbeat/tempo models; live-proven precision machinery (median-adaptive thresholds, refractory windows) ports into the offline post-processing; sustained objects (chords, vocal phrases, sections) become duration clips through the existing Event/planner path. Clip-per-note is the contract (Peter). Unattended agent tuning loops run sweeps over cached model intermediates — tokens ≈ 0, held-out split untouchable. Also the shipping-license audit: madmom + ADTOF models are CC BY-NC-SA (BUG-069). -->

# Audio Analysis Accuracy — measured detection, licensed models, sustained objects

**Status:** IN PROGRESS — P1 SHIPPED 2026-07-17 (harness core, D7/D10/D11/D14, live-show fixture + provisional split awaiting Peter's veto; metamorphic invariance failure = BUG-227, a pipeline finding, not loosened) · designed 2026-07-08 · Fable
**Prerequisites:** none. All work in `tools/audio_analysis/` plus two small Rust seams (new trigger-type variants + inspector rows) in P5.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter's vision, verbatim (2026-07-08): *"My vision is we analyse the track and you get
the full set of clips for the whole track with clips that actually make sense where the
notes and transients of those 'objects' exist."* And the granularity ruling: *"a
16th-note arp becomes sixteen clips a bar — this is actually correct behaviour. I want a
clip per object note/event."* The problem is therefore **accuracy, not granularity**:
false events (ghost notes, hat double-fires, noise onsets) on non-percussive stems and
hats/perc, and no representation at all for sustained objects (chords, pads, vocal
phrases, breakdowns) or track structure.

The governing insight, two halves:

1. **The grid does the heavy lifting.** deadmau5's public claim (July 2026) of near-perfect
   full-track autocue *"purely with RMS, peak, standard DSP techniques"* works because
   section boundaries are only allowed on bar positions — a trustworthy beat/downbeat grid
   absorbs the timing slop of crude features. Grid quality is therefore the foundation
   every other detector stands on, which is why the Beat This swap is in this design and
   not a separate one.
2. **Accuracy is measurable without a human.** MIDI-aligned and annotated datasets give
   exact per-instrument precision/recall; metamorphic perturbations give label-free
   invariant checks. Once measured, tuning is a parameter sweep over cached model
   outputs — cheap enough for unattended agent loops. Peter (2026-07-08): *"we should use
   these datasets to also improve the real-time detectors in the future"* — the harness
   is built detector-agnostic for that reason (Deferred #4).

Companion docs: `AUDIO_CLIP_DETECTION_DESIGN.md` (the per-clip ownership model this
feeds — its engine, orchestrator, parser, planner are reused untouched);
`AUDIO_OBJECT_TRACKING_DESIGN.md` (stem fixtures context); `docs/CHANNEL_TYPE_SYSTEM.md`
(named-channel vocabulary the realtime side shares); `AUDIO_OBJECT_INGEST_DESIGN.md`
(**consumes this doc's `eval/` harness** — F10: one harness, not two. Its per-instrument
Basic-Pitch replacement is sequenced *after* this doc's P3 baseline + P4 precision pass and
must beat the post-P4 Basic Pitch F₁ in `eval/`; and if it drops Basic Pitch from the
synth/pad stem, §4.3's chord clustering — which consumes Basic Pitch notes on that stem —
must re-point to its segmenter output or coexist as a separate emitter. See OBJECT_INGEST
D4/D5).

---

## 1. Audit — what exists (verified 2026-07-08)

Instruction to every phase: **extend, don't redesign.** The Rust side and the pipeline
skeleton are sound; the work is new emitters, a swap, and a harness beside them.

| Piece | Where | State |
|---|---|---|
| Beats/downbeats/tempo | `tools/audio_analysis/manifold_audio/bpm.py:180` (`_estimate_madmom_beats`, RNN+DBN), `:281` (downbeat phase), `:552` (tempo-hypothesis scoring), `:813–855` (fusion w/ autocorrelation) | madmom-based; 878-line fusion largely compensates for madmom uncertainty |
| Onsets (vocals + generic) | `tools/audio_analysis/manifold_audio/onset_detection.py:13,29` (madmom CNN/RNN) | madmom-based |
| Non-drum notes | `tools/audio_analysis/manifold_audio/gestures.py:1–5` — bass/synth/pad migrated to basic_pitch (`basic_pitch_detection.py`); only vocal gestures remain here (madmom CNN + spectral composite) | per-note emission, no duration for synth/pad; ghost-note prone |
| Drums | ADTOF via `external_tools.py`; per-instrument thresholds e.g. `analyzer.py:107` (`adtof_hihat_threshold`) | works well per Peter, hats/perc weakest |
| Stems | demucs (htdemucs) via `external_tools.py:160+` | works, cached by config hash (`:244`) |
| Event model | `tools/audio_analysis/manifold_audio/models.py:19–23` — `Event{confidence, duration_sec}` | **duration + confidence already exist** |
| Bass sustain split | `models.py:154–155` (`bass_duration_threshold_sec`) | the in-repo precedent for sustained-event emission |
| Duration → clip length | `crates/manifold-playback/src/percussion_planner.rs:131` — uses `duration_seconds` when present, clamped 0.0625–32 beats | **planner already duration-aware** |
| Cached re-threshold | `crates/manifold-core/src/audio_clip_detection.rs:152` (cached events; sensitivity/quantize re-plan without re-running Python), `:49` (`min_confidence` mapping) | already shipped — do not rebuild |
| Trigger vocabulary | `crates/manifold-core/src/percussion_analysis.rs:20` — `PercussionTriggerType { Unknown..Vocal, BassSustained = 10 }` | additive variants are the extension point |
| Live precision machinery | `crates/manifold-audio/src/analysis.rs:261–278` (median-adaptive ODF baseline, per-band + kick refractory windows), `:520+` (SuperFlux onset detector, Böck & Widmer DAFx 2013) | proven on stage; the algorithms to port offline |
| Section/structure detection | — | **does not exist** (searched `section|segment|structure` over `manifold_audio/` 2026-07-08; only demucs segment-length args) |
| Offline replay of live stack | `render_export_audio` path (audio-reactive export, shipped 2026-07-07) | the seam future realtime-detector eval rides on |

**Shipping-license audit (verified via web, 2026-07-08 — see BUG-069):**

| Component | License | Shipped in pipeline? | Verdict |
|---|---|---|---|
| madmom source | BSD | yes | ok, but moot — models are the problem |
| madmom model files | **CC BY-NC-SA 4.0** (commercial use requires contacting Widmer) | yes (beats, downbeats, tempo, onsets) | **must be removed before commercial release** — P2 + P6 |
| ADTOF (code + model) | **CC BY-NC-SA 4.0** | yes (drums) | landmine; replacement Deferred #1 with commercialization trigger |
| basic_pitch (Spotify) | Apache-2.0 | yes | clean |
| demucs | MIT | yes | clean (weights: ⚠ VERIFY-AT-IMPL — confirm htdemucs weight license in the repo's LICENSE at fetch) |
| Beat This (CPJKU) | MIT — code **and** published weights | incoming (P2) | clean |

## 2. Decisions

- **D1 — Beat This replaces madmom for beats, downbeats, and tempo.** Grid is built
  from Beat This's beat/downbeat times; BPM derives from inter-beat intervals; the
  madmom arms of the fusion (`_estimate_madmom_beats`, `_detect_madmom_downbeat_phase`,
  tempo-hypothesis scoring) are **deleted**, not paralleled. The pure-DSP
  autocorrelation arm stays as the explicit no-model fallback, and a run that used it
  stamps `"tracker": "autocorr_fallback"` in the JSON — never silent.
  Rejected: an A/B acceptance gate against madmom — Peter: *"Not really interested in
  the A/B of MADMOM vs BeatThis if MADMOM isn't maintained anymore?"* madmom is
  unmaintained AND its models are NC-licensed; the swap is mandatory, so the gate is a
  before/after scoreboard plus a short listen-list for Peter, not a decision.
- **D2 — Full madmom removal is the end state.** After P2, the vocal-onset CNN is the
  last madmom dependency; P6 replaces it with a SuperFlux port (the live detector's
  algorithm, `analysis.rs:520`) scored by the harness, then deletes madmom from
  `requirements.runtime.mac.txt` and the staged runtime.
- **D3 — Clip per note/hit is the contract.** Peter, verbatim: *"I want a clip per
  object note/event."* Rejected: a musical-object grouping layer (phrases/patterns
  replacing per-note clips) — proposed 2026-07-08, killed by Peter same day. Do not
  reinvent it. Accuracy work = fewer false events and fewer misses, never fewer true
  events.
- **D4 — Sustained objects are duration events through the existing path.** Chords/pads,
  vocal phrases, and sections emit `Event.duration_sec`; the planner already sizes
  clips from it. Peter: *"I want the clips to represent the duration of the events
  themselves (somewhat, still need to be long enough to render and see)"* → starts snap
  down to grid, ends snap up, floor 1 bar for Section events and 0.25 beat for others
  (both sweep-tunable). New `PercussionTriggerType` variants shaped like
  `BassSustained = 10`: `Chord = 11`, `VocalPhrase = 12`, `Section = 13`.
- **D5 — Section detection is standard DSP on the downbeat grid, v1.** Beat-synchronous
  per-stem RMS matrix → checkerboard novelty → boundaries snapped to downbeats, minimum
  section 4 bars; labels by rule from stem activity (drums absent + energy low →
  `break`; drums present + broadband high → `drop`; leading boundary → `intro`; else
  `section`). Peter on Harmonix: *"sounds very interesting, espically with the
  boundaries and labels."* Rejected for v1: a learned segmentation model — revive only
  if the harness shows rule-based labels are the weak link (Deferred #3).
- **D6 — Precision machinery ports from the live detectors, non-causally.** Rolling-
  median adaptive ODF baseline + refractory windows (`analysis.rs:261–278`) applied to
  the offline post-processing of ADTOF activations, basic_pitch candidates, and stem
  onsets — with offline luxuries the live path can't have: whole-track per-stem
  normalization and per-section renormalization. Port the *algorithms*, not the Rust —
  they are ~30 lines of numpy each.
- **D7 — AnalysisBundle: content-addressed cache of model intermediates.** Key =
  SHA-256 of source audio + per-stage pipeline version; value = stems, beat/downbeat
  times, ADTOF activations, basic_pitch note candidates, per-stem envelopes
  (npz + JSON). Default location `~/Library/Caches/Manifold/analysis-bundles/`,
  overridable `--cache-dir`. Two consumers: the app (re-detect and cross-project reuse
  become cache reads) and the harness (tuning iterates over cached arrays — seconds per
  track, no model re-runs). This extends the existing demucs config-hash cache
  (`external_tools.py:244`) to every model stage; it does not replace the app-side
  per-clip event cache (`audio_clip_detection.rs:152`), which stays.
- **D8 — Dataset use is evaluation/tuning only; pinned stance.** Peter: *"Surely using
  a dataset to just tune detection parameters is fine and not legally wrong?"* —
  pinned: internal evaluation and parameter tuning against NC-licensed datasets is
  acceptable; the hard lines are **never redistribute dataset audio** (fetch scripts +
  checksums are committed, audio never is) and **never train shipped model weights on
  NC data** (that day, the stance is re-decided). Contrast deliberately with BUG-069:
  the problem there is *shipping* NC-licensed model files, not evaluating against NC
  data.
- **D9 — Fixture tiers with an untouchable held-out split.** `dev` (~40 on-genre
  tracks: electronic-leaning Slakh, Harmonix electronic slice, MUSDB18 subset) for the
  tuning loop; `heldout` (similar size, disjoint) read only by the acceptance step;
  `full` (everything fetched) for rare regression sweeps. **No tuning process ever
  reads held-out fixtures** — this is the Goodhart guard that makes unattended loops
  trustworthy; the harness enforces it structurally (separate directories, the sweep
  CLI physically cannot take a heldout path).
- **D10 — Metrics are frozen at P1.** Per-instrument event P/R/F₁ at ±50 ms; beat F₁
  and downbeat F₁ at ±70 ms (standard MIR tolerance); section boundary F₁ at ±0.5 bar
  + label accuracy; duration IoU for sustained events; clip-economy diagnostics
  (events/bar/lane — **advisory only, never optimized**: optimizing density would
  violate D3). Every metric change after P1 is a Peter escalation, because it
  invalidates all recorded baselines.
- **D11 — Change acceptance = delta > 2× the measured noise floor, on held-out.**
  Torch on MPS is not bit-deterministic; P1 measures per-metric rerun variance (N=3
  full model passes on the dev set) and stores it as the noise floor. Bundles are
  stamped {pipeline_version, model name+version per stage, seed}; a bundle regenerated
  under different stamps never silently mixes into a comparison.
- **D12 — The unattended loop is scripts, not conversations.** Sweeps (grid/random over
  post-processing params) run as plain Python against cached bundles; an agent reads
  the scoreboard (aggregates + worst-10 tracks per metric) and intervenes only to
  change detector *code*, re-entering through the same acceptance gate. Every accepted
  change is a commit whose message carries the held-out scoreboard delta. Sequencing
  of this relative to the release is Peter's, not the doc's (his call, 2026-07-08).
- **D13 — Vocal ground truth is derived regions, not notes.** Gate the *clean* vocal
  stems of MUSDB18 to produce near-truth activity regions; score our detector (running
  on demucs output of the mix) against them. Note-level vocal datasets (DALI) are
  Deferred #2 — the product deliverable is phrase clips, so region truth suffices.
- **D14 — Absolute alignment by measurement, not fudge (added 2026-07-08, same
  session — Peter: grid/downbeat/BPM "often wrong" against the waveform, "some fudge
  factors and ms offsets applied at the moment").** Today one hand-tuned constant —
  `onset_compensation_seconds`, default 10 ms (`percussion_settings.rs:42`), added to
  every event at plan time (`percussion_planner.rs:85`) — absorbs at least three
  distinct physical offsets: decoder skew (pipeline decodes via ffmpeg, the app's
  waveform via its own media path; mp3/AAC priming delay differs per decoder — a
  per-format constant), model hop quantization (10–23 ms frames, center-vs-start
  conventions), and attack-vs-beat bias per model. The fix: **click-track truth
  fixtures** — clicks rendered at exactly known sample positions, exported as
  wav + mp3 + AAC, run through the full pipeline; the measured per-stage, per-format
  offsets are applied once at the seam where audio enters analysis, stamped into the
  AnalysisBundle (D7's version fields), and re-measured automatically when any model
  or ffmpeg version changes. End state: `onset_compensation` defaults to **zero** and
  remains only as an artistic offset (deliberately early triggers for visuals),
  never error correction. Rejected: keeping a hand-tuned global default — it cannot
  be right for wav and mp3 simultaneously.

## 3. The harness

Lives at `tools/audio_analysis/eval/` (a sibling package to `manifold_audio`, importing
it one-way; app code never imports eval code). Plain Python, same runtime env.

```
tools/audio_analysis/eval/
  fetch/            # one script per dataset: download → verify checksums → extract
                    #   only the manifest-listed tracks → distill → delete bulk
  fixtures.toml     # the manifest: id, dataset, split (dev|heldout|full), roles
                    #   (grid|drums|notes|vocals|sections|stems), license note
  bundles.py        # AnalysisBundle build/load (D7)
  metrics.py        # D10 implementations; mir_eval where it matches the definition
  metamorphic.py    # D-suite below
  sweep.py          # parameter sweeps over cached bundles; dev-split only by construction
  run.py            # CLI: `python -m eval.run --set dev --report scoreboard/<date>.json`
  scoreboard/       # committed JSON reports (small); the record of every baseline/delta
```

**Datasets (availability/licenses verified 2026-07-08; fetch scripts re-verify license
text on download and store it beside the manifest):**

| Dataset | Truth provided | Role | Size strategy | License |
|---|---|---|---|---|
| Slakh2100 (Zenodo 4599666) | aligned MIDI (notes + durations, per stem), true stems | bass/synth/keys notes, duration IoU, stem sanity | **test split only** (225 tracks, ~11 GB of the 105 GB flac total); `babyslakh_16k` for P1 bring-up | CC-BY 4.0 |
| Harmonix Set (github urinieto/harmonixset) | beats, downbeats, functional segments w/ labels (912 pop/EDM tracks) | grid + **sections** | annotations tiny (git); audio user-matched per track, incremental — start with the electronic slice | annotations open (CC0-style; agreement in repo) |
| MUSDB18-HQ / MUSDB18 | true stems | vocal regions (D13), demucs/gating eval | compressed edition (~4 GB) in full | research/NC — eval-only per D8 |
| MAESTRO v3 | real piano + aligned MIDI | sustained-polyphony tuning for basic_pitch post-processing | ~20 performances (individually addressable files) | CC BY-NC-SA — eval-only per D8 |
| E-GMD | real drum performances + MIDI | drum truth for the ADTOF-replacement day | **deferred** (Deferred #1) | ⚠ VERIFY-AT-FETCH |
| Self-rendered | agent-composed MIDI → synth render | on-genre arps/stabs/pads with perfect truth | generated on demand, grows as gaps appear | ours |

Rejected as ground truth: Peter's own Ableton sessions — his ruling 2026-07-08: *"My
Ableton sessions ARE nowhere suitable for this type of work, they're huge complex
projects."* Do not re-propose parsing them for fixtures. His **bounces** still serve as
the L4 taste gate (import + eyeball), which no dataset replaces. Also rejected:
ADTOF's own dataset for scoring drums — we ship the ADTOF *model*; scoring it on its
training data measures memorization (Peter caught this).

**Metamorphic suite (label-free, runs on any audio including Peter's bounces):**
gain ±6 dB → event times/counts unchanged; time-stretch ±5% → event times scale, grid
BPM scales; known stem mixed in at known offset → events appear there; noise floor
added at −40 dB → no new events. Violations are bugs, not tuning targets.

**Plausible-wrong architectures, forbidden by name:** you will want to (a) rewrite the
Python pipeline into a new unified "analysis v2" module — no: new emitters slot into
the existing `Event` JSON → parser → planner path, everything else stays; (b) build the
harness in Rust or wire it into the app — no: it is a tools-side Python package, full
stop; (c) let the sweep "just once" read held-out fixtures to check progress — no:
acceptance runs are a separate invocation and the only held-out reader; (d) keep
madmom importable "as a fallback" after P6 — no: deletion gate; (e) commit audio or
bundles to git — no: fetch scripts + checksums + scoreboards only.

## 4. Detector specifications

- **4.1 Beat This integration** (`beat_tracking.py`, new): wrap the `beat_this` pip
  package's file-to-beats inference (⚠ VERIFY-AT-IMPL: exact API + weight auto-download
  behavior from github.com/CPJKU/beat_this README at build time; pin the version).
  Output: beat times + downbeat times. Grid construction: reuse the existing
  beat-grid builder fed from these times; BPM = median IBI over the analysis window;
  keep the existing grid-confidence field, sourced from IBI variance. Seam brief for
  `bpm.py` in P2's entry: delete `_estimate_madmom_beats`, `_detect_madmom_downbeat_phase`,
  the madmom import block (`bpm.py:22–31`), and the hypothesis-scoring arm (`:552`);
  the autocorrelation estimator stays as the stamped fallback (D1).
- **4.2 Precision post-processing** (shared module, applied per detector): ODF/activation
  → rolling-median baseline (window ~1.5 s) → threshold = median + k·spread →
  peak-pick with refractory window (per-instrument, e.g. hats ~60 ms) → confidence =
  normalized exceedance. Whole-track per-stem normalization first; per-section
  renormalization once sections exist (P5+ reruns P4's sweep with it enabled).
- **4.3 Chord/pad events**: cluster basic_pitch notes on the synth/pad stem — onsets
  within 50 ms and pitch overlap → one chord; span = union of member notes; split when
  the sounding pitch-set changes; emit `Chord` with duration + mean confidence.
  *(F10 seam: this consumes basic_pitch notes on the synth/pad stem. If OBJECT_INGEST D4
  later replaces basic_pitch on that stem, this clustering must re-point to its segmenter
  output or coexist as a separate emitter — that doc's P3 brief owns the reconciliation.)*
- **4.4 Vocal phrases**: hysteresis gate on the vocal-stem envelope (on/off thresholds
  relative to track-normalized energy, min-on 0.5 beat, min-off 1 beat); each gated
  region = one `VocalPhrase` with duration; onsets within a region remain available as
  plain Vocal events (both types emitted; the inspector rows control which import).
- **4.5 Sections** (D5): emitted as `Section` events with duration + label string in
  the event payload (extend the JSON event dict — `_event_to_dict`, `analyzer.py:517` —
  with an optional `label` field; parser maps it into the clip name).

**Rust seams (P5 only).** Additive enum variants `Chord = 11`, `VocalPhrase = 12`,
`Section = 13` in `percussion_analysis.rs:20` (additive = load-compatible; ⚠
VERIFY-AT-IMPL: confirm the serde representation of `PercussionTriggerType` so old
projects deserialize unchanged — read the derive attributes at the file head).
Parser: map the new JSON types + `label` passthrough (`percussion_parser.rs`, shape
like the existing type mapping at `:115–136`). `InstrumentDetect` rows for
Chord/VocalPhrase/Section with the standard sensitivity slider + target layer
(`audio_clip_detection.rs:28–50` — the existing row shape, no new UI idioms). Planner:
untouched — the duration path (`percussion_planner.rs:131`) already does the work;
only the Section length floor (1 bar) lands as a per-type clamp beside the existing
0.0625–32 clamp.

## 5. Phasing

Common to all phases: test scope = `tools/audio_analysis` has no cargo tests — Python
phases gate on the harness's own pytest + CLI runs; Rust-touching phases (P5) run
focused `-p manifold-core -p manifold-playback --lib` plus the workspace sweep at the
end of P5 only. No GPU-proofs anywhere (nothing touches shaders). Git: Mode B, ONE
warm worktree for the workstream (`agent-worktree.py acquire`), orchestrator lands.

- **P1 — Harness core + noise floor.** *Entry:* repo tip; `python3 -c "import manifold_audio"`
  succeeds in the staged runtime. *Read-back:* this doc §2–§4; DESIGN_DOC_STANDARD §5.
  *Deliverables:* `eval/` package (layout §3), `bundles.py` (D7 cache, stamped),
  `metrics.py` (D10, frozen), `metamorphic.py`, fetch scripts for `babyslakh_16k` +
  Harmonix annotations + MUSDB18-compressed, `fixtures.toml` with dev/heldout split,
  noise-floor measurement (D11, N=3, committed to `scoreboard/`), and the **D14
  click-track alignment fixtures** (clicks at known sample positions, rendered to
  wav + mp3 + AAC) with the per-stage/per-format absolute-offset report. *Gate (positive):*
  `python -m eval.run --set dev` produces a scoreboard JSON; metamorphic suite passes
  on babyslakh; the alignment report exists and the measured correction is applied at
  the analysis-input seam (D14); pytest green. *Gate (negative):* `rg -l '\.wav|\.flac|\.mp3' --glob
  '*.gitignore' tools/audio_analysis/eval` shows audio dirs ignored; `git ls-files |
  rg '\.(wav|flac|mp3|npz)$'` → zero hits; `rg 'heldout' eval/sweep.py` → zero hits.
  *Demo:* the scoreboard JSON + one worst-track detail dump — L1 (no UI surface).
  *Forbidden:* inventing app seams; downloading full Slakh; metric definitions beyond D10.
- **P2 — Beat This swap.** *Entry:* P1 landed (`eval/run.py` exists); anchors
  `bpm.py:22–31,:180,:281,:552` re-verified. *Read-back:* §4.1 seam brief + D1/D2.
  *Deliverables:* `beat_tracking.py`, rewired grid construction, madmom beat/downbeat/
  tempo arms deleted, before/after scoreboard committed, listen-list (10 tracks, beat
  click renders) for Peter. *Gate (positive):* beat F₁ + downbeat F₁ ≥ madmom baseline
  − noise floor on dev AND heldout (record both numbers in the landing report;
  downbeat F₁ is the one expected to *rise*); grid absolute alignment ≤ 5 ms on the
  D14 click fixtures, per format (wav, mp3, AAC). *Gate (negative):*
  `rg 'madmom.features.(beats|downbeats|tempo)' tools/audio_analysis` → zero hits.
  *Demo:* scoreboard delta + listen-list — L2 (Peter listens, deferred OK as VD entry).
  *Forbidden:* keeping the madmom arm behind a flag; touching onset_detection.py (P6's).
- **P3 — Fixture pack build-out + full baseline.** *Entry:* P1–P2 landed. *Deliverables:*
  fetch scripts for Slakh test split, MAESTRO selection, Harmonix audio matching
  (incremental, best-effort — log unmatched), self-render generator v1; baseline
  scoreboard for every existing detector on the full pack. Agent-runnable overnight;
  *Gate:* fixtures.toml counts reported; every fixture has bundle + license note;
  negative gates from P1 re-run. *Demo:* baseline report — L1. *Forbidden:* tuning
  anything (this phase only measures); committing audio.
- **P4 — Precision pass (drums, bass, synth).** *Entry:* P3 baseline exists.
  *Read-back:* §4.2, D6, D9, D11. *Deliverables:* shared post-processing module,
  applied to ADTOF + basic_pitch + stem-onset paths; sweep configs; accepted params.
  *Gate:* held-out P/R/F₁ improves > 2× noise floor for hats + perc + synth (the named
  weak spots) with no instrument regressing beyond noise; metamorphic suite still green.
  *Demo:* before/after scoreboard — L1. *Forbidden:* touching event *counts* via
  density targets (D10: economy metrics are advisory); reading heldout in sweep.py.
- **P5 — Sustained objects + sections + Rust seams.** *Entry:* P2 landed (sections
  need the grid). *Read-back:* §4.3–4.5, D4/D5, round-trip gate in DESIGN_DOC_STANDARD
  §5. *Deliverables:* chord/vocal-phrase/section emitters; `label` field; enum variants
  + parser mapping + inspector rows; per-type length floors; **D14 Rust half** —
  `onset_compensation_seconds` default 10 ms → zero (`percussion_settings.rs:42`), the
  knob re-documented as artistic offset only; Section row ships default-OFF until its
  accuracy gate has passed on real tracks (Peter, 2026-07-08: sections are "a bit
  dangerous if we get them wrong"); plus the two logged detection-UX clunks while in
  these files (sensitivity commit-on-release = two undo entries per gesture; triggers
  don't follow the clip after move/warp). *Gate (positive):* section
  boundary F₁ ≥ 0.7 @ ±0.5 bar on the Harmonix electronic slice (record the number;
  escalate if the slice is <30 matched tracks); duration IoU ≥ 0.6 on Slakh sustained
  notes; **round trip**: save → reload → re-detect-from-cache reproduces identical
  clips; focused Rust tests + workspace sweep green. *Gate (negative):* old projects
  load (canonical fixture `Liveschool Live Show V6 LEDS.manifold` opens clean).
  *Demo:* headless import of one Harmonix track → timeline PNG showing section spans +
  chord/phrase clips with durations, read by the orchestrator — L2; performer gesture:
  drop a track, Detect, sections appear as clips you can hang looks on. *Forbidden:*
  new UI idioms (reuse the InstrumentDetect row shape); non-additive enum changes.
- **P6 — Vocal SuperFlux + madmom deletion.** *Entry:* P4 landed (shared post-processing
  exists). *Deliverables:* SuperFlux port for vocal/stem onsets (algorithm from
  `analysis.rs:520`, reimplemented in numpy); madmom removed from requirements +
  staged runtime + imports. *Gate (positive):* vocal onset F₁ ≥ old CNN baseline −
  noise floor on heldout (MUSDB-derived regions, D13). *Gate (negative):*
  `rg 'madmom' tools/audio_analysis --glob '!*.md'` → zero hits. *Demo:* scoreboard —
  L1. *Forbidden:* the "keep it as fallback" move (D2); porting Rust code verbatim
  instead of the algorithm.
- **P7 — Unattended loop automation.** *Entry:* P4 accepted once manually (the protocol
  has run under supervision). *Deliverables:* sweep-schedule scripts, acceptance
  automation (D11/D12), agent runbook (scoreboard-reading brief, worst-K triage).
  *Gate:* one full unattended cycle produces either an accepted commit with scoreboard
  delta or a clean no-change report. Post-release territory — Peter re-ranks
  (his call, 2026-07-08: release-relevance sequencing is his).

Phasing-completeness check (run 2026-07-08): every §2–§4 commitment maps to a phase —
harness/cache/metrics/noise floor + D14 alignment fixtures → P1; Beat This + fusion
deletion + alignment gate → P2; datasets + baselines → P3; precision/hats/perc → P4;
chords/phrases/sections/labels/Rust seams/inspector rows/length floors +
compensation-to-zero + Section default-off + the two detection-UX clunks → P5;
SuperFlux + madmom-zero → P6; loop → P7; ADTOF
replacement, DALI, learned sections, realtime tuning → Deferred #1–#4.

## 6. Decided — do not reopen

1. Clip per note/event; no grouping layer (Peter, 2026-07-08).
2. Beat This in, madmom fully out by P6; no A/B decision gate (Peter + license).
3. Peter's Ableton sessions are not fixtures (Peter, verbatim above).
4. Datasets: eval/tuning only; no audio in git; no training shipped weights on NC data.
5. Held-out split is untouchable by any tuning process.
6. Metrics frozen at P1; changes escalate to Peter.
7. Sections v1 = DSP rules on the downbeat grid, bar-quantized, labeled.
8. Harness is tools-side Python; detector-agnostic (offline now, realtime later).
9. Drum granularity stays per-hit — hats-as-pattern-changes was proposed and NOT taken
   (Peter's per-event ruling covers drums; do not re-propose).

## 7. Deferred

1. **ADTOF replacement** (NC license, BUG-069) — trigger: commercialization v1.0 gate,
   or drum accuracy work resuming. E-GMD + Slakh drums are the clean truth; the
   harness makes any candidate measurable. Approach settled with Peter 2026-07-08
   (*"I really like this idea of a trained model for drum stems only"*; do NOT email
   Zehren yet — his call). Two stages, one contract:
   - **Stage 1, DSP object detectors on the demucs drum stem** (no training): the live
     kick detector's logic offline with non-causal luxuries — larger/centered windows,
     whole-track normalization, backward sample-accurate attack refinement (feeds D14);
     clap/snare/hat/tom via per-onset features (centroid, flatness, band ratios, decay
     shape). Key mechanism: **cluster the track's onsets first (3–8 drum objects),
     label clusters by centroid signature** — per-track calibration, never
     one-onset-at-a-time global templates.
   - **Stage 2, own trained model**: small CRNN (1–5M params) on log-mel of the drum
     stem, 5–8 classes (clap split from snare, open/closed hat). Training data by
     construction, permissive-only (D8): E-GMD + Slakh + self-rendered drum MIDI
     through synthesized kits + EDM production-chain augmentation. **Domain-matching
     trick: train on demucs-SEPARATED renders, not clean stems** — the model learns
     the separation artifacts it will see at inference; this is where it beats
     mix-trained ADTOF in our domain. Ships as CoreML/ONNX weights we own.
   - **Contract**: both stages emit candidates-with-confidences into the same Event
     JSON, scored per-class on held-out; ship whichever wins per class (kick may stay
     DSP while hats go learned). All agent-runnable; Peter's ear is the top gate.
2. **DALI / note-level vocal truth** — trigger: phrase-region metrics prove
   insufficient for a vocal feature Peter actually wants.
3. **Learned section segmentation** — trigger: harness shows rule-based labels < what
   Harmonix says is achievable AND sections matter enough on stage to chase it.
4. **Realtime detector tuning on this harness** — trigger: next live-detector session;
   replay causal detectors over fixture audio via the offline path (Audit table, last
   row); Peter explicitly wants this (quote in intro).
5. **Peter's-bounce fixture set for L4 taste** — trigger: Peter supplies bounces;
   they join the metamorphic suite (label-free) + eyeball imports, never the tuned sets.
6. **`.als` automation import** — one-shot offline tool parsing the Ableton set
   file (gzipped XML: true breakpoints, curve shapes included) to migrate Ableton
   macro automation into native Manifold automation lanes 1:1, resolving each
   envelope's device parameter to its Manifold param via the existing
   `AbletonParamMapping` structural identity. OSC route rejected for 1:1: the
   Live Object Model exposes only `value_at_time` sampling, no breakpoint access
   (sampling + breakpoint simplification remains the fallback if the XML
   target-resolution proves gnarly). Not on the eval-set critical path —
   trigger: Peter asks (parked 2026-07-17).

## Addendum 2026-07-09 — BUG-069 rework reframed (Fable + Peter discussion)

- **Energy/section detection** (what [AUTO_POPULATE_DESIGN.md](AUTO_POPULATE_DESIGN.md)
  needs — calm/build/drop/groove, bar-snapped) is classic DSP, license-clean, in-house
  Rust. It survives the madmom/ADTOF removal; only beat/downbeat tracking and drum
  transcription lose models.
- **Genre tagging** (opt-in roll-pool weighting): BPM/rhythm heuristic first; model
  candidate PANNs — **weights CC BY 4.0** (Zenodo record 3987831, license field read
  directly 2026-07-09; attribution = in-app credit line). YAMNet (Apache 2.0) fallback.
  ChatGPT claimed the weights carried no separate license — wrong; the Zenodo record
  attaches CC BY 4.0. Always read the record itself.
- **Vocal detection**: stems when present (vocal-stem activity — free, exact); full-mix
  needs a model, separately vetted, never blocks the rest.

## Addendum 2026-07-17 — Live-show eval fixture + oracle stance (Fable + Peter discussion)

- **Peter's 20-minute live set becomes a first-class fixture set.** The manually
  placed clips are labels — three truth types with different reliability: the
  project's BPM grid = beat/downbeat/tempo truth (the set is grid-locked); clip
  starts in drum-built sections = onset truth within a tolerance window; clip
  starts in synth/ambient sections = section-boundary truth only (placed ahead of
  swells, quantized — NOT acoustic onsets). A small extractor reads the
  `.manifold` project (grid, clip starts, section boundaries) and emits harness
  labels. The quiet/ambient sections are deliberately valuable: they measure
  false-positive rate, the failure mode that ruins a show and that public onset
  benchmarks under-test. (Consistent with the 2026-07-08 "Ableton sessions are
  NOT fixtures" ruling — that barred his huge Ableton *project files*; this uses
  the exported audio plus the *Manifold* project's labels.)
- **Tier split by section (D9 guard applies).** Some sections join `dev`, some
  `heldout`; the show must not be tuning-set-only or it stops predicting stage
  behavior. Peter owes the split call (or the first ingest session proposes one).
- **Audio path.** The harness consumes the demucs separation of the master export
  (htdemucs via the existing `external_tools.py` path) — consistent with the
  domain-matching stance (tune on separated renders, not clean stems). Peter's
  real Ableton stems serve exactly one purpose: scoring demucs itself on his
  material (the separation-error yardstick). They are never harness input.
- **madmom/ADTOF as dense reference oracles on the show — within D8** (evaluation
  and parameter tuning only). They fill the density gap (every hit, not just
  where a clip was placed) but are NOT truth: disagreements with Peter's labels
  get spot-checked by ear. Carve-out against D2's deletion gate: after P6, madmom
  may exist ONLY inside the harness environment, never importable from the app
  pipeline — an eval-tool exemption, not a fallback.
- **Windowed real-time structure detector (Peter's proposal).** Rolling N-second
  window → tempo via onset autocorrelation, phrase/section via self-similarity +
  novelty, adaptive per-band thresholds — no trained weights. It competes on the
  same scoreboard against Beat This on the show fixtures; if it hits the bar
  on-distribution, it wins (also the license-clean outcome). Expected division
  stands: the window is memory of *this* song; trained weights are a prior over
  *music* — beat/downbeat on unfamiliar material and drum-object naming remain
  model candidates (Beat This offline; Deferred #1 Stage 2).
