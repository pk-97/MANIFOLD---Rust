# Section Export — mark cut points on the beat grid, export one clip per section

**Status:** SHIPPED 2026-08-26 (P1+P2 + journey-proof, all gates green) · design: k3 (lead), with Peter
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.
**Bead:** BUG-1lv6 (Section export: mark beat-grid sections, one export per section)

Peter's release-content workflow: compose a full track, mark where each clip
starts, export once, get a folder of named clips ready for Instagram/TikTok
scheduling. His words, settling the architecture: *"maybe we just go for
re-export here instead of this convoluted MANIFOLD only video support style
feature?"* — one render pass per section, no codec keyframe schemes. On stage
this changes nothing; in the studio it turns "export, trim, re-export, repeat"
into one export run.

Rejected upstream shapes (do not revive without Peter): keyframe-per-beat +
ffmpeg stream-copy slicing (AVAssetWriter only supports uniform GOP; per-frame
force-keyframe requires a VTCompressionSession rewrite of
`crates/manifold-media/native/MetalEncoderPlugin.m`), chaptered single-pass
export (multiple encoder sessions in one render pass — real but unneeded
complexity while N sequential exports are acceptable).

## 1. Audit — what exists (verified 2026-08-26)

| Piece | Where | State |
|---|---|---|
| `TimelineMarker` (beat, name, color; serialized camelCase) | `crates/manifold-core/src/marker.rs` | exists — extend with a flag |
| Marker storage, kept sorted by beat on insert | `crates/manifold-core/src/timeline.rs:26` (`Timeline::markers`), `timeline.rs:518` (`add_marker`) | exists |
| Marker undo commands | `crates/manifold-editing/src/commands/marker.rs` (`AddMarkerCommand`, `DeleteMarkerCommand`) | exists — extend or add toggle command |
| Export range (⌘I/⌘O), serialized on timeline | `crates/manifold-core/src/timeline.rs:18-22` (`export_in_beat`, `export_out_beat`, `export_range_enabled`) | exists |
| Beat-ranged export config | `crates/manifold-media/src/export_config.rs` (`ExportConfig.start_beat`, `end_beat`, `audio_start_beat`) | exists |
| Export entry + loop, cancel poll, audio mux offset per range | `crates/manifold-app/src/content_export.rs` (`ContentCommand::StartExport` handler; cancel at content_export.rs:242) | exists — wrap in section loop |
| Export command type | `crates/manifold-app/src/content_command.rs:248` (`StartExport(Box<ExportConfig>)`) | exists |
| Tempo map beat→seconds | `crates/manifold-core/src/tempo.rs:256` (`TempoMapConverter::beat_to_seconds_immut`) | exists, used by export already |
| ⌘I/⌘O keybindings | `crates/manifold-app/src/window_input.rs:2531-2536` | exists; no `"m"` binding found (⚠ VERIFY-AT-IMPL: `rg -n '"m"' crates/manifold-app/src/window_input.rs` — must still be zero hits) |
| Export settings UI (HDR toggle lives in settings popup) | `crates/manifold-ui/src/panels/settings_popup.rs:251` | exists — checkbox lands wherever the export action's settings surface is |

Everything load-bearing exists. This design is wiring plus one new flag —
nothing here is genuinely new except the section-derivation rule.

## 2. Decisions

- **D1 — Re-export per section, sequentially.** Each section is a full normal
  export with its own `start_beat`/`end_beat`. Consequences, stated honestly:
  N sections = N render passes, so export wall-time scales with section count.
  Accepted: render time instead of Peter's time; runs unattended.
- **D2 — Chapter-style sections; a section marker means "cut here", never
  "end here".** Sections derive from sorted section-flagged markers inside
  `[export_in_beat, export_out_beat)`: sections are `[in, m₁)`, `[m₁, m₂)`, …,
  `[mₙ, out)`. No pairing, so no broken/orphan end markers. Section markers
  outside the export range are ignored.
- **D3 — New field `is_section_boundary: bool` on `TimelineMarker`**,
  `#[serde(default)]` (camelCase `isSectionBoundary`). Old projects and old
  markers load with it `false` — existing markers never slice an export.
- **D4 — One export settings surface with a "Split at section markers"
  checkbox** (Peter: *"checkbox probably makes sense here"*). Zero section
  markers in range = today's single-export behavior, unchanged.
- **D5 — Section derivation happens on the content thread**, owner of the
  `Project`. `ExportConfig` gains `split_at_section_markers: bool`; the export
  handler derives the section list from `project.timeline` at export start.
  The UI never computes sections. Rejected: UI sends a precomputed section
  list — two homes for the same derivation, and the zero-new-systems test.
- **D6 — Output naming: `<output_base>--<marker-name>.mov`**, marker name
  sanitized (whitespace/punctuation → `-`, empty → `section-N` where N counts
  sections from 1). Duplicate names get `-2`, `-3`. The export's `output_path`
  becomes the base; single-export naming is untouched.
- **D7 — ⌘M drops a section-flagged marker at the playhead** (⌘M currently
  unbound — verified 2026-08-26, re-verify at impl). Plain marker creation
  keeps its existing path; section markers render distinctly (different glyph
  or color treatment — executor picks from existing marker-render code, no new
  rendering infrastructure).
- **D8 — Audio per section uses the existing mux path**: each section export
  sets `audio_start_beat = section start`, which the current muxer already
  turns into a zero-offset slice of the master audio
  (`content_export.rs:108` comment pins this behavior).

## 3. Data model & seams

- `TimelineMarker` (`crates/manifold-core/src/marker.rs`): add
  `pub is_section_boundary: bool` with `#[serde(default)]`. `TimelineMarker::new`
  sets `false`; add `as_section(mut self) -> Self` builder, shaped like the
  existing `with_name`/`with_color`.
- New editing command `ToggleMarkerSectionCommand { marker_id }` in
  `crates/manifold-editing/src/commands/marker.rs`, shaped like
  `DeleteMarkerCommand` (stores old flag value for undo). All marker mutation
  routes through `EditingService` — no direct writes.
- `ExportConfig` (`crates/manifold-media/src/export_config.rs`): add
  `pub split_at_section_markers: bool`. ExportConfig is constructed fresh per
  export (not serialized into projects) — no load migration.
- Section derivation, in `content_export.rs` at export start:

  ```
  fn derive_sections(timeline: &Timeline) -> Vec<(Beats, Beats, String)>
  // in/out from timeline.export_*; flag-filtered markers inside range;
  // returns one (start, end, name) per section; empty vec when flag off
  // or no section markers in range (→ existing single-export path).
  ```

- Export loop: the existing single-export body becomes a function called once
  per derived section with `start_beat`/`end_beat`/`audio_start_beat` and the
  per-section output path. Progress reporting gains a `section i of N` prefix;
  `CancelExport` stays polled per frame, aborts the whole run.
- Threading/ownership: no new threads, no new shared state. Sections run
  sequentially on the existing export loop.

**Plausible-wrong architecture, forbidden by name:** you will want to make the
UI send the section list, or to store derived sections on the `Project` — no.
Derivation lives in one place (`derive_sections` on the content thread) from
one source of truth (timeline markers + export range). Second forbidden move:
a "smart" single-pass-with-multiple-encoders optimization — that is the
rejected chaptered shape; escalate if sequential export proves too slow in
practice instead of building it.

## 4. Invariants & enforcement

- Existing markers never become section boundaries on load.
  Enforcement: round-trip test — project with markers saved pre-flag loads
  with `is_section_boundary == false` on all markers (fixture: canonical
  Liveschool project, which carries markers).
- `split_at_section_markers` with zero in-range section markers is
  byte-identical behavior to today's export. Enforcement: P1 gate test —
  one section run with flag on + no section markers produces exactly one file
  at the unmodified `output_path`.
- Every section file's duration matches its beat range within one frame.
  Enforcement: P1 gate — ffprobe on each output, duration compared against
  `beat_to_seconds` of the section range.
- Sections never overlap and cover `[in, out)` exactly. Enforcement: unit
  test on `derive_sections` over shuffled/edge-case marker sets (markers on
  `in`, on `out`, outside range, duplicated beats).

## 5. Phasing

### P1 — engine: flag, derivation, section loop (one session)

- **Entry state:** anchors above re-verified; `cargo nextest run -p manifold-core -p manifold-media` green.
- **Read-back:** this doc's D1–D6, D8 + forbidden moves; `marker.rs`,
  `timeline.rs:518`, `export_config.rs`, `content_export.rs` whole.
- **Deliverables:** `is_section_boundary` on `TimelineMarker`;
  `ToggleMarkerSectionCommand`; `split_at_section_markers` on `ExportConfig`;
  `derive_sections` + section loop in `content_export.rs`; filename
  sanitizing + collision suffixes; section progress prefix.
- **Gate:** the four invariant tests of section 4 (Invariants & enforcement) pass; a headless export
  (journey-proof harness pattern) of a 2-section range produces 2 files whose
  ffprobe durations match the beat ranges within one frame; audio in each
  file starts at the section's first beat (ffprobe stream start ~0).
- **Demo:** ffprobe output table per file — L2 (numbers Peter reads).
- **Test scope:** `cargo nextest run -p manifold-core -p manifold-editing -p manifold-media -p manifold-app`; clippy same crates.
- **Forbidden moves:** storing sections on Project; UI-side derivation;
  parallel section rendering; touching the encoder `.m`.
- **Invariant deliverable:** the round-trip and no-markers byte-identity tests
  named in section 4 (Invariants & enforcement).

### P2 — UI: ⌘M, marker rendering, checkbox (one session)

- **Entry state:** P1 on the branch, gates green; `rg -n '"m"' crates/manifold-app/src/window_input.rs` returns zero hits (else escalate for a different key).
- **Read-back:** D3, D4, D7 + forbidden moves; `window_input.rs:2531-2536`
  (the ⌘I/⌘O pattern to copy); marker rendering in the timeline viewport;
  the export settings surface.
- **Deliverables:** ⌘M adds section-flagged marker at playhead via
  `AddMarkerCommand` + section toggle; section markers visually distinct;
  "Split at section markers" checkbox wired to `ExportConfig`.
- **Gate:** L3 — a `scripts/ui-flows/` flow: place playhead, ⌘M, assert a
  section marker exists at the beat; tick the checkbox; run a short export;
  assert per-section files exist. Round-trip: save project, reload, section
  flags intact.
- **Demo:** the flow's assertion output + a PNG of section markers on the
  timeline (Peter looks) — L3 target.
- **Performer gesture:** scrub playhead to a drop, hit ⌘M twice two bars
  apart, export — two clips land, named, right lengths.
- **Test scope:** same crates + `-p manifold-ui`; clippy same.
- **Forbidden moves:** new marker-rendering infrastructure (extend what
  renders markers today); a separate export dialog; UI-side section math.

## 6. Decided — do not reopen

1. Re-export per section, sequential — no keyframe or codec schemes (Peter, 2026-08-26).
2. Chapter-style: marker = cut point; no start/end pairs.
3. Flag on `TimelineMarker`, serde-defaulted off; old projects unaffected.
4. Checkbox in the existing export settings surface, not a new dialog.
5. Section derivation on the content thread only; UI sends no section math.
6. Filenames `<base>--<marker-name>.mov`, `section-N` fallback, `-2` collisions.
7. ⌘M for section marker at playhead (re-verify binding is free at impl).

## 7. Deferred

- **Chaptered single-pass export** (one render pass, N encoder sessions) —
  revive only if sequential section export is measurably too slow on Peter's
  real projects.
- **Keyframe-per-beat export for lossless post-hoc slicing of arbitrary
  files** — revive if Peter needs to re-cut exports without re-rendering.
- **Content-scheduling scripts** (Instagram Graph API, TikTok drafts, folder
  pipeline) — separate tooling track, not part of this design; bead to be
  created when that work starts.
