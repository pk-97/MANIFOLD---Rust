# Pro DJ Link — Native CDJ/Mixer Sync, Track-Triggered Visual Scores

**Status: APPROVED design, not built · 2026-07-03 · Fable**
**Prerequisites: PERFORM_SURFACE P1 (booth status widgets); the sync-source seam
in manifold-playback (where timecode/Link authorities attach). Post-v1.0
candidate — Peter ranks (DESIGN_BUILD_ORDER). Hardware bring-up needs real
CDJs/mixer (Peter has gig access).**
**Companion: `DJ_PERFORMANCE_DESIGN.md` (DJing from Ableton; this doc is DJing
from the booth). Both are instances of one abstraction: external musical
timelines driving MANIFOLD.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before any
phase. Hardening level: conformance — re-derive all anchors at implementation.**

Peter's ask (2026-07-03): sync with Pioneer/AlphaTheta gear "using the same
backend as ShowKontrol" — network with CDJs and mixers so MANIFOLD becomes a
real-time visuals engine that follows a DJ set. Decision: **the deep door —
native protocol integration**, not the shallow AlphaTheta-bridge-to-Link path
(that bridge remains a free compatibility bonus once Ableton Link ships, but it
is not this design).

---

## 1. Protocol reality (pinned)

Pro DJ Link is Pioneer's unofficial-but-fully-reverse-engineered network
protocol. The public protocol analysis (Deep Symmetry's *dysentery* docs and
the *beat-link* reference implementation; *rekordcrate* in Rust for rekordbox
file formats) is the source of truth — **implement from the published analysis,
never re-derive from packet guessing** (synthesis-drift doctrine applied to
protocols). Capabilities it gives us:

- **Discovery + presence**: devices announce on the network; MANIFOLD joins as
  a *virtual device* to receive full status.
- **Beat packets**: per-beat, with BPM, pitch, and beat-within-bar — the
  tempo/phase authority.
- **Status packets**: play state, sync/master state, master handoff, and (on
  newer players) absolute track position.
- **Mixer state**: on-air flags per channel (which decks the crowd can hear).
- **Track metadata**: rekordbox ID, title/artist/key/BPM, beat grid, and
  **phrase analysis** (intro/verse/chorus song structure) via the metadata
  query protocols / raw rekordbox database access.

Legal/positioning note: unofficial protocol, shipped commercially by others for
years (ShowKontrol precedent). Marketed as "works with Pro DJ Link networks" —
trademark-careful wording, no claimed endorsement.

`⚠ VERIFY-AT-IMPL`: port numbers, packet layouts, per-model quirks (CDJ-2000 →
CDJ-3000 differences, Opus Quad divergences) — all against the current
dysentery/beat-link documentation at implementation time, not this doc.

## 2. Decisions

- **D1 — Deep door (Peter, 2026-07-03).** Full native stack: discovery, virtual
  device, beat/status/mixer ingest, metadata + phrase queries. Not a Link
  bridge wrapper.
- **D2 — Pro DJ Link is a peer sync source.** It attaches at the same seam as
  timecode and Ableton Link in manifold-playback: an external transport
  authority providing tempo + beat/bar phase. **Timecode doctrine applies
  unchanged: the external clock locks the score, never the render** — render
  free-runs at project FPS, score position is conducted.
- **D3 — Master deck conducts.** Tempo/phase authority = the Link network's
  master deck; master handoff on the players is followed automatically. Same
  mental model as DJ_PERFORMANCE D5 — the two features stay symmetric on
  purpose.
- **D4 — Track-triggered scores.** A mapping table (venue/show profile —
  precedent: MULTI_DISPLAY trigger-clip named-cue table): rekordbox track ID →
  MANIFOLD arrangement/scene/score. DJ loads the track, MANIFOLD cues its
  visual score; unmapped tracks fall to a default reactive program and are
  listed, never silently ignored. This is the ShowKontrol+Resolume market,
  native in one tool.
- **D5 — On-air gates the show.** Mixer on-air flags decide *whose* score is
  live; blends crossfade scores the way DJ_PERFORMANCE D12 does. A deck playing
  off-air is prep, not show.
- **D6 — Phrase analysis = optional triggers, not authority.** Where rekordbox
  phrase data exists, phrase boundaries (verse→chorus) can fire triggers
  (existing trigger machinery — no new modulation silo). Absent or wrong phrase
  data degrades to beat/bar sync only.
- **D7 — One I/O thread, existing channel pattern.** The Link stack runs on its
  own network thread feeding the content thread via the established
  lock-free/channel pattern (MIDI/OSC precedent). Approved here per the
  standard's must-escalate rule; no other new shared state.
- **D8 — Simulator-first testing.** A loopback Pro DJ Link simulator (replaying
  captured packet fixtures) drives all CI gates; real hardware is a bring-up
  checklist, not a test dependency. First gig: record packet captures →
  fixtures for regression.

## 3. What it buys on stage

A club books visuals without a video operator: MANIFOLD on the venue machine,
mapped once. Any DJ plays; visuals follow tempo, beat, track, and phrase — the
booth is the controller. For Peter's own DJ sets: full show production from
CDJs with zero laptop-jockeying.

## 4. Phasing

Forbidden, all phases: locking render cadence to network beats (D2) · packet
layouts synthesized from memory instead of the published analysis (§1) · new
shared state beyond the D7 channel · silent unmapped-track fallthrough (D4).

- **P1 — Network core.** Discovery, virtual-device join, beat/status/mixer
  ingest on the D7 thread; booth status widget (decks, BPM, master, on-air) on
  the perform surface. Gate: simulator fixtures decode to asserted device/beat/
  status streams; widget headless-PNG; negative gate — content-thread tick
  contains zero socket reads (`rg -n 'recv|socket' ` on engine tick modules).
- **P2 — Sync source.** Master-deck tempo/phase drives score lock at the
  timecode seam. Gate: simulator sweep (BPM changes, master handoffs) holds
  score-beat error within the timecode tolerance; handoff produces no score
  discontinuity beyond one beat quantum.
- **P3 — Metadata + track triggers.** Metadata/rekordbox queries, track table,
  mapping UI in the venue profile, D4 triggers + D5 on-air gating. Gate:
  simulator loads mapped + unmapped tracks — mapped fires its score, unmapped
  lands in the report list; on-air flip crossfades scores.
- **P4 — Phrase triggers + hardware bring-up.** D6 phrase events; per-model
  quirk pass on real gear; packet-capture fixtures recorded and committed.
  Gate: the bring-up checklist executed on a real booth (Peter), captures
  archived; phrase trigger demo filmed — release-trailer material
  (BUSINESS_PLAN §7).

## 5. Deferred (with triggers)

- **Waveform/artwork display** on the surface (protocol supports it): when the
  booth widget proves insufficient without it.
- **Opus Quad / newer-ecosystem divergences**: on demand, per hardware access.
- **Driving lighting from Link data** (beyond visuals): rides the existing
  Art-Net/trigger story; nothing new needed here.
- **rekordbox USB pre-analysis import** (reading a DJ's stick before the set):
  when a real venue workflow asks for it — rekordcrate makes it tractable.
