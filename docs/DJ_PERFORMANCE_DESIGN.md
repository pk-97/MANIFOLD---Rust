# DJ Performance Mode — Library, Crate Compile, Two-Deck Orchestration over Ableton

**Status: APPROVED design, not built · 2026-07-03 · Fable**
**Prerequisites: ABLETON_SHOW_SYNC (.als parser + per-track scores);
PERFORM_SURFACE P1 (widget substrate); the AbletonOSC bridge (shipped);
MEDIA_BACKEND P1 (audio decode for cue preview). Post-v1.0 candidate — Peter
ranks (DESIGN_BUILD_ORDER).**
**Companion: `PRO_DJ_LINK_DESIGN.md` (the other half of "external musical
timelines" — DJing on CDJs instead of from Ableton).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before any
phase. Hardening level: conformance — re-derive all anchors at implementation.**

Peter's ask (2026-07-03): DJ with Ableton the way Rekordbox DJs work a booth —
browse a pre-analysed library of his own music (BPM, key, stems, instruments),
cue and align tracks like a DJ set, while keeping the live-performance side.
"Doing this inside Ableton is pretty much impossible" — session-view DJing
exists but is "really ugly and a surface you need to memorise for every show
and manually build out yourself." MANIFOLD becomes the intelligent orchestrator
surface; Ableton stays the audio engine.

---

## 1. Decisions (converged 2026-07-03, do not reopen)

- **D1 — Instruments stay live. Never flatten MIDI to stems.** (Peter's explicit
  correction.) A "track" in the library is a performable thing — stems + live
  instruments — or the whole point of using Ableton as the engine is lost.
- **D2 — Authoring stays in arrangement view.** Songs are written as normal Live
  projects. Locators name sections; a track-naming convention maps tracks to
  layout slots. No new authoring surface on the Ableton side.
- **D3 — Fixed two-deck compiled layout.** The compile emits a session-view set
  with **Deck A and Deck B, each 8 stem tracks + 8 performance tracks**
  (+ master/cue). Two decks are *forced*, not stylistic: session view plays one
  clip per track, and a blend needs two songs on disjoint tracks. The fixed
  shape bounds everything — compile, OSC map, surface, CPU.
- **D4 — Sections = locators + 8-bar grid.** Locators are named landmarks
  (intro/drop/…); the 8-bar grid is the universal subdivision. One systemic
  layout for every song, every show — learn it once (Peter: "systemic, easy to
  learn").
- **D5 — Master-deck tempo rule.** One Live set = one clock; nothing can ever be
  out of time by construction. Master tempo = the master deck's native BPM,
  always. On master handoff, tempo **glides** to the new BPM over N bars
  (setting; 0 = jump). The incoming song plays warped at master tempo until it
  takes master. No beatmatching exists anywhere (Peter: "no DJ is going to
  beatmatch out of time tracks").
- **D6 — Transitions: cut and blend both ship.** Cut = quantized launch +
  tempo jump. Blend = overlap on the two decks + the D5 glide + crossfade
  (grouped volume ramps over OSC).
- **D7 — Every crate song compiles into both decks** (duplicated clip slots), so
  any song can be cued onto either deck in any order — ordering is a live
  decision, not a prep decision. Performance tracks are **Instrument Racks with
  one chain per song**, chain-switched on song load: instruments stay fully
  playable, 16 instrument tracks total regardless of crate size.
- **D8 — Automation converts, per section, self-contained.** Arrangement track
  automation is sliced at section boundaries and rebased into session **clip
  envelopes** (same breakpoint data in the .als XML — Live's own
  consolidate-to-scene proves the transform). Boundary values are sampled so
  every section starts from its own correct state — out-of-order jumps sound
  right by construction. Cannot convert (no clip to live in): **tempo
  automation** (fights D5 — flagged), **master/return automation** (flagged).
  The compile report lists what carried and what didn't; nothing drops silently.
- **D9 — Crate prep is a first-class workflow.** Pick tonight's songs → compile →
  cost report (instrument RAM/CPU, validation findings). Mid-show loading of an
  uncompiled song is impossible (Live has no API to load content into a running
  set) — and that's fine; it matches how DJs prep crates. Compile **validates,
  never guesses**: >8 stems, unmapped instruments, tempo automation → report at
  prep, not surprise at show.
- **D10 — MANIFOLD launches clip slots (track, slot), never Live scenes** — a
  scene row spans both decks and would fire them together.
- **D11 — Cue preview is MANIFOLD-native.** The library scanner mixes each
  song's stems offline (audio_mixdown precedent in manifold-media) and plays
  preview to the headphone/cue output directly. Live needs no cue bus.
  MIDI-instrument tracks are silent in previews — flagged, acceptable (you know
  your own songs).
- **D12 — Visuals ride the sections.** Each song's ABLETON_SHOW_SYNC score is
  sliced by the same section map; cueing a song cues its visuals. This is the
  differentiator: free-form DJ set with full-show production value, one person.
- **D13 — The compile writes follow actions so a launched song "just plays."**
  Each section clip gets follow action = *next slot* after its section length;
  the last section = stop. Launch section 1 and the song plays through exactly
  like the arrangement with zero intervention. The DJ layer sits *on top* of
  that default: jump = launch a different slot; **hold/loop a section =
  MANIFOLD quantized re-launch of the same slot each cycle** (release = stop
  re-launching and the follow action carries forward). Looping never depends on
  editing follow actions live — the Live API may not expose them at runtime
  (`⚠ VERIFY-AT-IMPL`: LOM/AbletonOSC follow-action access; the re-launch
  pattern is the committed mechanism regardless). Surface shows an end-of-deck
  warning as the last section approaches.

## 2. The pieces

| Piece | What it is |
|---|---|
| Library scanner | Walks a projects folder; per song: BPM, locators/sections, track inventory (stems, instruments), key (chromagram over the stem mixdown), preview render. Cached, re-scanned on change. |
| Crate compiler | .als **writer** (gzipped XML — the load-bearing new capability) emitting the D3 layout: clips sliced per D4, automation per D8, racks/chains per D7. Emits the compile report. |
| Orchestrator surface | Perform-surface widget set: library browser (BPM/key/sections/instruments), two deck views (current + cued song, section position, next-launch), cue/pre-listen, quantized launch, tempo-glide control, crossfader. |
| OSC conduction | Clip-slot launches, chain selection, tempo glide, volume ramps — all AbletonOSC, existing bridge. |

`⚠ VERIFY-AT-IMPL`: AbletonOSC surface for clip-slot launch / tempo set / rack
chain select — confirm coverage before P3 (`assets/abletonosc-patches/` may need
extending; precedent exists for patching it).

## 3. Phasing

Forbidden, all phases: flattening instruments (D1) · silent compile drops (D8/D9)
· launching Live scenes (D10) · a second tempo authority (D5) · per-frame
allocation in the surface widgets (hot-path discipline).

- **P1 — Library scanner + browser.** Read-only: .als parse (rides the
  show-sync parser), section map, stem mixdown + key detection, preview
  playback to cue output. Gate: scanner over Peter's real projects folder
  produces correct BPM/sections/inventory for known songs (fixture assertions);
  browser widget renders the library headless-PNG.
- **P2 — Crate compiler.** The .als writer + layout + slicing + automation
  transform + racks. Gate: **round-trip fixture** — compile a two-song crate,
  Live opens it clean (manual gate, Peter), re-parse with our own parser and
  assert layout/slice/envelope/follow-action structure (D13); automation
  boundary values value-level-asserted against the arrangement source. Manual
  gate: launch section 1 in Live, the song plays through unattended. Compile
  report golden test. Negative gate: a fixture with tempo automation + 9 stems
  produces flags, not output mutations.
- **P3 — Orchestrator surface.** Deck views, cue, quantized clip-slot launches,
  master-deck tempo glide, crossfade. Gate: against a live Ableton instance —
  scripted launch sequence lands on bar boundaries (OSC echo assertions);
  headless-PNG for the surface.
- **P4 — Visuals coupling + polish.** Section-sliced scores follow deck
  launches; blend behavior on the visual side (master deck owns the score;
  on-air crossfade). Gate: demo crate + filmed run-through — this phase's
  output is also a release trailer (BUSINESS_PLAN §7).

## 4. Deferred (with triggers)

- **Live-set streaming tricks** (loading songs mid-show): blocked on Ableton's
  API forever until they ship one; revisit only if Live adds set-merge/load.
- **Stem separation of finished mixes** (DJing songs that were never Live
  projects): ML lane (demucs-class) — own design if Peter wants foreign tracks
  in the library.
- **Four decks**: when two demonstrably constrain a real set, not before —
  doubles the fixed track cost.
- **Key-shift on load** (harmonic mixing beyond display): Live warp/transpose
  per clip is possible; add when harmonic mixing proves limiting live.
