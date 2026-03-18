# Tier 1 Fix Plan: `manifold-editing` + `manifold-playback` Parity Remediation

**Status: COMPLETE** — Implemented 2026-03-18, commit `be4c7f0`

**Generated:** 2026-03-18 from line-by-line audit of all Unity Editing/*.cs, Playback/*.cs, Sync/*.cs against Rust manifold-editing/src/*.rs and manifold-playback/src/*.rs

**Methodology:** Every fix references exact Unity source file and line numbers. The implementing agent MUST read the Unity source — not this document — as the source of truth.

**Dependency:** Tier 0 fixes (manifold-core) should be completed first, especially `IParamSource`, `IEffectContainer`, and `ParamDef` restructuring.

---

# PART A: `manifold-editing`

---

## Phase 1: Command Infrastructure

### 1A. Add CompositeCommand

**Unity source:** `ClipCommands.cs` lines 777-801
**Rust file:** `command.rs`

CompositeCommand is entirely missing. It groups multiple commands into a single undoable unit with **reverse-order undo**.

```
struct CompositeCommand {
    description: String,
    commands: Vec<Box<dyn Command>>,
}
// Execute: iterate forward
// Undo: iterate REVERSE
```

This is used by multi-operation editing (drag+drop, region operations, etc.).

### 1B. Fix UndoRedoManager capping logic

**Unity source:** `UndoRedoManager.cs` lines 31-32 (PushUndo called from both Execute and Redo)
**Rust file:** `undo.rs` lines 52-54

**Bug:** Rust only caps the undo stack during `redo()`, not during `execute()`. Unity caps in `PushUndo()` which is called from both paths.

**Fix:** Move the capping logic into a shared helper (equivalent to Unity's `PushUndo()`) called from both `execute()` and `redo()`.

### 1C. Add ILayerLifecycleCallbacks trait

**Unity source:** `ILayerLifecycleCallbacks.cs` lines 10-14
**Rust file:** New trait, likely in `command.rs` or a new `callbacks.rs`

```rust
pub trait LayerLifecycleCallbacks {
    fn on_layer_added(&mut self, layer: &Layer);
    fn on_layer_removed(&mut self, layer: &Layer);
}
```

This is used by layer add/delete commands to notify the UI/compositing layer for OSC registration, effect cleanup, etc.

---

## Phase 2: Missing Command Classes

### 2A. Fix ChangeParamEnvelopeCommand split

**Unity source:** `SettingsCommands.cs` lines 715-770
**Rust files:** `commands/envelopes.rs`

Unity has ONE command that atomically captures/restores ALL envelope state (attack, decay, sustain, release, targetNormalized, enabled). Rust incorrectly split this into 3 separate commands:
- `ChangeEnvelopeADSRCommand`
- `ChangeEnvelopeTargetNormalizedCommand`
- `ToggleEnvelopeEnabledCommand`

**Fix:** Create a single `ChangeParamEnvelopeCommand` that matches Unity's atomic capture pattern. The existing split commands can remain as convenience wrappers but the atomic version must exist.

### 2B. Review ChangeEnvelopeRoutingCommand

**Rust file:** `commands/envelopes.rs` lines 130-175

This command has NO Unity equivalent. It changes envelope target effect type and param index. Verify whether this is an intentional extension or synthesized code that should be removed.

### 2C. Review RescaleBeatsForBpmChangeCommand

**Rust file:** `commands/settings.rs` lines 501-556

This command has NO Unity equivalent. Verify whether this is intentional or synthesized.

### 2D. Fix MoveClipCommand generator type capture timing

**Unity source:** `ClipCommands.cs` line 32 (captures in constructor)
**Rust file:** `commands/clip.rs` lines 29-34 (captures on first execute)

Unity captures `oldGeneratorType` at command construction time. Rust defers capture to `execute()`. If the clip is modified between creation and execution, Rust may capture wrong state.

**Fix:** Capture generator type in the constructor (equivalent to `new()`), not on first execute.

### 2E. Verify UngroupLayersCommand undo re-parenting

**Unity source:** `LayerGroupCommands.cs` lines 153-174
**Rust file:** `commands/layer.rs` lines 240-243

Unity's undo explicitly re-parents children to the group layer. Rust relies on `replace_layer_order()` restoring parent IDs implicitly. Verify that `Layer::clone()` preserves `parent_layer_id` — if not, undo silently fails.

---

## Phase 3: EditingService High-Level Operations

### Architecture Note

Unity's `EditingService` is tightly coupled to `UIState` — it reads selection, cursor, layer state directly from UI. Rust's `EditingService` is stateless — all methods take explicit parameters. This is an intentional architectural difference.

However, many high-level operations are completely missing and need to be added either to `EditingService` or to the app layer that orchestrates it.

### 3A. Missing Selection Operations

**Unity source:** `EditingService.cs`

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `get_effective_selected_clips()` | 192-206 | Returns clips from region if active, else individual selection |
| `select_region_to(beat, layer_index)` | 216-262 | Shift+Click anchor-based region selection |
| `select_all_clips()` | 264-276 | Select all clips on all layers |
| `update_region_from_clip_selection()` | 283-303 | Compute region bounds from individual selection |

### 3B. Missing Compound Operations

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `cut_selected_clips()` | 481-544 | Copy + delete in region or individual mode |
| `split_clips_for_region_move()` | 1135-1143 | Returns compound split commands + interior clips |
| `split_selected_clips_at_playhead()` | 1149-1197 | Public split-at-playhead wrapper |
| `delete_selected_layers()` | 365-412 | Layer deletion with single-layer guard |
| `toggle_mute_selected_clips()` | 418-449 | Mute toggle using MuteClipCommand |
| `group_selected_layers()` | 1316-1349 | Layer grouping using GroupLayersCommand |

### 3C. Missing UI-Interaction Operations

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `on_clip_selected(clip)` | 134-152 | Shows inspector panel for selected clip |
| `on_waveform_drag_delta_beats()` | 1355-1423 | Waveform-specific drag state management |
| `on_waveform_drag_end()` | 1429-1464 | Waveform drag commit |
| `get_current_grid_step()` | 949-954 | Returns grid interval in beats |

### 3D. Fix shrink_clips_by_grid min duration

**Unity source:** `EditingService.cs` line 861
**Rust file:** `service.rs` line 761

Unity uses fixed `minDuration = 0.25f`. Rust uses `grid_step.max(0.25)` which is MORE restrictive when grid_step > 0.25.

**Fix:** Use fixed `0.25` minimum, matching Unity.

### 3E. Fix duplicate_clips missing region restoration

**Unity source:** `EditingService.cs` lines 743-758
**Rust file:** `service.rs` lines 535-589

Unity's `DuplicateSelectedClips()` restores region selection after duplication (Ableton-style behavior). Rust's `duplicate_clips()` doesn't handle this.

**Fix:** Add region restoration logic matching Unity lines 743-758.

---

## Phase 4: EffectClipboard Architecture

### 4A. Verify clipboard singleton vs instance

**Unity source:** `EffectClipboard.cs` lines 11-56 (static singleton)
**Rust file:** `clipboard.rs` lines 4-45 (instance struct)

Unity uses a static singleton. Rust uses an instance-based struct requiring callers to pass mutable reference. This changes the calling convention throughout the codebase. Document this as an approved divergence or port to a static/global pattern.

---

# PART B: `manifold-playback`

---

## Phase 5: PlaybackEngine Critical Fixes

### 5A. Make sync_clips_to_time() PUBLIC

**Unity source:** `PlaybackEngine.cs` line 998 (`public void SyncClipsToTime()`)
**Rust file:** `engine.rs` line 643 (private)

This is the **sole idempotent authority for playback state** per CLAUDE.md. It MUST be public.

### 5B. Make process_pending_pauses() PUBLIC

**Unity source:** `PlaybackEngine.cs` line 722 (`public void ProcessPendingPauses()`)
**Rust file:** `engine.rs` line 608 (private)

Called from PlaybackController.Update(). Must be public.

### 5C. Add missing public methods

**Unity source:** `PlaybackEngine.cs`

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `invalidate_prewarm()` | 418 | Clear prewarm cache |
| `sync_clip_loop_state(clip)` | 1111 | Update clip looping state |
| `clear_pending_pauses()` | 1171 | Clear pending pause list |
| `get_timeline_active_clips_at_current_beat()` | 1031 | Return active clips (currently private with different signature) |

### 5D. Fix start_clip() signature

**Unity source:** `PlaybackEngine.cs` line 576 (`public void StartClip(TimelineClip timelineClip)`)
**Rust file:** `engine.rs` line 539 (adds `realtime_now: f64`)

Rust added a `realtime_now` parameter not in Unity. Verify all call sites pass correct values — currently `sync_clips_to_time()` passes `0.0` as fallback which may cause timing issues.

### 5E. Add missing callback delegates

**Unity source:** `PlaybackEngine.cs` lines 248-275

Missing from Rust:
- `LogError` callback (only Log and LogWarning exist)
- `ReplenishWarmCache` callback
- `OnDriftCorrected` callback
- `BeatSnappedBeatResolver` callback
- `AbsoluteTickResolver` callback
- `RecordCommandDelegate` callback
- `ShowDebugLogs` flag

These are CRITICAL for live MIDI recording, drift correction feedback, and debug instrumentation.

### 5F. Add missing scratch buffers

**Unity source:** `PlaybackEngine.cs` lines 195-197, 201, 211-214

Missing from Rust:
- `to_pause_list: Vec<String>` (line 195)
- `became_ready_list: Vec<String>` (line 196)
- `clips_to_stop_drift: Vec<String>` (line 197)
- `cached_get_media_length: HashMap<String, f32>` (line 201)
- `LayerIndexDescending` comparator (line 211)
- `StartBeatAscending` comparator (line 213)

---

## Phase 6: ClipScheduler & ActiveTimelineClipWindow

### 6A. ClipScheduler — EXCELLENT PARITY

**Status: No fixes needed.** All logic, edge cases, and constants match. Micro-clip skip guard present. Rust adds unit tests Unity doesn't have.

### 6B. ActiveTimelineClipWindow — EXCELLENT PARITY

**Status: No fixes needed.** Perfect mechanical port. All binary search logic, edge cases, and constants match exactly.

---

## Phase 7: LiveClipManager Fixes

### 7A. Fix looping epsilon in commit

**Unity source:** `LiveClipManager.cs` (commit logic checks `heldBeats > liveClip.DurationBeats + 0.001f`)
**Rust file:** `live_clip_manager.rs` lines 617-620 (checks `held_beats > original_duration` — missing `+ 0.001f`)

**Fix:** Add the `0.001` epsilon to match Unity's precision guard.

### 7B. Port ClipLauncher bridge logic

**Unity source:** `ClipLauncher.cs` (373 lines) — **ENTIRELY MISSING FROM RUST**

This is the bridge between MIDI input and LiveClipManager. Missing:
- MIDI input handling (NoteOn/NoteOff routing)
- Random clip selection logic
- In-point randomization
- Stale NoteOff timing guard (5ms threshold, line 129-143)
- Channel filtering for NoteOff (line 126)
- Auto-commit on NoteOn collision (lines 54-58)
- OnClipLaunched/OnClipStopped events
- Deterministic random seed handling

### 7C. Port recording provenance tracking

**Unity source:** `LiveClipManager.cs` lines 820-854

Missing methods:
- `TrackRecordingClipStart()`
- `FinalizeRecordingClip()`
- `RemoveRecordingClipStart()`

No `TempoRecorder` equivalent exists in Rust.

### 7D. Port live prewarm candidates

**Unity source:** `LiveClipManager.cs` lines 860-965

`AppendLivePrewarmCandidates()` is entirely absent from Rust. This populates prewarmed clip candidates for video decoding.

---

## Phase 8: GeneratorRenderer

### 8A. Port GeneratorRenderer

**Unity source:** `GeneratorRenderer.cs` (~500 lines) — **ONLY A STUB EXISTS IN RUST**

`renderer.rs` contains only the `ClipRenderer` trait and a `StubRenderer` test implementation. The actual `GeneratorRenderer` with per-layer instance management, RT pool, animation progress tracking, and rendering logic is not ported.

**Missing components:**
- Per-layer generator instance management (`layerGenerators`, `layerGenTypes`)
- RT pool (acquire/release pattern)
- Per-clip animation progress tracking (`animProgress`)
- Shared line material lifecycle
- `RenderAll()` frame loop
- Per-layer trigger counting

**Note:** This is renderer-side code. May be deferred if generator rendering is handled differently in the Rust architecture. Verify with manifold-renderer audit.

### 8B. Add OnProjectLoaded to ClipRenderer trait

**Unity source:** `IClipRenderer.cs` lines 16-17
**Rust file:** `renderer.rs` trait definition

Missing method:
```rust
fn on_project_loaded(&mut self, project: &Project);
```

---

## Phase 9: Modulation System

### 9A. Add isPausedByUser check to driver evaluation

**Unity source:** `ParameterDriverManager.cs` line 65 (`if (!driver.enabled || driver.isPausedByUser) continue;`)
**Rust file:** `modulation.rs` lines 99-100

Rust only filters on `d.enabled`, missing the `isPausedByUser` check. This depends on Tier 0 Phase 3C adding the `is_paused_by_user` field to `ParameterDriver`.

### 9B. Add currentLevel tracking to envelope evaluation

**Unity source:** `EnvelopeEvaluator.cs` lines 96, 192, 270 (`env.currentLevel = adsrValue;`)
**Rust file:** `modulation.rs`

Rust computes ADSR values but never writes them back to `env.current_level`. This field is needed for UI envelope visualization.

**Fix:** After computing ADSR, write `env.current_level = adsr_value` (requires adding `current_level: f32` field to `ParamEnvelope` in manifold-core, marked `#[serde(skip)]`).

### 9C. Fix envelope clone allocation

**Rust file:** `modulation.rs` line 366

Rust clones the entire envelope list to avoid borrow conflict. Unity iterates without cloning. This is a per-frame allocation on a hot path — violates the performance invariant.

**Fix:** Use index-based iteration or unsafe pointer aliasing to avoid the clone.

---

## Phase 10: VideoTimeCalculator

### 10A. EXCELLENT PARITY — No fixes needed

`video_time.rs` is a faithful line-by-line port. All logic, constants, and edge cases match exactly.

---

## Phase 11: Transport & Sync System

### 11A. Fix SyncArbiter signature divergence (FM-4)

**Unity source:** `SyncArbiter.cs` lines 55-131
**Rust file:** `sync.rs`

All SyncArbiter methods changed signatures to pass `authority` and `target` explicitly instead of storing them as instance fields. This is a FM-4 (Rustifying Semantics) violation.

**Unity pattern:**
```csharp
private readonly ISyncArbiterTarget target;  // injected in constructor
public bool Play(ClockAuthority source) {
    if (source != CurrentAuthority) return false;
    target.Play();
}
```

**Rust pattern:**
```rust
pub fn play(&mut self, source: ClockAuthority, authority: ClockAuthority,
            target: &mut dyn SyncArbiterTarget) -> bool
```

**Fix:** Consider storing a reference/callback pattern that matches Unity's encapsulation, or document this as an approved architectural divergence.

### 11B. Port LinkSyncController.SyncTransportFromLink()

**Unity source:** `LinkSyncController.cs` lines 165-190 — **MISSING**
**Rust file:** `link_sync.rs`

The entire transport synchronization logic is missing. `update()` is a no-op stub.

Port:
1. `SyncTransportFromLink()` — transport play/pause from Link state
2. Full `Update()` body — poll native Link state, update tempo/beat/peers, call SyncTransportFromLink

**Note:** Requires native AbletonLink FFI (separate platform concern). The logic should be ported even if the native calls are stubbed.

### 11C. Port MidiClockSyncController.Update() state machine

**Unity source:** `MidiClockSyncController.cs` lines 215-296 — **ENTIRE METHOD IS A STUB**
**Rust file:** `midi_clock_sync.rs` lines 140-143

The core per-frame state machine is completely missing:
1. Poll MidiClock state
2. Detect state changes
3. Update activity timer
4. Call UpdateBpmFromClock()
5. Manage ExternalTimeSync
6. Sync transport (Play/Pause via SyncArbiter)
7. Call SyncPositionToPlayback()

Also missing: `SyncPositionToPlayback()` (lines 368-436) and `ResetTransportTimeIntegrator()` (lines 357-362).

**Note:** `UpdateBpmFromClock()` IS ported (lines 157-202) but marked `#[allow(dead_code)]` because nothing calls it.

### 11D. Port OscSyncController

**Unity source:** `OscSyncController.cs` (~192 lines) — **ENTIRELY MISSING**

Provides OSC timecode sync including:
- SMPTE timecode parsing (drop-frame and non-drop-frame)
- Transport auto-play/pause based on timecode activity
- Timecode offset support
- Seek threshold logic

### 11E. Port OscReceiver

**Unity source:** `OscReceiver.cs` (~192 lines) — **ENTIRELY MISSING**

Thread-safe OSC message listener with:
- Background thread message parsing
- Main thread dispatch queue
- Subscribe/Unsubscribe API
- Message deduplication (latest-only pattern)

### 11F. Port OSC Bridge classes

**Unity sources — ALL MISSING:**

| Class | Lines | Description |
|-------|-------|-------------|
| `OscParameterRegistry.cs` | ~160 | Central OSC address→callback manager |
| `MasterEffectOscBridge.cs` | ~110 | Per-project master effect OSC registration |
| `LayerEffectOscBridge.cs` | ~102 | Per-layer effect OSC registration |
| `LayerOscBridge.cs` | ~149 | Per-layer coordinator (opacity, effects, generators) |
| `GeneratorOscBridge.cs` | ~108 | Per-layer generator param OSC registration |

**Total: ~629 lines of OSC infrastructure not ported.**

These depend on `EffectDefinitionRegistry` and `GeneratorDefinitionRegistry` methods from Tier 0 (OSC address generation, param range mapping).

### 11G. Native FFI for AbletonLink and MidiClock

**Unity sources:** `AbletonLink.cs` (~219 lines), `MidiClock.cs` (~360 lines)

Both are P/Invoke wrappers around native plugins. Rust equivalents need:
- AbletonLink: `ableton-link` crate or custom FFI to the native library
- MidiClock: `midir` crate for MIDI input + SPP/Clock tick parsing

**Recommendation:** Port the logic layer (controller classes) first with stubbed native calls. Native integration is a platform concern.

---

## Phase 12: Missing Traits

### 12A. Port IPlaybackNotifier trait

**Unity source:** `IPlaybackNotifier.cs` lines 9-18
**Rust file:** Not present

```rust
pub trait PlaybackNotifier {
    fn mark_compositor_dirty(&mut self);
    fn notify_generator_type_changed(&mut self, layer: &Layer, new_type: GeneratorType);
}
```

### 12B. Port ISyncTarget trait

**Unity source:** `ISyncTarget.cs` lines 8-30
**Rust file:** Not present

Two interfaces: `ISyncTarget` and `ISyncArbiterTarget`. The `SyncArbiterTarget` trait exists in `sync.rs` but `ISyncTarget` is missing.

### 12C. Fix ILiveClipHost missing methods

**Unity source:** `ILiveClipHost.cs`
**Rust file:** `live_clip_manager.rs` trait

Missing from Rust trait:
- `show_debug_logs() -> bool`
- `get_tempo_source_at_beat(beat: f32) -> TempoPointSource`
- `invalidate_lookahead_prewarm()`

---

# VERIFICATION CHECKLIST

After implementing all phases:

- [ ] `CompositeCommand` exists with reverse-order undo
- [ ] Undo capping happens in both `execute()` and `redo()` paths
- [ ] `sync_clips_to_time()` is public
- [ ] `process_pending_pauses()` is public
- [ ] `ParameterDriver` evaluation checks `is_paused_by_user`
- [ ] Envelope ADSR writes back `current_level`
- [ ] LiveClipManager commit has `+ 0.001` epsilon
- [ ] `shrink_clips_by_grid()` uses fixed 0.25 min duration
- [ ] SyncArbiter signature divergence documented or fixed
- [ ] `cargo build` succeeds for manifold-editing and manifold-playback
- [ ] `cargo test` passes for both crates
- [ ] No per-frame allocations in modulation hot path (envelope clone removed)

---

# PRIORITY ORDER

**P0 — Breaking bugs:**
1. Phase 5A: sync_clips_to_time visibility
2. Phase 1B: Undo capping bug
3. Phase 7A: Looping epsilon
4. Phase 9B: Envelope currentLevel tracking
5. Phase 5B: process_pending_pauses visibility

**P1 — Missing critical infrastructure:**
6. Phase 1A: CompositeCommand
7. Phase 5E: Callback delegates
8. Phase 7B: ClipLauncher bridge
9. Phase 9A: isPausedByUser check
10. Phase 12C: ILiveClipHost missing methods

**P2 — Feature gaps (OSC/sync):**
11. Phase 11B-11G: Sync system ports
12. Phase 8A: GeneratorRenderer

**P3 — High-level editing operations:**
13. Phase 3A-3E: EditingService methods
14. Phase 2A-2E: Command fixes

---

# FILES CHANGED (Summary)

| File | Changes |
|------|---------|
| `manifold-editing/src/command.rs` | Add CompositeCommand, LayerLifecycleCallbacks trait |
| `manifold-editing/src/undo.rs` | Fix capping logic |
| `manifold-editing/src/service.rs` | Add missing high-level operations, fix shrink min |
| `manifold-editing/src/commands/envelopes.rs` | Add atomic ChangeParamEnvelopeCommand |
| `manifold-editing/src/commands/clip.rs` | Fix MoveClipCommand capture timing |
| `manifold-editing/src/commands/layer.rs` | Verify UngroupLayers undo |
| `manifold-playback/src/engine.rs` | Make methods public, add callbacks, add scratch buffers |
| `manifold-playback/src/live_clip_manager.rs` | Fix epsilon, add ILiveClipHost methods, port recording |
| `manifold-playback/src/modulation.rs` | Add isPausedByUser, currentLevel, fix clone |
| `manifold-playback/src/renderer.rs` | Add OnProjectLoaded, port GeneratorRenderer |
| `manifold-playback/src/sync.rs` | Fix SyncArbiter signatures, add traits |
| `manifold-playback/src/link_sync.rs` | Port SyncTransportFromLink, full Update |
| `manifold-playback/src/midi_clock_sync.rs` | Port Update state machine, SyncPositionToPlayback |
| `manifold-playback/src/osc_sender.rs` | Complete OscPositionSender |
| (new) `manifold-playback/src/clip_launcher.rs` | Port ClipLauncher |
| (new) `manifold-playback/src/osc_receiver.rs` | Port OscReceiver |
| (new) `manifold-playback/src/osc_registry.rs` | Port OscParameterRegistry |
| (new) `manifold-playback/src/osc_bridges.rs` | Port all 4 OSC bridge classes |
