# Auto-Populate — section detection + grammar-legal visual rolls

**Status:** DIRECTION captured 2026-07-09 · Fable + Peter discussion · not designed to STANDARD, not built. Real design doc(s) to be authored (Opus) inheriting this whole file. Gated on the BUG-069 license rework (madmom/ADTOF out).
**Companion docs:** [MAPPING_GRAMMAR_DESIGN.md](MAPPING_GRAMMAR_DESIGN.md) (the legal space rolls sample from), [TIMELINE_INGEST_DESIGN.md](TIMELINE_INGEST_DESIGN.md) (this is its compose stage), [AUDIO_ANALYSIS_ACCURACY_DESIGN.md](AUDIO_ANALYSIS_ACCURACY_DESIGN.md) (detection stack + 2026-07-09 addendum).

## Concept

Drop a track in → sections detected (calm/build/drop/etc.) → generators, effects,
and param wirings pre-assigned per section as a "roll," sampled from the mapping
cards, not uniform-random. Re-roll to explore combos fast. A pure randomiser rolls
garbage; rolling within the grammar is chords-in-key vs random notes.

## Roll UX (Peter-settled, 2026-07-09)

- Rolls land **on the timeline** as real layers/clips — audition = press play; keep
  = keep editing. No preview dialog.
- **Section locks**: lock what you like, re-roll the rest. Re-roll has **depths**
  mirroring the grammar tiers: whole section / keep generator re-roll effects+wiring
  / palette only.
- **Track-level coherence is itself a rollable, lockable level** (palette + generator
  family holding across sections). Lock it = coherent mode; unlock = independent
  per-section rolls. Peter: support both — and this makes both one mechanism, no
  mode switch.
- **Seeds + history**: every roll reproducible by seed; a small per-section history
  strip to flip back ("the previous one was better" is the common judgment).
- **Curated pools**: rolls draw only from presets with mapping cards, weighted by
  section energy and by opt-in **genre tags** (user-assigned per preset pack:
  "these are DnB visuals" etc.). Tags weight pools, never hard-filter.
- **Provenance invariant (Peter, 2026-07-09): re-roll only replaces roll-owned
  clips.** Every clip knows roll-generated vs user-authored; any user edit (trim,
  move, retime) promotes the clip to user-owned and re-rolls flow around it.
  Manual timings are never clobbered. Editing is an implicit lock.

## Section detector (output contract + shape)

- Output: bar-snapped boundaries · class per section from a small audible set
  {intro/outro, breakdown, build, drop, groove} · **vocal is a flag, not a class**
  (a drop with vocals is still a drop) · continuous descriptors (bass presence,
  onset density, brightness) for pool weighting · confidence.
- Shape: classic license-clean DSP — novelty curve (spectral flux + multi-band
  loudness), self-similarity confirmation, min section length 4–8 bars,
  classification **relative to the track** (a drop is loud *for this track*).
  Builds = rising energy/brightness ramp terminating at a boundary (the sweep
  detector concept, offline). No models needed for v1.
- Corrections are expected: boundaries draggable, labels editable on the timeline.
  A 90%-right draft with cheap correction beats a 99% detector shipped later.
- Dependencies: a license-clean beat/downbeat grid (madmom replacement; fixed-tempo
  EDM is the easy case). Vocal flag: free+exact via stems when present ("is the
  vocal stem active"); full-mix vocal detection needs a model — separately vetted,
  never holds the rest hostage.

## Genre tagging (audio side)

BPM + rhythm-feel heuristic first (DnB ~172, house ~124 — covers EDM subgenres with
zero models). Model candidate: **PANNs** — weights CC BY 4.0 (Zenodo 3987831,
verified 2026-07-09; in-app credit line required). Fallback: YAMNet (Apache 2.0).
AudioSet-family training-data provenance is an industry gray zone — accepted risk,
logged in COMMERCIALIZATION_DESIGN.

## Live mode — one detector, two clock domains

The same detector runs over a trailing window of live input (manifold-audio ring
buffer + off-RT analysis worker — infrastructure exists). Nothing answers
instantly; confidence accumulates. BPM/downbeat lock first → full-mix live audio
becomes beat-aligned (step actions and phrase-tier moves work with no stems, no
pre-analysis). Genre stabilizes ~30s → re-weights roll pools live. Confidence
collapse at a DJ transition = a detectable **track-change event**, wireable.
Stage payoff: prepared shows stop being the only mode — someone else plays,
MANIFOLD listens, converges, performs from the same grammar.

## Owed / not decided

- Everything at DESIGN_DOC_STANDARD level — this is direction, not a contract.
- First worked mapping card (future discussion with Peter — see MAPPING_GRAMMAR).
- PANNs vetting pass before commitment (weights license verified; integration and
  training-data risk sign-off not done).
