# MANIFOLD Rust Port — Phase 2 Sub-Plan

## Context

Phase 1 proved the architecture (4 crates, 33 tests, real .manifold files). Phase 2 completes the domain core so it's a fully functional library. This is the detailed execution plan — every step can run automated without user input.

**Master roadmap:** `/Users/peterkiemann/MANIFOLD - Rust/docs/ROADMAP.md`

**C# references (read in planning):**
- `Assets/Scripts/Editing/ClipCommands.cs` (802 LOC) — 14 commands + CompositeCommand
- `Assets/Scripts/Editing/SettingsCommands.cs` (818 LOC) — 20 commands (BPM, resolution, effects, drivers, envelopes)
- `Assets/Scripts/Editing/EffectGroupCommands.cs` (344 LOC) — 7 group commands
- `Assets/Scripts/Editing/LayerGroupCommands.cs` (177 LOC) — 2 layer group commands
- `Assets/Scripts/UI/Timeline/EditingService.cs` (1,476 LOC) — mutation gateway
- `Assets/Scripts/Playback/LiveClipManager.cs` (971 LOC) — phantom clip lifecycle

---

## Step 1: Core Model Helpers

Add methods to data models needed by commands.

**`manifold-core/src/timeline.rs`:**
- `insert_layer(&mut self, index: usize, layer: Layer)` — inserts, reindexes
- `remove_layer(&mut self, index: usize) -> Layer` — removes, reindexes
- `insert_existing_layer(&mut self, index: usize, layer: Layer)` — for redo
- `replace_layer_order(&mut self, new_order: Vec<Layer>)` — atomic reorder, reindexes all
- `add_layer_default(&mut self) -> usize` — adds empty video layer (for LiveClipManager's `while layers.count <= index`)

**`manifold-core/src/layer.rs`:**
- `insert_clip_at(&mut self, index: usize, clip: TimelineClip)`
- `find_clip_index(&self, clip_id: &str) -> Option<usize>`
- `snapshot_gen_params(&self) -> Vec<f32>`
- `snapshot_gen_drivers(&self) -> Vec<ParameterDriver>`
- `snapshot_gen_envelopes(&self) -> Vec<ParamEnvelope>`
- `change_generator_type(&mut self, new_type: GeneratorType)`
- `restore_generator_state(&mut self, old_type, params, drivers, envelopes)`
- `set_gen_param_base(&mut self, index: usize, value: f32)`

**`manifold-core/src/effects.rs`:**
- `EffectInstance::set_base_param(&mut self, index: usize, value: f32)` — ensures vec capacity

**`manifold-core/src/clip.rs`:**
- `TimelineClip::clone_at(&self, new_start_beat: f32) -> TimelineClip` — clone with new ID and start beat (already exists, verify it works for split)
- `TimelineClip::new_video(video_clip_id, layer_index, start_beat, duration_beats, in_point) -> Self`
- `TimelineClip::new_generator(gen_type, layer_index, start_beat, duration_beats) -> Self`

**`manifold-core/src/settings.rs`:**
- `ProjectSettings::get_quantize_interval_beats(&self) -> f32`
- `ProjectSettings::quantize_beat(&self, beat: f32) -> f32`

**`manifold-core/src/tempo.rs`:**
- `TempoMap::clear(&mut self)`
- `TempoMap::clone_points(&self) -> Vec<TempoPoint>`
- `TempoMap::get_sorted_points(&self) -> &[TempoPoint]`

**`manifold-core/src/selection.rs`:**
- `SelectionRegion` already exists, add `set_region` / `clear_region` methods

**Verify:** `cargo test --workspace` — all 33 existing tests pass + new unit tests for helpers.

---

## Step 2: EffectTarget + Resolve

**New: `manifold-editing/src/commands/effect_target.rs`**

```rust
#[derive(Debug, Clone)]
pub enum EffectTarget {
    Clip { clip_id: String },
    Layer { layer_index: usize },
    Master,
}
```

`resolve_effects_mut(project: &mut Project, target: &EffectTarget) -> Option<(&mut Vec<EffectInstance>, &mut Vec<EffectGroup>)>`

This function resolves the target to mutable references. For `Clip`, it finds the clip by ID and returns `(&mut clip.effects, &mut clip.effect_groups)`. For `Layer`, returns the layer's. For `Master`, returns the project settings'.

---

## Step 3: Commands — Clip (6 new)

All in `manifold-editing/src/commands/clip.rs` (add to existing file).

**Rust pattern**: Commands store identifiers + old/new values. Resolve via `&mut Project` at execute/undo time. No object references.

| Command | Fields | Execute | Undo |
|---------|--------|---------|------|
| `SwapVideoCommand` | clip_id, old/new video_clip_id, old/new in_point, old/new duration_beats | Set new values on clip | Restore old values |
| `SlipClipCommand` | clip_id, old/new in_point | Set new in_point | Restore old |
| `ClipEffectsCommand` | clip_id, old/new (invert, loop, loop_dur, tx, ty, scale, rot) | Set all new | Restore all old |
| `ChangeClipLoopCommand` | clip_id, old/new (is_looping, loop_duration_beats) | Set new | Restore old |
| `ChangeClipRecordedBpmCommand` | clip_id, old/new recorded_bpm | Set new | Restore old |
| `SplitClipCommand` | clip_id, tail_clip: TimelineClip, layer_index, old/new duration_beats | Trim original, add tail to layer | Remove tail, restore original duration |

---

## Step 4: Commands — Layer (5, replace placeholder)

Replace `PlaceholderLayerCommand` in `manifold-editing/src/commands/layer.rs`.

| Command | Fields | Key Logic |
|---------|--------|-----------|
| `AddLayerCommand` | layer: Option<Layer>, name, layer_type, gen_type, insert_index, parent_group_id | First execute creates layer, redo re-inserts same object |
| `DeleteLayerCommand` | layer: Layer, layer_index | Execute removes, undo re-inserts at index |
| `ReorderLayerCommand` | old_order: Vec<Layer>, new_order: Vec<Layer>, old/new parent_ids: HashMap<String,Option<String>> | Atomic reorder via `replace_layer_order` |
| `GroupLayersCommand` | selected_layer_ids: HashSet<String>, group_layer: Option<Layer>, original_order: Vec<Layer> | Creates group layer, reparents children, reorders |
| `UngroupLayersCommand` | group_layer: Layer, child_layer_ids: Vec<String>, original_order: Vec<Layer> | Removes group, clears parent_ids |

**Borrow checker note**: Layer commands clone the Layer for snapshot (Layer implements Clone). On undo, they `replace_layer_order` with the saved snapshot.

---

## Step 5: Commands — Settings (10 new)

In `manifold-editing/src/commands/settings.rs` (add to existing file that has ChangeBpm and ChangeResolution).

All simple old/new value swaps:
- `ChangeQuantizeModeCommand` (settings.quantize_mode)
- `ChangeFrameRateCommand` (settings.frame_rate)
- `ChangeLayerMidiNoteCommand` (layer_index, layer.midi_note)
- `ChangeLayerBlendModeCommand` (layer_index, layer.default_blend_mode)
- `ChangeLayerOpacityCommand` (layer_index, layer.opacity)
- `ChangeGeneratorParamsCommand` (layer_index, old/new Vec<f32> — calls `set_gen_param_base` in loop)
- `ChangeGeneratorTypeCommand` (layer_index, old/new type + snapshot params/drivers/envelopes)
- `ChangeMasterOpacityCommand` (settings.master_opacity)
- `RestoreRecordedTempoLaneCommand` (old_bpm, old/new tempo points — full implementation, not stub)
- `ClearTempoMapCommand` (old tempo points, current_bpm — full implementation, not stub)

---

## Step 6: Commands — Effects (5, replace placeholder)

Replace `PlaceholderEffectCommand` in `manifold-editing/src/commands/effects.rs`.

All use `EffectTarget` to route to clip/layer/master effect lists.

| Command | Key Logic |
|---------|-----------|
| `AddEffectCommand` | target + effect + insert_index. Execute: insert at index. Undo: remove. |
| `RemoveEffectCommand` | target + effect + removed_index. Execute: remove. Undo: insert at index. |
| `ReorderEffectCommand` | target + from/to index. Execute: remove+insert with adjustment. Undo: reverse. |
| `ToggleEffectCommand` | target + effect_index + old/new enabled. |
| `ChangeEffectParamCommand` | target + effect_index + param_index + old/new value. |

**Note:** C# commands hold `IList<EffectInstance>` directly. In Rust, commands store `EffectTarget` and resolve at execute time via `resolve_effects_mut`.

---

## Step 7: Commands — Effect Groups (7 new)

**New: `manifold-editing/src/commands/effect_groups.rs`**

| Command | Key Logic |
|---------|-----------|
| `GroupEffectsCommand` | target + grouped_effect_indices + group_name. Makes effects contiguous, assigns group_id. Undo restores original positions + group_ids. |
| `UngroupEffectsCommand` | target + group_id. Clears group_id on members, removes group. Undo restores. |
| `ToggleGroupCommand` | target + group_id + old/new enabled. |
| `RenameGroupCommand` | target + group_id + old/new name. |
| `ChangeGroupWetDryCommand` | target + group_id + old/new wet_dry. |
| `ReorderRackCommand` | target + group_id + target_insert_index + original_indices. Moves contiguous block. |
| `MoveEffectToRackCommand` | target + effect_index + old/new group_id + old/new index. |

---

## Step 8: Commands — Drivers (6 new)

**New: `manifold-editing/src/commands/drivers.rs`**

All operate on a driver list (resolved via EffectTarget + driver_index or via layer generators).

| Command | Fields |
|---------|--------|
| `AddDriverCommand` | target/layer, driver: ParameterDriver |
| `ToggleDriverEnabledCommand` | target/layer, driver_index, old/new enabled |
| `ChangeDriverBeatDivCommand` | target/layer, driver_index, old/new BeatDivision |
| `ChangeDriverWaveformCommand` | target/layer, driver_index, old/new DriverWaveform |
| `ToggleDriverReversedCommand` | target/layer, driver_index, old/new reversed |
| `ChangeTrimCommand` | target/layer, driver_index, old/new min, old/new max |

**Driver target**: Drivers live on `EffectInstance.drivers` or `Layer.gen_params.drivers`. Need a `DriverTarget` enum:
```rust
pub enum DriverTarget {
    Effect { effect_target: EffectTarget, effect_index: usize },
    GeneratorParam { layer_index: usize },
}
```

---

## Step 9: Commands — Envelopes (7 new)

**New: `manifold-editing/src/commands/envelopes.rs`**

| Command | Location |
|---------|----------|
| `AddParamEnvelopeCommand` | clip_id, envelope |
| `RemoveParamEnvelopeCommand` | clip_id, envelope, index |
| `ChangeParamEnvelopeCommand` | clip_id, env_index, old/new (A,D,S,R,target,enabled) |
| `ChangeEnvelopeADSRCommand` | clip_id, env_index, old/new (A,D,S,R) |
| `ChangeParamEnvelopeTargetCommand` | clip_id, env_index, old/new target |
| `AddLayerEnvelopeCommand` | layer_index, envelope |
| `RemoveLayerEnvelopeCommand` | layer_index, envelope, index |

Plus generic versions:
- `AddEnvelopeCommand` (list target, envelope)
- `ToggleEnvelopeEnabledCommand` (list target, env_index, old/new enabled)

---

## Step 10: Commands — Selection (1 new)

**New: `manifold-editing/src/commands/selection.rs`**

`SetSelectionRegionCommand`: stores old/new `SelectionRegion`. Operates on a `SelectionState` struct (not UI — kept in domain).

---

## Step 11: Command Module Wiring

**`manifold-editing/src/commands/mod.rs`** — update to export all new modules:
```rust
pub mod clip;
pub mod layer;
pub mod settings;
pub mod effects;
pub mod effect_target;
pub mod effect_groups;
pub mod drivers;
pub mod envelopes;
pub mod selection;
```

---

## Step 12: EditingService

**New: `manifold-editing/src/service.rs`**

Core struct:
```rust
pub struct EditingService {
    undo_manager: UndoRedoManager,
    clipboard: Vec<ClipboardEntry>,
    data_version: u64,
    saved_at_version: u64,
    cmd_scratch: Vec<Box<dyn Command>>,
    id_scratch: Vec<String>,
    clip_scratch: Vec<TimelineClip>,
}

struct ClipboardEntry {
    source_clip: TimelineClip,
    beat_offset: f32,
    layer_offset: i32,
}
```

**Host trait** (replaces C# UIState/CoordinateMapper/PlaybackController):
```rust
pub trait EditingHost {
    fn current_beat(&self) -> f32;
    fn seconds_per_beat(&self) -> f32;
    fn grid_interval_beats(&self) -> f32;
    fn floor_beat_to_grid(&self, beat: f32) -> f32;
    fn snap_beat_to_grid(&self, beat: f32) -> f32;
    fn request_clip_sync(&mut self);
    fn mark_compositor_dirty(&mut self);
}
```

**Methods (in implementation order):**

1. **Mutation gateway**: `execute()`, `record()`, `undo()`, `redo()`, `set_project()`, `mark_clean()`, `is_dirty()`, `data_version()`
2. **Clip lookup**: `find_clip_by_id()` — delegates to `Timeline::find_clip_by_id`
3. **Selection**: `get_clips_in_region()`, `get_effective_selected_clips()` (takes region + selected IDs as params, no UIState dependency)
4. **Overlap enforcement**: `enforce_non_overlap(project, placed_clip, ignore_ids) -> Vec<Box<dyn Command>>` — 4 cases from C# (covers-both → delete, covers-start → trim start, covers-end → trim end, middle → trim + split)
5. **Region helpers**: `split_clip_at_beat()`, `split_clips_at_region_boundaries()`, `trim_clip_to_region()`
6. **Clipboard**: `copy_clips()` (with beat/layer offset), `paste_clips()` → `PasteResult`, `cut_clips()`
7. **Duplicate**: `duplicate_selected_clips()` (region-aware: shifts region forward)
8. **Delete**: `delete_selected_clips()` (splits at boundaries, deletes interior)
9. **Create**: `create_clip_at_position(beat, layer_index)`
10. **Nudge**: `nudge_selected_clips(beat_delta)`
11. **Extend/shrink**: `extend_by_grid_step()`, `shrink_by_grid_step()`
12. **Layer ops**: `group_selected_layers()`, `delete_selected_layers()`, `move_clip_to_layer()`
13. **Split**: `split_selected_at_beat(beat)`, `split_for_region_move(region)`

**Not ported** (UI-specific or audio-dependent):
- `OnClipSelected` (pure UI routing)
- `SelectRegionTo`, `SelectAllClips` (UI state)
- `OnWaveformDrag*` (audio controller dependency)
- `SetAudioStartBeat` (audio dependency)
- `ToggleMuteSelectedClips` (trivial, but depends on UI selection — add as utility)

---

## Step 13: LiveClipManager

**New: `manifold-playback/src/live_clip_manager.rs`**

```rust
pub struct LiveClipManager {
    live_slots: HashMap<i32, TimelineClip>,
    live_slots_list: Vec<(i32, TimelineClip)>,  // parallel iteration (zero-alloc)
    live_slot_clip_ids: HashSet<String>,
    pending_by_clip_id: HashMap<String, PendingLiveLaunch>,
    pending_by_layer: HashMap<i32, String>,
    pending_by_tick: BTreeMap<i32, Vec<String>>,
    activation_buffer: Vec<String>,
    last_live_trigger_at: f64,
}

struct PendingLiveLaunch {
    clip: TimelineClip,
    layer_index: i32,
    target_tick: i32,
    midi_note: i32,
}
```

**`LiveClipHost` trait:**
```rust
pub trait LiveClipHost {
    fn current_project(&self) -> Option<&Project>;
    fn current_beat(&self) -> f32;
    fn current_time(&self) -> f32;
    fn is_recording(&self) -> bool;
    fn is_playing(&self) -> bool;
    fn get_bpm_at_beat(&self, beat: f32) -> f32;
    fn get_beat_snapped_beat(&self) -> f32;
    fn get_current_absolute_tick(&self) -> i32;
    fn stop_clip(&mut self, clip_id: &str);
    fn mark_sync_dirty(&mut self);
    fn mark_compositor_dirty(&mut self);
    fn register_clip_lookup(&mut self, clip_id: &str, clip: &TimelineClip);
    fn record_command(&mut self, cmd: Box<dyn Command>);
    fn beat_to_timeline_time(&self, beat: f32) -> f32;
}
```

**Methods:**
1. **Lifecycle**: `clear_all()`, `clear_on_seek(seek_delta)`, `notify_clip_stopped(clip_id)`, `is_live_slot_clip(clip_id)`
2. **Quantize math** (pure functions): `compute_duration_beats()`, `compute_snap_beat_from_tick()`, `compute_held_beats_from_ticks()`, `get_quantize_interval_ticks()`
3. **Pending queue**: `queue_pending()`, `remove_pending()`, `try_get_pending_for_commit()`
4. **Activation**: `activate_live_slot_now()`, `activate_due_pending_launches()`
5. **Trigger**: `trigger_live_clip()`, `trigger_live_generator_clip()`
6. **Commit**: `commit_live_clip(project: &mut Project, host: &mut dyn LiveClipHost, ...)`

**Commit takes `&mut Project` explicitly** — key borrow checker pattern. The host trait provides read-only state, project mutation is explicit parameter.

**Not ported** (runtime-specific):
- `AppendLivePrewarmCandidates` (depends on VideoClip/VideoLibrary for prewarming — Phase 6)
- `RecordingProvenance` tracking (TempoRecorder dependency — defer to audio pipeline)
- `PerfLogger` calls (diagnostics — not needed for domain correctness)

---

## Step 14: SyncSource Trait

**New: `manifold-playback/src/sync_source.rs`**
```rust
pub trait SyncSource {
    fn is_enabled(&self) -> bool;
    fn display_name(&self) -> &str;
    fn enable(&mut self);
    fn disable(&mut self);
    fn toggle(&mut self) { if self.is_enabled() { self.disable() } else { self.enable() } }
}
```

No implementations. Trait definition only for Phase 7.

---

## Step 15: Playback Integration

**Modify `manifold-playback/src/engine.rs`:**
- Add `live_clip_manager: LiveClipManager` field
- In `tick()`: call `live_clip_manager.activate_due_pending_launches()` before clip scheduling
- Include `live_slots_list` clips in the active clips set for compositor

**New trait in `manifold-playback/src/engine.rs`:**
```rust
pub trait PlaybackNotifier {
    fn mark_compositor_dirty(&mut self);
    fn notify_generator_type_changed(&mut self, layer_index: usize);
}
```

**Update `manifold-playback/src/lib.rs`:** export `live_clip_manager`, `sync_source`

---

## Step 16: Test Suite

**Target: ~100 tests** (up from 33).

**`manifold-editing/tests/command_roundtrips.rs`** (~40 tests):
- One test per command type: create project fixture, execute, assert change, undo, assert restored
- Group the tests by category: clip, layer, settings, effects, groups, drivers, envelopes

**`manifold-editing/tests/service_integration.rs`** (~15 tests):
- `overlap_covers_both_deletes` — place clip over existing, verify delete
- `overlap_covers_start_trims` — place clip overlapping start, verify trim
- `overlap_covers_end_trims` — place clip overlapping end, verify trim
- `overlap_splits_middle` — place clip in middle, verify trim + new tail
- `copy_paste_roundtrip` — copy clips, paste at offset, verify new IDs
- `paste_preserves_relative_offsets` — 3 clips, paste, verify offsets
- `duplicate_region_shifts_forward` — duplicate selection, verify region shift
- `delete_region_splits_at_boundaries` — delete with boundary-straddling clips
- `create_clip_at_position` — verify snapped beat + overlap enforcement
- `nudge_selected_clips` — verify beat delta applied
- `multi_step_undo_redo` — 5 operations, undo all, redo all
- `undo_count_matches` — verify data_version increments
- `split_at_beat` — verify original trimmed, tail created
- `extend_shrink_by_grid` — verify duration change by grid step
- `move_clip_to_layer` — verify layer transfer

**`manifold-playback/tests/live_clip.rs`** (~10 tests):
- `trigger_creates_phantom_clip`
- `commit_with_recording_adds_to_timeline`
- `commit_without_recording_discards`
- `pending_launch_queue_activates_at_tick`
- `clear_on_seek_removes_all_slots`
- `quantize_snap_beat_from_tick`
- `quantize_duration_beats`
- `multiple_layers_independent_slots`
- `held_beats_from_ticks_with_quantize`
- `second_trigger_on_same_layer_replaces`

---

## Verification

1. `cargo build --workspace` — zero errors
2. `cargo test --workspace` — all ~100 tests pass
3. `cargo clippy --workspace` — zero warnings
4. All 33 Phase 1 tests unchanged and passing
5. Every new command has at least one undo roundtrip test
6. EditingService overlap enforcement tested against all 4 cases
7. LiveClipManager tested with mock host

---

## Execution Order

Steps 1-11 → 12 → 13-14 → 15 → 16

Steps 1-11 are the foundation (model helpers + commands).
Step 12 (EditingService) uses all commands.
Steps 13-14 (LiveClipManager + SyncSource) are independent of EditingService.
Step 15 wires LiveClipManager into the engine.
Step 16 tests everything.
