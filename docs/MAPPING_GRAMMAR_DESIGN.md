# Mapping Grammar — the taste layer for audio-reactive mappings

**Status:** DIRECTION captured 2026-07-09 · Fable + Peter discussion · not a full design, not built. **Card corpus DRAFTED 2026-07-10** (Peter authorized bulk drafting, Fable authored): [MAPPING_CARDS.md](MAPPING_CARDS.md) — house rules + cards for every shipped preset and draft piece, all `draft-unjudged`. Peter's judging against real music converts drafts to authoritative cards; his rejections feed the veto list here.
**Companion docs:** [AUTO_POPULATE_DESIGN.md](AUTO_POPULATE_DESIGN.md) (consumes cards), [VISUAL_PIECES_GRAPH_DRAFTS.md](VISUAL_PIECES_GRAPH_DRAFTS.md) (the 24 drafts cards will be written for).

## The claim

Whether audio-reactive visuals read as *musical* has little to do with detection
accuracy and everything to do with which correspondences you choose. The plumbing
(modulation matrix, kick/sweep events, param step actions) is shipped; this doc is
the layer that says which wirings a musician's eye accepts as playing and which it
dismisses as a screensaver.

## The rules (settled in discussion, 2026-07-09)

1. **Events vs envelopes.** Discrete audio events (kick, transient, phrase boundary)
   drive discrete visual *changes* — a step, a spawn, a cut, a morph notch (Step /
   Random param actions). Continuous signals (bass energy, brightness) drive
   continuous params (Continuous mode). The screensaver anti-pattern is an event
   mapped through an envelope onto a continuous param (kick → size swell):
   everything breathes, nothing happens.
2. **Structural weight matching.** Music has sizes — sub-beat texture, beat, bar,
   8/16-bar phrase, section — and visual changes have sizes: grain/shimmer (texture),
   pulse/step (beat), motion/rotation (bar), camera move (phrase), palette/scene
   (section). An audio level drives a visual dimension of the same weight.
   Every-kick-changes-the-palette = chaos; drop-only-changes-brightness = limp.
3. **Envelope shape is groove.** Hits are fast-attack slow-decay; symmetric smoothing
   reads as underwater lag. Attack/release tuning per binding is the visual pocket —
   always retuned by eye against real music.
4. **Anticipation, not just reaction.** Builds/risers should visibly build (converge,
   compress, brighten) and resolve at the drop. A follower can only react; tension
   needs the phrase/section structure.
5. **Counterpoint and restraint.** One feature fanned across five params is one loud
   voice (drop move, not a default). Independent features on independent dimensions
   = counterpoint. A binding that only engages in one section is arrangement; if
   everything reacts all the time, nothing reads.

## The mapping card

Per-preset, machine-readable: one row per performable param — name, tier
(texture/beat/bar/phrase/section), audio feature, mode (Continuous/Step/Random),
envelope shape, optional engage-condition. Cards are (a) the review surface (Peter
reviews cards, not graph JSON), (b) the data auto-populate rolls within, (c) where
his vetoes accumulate (e.g. "kick never touches hue" after two rejections — the doc
grows a veto list from real accepts/rejects).

**Open:** whether "engage only during section X" has a mechanism today (scenes?) or
is a small feature — check before any card claims it.

## Owed

- ~~First worked card~~ superseded 2026-07-10: full draft corpus in MAPPING_CARDS.md. Now owed: **Peter's first judging pass at the rig** — wire one shipped card (Tesseract or Fluid Sim 2D recommended: kick + band sends cover their signature rows today) and judge against real music. Text review validates structure only, never taste (Peter, 2026-07-10).
- The veto list — empty until real cards get judged. MAPPING_CARDS.md §Open questions holds the first four candidates.
- H13 (engage = clip placement) — structural confirm from Peter.
