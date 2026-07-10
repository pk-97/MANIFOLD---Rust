# Audio Object Ingest — the tracker applied offline, for timeline clip generation

**Status:** APPROVED direction, conformance treatment (STANDARD §9 — deeper in the
build order; re-derive inventories at execution) · 2026-07-06 · Fable
**Prerequisites:** AUDIO_OBJECT_TRACKING_DESIGN.md P0–P2 (the core this reuses), plus
**P1 here is blocked on Peter** (labeled clips — see Phasing).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The audio-clip → timeline-clip auto-generation currently rates, in Peter's words,
*"a 5/10 at the moment"* (2026-07-06). This doc governs how the object tracker built
in [AUDIO_OBJECT_TRACKING_DESIGN.md](AUDIO_OBJECT_TRACKING_DESIGN.md) is applied to
that pipeline — and, first, how the 5/10 becomes a measurement so we replace the part
that is actually failing.

**The relation contract (binding, mirrored in the realtime doc §8): one core, two
regimes.** The salience function and ridge tracker have exactly one implementation, in
`manifold-audio`. Offline runs it non-causally: the same causal forward pass, then
passes the live path cannot afford — backward smoothing, whole-clip segment extraction
(tracker birth/death → note/gesture events with their pitch curves). The offline side
may ADD passes over the causal output; it may never fork the salience math or maintain
a second tracker. Improvements graded in the harness make both regimes better; that is
the point.

---

## 1. Audit — what exists (verified 2026-07-06; re-derive at execution)

| Piece | Where | State |
|---|---|---|
| Detection backend (external process) | `tools/audio_analysis/manifold_audio/` — `analyzer.py` orchestrates | Python, bundled runtime (`percussion_backend.rs` resolves it). Demucs 4-stem separation → per-stem detection. |
| Drum transcription | `adtof_detection.py` (ADTOF, ML, per-class thresholds) | Ships. NOT suspected weak — kicks/snares/hats are events, the model's home turf. Measure before touching (P1). |
| Melodic transcription | `basic_pitch_detection.py` (Spotify Basic Pitch; bass/synth/pad "migrated to Basic Pitch" per `gestures.py` header) | Ships. **Prime suspect** for the 5/10 on Peter's material: it transcribes *notes*; a supersaw dive is a continuous glide (stair-step fragments), growls are inharmonic (fragmentation/octave errors). A suspicion, not a finding — P1 measures it. |
| Vocal gestures | `gestures.py` (madmom onsets on the vocal stem) | Ships. |
| Rust ingest side | `percussion_parser.rs` / `percussion_planner.rs` / `percussion_orchestrator.rs` (manifold-playback), `audio_clip_detection.rs` (manifold-core: per-clip `DetectionConfig`, cached events, re-plan without re-analysis) | Ships. The orchestrator already receives `stemPaths` from the pipeline JSON — the stems are on disk and addressable from Rust (§8 detect-and-group). |
| Event model | `percussion_analysis.rs` (`PercussionEvent { trigger_type, time_seconds, confidence, duration_seconds }`) | Ships. Carries no curve/gesture payload — extending it is P3 schema work, `⚠ VERIFY-AT-IMPL: re-read the struct + its serde before extending`. |
| Offline decode + analysis core | `audio_decoder.rs` (symphonia, any format → PCM), `StreamingSendAnalyzer` + harness | Ships. The harness already IS the offline runner shape: decode → causal pass → artifacts. |

## 2. Decisions

**D1 — Measure before replacing (the 5/10 becomes per-instrument numbers).** Peter
labels 2–3 clips he knows (which hits, which sustained regions, where boundaries
belong); a scoring script computes precision/recall per trigger type for the current
backend. Only components that measure weak get replaced. **Rejected: "Basic Pitch is
obviously the problem, start rebuilding"** — plausible, unmeasured; if ADTOF is also
weak on his electronic drums that's a threshold/profile fix discovered by the same
measurement, and if conflict-resolution or Rust-side planning (quantize, sensitivity
mapping) eats good events, replacing the detector fixes nothing.

**D2 — Offline = causal pass + backward smoothing + segmentation, in Rust.** The
non-causal mode runs the exact causal tracker, records its per-hop trace, then: a
backward pass reconciles takeovers/dropouts with hindsight (a challenger that won was
the object all along — rewrite the trace back to its true start), and a segmenter cuts
the trace at acquisitions/releases into events: `{start, duration, pitch curve,
presence curve, mean presence as confidence}`. Ridge birth/death gives sustained
material honest boundaries — the thing note-transcription structurally can't:
the clip carries the *gesture* (its actual pitch/brightness contour), not a MIDI
number. **Rejected: Viterbi over the whole clip as a separate offline tracker** —
better in theory, forks the core in practice; the two-regime contract wins until the
shared tracker is proven insufficient on the measured eval set (that finding is the
revival trigger).

**D3 — Integration point: in-process Rust analysis of the demucs stems.** Python keeps
what it is good at and what measured fine (D1): separation (demucs), drums (ADTOF, if
it measures well), vocals. The object segmenter runs Rust-side in the app, on the stem
files the pipeline already reports (`stemPaths`), on a background thread exactly like
the orchestrator's existing post-processing — no new process, no new IPC, no Python
fork of the math. Its events merge into the same cached `PercussionAnalysisData` so
`DetectionConfig` re-planning (sensitivity, quantize, routing) works unchanged.
**Rejected: porting salience/tracking to Python** — forks the core, violates the
relation contract. **Rejected: rewriting the whole Python pipeline in Rust** — demucs
and ADTOF are ML models with Python ecosystems; the boundary stays where the models
are.

**D1a — SOTA survey note (web-surveyed 2026-07-06).** Basic Pitch remains the 2025
literature's lightweight baseline; MT3-family transformers emit quantized note tokens
(structurally wrong for glides — skip); **Timbre-Trap (Sony, ICASSP 2024) joins the
P1/P3 comparison list** (frame-level salience output, Basic-Pitch-class cost). No
published work benchmarks transcription on growl/glide material — P1's labeled-clip
measurement is doing work the literature hasn't, which is one more reason it precedes
any replacement.

**D4 — Basic Pitch is replaced per-instrument, by measurement, not wholesale.** If
P1 confirms bass/synth/pad as the weak rows, the object segmenter takes those stems
and Basic Pitch is dropped from them; any row that measures fine keeps its current
detector. The pipeline JSON contract is additive during transition — but the transition
ends inside the same phase that starts it (no permanent dual path; the losing detector
for a row is removed from that row's flow in the same change,
per no-transitional-states).
**[AMENDED 2026-07-10 (F10) — the number to beat is the *post-precision-pass* Basic
Pitch, not raw Basic Pitch.** `AUDIO_ANALYSIS_ACCURACY_DESIGN.md` P4 tunes Basic Pitch's
post-processing on these same synth/pad stems. Replacing a detector before that pass runs
would replace one the pass might fix. So D4's per-instrument replacement is **sequenced
after ANALYSIS_ACCURACY P3 (baseline) + P4 (Basic Pitch precision pass)**: the object
segmenter must beat the post-P4 Basic Pitch F₁ on those stems, measured in the *same*
harness (D5), or it does not replace it.]**

**D5 — One measurement harness: consume `AUDIO_ANALYSIS_ACCURACY`'s `eval/`, don't build
a second. [NEW 2026-07-10 (F10).]** `AUDIO_ANALYSIS_ACCURACY_DESIGN.md` (2 days newer than
this doc) builds the real measurement substrate — `tools/audio_analysis/eval/` with frozen
per-instrument P/R/F₁ metrics (its D10), fixture tiers, and an untouchable held-out split
(its D9). This doc's P1 originally specified "a scoring script (Python, beside the
pipeline)"; that would be a second harness contesting the same Basic-Pitch/synth-stem
surface with an opposite fixture doctrine. Reconciled: **there is one harness (`eval/`),
and this doc consumes it.** The two uses don't conflict because they are different
questions — ANALYSIS_ACCURACY runs a *tuning-and-baseline* loop (tuned `dev` sets, held-out
`test`); this doc's P1 runs a *one-shot component measurement* ("does Basic Pitch measure
weak on Peter's material?") using `eval/`'s `metrics.py` with Peter's labeled clips as a
**measurement-only fixture tier** — never a tuning set (consistent with ANALYSIS_ACCURACY
Deferred #5, "Peter material is never the tuned sets"). **Chord-emitter seam:**
ANALYSIS_ACCURACY §4.3 clusters Basic Pitch notes on the synth/pad stem into `Chord`
events. If D4 drops Basic Pitch from those stems, §4.3's chord clustering must re-point to
the object segmenter's output (or the two coexist — segmenter for sustained-object gestures,
§4.3 chords as a separate emitter). Name and preserve this seam in the P3 brief; do not
silently break §4.3.

## 3. Phasing (briefs at conformance level; full briefs written when P0–P2 of the realtime doc land)

**P1 — Measurement.** *Blocked on Peter (named decider): labeled clips.* Deliverables:
Peter's labeled clips added as a measurement-only fixture tier in
`AUDIO_ANALYSIS_ACCURACY`'s `eval/` harness (D5 — **not** a bespoke script), a
per-instrument component-measurement run through `eval/`'s `metrics.py` (tolerance
±30 ms events / ±100 ms boundaries), per-instrument P/R table committed to this doc's
§1. Gate: the table exists and names the weak rows; no new harness, no code changed in the
detectors. *Entry note: if `eval/` has not landed yet (ANALYSIS_ACCURACY P1), this phase
waits on it rather than forking a parallel scorer.*
Demo: the table — L2.

**P2 — Offline mode in the harness.** `mod_harness --segments`: non-causal pass (D2)
over a decoded file or selftest; renders segments as bracketed spans with pitch curves
over the spectrogram + emits JSON. Gates (synthetic, machine-checkable): dive = ONE
segment spanning ≥90% of the glide with a monotone-down curve; kicks = 8 segments (or
0 tonal segments — decide the drum-stem semantics at brief time from P1 data); riser =
0 segments above confidence floor; wobble = 1 segment, flat pitch curve. Demo: the
PNGs — L2.

**P3 — Pipeline integration.** Rust segmenter on the stems the orchestrator already
has (D3), `PercussionEvent` extension for gesture payload (schema + serde + parser
round-trip gate per STANDARD §5), per-instrument replacement per D4, re-run the P1
scoring — **the gate is the measured number moving on the weak rows** (target set at
brief time from the P1 baseline), with held-out clips the builder didn't tune against.
Demo: detect-and-group on a labeled clip, before/after timelines — L2 minimum, L4
(Peter on his own material) the real bar.

## 4. Plausible-wrong turns, forbidden by name

- **You will want to start at P3** because integration is the visible win — no. P1's
  table is what stops us replacing the wrong component (D1).
- **You will want a second tracker "tuned for offline"** — no. Relation contract; the
  offline mode adds passes over the shared core's output.
- **You will want to keep Basic Pitch running in parallel "for comparison"** in
  shipped flows — no. Comparison lives in P1/P3 scoring runs; shipped rows have one
  detector each (D4).
- **You will want to shape the segmenter around the labeled clips** — the P3 gate
  uses held-out material (STANDARD §5 fixture-overfitting rule).
- **You will want to write "a quick scoring script beside the pipeline"** — no. That is
  the second harness F10 killed; measurement runs through `AUDIO_ANALYSIS_ACCURACY`'s
  `eval/` (D5). One harness, one set of frozen metrics.

## 5. Decided — do not reopen
1. One core, two regimes; offline adds passes, never forks (relation contract).
2. Measurement precedes replacement; per-instrument, not wholesale (D1/D4).
3. Integration is in-process Rust on the existing stems; Python keeps separation +
   whatever measures well (D3).
4. One measurement harness — `AUDIO_ANALYSIS_ACCURACY`'s `eval/`, consumed here, never
   forked; replacement is sequenced after that doc's P3/P4 baselines (D5/D4, F10).

## 6. Deferred
- **Wobble-rate/phase and object-texture metadata on ingested clips** — after the
  realtime deferred family ships.
- **Replacing demucs/ADTOF** — no trigger short of P1 measuring them weak.
- **MIDI export of segmented gestures** — nice, out of scope until asked for.
