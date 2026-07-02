<!-- index: Ableton .als import + re-sync design — select a Live set, MANIFOLD builds the show score (tempo map, layers, cues, trigger clips from MIDI, audio stems); three-way merge on re-import so manual MANIFOLD edits survive. Approved 2026-07-02; Sonnet implementation contract. -->

# Ableton Show-Sync — Design & Implementation Contract

**Status: APPROVED (Peter, 2026-07-02). Not implemented. Sonnet-executable.**
**Prerequisites: none (rides existing bridge + trigger-clip infra). Sequencing: `docs/DESIGN_BUILD_ORDER.md` wave 2.**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any
phase. Conformance-hardened: audit claims are a 2026-07-02 snapshot — run the §8.3
pre-flight (re-verify bridge/timeline anchors) before each phase.**

One gesture: *File → Import Ableton Set…* → pick an `.als` → MANIFOLD builds the
show's score on the timeline — tempo map, a layer per track, cues from locators,
beat-exact trigger clips from every MIDI note, audio stem clips ready for
per-clip detection. Then the user tunes: drops generators into the scaffold,
wires visuals to the trigger lanes. Re-import after the arrangement changes in
Ableton and everything moves — manual MANIFOLD edits survive.

The model, in Peter's words: **"events are just clips — everything triggers off
the rising clip edge."** The import creates no new runtime concepts. Every
extracted feature lands as an existing entity: `TimelineClip`, `Layer`,
`TempoPoint`, cue. The only new machinery is the parser, the provenance field,
and the merge.

---

## §1 What already exists (audit, 2026-07-02)

| Piece | Where | Relevance |
|---|---|---|
| Trigger-clips-from-analysis pattern | `manifold-core/src/audio_clip_detection.rs`, `TimelineClip.detection_source` ([clip.rs:43](../crates/manifold-core/src/clip.rs)) | The exact provenance shape this design extends: generated clips point back at their source; re-run clears only its own output. `.als` import is the symbolic twin of detection. |
| Phantom clips from live MIDI | NoteOn creates, NoteOff commits (CLAUDE.md invariant) | Proof that note→clip is the established mapping. Import is the same thing offline. |
| Tempo map | `manifold-core/src/tempo.rs` — `TempoMap`, `TempoPoint`, `TempoPointSource`, beat↔seconds converters | Direct landing for Ableton's tempo automation. Beat-indexed, like the .als. |
| Layer model | `manifold-core/src/layer.rs` — `name`, `layer_type`, `parent_layer_id` (grouping), `clips`, `enforce_non_overlap()` | Tracks → layers, group tracks → parent layers. Non-overlap is the write-time invariant that shapes MIDI landing (§4.3). |
| Clip model | `manifold-core/src/clip.rs` — `TimelineClip` beat-primary, `new_generator` / `new_audio`, `color_override`, `recorded_bpm`/`warp_ratio` | Trigger clips = generator clips; stems = audio clips with warp. No `name` field on clips — clip names do not import (layer names do). |
| Cues | `CuePoint` in `manifold-playback/src/ableton_bridge.rs:120` — **runtime-only, arrives over OSC** | No persistent cue storage exists. §4.5 adds one (small, project-level) so the show works with Ableton closed. |
| OSC bridge | `ableton_bridge.rs` — macros, transport, structural identity via `device_class_name` (`ableton_mapping.rs`) | **Unchanged by this design.** File import = structure, offline. OSC = live values. §4.6 pre-seeds the mapping picker from the parsed set. |
| Per-clip audio detection | `docs/AUDIO_CLIP_DETECTION_DESIGN.md` — stem + trigger clips, clear-by-source, lane reuse | Post-import, imported stem clips get detection for free — it's a property of audio clips. |
| Undo | `EditingService` → `Command`, sole mutation gateway | Import and re-sync are each ONE undo entry. |

---

## §2 Decisions (settled — don't reopen)

| # | Decision |
|---|---|
| D1 | **One-way.** `.als` → MANIFOLD only. MANIFOLD never writes or modifies the Live set. |
| D2 | **Everything lands as existing entities.** Clips, layers, tempo points, cues. Events are clips; the rising clip edge is the trigger. No "trigger lane" entity, no event streams. |
| D3 | **Provenance on the entity, baseline on the project.** `Option<AbletonRef>` on `Layer` and `TimelineClip` (serde `skip_serializing_if = "Option::is_none"`, following `detection_source` / `color_override` precedent — old projects round-trip byte-identical). Project-level `AbletonImportState` holds the set path, Live version, per-track import modes, and the **normalized baseline snapshot** of the last import. |
| D4 | **Re-sync is a three-way merge**: baseline vs new `.als` vs current project, per mirrored field. "Touched" is *computed* (current ≠ baseline), never a runtime flag. Rules in §5.2. |
| D5 | **Conflicts default to keep-MANIFOLD**, listed in a sync report. Never silently clobber a hand edit. |
| D6 | **Deletions:** entity gone from the new `.als` → if locally untouched, delete it (it was a pure mirror); if locally modified, move it to an **"Ableton (orphaned)"** group layer, muted, and report. Never auto-delete user work. |
| D7 | **Identity:** Ableton XML `Id` attributes first; fallback heuristic `(track match, same name, ≥50% time overlap)`; else treated as new. Matching is per-track, then per-clip within matched tracks. |
| D8 | **Drum racks pitch-split: one layer per pad**, named from the drum-rack chain names ("Kick", "Snare 909"…), grouped under a parent layer named after the track. Required anyway — overlapping notes on one layer would violate `enforce_non_overlap`. |
| D9 | **Melodic MIDI tracks stay one layer**; overlapping notes (chords) are resolved by the existing `enforce_non_overlap` trim on insert. Chords collapse toward their rising edge, which is what a trigger cares about. Note durations import as-is (gate length), no minimum floor. |
| D10 | **MIDI note → generator clip** (`TimelineClip::new_generator`): `start_beat` = note start, `duration_beats` = note duration, velocity → new `trigger_velocity: Option<f32>` (§4.4). User assigns generators to the layer afterward — that's the "tune" step. |
| D11 | **Audio arrangement regions → real audio clips** (`new_audio`) referencing the set's sample files, **muted by default** (Ableton is front-of-house audio; MANIFOLD stems exist for per-clip detection and export mixdown). Warp handling in §4.2. |
| D12 | **Selective import dialog:** per-track mode — `Full` / `Skip` (default: Full for tracks with content). Modes persist in `AbletonImportState` and are re-applied plus re-editable on re-sync. |
| D13 | **Import and re-sync are each one undo entry** through `EditingService` (single command wrapping all mutations). |
| D14 | **Parsing:** `flate2` gunzip + `quick-xml` streaming, on a background thread; the parsed `NormalizedSet` is applied on the content thread via one command. Pin to Peter's Live major version (12); unknown schema → loud, user-visible error, never a panic, never a partial import. |
| D15 | **Rack macro inventory pre-seeds the OSC mapping picker** — parsed device racks (name, `device_class_name`, macro names) populate `AbletonSetContext` so mapping targets are offered before the bridge ever connects. |
| D16 | **Out of scope v1:** automation-envelope import (lands as a natural extension once `docs/AUTOMATION_LANES_DESIGN.md` is built — envelopes → lanes), session-view scenes (session mode not built), time signatures beyond the project's beats-per-bar if trivially mappable, writing `.als`, video/return/master track content. |

---

## §3 Extraction — what the `.als` gives us

An `.als` is gzipped XML: Ableton's entire document model. Nearly everything is
**symbolic — parse, don't infer.** Audio analysis (existing detection pipeline)
is the *fallback* for audio-only tracks, not the front door.

### 3.1 Fixture-first schema pinning (mandatory workflow)

The XML schema is undocumented and shifts between Live majors. Do NOT hardcode
absolute element paths from this doc — **pin them from fixtures**:

1. Export minimal fixture sets from Peter's Live 12: one MIDI track + drum rack,
   one melodic MIDI track, one warped audio track, locators, tempo automation,
   a group track. Commit the `.als` fixtures under `crates/manifold-io/tests/fixtures/als/`.
2. Gunzip, inspect, and pin the observed element paths in ONE module
   (`manifold-io/src/als/schema.rs`) with the Live version they were observed in.
3. The parser locates by element-name pattern with bounded depth, not absolute
   nesting — resilient to minor-version shuffles. Loud error on major mismatch (D14).

### 3.2 Feature inventory

| Feature | Nature | Notes for the parser |
|---|---|---|
| Tempo automation + initial tempo | symbolic, beat-indexed | Master track mixer's Tempo events (`FloatEvent` time/value). Time < 0 = initial value. |
| Locators | symbolic | `Locator` nodes: beat time + name. → cues. |
| Tracks | symbolic | `MidiTrack` / `AudioTrack` / `GroupTrack` (+ Return/Master, ignored v1). `Id` attribute, `EffectiveName`/`UserName`, color (palette index), group membership. |
| Arrangement clip regions | symbolic, beats | Per-track arranger events: `AudioClip` / `MidiClip` with current start/end in beats, `Id`, color, loop state. |
| **MIDI notes** | symbolic — **the gold** | Per `MidiClip`: key tracks → note events (time, duration, velocity, off-velocity, enabled). A drum track's MIDI *is* the hit timeline. Times are clip-relative beats; add clip start, account for clip loop unrolling within the region. |
| Drum rack pad names | symbolic | Drum-rack branch chains carry names + receiving note. **Known quirk: the receiving-note encoding is non-obvious (historically inverted, e.g. `128 − note`) — verify against the fixture before trusting.** |
| Warp markers | symbolic | Per audio clip: `(SecTime, BeatTime)` pairs — the exact sample↔beat map. |
| Sample file refs | symbolic | Per audio clip: relative + absolute path hints. Resolve relative-to-set first (project folder `Samples/`), absolute fallback; missing file → import the clip anyway, flag in report. |
| Rack devices + macro names | symbolic | For D15. Same `device_class_name` structural identity the bridge already validates against. |
| Automation envelopes | symbolic | Present in the file; **deferred** (D16) until automation lanes exist. |
| Onsets in audio-only stems | **inferred** | Not in the XML. Post-import, existing per-clip detection runs on the imported stem clips — user-initiated, per current detection UX. |

---

## §4 Mapping — where each feature lands

| Ableton | MANIFOLD | Detail |
|---|---|---|
| Set tempo + tempo automation | `TempoMap` points | §4.1 |
| Locator | Project cue (new storage) | §4.5 |
| Group track | `Layer` with children via `parent_layer_id` | name + color |
| MIDI track (drum rack) | Parent layer + **one child layer per pad** | §4.3 |
| MIDI track (melodic) | One layer | §4.3 |
| MIDI note | Generator clip (trigger) | §4.4 |
| Audio track | Audio layer | |
| Audio region | Audio clip, muted | §4.2 |
| Track/clip color | Layer color / `color_override` | Ableton palette index → RGB via a pinned lookup table (approximate is fine) |
| Rack macros | `AbletonSetContext` pre-seed | D15; no mapping created, just the picker inventory |

### 4.1 Tempo

Import all tempo events as `TempoPoint`s (add a `TempoPointSource` variant for
Ableton import if the enum needs one). Initial tempo → point at beat 0
(`ensure_default_at_beat_zero` semantics). On re-sync, tempo points carry
provenance like clips (they're identified by beat position + source).

### 4.2 Audio regions → audio clips (warp)

- `audio_file_path` from the resolved sample ref; `source_duration` from decode.
- `start_beat` / `duration_beats` straight from the arrangement (already beats).
- `in_point` (seconds) from the warp-marker map evaluated at the region start.
- `recorded_bpm`: derive the **effective source BPM** from the warp-marker span
  covering the region (`beats spanned / seconds spanned × 60`) so
  `warp_ratio()` reproduces Ableton's average playback rate. Warp OFF in
  Ableton → `recorded_bpm = 0` (native speed) — matches MANIFOLD's convention.
- **Non-uniform warp inside one region** (markers imply varying rate): import
  with the average, add a per-clip warning to the import report. Detection
  beat-placement degrades gracefully; do not attempt piecewise warp in v1.
- Clips import muted (D11); the layer is a normal audio layer — unmute for
  export mixdown or audio-mod sends as the user chooses.

### 4.3 MIDI tracks → layers

- **Drum rack detected** (track's device chain contains one): create a parent
  layer named after the track; one child layer per pad **that has notes**,
  named from the pad chain. Route each note to its pad's layer. Pads are
  monophonic-ish; same-pad retriggers that overlap are resolved by
  `enforce_non_overlap` (previous hit's gate ends where the next begins).
- **No drum rack**: one layer; all notes on it; chords trimmed by
  `enforce_non_overlap` on insert (D9). Insert notes in ascending start order
  so the trim is deterministic (later onset wins the overlap).
- Layer type: same type detection trigger clips use today (generator clips on a
  standard content layer). The user assigns generator presets afterward.

### 4.4 Velocity — new field, one consumer

`TimelineClip` gains:

```rust
/// Trigger intensity for clips imported from MIDI (velocity / 127) or future
/// velocity-aware sources. `None` = full intensity (1.0). Read by the layer
/// trigger machinery; never affects timing.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub trigger_velocity: Option<f32>,
```

One consumer ships with it: expose the active clip's resolved velocity
alongside the existing `trigger_count` in `LayerGeneratorState` (see
`generators/clip_trigger.rs` for the trigger-count pattern) so generators and
param bindings can read "how hard was this hit." No UI beyond that in v1 —
hand-placed clips default to `None`/1.0. This is what makes a kick lane feel
played rather than sequenced: hard hits flash harder.

### 4.5 Cues — small persistent store

New project-level `Vec<CueMarker>` (`beat: Beats`, `name: String`,
`ableton_ref: Option<AbletonRef>`), serialized with the project, mutated only
through commands. Locators import here. The perform-mode HUD prefers **live OSC
cues when the bridge is connected** (current behaviour, unchanged) and falls
back to project cues — the show works with Ableton closed. Skip-serializing
when empty so legacy fixtures stay byte-identical.

### 4.6 What is NOT created

No video layers, no generator assignments, no effect chains, no mappings, no
audio routing changes. The import builds the **score**; the user builds the
**instrumentation**. Import never touches any entity that lacks an
`AbletonRef` (with the single exception of appending to the orphan group, D6).

---

## §5 Provenance & re-sync (the merge)

### 5.1 Data

```rust
/// Identity link from a MANIFOLD entity back to the Ableton set element it
/// mirrors. Presence marks the entity as import-managed; absence = hand-made,
/// invisible to re-sync.
pub struct AbletonRef {
    pub track_id: i64,            // Ableton XML track Id
    pub element_id: Option<i64>,  // clip Id / locator ordinal; None for the track/layer itself
    pub kind: AbletonRefKind,     // Track | Pad(note) | Clip | Note | Locator | TempoPoint
}
```

`AbletonImportState` (on `Project`, skip-serializing if `None`):
set path, Live version string, import timestamp, per-track import modes (D12),
and `baseline: NormalizedSet` — the normalized parse of the **last applied**
import (tracks, regions, notes, locators, tempo events, with their Ableton IDs
and the mirrored field values). Normalized form is a versioned Rust struct
serialized with the project (V2 ZIP handles size; a dense set's baseline is
a few hundred KB of JSON — acceptable; do NOT store the raw XML).

**Mirrored fields** (the merge operates ONLY on these):
- Layer: `name`, color, order among imported siblings, parent group
- Clip: `start_beat`, `duration_beats`, `color_override`, `trigger_velocity`,
  and for audio clips `in_point` / `recorded_bpm` / `audio_file_path`
- Cue: `beat`, `name`
- Tempo point: `beat`, `bpm`

Everything else on those entities (mute, lock, transforms, string params,
detection state, loop settings…) is MANIFOLD-owned and never written by re-sync.

### 5.2 Merge rules (per matched entity, per mirrored field)

Let **B** = baseline value, **A** = new .als value, **M** = current MANIFOLD value.

| A vs B | M vs B | Action |
|---|---|---|
| unchanged | unchanged | nothing |
| changed | unchanged | apply A (Ableton moved it, user didn't touch it) |
| unchanged | changed | keep M (user's edit stands) |
| changed | changed, M == A | nothing (convergent) |
| changed | changed, M ≠ A | **conflict → keep M, report** (D5) |

Entity add/remove:

| Case | Action |
|---|---|
| In new .als, not in baseline | create (respecting the track's import mode) |
| In baseline, not in new .als, M untouched on all mirrored fields | delete |
| In baseline, not in new .als, M touched | move to "Ableton (orphaned)" group, mute, report (D6) |
| In baseline, `AbletonRef`'d entity deleted by user in MANIFOLD | do NOT recreate; record the ID as user-suppressed in `AbletonImportState` so every future re-sync skips it |

Notes (MIDI events) are matched within their clip by `(pitch, start)` with a
small epsilon; a moved note is a delete + add — cheap and correct, since note
clips are pure mirrors the user rarely edits individually. If the user HAS
edited an individual note clip, the touched rules above protect it.

### 5.3 Sync report

Re-sync ends with a modal summary: counts (applied / kept / conflicts /
orphaned / new / deleted), then the itemized conflict + orphan list (entity,
field, kept value, incoming value). Also surfaced: missing sample files,
non-uniform warp approximations, unmatched-ID fallback matches (§D7) so the
user can spot a wrong guess. Plain data in `ContentState`; UI renders it.

### 5.4 The command shape

`ImportAbletonSetCommand { normalized: NormalizedSet, modes: TrackModes }` —
computed diff applied atomically, one undo entry (D13). Undo restores both the
entities AND the previous `AbletonImportState` (baseline swaps back), so
undo → re-import is idempotent. Parse happens off-thread *before* the command
is issued; the command itself is pure model mutation (no I/O on the content
thread).

---

## §6 Import dialog

- File picker → background parse → dialog with the track list: name, type
  (drums / MIDI / audio / group), content summary ("214 notes, 3 pads" /
  "12 regions"), and a per-track mode toggle `Full` / `Skip` (D12).
- Defaults: everything with content = Full. Group tracks import iff any child does.
- Re-sync entry point: *File → Re-sync Ableton Set* (enabled when
  `AbletonImportState` exists) — re-parses the same path (re-pickable if
  moved), shows the same dialog pre-filled with saved modes, then runs §5.
- Switching a track from Skip→Full on re-sync imports it fresh; Full→Skip
  treats its entities per the deletion rules (untouched mirrors delete,
  touched ones orphan).

---

## §7 Implementation notes

- **New module:** `manifold-io/src/als/` — `schema.rs` (pinned element names,
  version check), `parse.rs` (gunzip + streaming parse → `NormalizedSet`),
  `normalize.rs` (loop unrolling, pad routing, warp math). Pure functions,
  no GPU, no content-thread types beyond `manifold-core` models.
- **Merge:** `manifold-editing/src/commands/ableton_import.rs` — diff of
  `(baseline, new, project)` → `Vec<MergeOp>` as pure functions (unit-testable
  without a project), then one command applies them.
- **Threading:** parse + sample-path resolution on a background worker
  (pattern: `manifold-renderer/src/background_worker.rs`); result crosses to
  the content thread as one `ContentCommand`. Never parse on the content thread.
- **Dependencies:** `flate2` + `quick-xml` in `manifold-io` only. Both are
  boring, widely-used crates; no new shared state anywhere.
- **Scale:** a real set is 30–60 tracks, thousands of notes → tens of layers,
  a few thousand clips. Typical MANIFOLD projects already run 2928 clips / 53
  layers — within the model's design envelope. Import is a user action, not a
  hot path; the only per-frame cost is the clips existing, which is the
  already-paid cost of clips.
- **Live version drift:** when a future Live version breaks the parser, the
  fix is a new fixture + `schema.rs` update — the failure mode is a loud
  version error, never a wrong import.

## §8 Phasing (Sonnet)

- **P1 — Parse.** Fixtures (§3.1), `als/` module, `NormalizedSet`, version
  pinning. Tests: golden normalized output per fixture; tempo/warp math unit
  tests; drum-rack pad routing incl. the receiving-note quirk.
- **P2 — First import.** `AbletonRef` + `AbletonImportState` + `CueMarker`
  store + `trigger_velocity` (fields + serde round-trip tests, byte-identical
  legacy fixtures). Import dialog, `ImportAbletonSetCommand` create-only path,
  velocity exposure in `LayerGeneratorState`. Milestone: Peter imports his
  live set, sees the full score, undo works.
- **P3 — Re-sync.** Baseline persistence, three-way diff (§5.2) as pure
  functions with exhaustive rule-table tests, orphan group, user-suppressed
  IDs, sync report UI. Milestone: edit set in Ableton → re-sync → moves
  applied, hand edits intact, report correct.
- **P4 — Polish.** Macro pre-seed into `AbletonSetContext` (D15), missing-file
  and warp warnings, fallback identity matching (D7) with report surfacing,
  palette color table.

Testing scope per CLAUDE.md: per-crate focused tests throughout
(`manifold-io`, `manifold-editing`, `manifold-core`); the merge rule table is
the load-bearing test surface. Full workspace sweep only at P2/P3 completion
(serialization fields touch project I/O = infrastructure).

## §9 Deferred / non-goals

- Automation envelopes → automation lanes (after AUTOMATION_LANES ships).
- Session-view scenes → session mode (after SESSION_MODE ships).
- Piecewise per-marker warp fidelity inside one region.
- Live-OSC structural population (OSC stays values/transport; structure comes
  from the file).
- Writing `.als` / round-trip. MANIFOLD is a reader.
