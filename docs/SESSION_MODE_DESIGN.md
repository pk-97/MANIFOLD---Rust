# Session Mode — Design

Ableton-style scene/clip launching as a second performance surface. Users who never touch the timeline can drop content into a grid (layers × scenes) and launch slots/scenes quantized to the beat. Timeline users can slice arrangement sections into scenes and back.

Status: design approved, not implemented. Written 2026-07-02 against `feat/timeline-ui-redesign`.

## 1. The core insight

MANIFOLD already runs two clip sources through one authority:

- `sync_clips_to_time()` (`crates/manifold-playback/src/engine.rs:1094`) merges **timeline actives** (`query_active_timeline_clips`, engine.rs:838) and **live slots** (`LiveClipManager::fill_live_slot_refs`) into one `compute_sync` diff (engine.rs:1118).
- `LiveClipManager` (`crates/manifold-playback/src/live_clip_manager.rs`) already holds live-triggered clips *outside the timeline* — `HashMap<layer_index, TimelineClip>` — with quantized pending launches, one slot per layer, clear-on-seek.

Session mode generalizes this: **authored, persistent slots** in the `Project`, launched through the same engine path. `sync_clips_to_time` stays sole authority. No second playback engine.

The Ableton mapping that makes this cheap:

| Ableton | MANIFOLD |
|---|---|
| Track | Layer |
| Session clip (MIDI notes trigger instrument) | `ClipSequence` (timed `TimelineClip`s ARE the trigger events) |
| One session clip playing per track | One slot playing per layer (existing live-slot rule) |
| Scene = row | `Scene` = one slot per layer |
| Global quantize | Launch quantize in beats |
| Back to Arrangement | Clear per-layer session override |

There is no "trigger clip" type. In Ableton, notes and instrument are separate; here the clip is both trigger and content. A session clip is a **container of timed clips**: clip start = NoteOn, clip end = NoteOff.

## 2. Non-goals (v1)

- Follow actions, legato launch modes, per-slot quantize overrides (global quantize only).
- Audio layers in slots (export mixdown assumes timeline; defer).
- Group layers (`parent_layer_id`) get no slots; leaf layers only.
- Automation envelopes inside sequences (leave a serde-optional field slot for the automation-lanes feature to fill later; do NOT design it now).
- Session recording UI polish — but the commit path must land in v1 (§7), it's the cheapest killer feature.

## 3. Data model (`manifold-core`)

New module `crates/manifold-core/src/session.rs`:

```rust
pub struct ClipSequence {
    pub length_beats: Beats,          // loop length; >= max clip end
    pub clips: Vec<TimelineClip>,     // start_beat RELATIVE to sequence start; non-overlapping, sorted
}

pub struct SessionSlot {
    pub layer_id: LayerId,            // identity, NOT index — layers reorder
    pub scene_id: SceneId,            // new newtype, same pattern as LayerId/ClipId
    pub sequence: ClipSequence,
    pub name: String,                 // display; default from first clip
    pub color: Option<[u8; 3]>,
}

pub struct Scene {
    pub id: SceneId,
    pub name: String,
    pub color: Option<[u8; 3]>,
}

pub struct SessionGrid {
    pub scenes: Vec<Scene>,           // row order
    pub slots: Vec<SessionSlot>,      // flat; at most one per (layer_id, scene_id)
    #[serde(skip)]
    slot_lookup: AHashMap<(LayerId, SceneId), usize>,  // rebuilt on mutation, same pattern as Timeline::clip_lookup
    #[serde(skip)]
    slot_lookup_dirty: bool,
}
```

On `Project`:

```rust
#[serde(default, skip_serializing_if = "SessionGrid::is_empty")]
pub session: SessionGrid,
```

Rules:

- `ClipSequence` reuses `TimelineClip` unchanged. All existing clip kinds (generator / video / image) are legal. Discriminants (`video_clip_id` etc.) work as-is.
- Non-overlap inside a sequence: reuse the `Layer::enforce_non_overlap()` logic — extract it to a free function over `&mut Vec<TimelineClip>` and call from both. Write-time invariant, same as lanes.
- Degenerate case is the common case: a sequence with one clip at beat 0 = a plain launchable clip. No special type.
- Serde: follow `docs`-standard conventions (`#[serde(default)]` everywhere, `skip_serializing_if` for empties) so pre-session projects round-trip byte-identically. Bump `project_version`; no load migration needed (field defaults to empty). The canonical fixture `Liveschool Live Show V6 LEDS.manifold` must load and re-save unchanged.

## 4. Runtime state (`manifold-playback`)

Session playback state is **runtime-only, never serialized, never in undo**. New module `crates/manifold-playback/src/session_state.rs`, owned by `PlaybackEngine` (sibling of `live_clip_manager`):

```rust
pub struct SessionRuntime {
    playing: AHashMap<LayerId, PlayingSlot>,      // at most one per layer
    pending: Vec<PendingSlotLaunch>,              // launches waiting for quantize boundary
    session_override: AHashSet<LayerId>,          // layers detached from arrangement (§6)
    quantize_beats: Beats,                        // global launch quantize, default 4.0 (1 bar)
}

struct PlayingSlot {
    layer_id: LayerId,
    scene_id: SceneId,
    launch_beat: f64,       // global beat the slot started at (post-quantize)
}

struct PendingSlotLaunch {
    layer_id: LayerId,
    scene_id: SceneId,      // or Stop — see LaunchAction below
    target_beat: f64,       // next quantize boundary at enqueue time
}
```

### Resolution (per tick, allocation-free)

Inside `sync_clips_to_time`, after `query_active_timeline_clips` and live-slot fill, add a third ref source `fill_session_refs(&mut self.session_refs_scratch)`:

For each `PlayingSlot`:

1. `elapsed = current_beat - launch_beat` (both f64; `current_beat` already exists on the engine).
2. `local = elapsed % length_beats`; `iteration = floor(elapsed / length_beats)`.
3. Linear scan the sequence's sorted clips for the one containing `local` (sequences are small; no binary search needed).
4. If found, push an `ActiveClipRef` with:
   - `clip_id`: the inner clip's own id,
   - `layer_index`: resolved from `layer_id` via the project's layer order (resolve once per tick into a scratch map, or reuse an existing layer-id→index lookup if one exists),
   - `start_beat`: `launch_beat + iteration * length_beats + clip.start_beat` **in global beats** — this makes the scheduler's existing loop/progress math correct without modification,
   - `duration_beats`, `is_looping`, `is_video` from the inner clip.
5. Mark these refs as session refs the same way live-slot refs are marked (see `ActiveClipRef::is_live_slot()` — add a parallel discriminant or a source enum; follow whatever shape `is_live_slot` uses).

**Sequence wrap = stop + start.** When `iteration` increments and the same inner `clip_id` spans the boundary, the clip must hard-restart from its `in_point` (Ableton loop semantics). The global `start_beat` computed in step 4 changes when `iteration` changes, but `compute_sync` diffs by `clip_id`, so it will NOT see a restart. Handle explicitly: `SessionRuntime` tracks `last_iteration` per playing slot; on change, if the active inner clip_id is unchanged, emit stop+start for it (engine-level, same calls `sync_clips_to_time` makes from `to_stop`/`to_start`).
6. Clip start resolution: `sync_clips_to_time`'s start loop (engine.rs:1138) resolves `TimelineClip` by source; add the third arm — resolve from `SessionGrid` via `slot_lookup` + clip scan.

Statelessness rule: everything except `playing`/`pending`/`session_override`/`last_iteration` is derived from `current_beat` every tick. No per-tick mutation of launch state. Scrub-safe by construction.

### Seek behavior

Large seeks currently clear live slots (`clear_on_seek`, live_clip_manager.rs:209). Session slots are the opposite: they are beat-anchored and stateless, so **seeking does not stop them** — `elapsed` just changes. This is correct for the Ableton-sync case (timeline jumps while session clips keep looping). Pending launches DO get retargeted: on seek, recompute each `target_beat` to the next quantize boundary after the new position.

### Transport stop

Transport stop stops all session playback and clears `pending` (Ableton behavior). `session_override` is NOT cleared — layers stay detached until explicit Back to Arrangement.

## 5. Launch semantics

New `ContentCommand` variants (NOT undoable `Command`s — launches are performance gestures like MIDI triggers, they never touch `UndoRedoManager`):

```rust
ContentCommand::SessionLaunchSlot { layer_id, scene_id }
ContentCommand::SessionStopSlot { layer_id }
ContentCommand::SessionLaunchScene { scene_id }
ContentCommand::SessionStopAll
ContentCommand::SessionBackToArrangement { layer_id: Option<LayerId> }  // None = all layers
ContentCommand::SessionSetQuantize { beats: Beats }
```

Semantics (all Ableton-standard):

- **Launch slot**: enqueue `PendingSlotLaunch` at the next quantize boundary `target = ceil(current_beat / q) * q` (if exactly on a boundary, launch now). Replaces any pending launch for that layer. At `target`, it replaces the layer's `PlayingSlot` and sets `session_override` for that layer.
- **Empty slot cells don't exist** — the grid is sparse. Launching a (layer, scene) with no slot = stop button for that layer in that scene row: enqueue a quantized stop. This matches Ableton (empty cell click stops the track's clip).
- **Launch scene**: for every layer with a slot in that scene → launch it; for every layer currently playing a session slot but with NO slot in the scene → quantized stop (Ableton default "stop other tracks" behavior). Layers never session-launched are untouched.
- **Stop slot**: quantized stop; `session_override` stays set (layer goes black, does NOT fall back to arrangement — Ableton behavior).
- **Back to arrangement**: immediate (not quantized); clears `session_override` (and stops the playing slot) for the layer(s). Timeline clips resume via normal sync on the next tick.
- Quantize `0` = launch immediately.

MIDI/OSC mapping of these commands: out of scope here — they ride the existing mapping infra as new mappable actions, same as any other `ContentCommand`. Ableton scene-launch sync = the AbletonOSC bridge translating scene-fired events into `SessionLaunchScene`; the command surface is the whole hook (design the id mapping in the Ableton-sync project, not here).

## 6. Arrangement suppression

A layer in `session_override` plays ONLY session content. In `query_active_timeline_clips` (engine.rs:838), skip layers whose `layer_id` is in `session_override` (pass the set in, or filter the scratch after). That's the entire integration — the scheduler diff then stops arrangement clips and starts session clips with no further changes.

Rendering, compositing, effects, LED output are untouched: they consume active clips and have no concept of where a clip came from. Layer effects still apply to session-launched content on that layer — this is load-bearing and falls out free.

## 7. Grid editing + timeline conversion (`manifold-editing`)

Normal undoable `Command`s (`crates/manifold-editing/src/commands/`), one file `session_commands.rs`:

- `AddSceneCommand`, `RemoveSceneCommand`, `RenameSceneCommand`, `ReorderSceneCommand`
- `SetSlotCommand { layer_id, scene_id, slot: Option<SessionSlot> }` (set/replace/clear; stores prior for undo)
- `CaptureRangeToSceneCommand { start_beat, end_beat, scene_id: Option<SceneId> }` — the timeline→session converter:
  1. For each leaf non-audio layer with clips intersecting `[start, end)`: clone intersecting clips, trim to the range (reuse the existing clip split/trim math — clip head-trim must advance `in_point` by the trimmed beats converted to seconds; find and reuse the existing split command's logic, do not reimplement), rebase `start_beat -= range_start`.
  2. Build `ClipSequence { length_beats: end - start, clips }` per layer → `SessionSlot`.
  3. `None` scene_id = append a new scene named from the nearest `TimelineMarker` at/before `start_beat`, else "Scene N".
- `PasteSlotToTimelineCommand { layer_id, scene_id, at_beat }` — reverse: clone sequence clips, `start_beat += at_beat`, insert into the layer lane (`enforce_non_overlap` handles collisions). Fresh clip ids via the existing `duplicated()` path (clip.rs:183) — REQUIRED both directions; shared `ClipId`s across timeline and grid would collide in `clip_lookup` and effect resolution.

`CaptureRangeToSceneCommand` also clones with `duplicated()` — a slot never shares clip ids with the lane it came from.

**Session recording** (v1-minimal): when transport is recording (the `TempoRecorder`/phantom-commit infra), each session slot start/stop writes its resolved clips into the arrangement exactly like phantom-clip commit does today. Reuse that path; the only new code is sourcing the committed clip from a `PlayingSlot` instead of a MIDI note. If the phantom commit path resists reuse, cut recording from v1 and file it — do not build a parallel recorder.

## 8. UI (last phase)

New dock panel "Session" — grid of layers (columns, arrangement order) × scenes (rows). Per cell: slot name/color, play state (stopped / pending-flash / playing with loop progress), click = launch, empty cell click = stop. Scene-launch button per row, Back to Arrangement per column + global. Reuse timeline header identity colors (`prefer-high-saturation-identity-colors`). Follow the existing dock/panel infra from the graph-editor redesign; no new UI primitives expected. Drag interactions (timeline↔grid) are v2; v1 conversion happens via commands on selection/marker range.

## 9. What does NOT change

- `sync_clips_to_time` remains sole playback authority; session adds an input, not an authority.
- Primary time model stays beats; slots quantize and resolve in beats; `in_point` stays `Seconds`.
- `EditingService` remains sole mutation gateway (grid edits). Launches are engine-level `ContentCommand`s, matching MIDI phantom triggers.
- No new shared state; `SessionRuntime` lives on the content thread inside the engine. UI reads play-state via `ContentState` snapshots (add pending/playing per layer + scene list to the snapshot).
- Hot path: per-tick resolution is scratch-buffer only; `AHashMap`/`AHashSet` keyed by ids; no allocation.

## 10. Phasing

| Phase | Scope | Test gate |
|---|---|---|
| P1 | `session.rs` model + `Project.session` + serde | `manifold-core --lib` round-trip tests; Liveschool fixture byte-identical re-save |
| P2 | `SessionRuntime`, third ref source, suppression, `ContentCommand` variants | `manifold-playback --lib`: resolution math (local/iteration/wrap), quantize targeting, scene launch/stop matrix, seek/stop behavior — all headless, no GPU |
| P3 | `session_commands.rs` incl. capture/paste | `manifold-editing --lib`: undo round-trips; capture trim math vs split-command parity |
| P4 | Grid panel + `ContentState` plumbing | headless PNG verification (`reference_ui_headless_png_verification`) |
| P5 | Session recording via phantom-commit reuse | record a launched sequence, verify arrangement clips match resolution math |

P2 is the risk concentration: the wrap-restart rule (§4) and suppression interaction with solo/mute. Everything else is mechanical.

Full `cargo test --workspace` gates P2 (engine/scheduler are infrastructure per the testing-scope rule); P1/P3/P4 use focused crate tests.

## 11. Decided questions (do not reopen)

- Real per-layer session grid, not quantized-jump-on-one-playhead. It's a product surface for timeline-free users.
- Slots are per-layer; scenes are rows. 1:1 Ableton mapping, keyed by `LayerId` not index.
- Sequence wrap hard-restarts clips from `in_point` (stateless resolution wins over cross-loop continuity).
- Launches are not undoable; grid edits are.
- Seek does not kill session playback; transport stop does.
- `session_override` persists until explicit Back to Arrangement, including after a slot stops.
