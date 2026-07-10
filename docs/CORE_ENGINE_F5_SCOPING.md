# F5 scoping note — authored-vs-live split for `settings.bpm` and `settings.clock_authority`

**Status: scoping input for a Fable design pass. Not a design doc. 2026-07-10, Opus.**
Verdict already taken with Peter: **runtime** (split each field into an authored value
that serializes and a live value that never does). This note inventories the read sites
so the design pass starts from evidence, not a grep. It deliberately stops short of the
per-site rulings — those are the design's job; the leans here are input.

## The bug (confirmed)

Two writers mutate *serialized* settings fields every frame, outside `EditingService`,
bumping neither `data_version` nor undo:

- `sync_project_bpm_from_current_beat` writes `project.settings.bpm` — `engine.rs:1765`.
  It reads `settings.bpm` as the fallback into `get_bpm_at_beat` then writes the result
  back; when a live external tempo is present, that live BPM overwrites the *authored*
  tempo every tick.
- The authority auto-detect writes `project.settings.clock_authority` — `content_thread.rs:1471`.
  A manual `apply_authority_exclusively` (`transport_controller.rs:75`) is overwritten
  one frame later, so the user's chosen authority never sticks; and a save mid-show
  persists whatever authority/BPM was transiently live.

## The fix shape (not "move to runtime")

`settings.bpm` is **not** merely a live readout — ~80 read sites correctly depend on it
as the *authored project tempo* (the anchor for beat↔seconds math). Moving it wholesale
to runtime would break all of them. The fix is to **split** each field:

- **Authored** (stays in `settings`, written ONLY through `EditingService`/load):
  `settings.bpm` = the tempo the user drew/set; `settings.clock_authority` = the user's
  chosen authority *preference*.
- **Live** (new runtime field on the engine / content thread, never serialized — the
  `SessionRuntime` precedent): `displayed_bpm` (what the auto-detect / external clock
  currently reads) and `active_authority` (what auto-detect resolved this frame).

Then reclassify each read site: does it want the authored anchor (keep) or the live
value (redirect)? Getting one wrong silently retimes the show or displays the wrong
number — this is why it's a design pass with Peter's eye, not a mechanical sweep.

## `settings.clock_authority` — 7 read sites (small; most want LIVE)

| Site | Reads it for | Lean |
|---|---|---|
| `sync.rs:112` | the arbiter's source-vs-authority GATE | **live** (`active_authority`) |
| `content_commands.rs:132` | Play beat-alignment (CLK) | **live** |
| `content_thread.rs:587` | per-frame sync logic | **live** |
| `content_thread.rs:1180` | per-frame logic | **live** |
| `app_render.rs:2900` | UI `display_name` (what's active now) | **live** |
| `transport_controller.rs:50` | authority cycling | **live** current, but the manual apply SETS the preference |
| `engine.rs:1628` | (verify: tempo-recording gate?) | **verify in the design pass** |

Lean: the auto-detect writes a runtime `active_authority`; all "what's active now" reads
use it; `settings.clock_authority` becomes the preference the auto-detect uses as its
Internal-vs-detected baseline and that the manual apply writes. Confirm `engine.rs:1628`'s
intent before ruling.

## `settings.bpm` — ~80 read sites (large; most want AUTHORED)

Overwhelmingly beat↔seconds math that MUST read the authored anchor — do **not** redirect:
`warp_ratio` / `seconds_per_beat` throughout `input_host.rs`, `ui_bridge/*`, `editing_host.rs`,
`app_lifecycle.rs`; import (`midi_import.rs:118`, `percussion_import.rs:237/291`,
`percussion_orchestrator.rs:584`); `audio_mixdown.rs:128`; `audio_layer_playback.rs:253`;
all the editing/undo commands (`undo.rs`, `commands/settings.rs`, `commands/clip.rs`);
the `TempoMapConverter::beat_to_seconds*` fallback args in `engine.rs`; `project.rs`
`ensure_default_at_beat_zero`. These are the authored contract and stay on `settings.bpm`.

Candidate **display/live** reads to redirect to `displayed_bpm` — the design must confirm
each:
- `ui_bridge/state_sync.rs:203-210` — already branches on authority for display
  ("When clock authority is Internal, use project.settings.bpm…"); this is the primary
  display seam and the natural home for the authored-vs-live choice.
- `content_thread.rs:651` / `:1171` / `:1723` — outbound tempo to senders (M4L / Ableton);
  these want the *live* rate. (Note the M4L path is retired.)
- `content_commands.rs:947`, `app_render.rs` HUD reads — verify display vs anchor.

The single write to kill is `engine.rs:1765`; it becomes a write to `displayed_bpm`.
`settings.bpm` then changes only via the BPM edit command and load.

## Gate for the build

Must round-trip the Liveschool fixture (`Liveschool Live Show V6 LEDS.manifold`):
save → reload → authority and authored BPM equal what was authored, regardless of what
was transiently live at save time. That round-trip IS the proof the split worked.
