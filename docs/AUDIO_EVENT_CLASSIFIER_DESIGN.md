# Audio Event Classifier — name the hit the DSP front-end already found

**Status:** APPROVED design, not built · 2026-07-18 · Fable (authored in-session with Peter)
**Prerequisites:** none — the harness, shared data store, and truth assets all landed 2026-07-18 (`74c14de6` and ancestors).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The governing insight, measured 2026-07-18: **detection is solved, naming is the
wall.** On raw single-track masters of Peter's live show, the license-clean DSP
front-end finds 934/934 hand-placed drum hits with 2–9ms median timing error —
but the signature labeler gets snare labeled-recall 0.00 on 3 of 5 songs. This
design adds the one missing component: a tiny trained classifier that names each
detected onset. Peter's directives, verbatim, which decide the shape:

- Target: *"I think we aim for the classifier to be trained for the single track
  masters. If we can get it working on single tracks we can avoid all of the stem
  stuff."*
- Trainer language: *"Python for training yes"* — inference *"rust native too,
  faster, safer, removes the need for this awkward and slow python pipeline."*
- Synthetic data caution: *"I'm cautious of the synthetic masters"* → the
  composited-data share is a measured dial, never the foundation (D6).
- **Training approval:** this doc IS the scoped approval AUDIO_ANALYSIS_ACCURACY
  §7.1 required — the small classifier only (*"let's start"*, 2026-07-18), NOT the
  full Stage-2 transcription CRNN, which stays parked.

On stage this buys: per-drum triggers from the live feed (snare-only strobes,
hat shimmer) and, offline, master-only import analysis with no demucs and no
Python in the product. Companion docs: AUDIO_ANALYSIS_ACCURACY_DESIGN.md (the
measurement campaign this closes), AUDIO_OBJECT_TRACKING_DESIGN.md (sustained
material stays the tracker's job), KICK_SWEEP_EVENT_DESIGN.md (live ridge
detector — future side-input, Deferred).

## 1. Audit — what exists (verified 2026-07-18, this session)

| Piece | Where | State |
|---|---|---|
| DSP onset front-end | `tools/audio_analysis/manifold_audio/stage1_dsp_detection.py` (`detect_onsets`) | Raw-master any-onset recall 934/934, median err 2–9ms (5 liveshow songs); BUG-241 fixed + threshold 0.075 tuned same day |
| Labeler (to be replaced) | same file, `_label_clusters*` | The wall: snare 0.00 on 3/5 show songs; fitted profiles regress off-domain |
| Liveshow dense truth | `eval/liveshow_labels/` + `sweep_p4.DENSE_IN_WINDOW` | 1,771 labels (kick 396 · snare 707 · hat 545 · synth 48 · bass_sustained 48 · vocal 27); heldout pair `liveshow_stagnate`/`liveshow_basalt` NEVER touched in dev |
| E-GMD | `eval/data/egmd` via `eval/fetch/egmd.py` | 63 perf fetched (59 dev / 4 heldout), CC-BY 4.0 verified at fetch; Range fetcher can pull more |
| Kick anchors | `tests/fixtures/audio_labels/*.csv` | 73 hand-verified kicks, 5 real tracks |
| Self-render pipeline | `eval/fetch/self_render.py` | Exact-truth synthetic; the compositing precedent for D6 |
| Eval harness / exam | `eval/bakeoff_b1.py`, `eval/sweep_p4.py`, scoreboard | Per-class F1, DENSE_IN_WINDOW windowing, heldout discipline — the classifier plugs in as a labeling arm, harness unchanged |
| Shared data store | `eval/paths.py` `DATA_ROOT` | Worktree-safe, no re-downloads |
| Torch runtime | `tools/audio_analysis/BundledRuntime` | torch 2.8.0 + MPS — training runs locally (M4 Max, 36GB, measured) |
| Live machinery (Rust) | `crates/manifold-audio` (`analysis.rs`: kick ridges, pitch/presence tracker; `manifold-spectral` SR-invariant grid, BUG-052) | NOT used offline today; P4 consumes the spectral grid; ridge/tracker side-inputs Deferred |

**Licensing constraints (load-bearing):** ADTOF + madmom models are CC BY-NC-SA —
their OUTPUTS may never become training labels (license laundering). Slakh2100 is
NC-SA and MUSDB18 research-only — both BANNED from training data. Allowed: E-GMD
(CC-BY), Peter's own masters/stems/samples, self-composited renders. BUG-069 is
the tracker for the commercialization gate this design ultimately serves.

## 2. Decisions

- **D1 — Raw single-track masters are the target domain; demucs appears nowhere
  in any product path.** Peter's call (quoted above). Measured basis: masters
  beat demucs stems for event recall (demucs LOST events: 0.79/0.73 recall on two
  songs vs 1.00 raw). Rejected: demucs-stem pipeline — extra dependency, Python,
  and measurably worse detection. Demucs survives only as a dev-side data tool.
- **D2 — Two-stage: existing DSP front-end detects, classifier names.** The
  front-end is proven (audit row 1) and stays untouched. Rejected: end-to-end
  learned transcription (ADTOF's shape) — needs poisoned-license data scale and
  re-solves a solved problem.
- **D3 — Class vocabulary: `kick, snare, hat, perc, synth, vocal, other` — train
  fine, report coarse.** `other` is load-bearing: on masters, a third to half of
  onsets are non-drum content, so the classifier doubles as the precision filter.
  Output-side merges (e.g. Peter's kick/drum/hat) are free and governed by the
  per-track confusion matrix; never train coarse (fine labels exist in the data;
  merging at output preserves the option). Sustained bodies (pads, roars, held
  vocals) are REGIONS, out of scope — the classifier names their onset (likely
  `synth`/`other`), the tracker owns their body.
- **D4 — Input: log-mel patch around the onset + DSP side-features.** Defaults:
  64 mel bands, 20Hz–16kHz, ~100ms span (≈10ms pre-onset + ≈90ms post), hop
  ≈6ms → 64×16 patch, plus the front-end's per-band flux/ratio scalars at the
  onset (free features, already computed). These are P2 knobs with committed
  RANGES: span 60–160ms, mels 48–96. Trigger to escalate: any knob wanting to
  leave its range.
- **D5 — Model: small 2D CNN, ≤500k params, <1ms single-hit CPU inference.**
  3–4 conv blocks → global pool → dense head. Rejected: transformers (no benefit
  at this scale), waveform-domain nets (spectrogram CNN is the robust default for
  short-sound classification). No in-repo precedent exists — this is the repo's
  first trained model; the inference precedent is set HERE (D9), deliberately.
- **D6 — Training data: real-first dial.** Priority order: (1) liveshow dev
  labels (in-window patches only — outside windows truth is undefined), (2)
  E-GMD hits composited over Peter-owned/self-made backing at randomized levels,
  (3) manifold_own kick anchors, (4) composited masters from Peter-owned samples
  and stems through a mastering chain (compression/limiting/EQ) — share of (4)
  is a per-run logged dial starting LOW; raise it only if dev numbers say
  underfitting. Augmentation: random EQ, gain, limiting, ±10ms onset jitter,
  polarity. **Forbidden by name: ADTOF/madmom outputs as labels; Slakh/MUSDB
  audio in any training set.**
- **D7 — Trainer is Python/PyTorch, dev-only, never ships.** Lives in
  `tools/audio_analysis/train/`. Rejected: Rust training (burn/candle) — kills
  iteration speed for zero product benefit; revive trigger = in-app retraining
  as a product feature.
- **D8 — The existing harness is the exam; the ship bar is mechanical.** Dev
  iteration reads dev slices only. A ship candidate reads heldout ONCE
  (liveshow_stagnate + liveshow_basalt dense-in-window + E-GMD heldout): ships if
  per-class F1 ≥ (ADTOF's same-slice F1 − 0.05) for kick/snare/hat, and `other`
  false-negative rate on drums <10% (drums misfiled as `other` kill triggers).
  Classes failing the bar get recorded per class, same as the bake-off — mixed
  outcomes ship mixed.
- **D9 — Inference is pure Rust in `manifold-audio`, proven by value parity.**
  No FFI, no torch, no Python at runtime. Weights artifact: versioned file,
  JSON header (`{version, arch, mels, frames, hop_ms, classes, means, stds}`) +
  little-endian f32 blobs. Parity: Python exports N fixture patches + logits;
  the Rust test asserts max-abs-diff ≤ 1e-4 (the GPU value-parity suite is the
  house pattern). Mel extraction reuses `manifold-spectral`'s SR-invariant grid.

**Consequences, stated honestly:** D1 means finished/other-artist tracks with
buried drums are the hard case and may underperform ADTOF for a while — the dial
(D6) and rounds (P3) exist for exactly that gap. D3's `other` class makes class
imbalance severe (perc n≈small, vocal n=27); P2 must class-balance sampling and
report per-class support, and thin classes may ship as `perc`-merged (recorded,
not silent). D9's hand-rolled inference is ~200 lines of unglamorous math whose
only defense is the parity test — treat that test as load-bearing, not optional.

## 3. Seams (committed)

- **Weights file:** `assets/models/audio_event_classifier_v1.aec` (format per
  D9). Loaded read-only; missing file = feature absent, loudly logged — never a
  silent fallback to the signature labeler.
- **Rust:** `manifold-audio`: `pub struct EventClassifier` with
  `pub fn load(path: &Path) -> Result<Self>` and
  `pub fn classify(&self, patch: &MelPatch) -> ClassScores` (`ClassScores` =
  fixed array over the D3 vocabulary + `argmax()` helper). `MelPatch` built by a
  new `manifold-spectral` helper from the existing spectrogram config. No new
  threads, no shared state; callers own scheduling.
- **Python:** `tools/audio_analysis/train/` — `dataset.py` (patch extraction +
  license allowlist), `compose.py` (D6 compositing), `train.py`, `export.py`
  (weights + parity fixtures). Eval side: `stage1_dsp_detection.detect_drums_stage1`
  gains an optional classifier-labeling mode behind an explicit argument; the
  scoreboard's Stage-1 arm runs it when the weights file exists in DATA_ROOT.

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| No banned-license audio/labels in training | `dataset.py` builds ONLY from an allowlist manifest (`train/sources.toml`); `eval/tests/test_train_license_allowlist.py` asserts every manifest entry names its license ∈ {CC-BY, CC0, ours} and rejects paths matching `slakh|musdb|adtof|madmom` |
| Heldout consumed only at ship-candidate | heldout loaders stay in `heldout_acceptance_p4.py`-style separate module; `rg`-gate in P2/P3 briefs: zero references to `stagnate|basalt|heldout` in `train/` |
| Rust/Python parity ≤ 1e-4 | `manifold-audio` test `classifier_parity` over exported fixtures (P4 deliverable) |
| Inference stays allocation-free after load | `EventClassifier::classify` takes `&self`, returns by value; reviewed at P4 gate. Enforcement: none beyond review — honest gap, acceptable for a non-per-frame path (classification is per-event) |

## 5. Phasing

Worktree: one workstream slot (`lane/audio-event-classifier`). Executor: Sonnet,
medium effort, per phase brief. Gates run by the orchestrating session. Python
runs use the bundled runtime (`tools/audio_analysis/BundledRuntime/macOS/python/bin/python3.12`,
PYTHONPATH = the worktree's `tools/audio_analysis`). All data writes go to
`DATA_ROOT` (main-checkout store), code stays in the worktree.

### P1 — Dataset pipeline (1 session)
**Entry:** audit anchors re-verified (`detect_onsets` exists; liveshow labels load;
E-GMD present in DATA_ROOT). **Read-back:** this doc §2 D3/D4/D6 + §4 + the
DENSE_IN_WINDOW machinery in `sweep_p4.py`.
**Deliverables:** `train/sources.toml` + `dataset.py` extracting labeled mel
patches (D4 defaults) from: liveshow dev in-window events, E-GMD dev hits,
manifold_own kicks, self_render; `other`-class patches mined from liveshow
master onsets that fall INSIDE a class's window but match no truth within 50ms
(they are real detections of non-labeled content) — plus synth/bass/vocal truth
as their own classes; the license test (§4); per-class count report to stdout.
**Gate:** license test green; extraction unit tests green (patch shape, jitter,
window discipline); printed class support table (expect kick ≈400+, snare ≈700+,
hat ≈550+, other ≈1000+, vocal thin). **Demo:** none — L1.
**Forbidden:** touching heldout; any Slakh/MUSDB path; demucs; class-collapsing
thin classes silently (report, don't merge).

### P2 — Trainer + first number (1 session) → CHECKPOINT Peter
**Entry:** P1 gate output. **Read-back:** D5/D7/D8 + P1's support table.
**Deliverables:** `train.py` (seeded, class-balanced, MPS; cross-entropy;
cosine schedule; ~100 epochs), `export.py` (weights v1 + 32 parity fixtures),
classifier-labeling mode in `stage1_dsp_detection.py` (explicit arg, off by
default), scoreboard run on DEV slices with the classifier arm.
**Gate:** training completes reproducibly (two runs, same seed, same final loss
±1%); dev scoreboard JSON committed with per-class classifier numbers beside
ADTOF's; per-class confusion matrix printed. NO bar this phase — the number is
the deliverable. **Demo:** the scoreboard JSON — L1. **Checkpoint:** Peter reads
the first number; P3 proceeds on his continue.
**Forbidden:** heldout; tuning against liveshow heldout songs "just to check";
training-data edits beyond P1's pipeline (that's P3's job).

### P3 — Judged data-recipe rounds (≤3 rounds, 1 session each) → ship call
**Entry:** P2 checkpoint = continue. **Read-back:** D6 dial rules + D8 bar +
prior round's verdict.
Per round: Sonnet adjusts ONE thing (composited-master share, augmentation set,
class weights, patch span within D4 ranges), retrains, reports dev per-class
delta + confusion matrix; orchestrator judges (accept/revert), same protocol as
the P4 threshold sweeps. After the final accepted round: heldout read per D8,
once. **Gate per round:** dev scoreboard + explicit statement of the one change.
**Final gate:** heldout numbers vs the D8 bar, per class. **Demo:** scoreboard —
L1. **Checkpoint:** Peter's ship/park call per class.
**Forbidden:** multiple simultaneous changes; heldout before the final read;
raising the composited share past 50% without an underfitting finding to cite.

### P4 — Rust inference + parity (1 session)
**Entry:** shipped weights from P3 (or P2 baseline if Peter ships early).
**Read-back:** D9 + §3 seams + `manifold-spectral` grid (BUG-052 notes).
**Deliverables:** `MelPatch` builder in `manifold-spectral`; `EventClassifier`
in `manifold-audio`; weights loader (versioned, forward-compatible header);
`classifier_parity` test over P2's exported fixtures; timing measurement
printed by the test (expect ≪1ms/hit).
**Gate:** parity ≤1e-4 on all fixtures; focused `cargo nextest run -p
manifold-audio -p manifold-spectral`; clippy scoped; the timing number reported.
**Demo:** none — L1 (product wiring is P5). **Forbidden:** FFI, torch/ort/onnx
deps in the workspace, Python invocation from Rust, silent fallback when the
weights file is absent.

### P5 — Offline analysis integration (1 session)
**Entry:** P4 green. **Read-back:** §3 seams; the offline analyzer's current
labeling path. **Deliverables:** the offline import-analysis path can use
`EventClassifier` (explicit setting, default ON when weights present, absence
loudly logged); one end-to-end run on a manifold_own master committed as a
JSON artifact. **Gate:** artifact diffed against the Python classifier arm's
output on the same file (event-level match ≥99% after D9 parity). **Demo:** the
JSON artifact — L2. Live-rig integration is Deferred, not this phase.

## 6. Decided — do not reopen
1. Masters-first; demucs banned from product paths (D1).
2. Detect-then-classify; the DSP front-end is not retrained or replaced (D2).
3. Fine class vocabulary incl. `other`; coarse only at output (D3).
4. Python trains, Rust ships, parity-tested (D7/D9).
5. The bake-off harness is the exam; heldout spends once per ship candidate (D8).
6. ADTOF/madmom outputs never become labels; Slakh/MUSDB never enter training (D6).

## 7. Deferred
- **Live-rig integration** (per-drum live triggers, classifier veto for kick
  ridge false-fires, tracker side-inputs to the classifier). Trigger: P5 shipped
  + Peter's live feel-pass decision. Needs its own short design (thread
  residency + latency budget: the ~30–100ms naming lag and its predictive
  workaround are the open UX questions).
- **In-app retraining / per-track calibration.** Trigger: product demand; would
  revive Rust-native training (D7's rejected path).
- **Beat This Rust port + weights-license verification.** Separate workstream;
  unrelated to this model. Trigger: offline pipeline de-Pythoning after this
  ships.
- **Roar/sustained-texture handling.** The tracker's region machinery, not this
  classifier. Trigger: AUDIO_OBJECT_TRACKING ingest work resuming.
- **Expanded E-GMD fetch** (more than 63 performances). Trigger: P3 underfitting
  finding naming acoustic-drum data as the gap.
- **`vocal` class (added at P1, 2026-07-18).** P1 found ALL 27 vocal truth
  labels live in the two heldout songs — zero permitted vocal training data
  exists (heldout discipline honored). v1 trains 6 classes (D3 minus vocal);
  vocal onsets fall to `synth`/`other` on their own merits. Trigger to revive:
  dev-side vocal labels (Peter labels vocal moments in dev songs, or a
  Peter-owned acapella source enters sources.toml). Surfaced at the P2
  checkpoint for Peter's confirmation.
