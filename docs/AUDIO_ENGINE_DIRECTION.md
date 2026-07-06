# MANIFOLD as the Audio Engine — Direction Memo

Staged path toward live shows needing no Ableton: stems + mixer + built-in DSP first, AU instrument hosting decided later with data, full production DAW never.

**Status:** DIRECTION (2026-07-07, Fable) — not a design doc, deliberately does not
satisfy DESIGN_DOC_STANDARD.md. This captures a strategic conversation with Peter so
the framing survives the Fable handoff. A design session (Opus) turns Stage 1 into a
real doc when the time comes. Post-release direction — nothing here touches the ~Aug
authoring/export push.

**The idea (Peter's):** grow MANIFOLD toward standard DAW features — MIDI, VST/AU
hosting, EQs, compressors, saturators — so that a live show needs no Ableton at all:
visuals *and* audio from one process. Ties into the deck A/B DJ workflow
(`DJ_PERFORMANCE_DESIGN.md`).

## 1. The load-bearing split

"Add DAW features" is two projects wearing one name, and the split falls exactly on
DJ-design D1 (Peter's own correction: **instruments stay live, never flattened to
stems**):

1. **Stems live in MANIFOLD** — playback, a real mixer, built-in DSP inserts
   (EQ/comp/saturator), deck A/B crossfade. Tractable; most of the substrate exists.
2. **Live instrument tracks in MANIFOLD** — MIDI clips driving hosted AU/VST
   instruments. The monster. This is the *only* part that forces plugin hosting,
   and D1 is the reason it can't be waved away.

So the real question is never "should MANIFOLD be a DAW" — it's "how much of the set
runs from stems, and is the remainder worth hosting plugins for?" That's answerable
with data (Peter's actual set), not taste.

## 2. What already exists (verified 2026-07-07)

- **Live playback:** `AudioLayerPlayback` (manifold-playback) — kira, one sub-track
  per audio layer, one voice per active clip, varispeed warp, post-fader tap per
  layer implemented as a kira `Effect` feeding the analysis ring buffer.
- **Export parity:** `audio_mixdown.rs` offline mix mirrors live playback exactly.
- **Analysis/modulation:** manifold-audio, pay-per-use analysis, sends/sources UX
  shipped 2026-07-06.
- **MIDI input:** phantom clips (NoteOn/NoteOff). Input only — no MIDI clip
  sequencing to instruments.
- **Beats as the native time model** — the thing DAWs are built around, already owned.

## 3. Stage 1 — mixer + inserts + decks over stems

Per-layer insert chain (EQ, compressor, saturator), master bus, crossfader/deck
surface over MANIFOLD's own audio clips. Kills Ableton for any set that can run
flattened.

- The DSP itself is the easy part: biquads, envelope followers (already exist for
  analysis), waveshapers. Weeks, not months.
- **Open engine question — price before assuming:** kira's `Effect` trait already
  runs custom DSP on its audio thread (the post-fader tap proves the pattern), and
  per-layer sub-tracks already exist. Inserts *may* fit inside kira. The
  alternative is owning the CoreAudio render callback — a third real-time thread
  with the video path's hot-path discipline (no allocation, ~3–5 ms budget). Don't
  pre-commit; the stage-1 design session decides with kira's routing limits
  (bus/send topology, latency reporting, PDC) in hand.
- **Peter's lean (2026-07-07): the endpoint is a custom low-level engine.** The
  precedent is manifold-gpu — wgpu carried the renderer until the requirements
  were fully known, then was deleted; kira plays the same scaffold role here.
  Sequence kira-first to learn the requirements, replace when the seams hurt.
  Note the embryo already exists: `render_export_mix` (audio_mixdown.rs) is a
  hand-rolled mixer (sum, gain, varispeed resample); the custom engine is
  roughly that, in a CoreAudio callback, with lock-free parameter delivery.
- **Swap timing (asked and answered 2026-07-07): not before Stage 1.** Measured
  footprint: kira is confined to manifold-playback, effectively one file
  (`audio_layer_playback.rs`, ~600 lines) — the swap is a phase, not an epic.
  But pre-Stage-1 it buys nothing user-visible while re-owning what kira gives
  free (device hot-plug, sample-rate changes, buffer negotiation, underruns).
  The trigger is the first insert: that's when hand-mirroring live vs export
  starts compounding, and one shared mixer core makes parity true by
  construction. So: engine swap = Stage 1, phase 1.
- Export parity is a standing contract: every insert added live must be mirrored in
  `audio_mixdown.rs` or the what-you-hear-is-what-exports invariant breaks.

## 4. Stage 2 — MIDI clips + AU instrument hosting (decide later, with data)

Only after Stage 1 proves the engine, and only if the set still needs live
instruments that can't stay in a hardware synth or a second machine.

- AU-only, never VST3 first: native macOS APIs, no SDK licensing, plugin UIs live in
  their own NSWindows so the bitmap UI is untouched.
- The underestimated parts, in order of pain: **crash isolation** (a misbehaving
  plugin takes down the show — Ableton has twenty years of hardening; on stage this
  is the argument that Live keeps earning its place), plugin delay compensation,
  plugin state serialization, parameter automation into plugins.

## 5. What this does to the DJ design

The compile abstraction survives with a different target. Today:
arrangement + locators → fixed 2-deck `.als` (Live as engine). Future: same
authoring, same sections → a **MANIFOLD deck project** (stems + section markers),
MANIFOLD as engine. D12 (visuals ride sections) gets *stronger* — one clock, one
process, no bridge. Authoring stays in Ableton either way.

## 6. What probably never happens

Full production DAW — recording, audio editing, VST3, the workflow war with
Ableton/Bitwig. "Author in Ableton, perform in MANIFOLD" is also the positioning
(`positioning-ableton-m4l`) that complements Ableton instead of declaring war on it.
Stage 1 + maybe Stage 2 is "the live show needs no Ableton"; it is not "MANIFOLD is
a DAW."
