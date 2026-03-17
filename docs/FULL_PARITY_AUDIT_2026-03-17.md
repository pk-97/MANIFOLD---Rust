# MANIFOLD Rust Port — Full Parity Audit

**Date:** 2026-03-17
**Method:** Line-by-line comparison of every Rust file against its Unity C# source
**Auditor:** Claude Opus 4.6 (7 parallel deep-audit agents across all subsystems)

---

## Executive Summary

PORT_STATUS.md claimed ~86% completion (140 ported / 8 partial / 26 missing). **The reality is ~55-60% functionally complete.** Many files marked "ported" are structurally present but functionally incomplete — missing methods, wrong logic, stubbed features, or behavioral divergences invisible without line-by-line comparison.

### Revised Completion Estimate

| Subsystem | PORT_STATUS Claim | Actual Functional | Gap |
|---|---|---|---|
| Core data models | 97% | ~80% | Missing registries, traits, methods |
| Editing | 88% | ~65% | Commands done, service half-empty |
| Playback | 78% | ~60% | Critical guards and wiring missing |
| Effects | 53% | **35%** | 30 of 46 types missing |
| Generators | 100% | ~90% | 2 of 18 broken |
| Compositor | 100% | ~70% | Refactored, unverified |
| UI | 81% | ~75% | 3 critical panels missing |
| IO | 50% | ~30% | Saver is a stub |
| App | 80% | ~60% | Scattered, stubbed features |

---

## 1. manifold-core — Data Models, Types, Registries

**Verdict: ~80% functional**

### What's Correct

- **Project, Timeline, Layer, TimelineClip, Settings, TempoMap** — all structs have correct fields, correct serde annotations, correct default values
- **SelectionRegion** — exact match (region-based: start_beat, end_beat, start_layer_index, end_layer_index)
- **All enums** — BlendMode (0-12), EffectType (0-39), GeneratorType (0-19), LayerType (0-2), TempoPointSource, BeatDivision (20 variants), DriverWaveform, PlaybackState, QuantizeMode, ClockAuthority, ResolutionPreset — all variants present with correct numeric values
- **EffectDefinitionRegistry** — param_defs() on EffectType fully ported with correct param counts, names, min/max/defaults
- **TempoMap** — all methods (get_bpm_at_beat, add_or_replace_point, beat_to_seconds, seconds_to_beat) — piecewise integration logic correct
- **BeatQuantizer** — BPM_STEP (0.01), BEAT_STEP (0.0001), TIME_SECONDS_STEP (0.0001) all match
- **Timeline** — clip_lookup, rebuild_clip_lookup, find_clip_by_id with self-healing, binary search helpers, dual-sort-cache active clip collection — all correct
- **Layer** — generate_layer_color (golden ratio), ensure_clip_ordering_caches, collect_active_clips_at_beat — all correct
- **TimelineClip** — all fields, end_beat(), is_active_at_beat(), overlaps_with(), clone_with_new_id(), clone_at(), clamped setters — all correct
- **EffectInstance** — clone_deep, reset_param_effectives, ensure_base_values, align_to_definition — all correct
- **EffectGroup** — new(), clone_with_new_id() — correct
- **ParameterDriver::evaluate()** — correct waveform logic for sine/triangle/sawtooth/square/random
- **ParamDef** struct — all fields present (name, min, max, default_value, whole_numbers, is_toggle, value_labels, format_string, osc_suffix)
- **Color** — RGBA fields, WHITE/BLACK/CLEAR constants, hsv_to_rgb() — all correct
- **RecordingProvenance** — pure data holder, all fields correct
- **PercussionImportState** — all fields correct
- **MidiMappingConfig** — rebuild_dictionary, get_mapping_for_note, purge_orphaned_clip_ids — correct
- **ProjectSettings** — all fields present, all defaults correct (1920x1080, 60fps, 120bpm, 4/4, pool size 10, max layers 8, osc port 9001)

### Critical Gaps

#### GAP-CORE-1: GeneratorDefinitionRegistry NOT PORTED
**Severity:** CRITICAL — blocks all generator UI/configuration
**Details:** GeneratorType enum has NO `param_defs()` method, NO `display_name()` method, NO registry equivalent. EffectType has full param_defs() but GeneratorType has nothing.
**Unity source:** `GeneratorDefinitionRegistry.cs`
**Impact:** Generators can render (params read by index) but cannot be configured through generic UI (ParamCardBuilder pattern). No display names for dropdown labels.
**Required:**
- `GeneratorType::param_defs() -> &'static [ParamDef]` with all param definitions per generator type
- `GeneratorType::display_name() -> &'static str`
- `GeneratorType::from_index(usize) -> Option<GeneratorType>`

#### GAP-CORE-2: GeneratorParamState::reset_effectives() Behavioral Divergence
**Severity:** HIGH — silently breaks modulation system
**Details:**
Unity resets ONLY params that have active drivers or envelopes:
```csharp
// Unity — only reset driven params
for (int d = 0; d < drivers.Count; d++) {
    var driver = drivers[d];
    if (driver.enabled && driver.paramIndex >= 0 && driver.paramIndex < paramValues.Length)
        paramValues[driver.paramIndex] = baseParamValues[driver.paramIndex];
}
// Same for envelopes
```
Rust resets ALL params unconditionally:
```rust
// Rust — resets everything
if let Some(base) = &self.base_param_values {
    for (i, &val) in base.iter().enumerate() {
        if i < self.param_values.len() {
            self.param_values[i] = val;
        }
    }
}
```
**Impact:** Any param manually adjusted by the user gets overwritten every frame if ANY driver exists on the same effect/generator, even if that specific param has no driver.

#### GAP-CORE-3: IParamSource / IEffectContainer Traits Not Implemented
**Severity:** HIGH — blocks generic param UI
**Details:** Unity has `IParamSource` (GetParam, SetParam, GetParamDef, ParamCount) and `IEffectContainer` (Effects, EffectGroups, HasModularEffects, Envelopes). These interfaces are implemented on EffectInstance, GeneratorParamState, TimelineClip, Layer, ProjectSettings. Rust has no trait equivalents, meaning no generic parameter editing UI is possible.

#### GAP-CORE-4: BeatDivision Helper Methods Missing
**Severity:** MEDIUM
**Details:** `beats()` is hardcoded inline in ParameterDriver::evaluate() instead of being a reusable method on BeatDivision. Missing: `label()`, `is_dotted()`, `is_triplet()`, `base_division()`, `toggle_dotted()`, `toggle_triplet()`.
**Unity source:** `BeatDivisionHelper.cs`

#### GAP-CORE-5: ResolutionPreset Helper Methods Missing
**Severity:** MEDIUM
**Details:** No `dimensions()`, `display_name()`, `preset_name()`, `dropdown_label()` on ResolutionPreset. Setting a preset should auto-update output_width/output_height.

#### GAP-CORE-6: VideoLibrary Read-Only
**Severity:** MEDIUM — blocks content management
**Details:** Missing methods: AddClip, RemoveClip, HasClip, Clear, ScanDirectory, ScanLayerFolder, ValidateClips. Rust version is lookup-only.
**Unity source:** `VideoLibrary.cs`

#### GAP-CORE-7: GeneratorParamState Missing State Management
**Severity:** MEDIUM — blocks editing
**Details:** Missing: GetParam, SetParam, GetParamBase, SetParamBase, FindDriver, FindEnvelope, SnapshotParams/Drivers/Envelopes (undo), Restore (undo), Drivers/Envelopes lazy creation properties.

#### GAP-CORE-8: MidiMappingConfig Missing Mutation Methods
**Severity:** LOW
**Details:** Missing: AssignClipToNote, RemoveClipFromNote, ClearNote, GetClipIdsForNote, GetAllNotes. Core lookup works, but no mutation API.

#### GAP-CORE-9: EffectInstance Missing IParamSource Methods
**Severity:** LOW
**Details:** FindDriver(), CreateDriver(), RemoveDriver() not exposed as public API (though drivers_mut() provides raw access).

---

## 2. manifold-editing — Commands & EditingService

**Verdict: Commands 100%, Service ~50%**

### What's Correct

#### All 48+ Command Types — FULLY PORTED

**Clip Commands (11):**
- MoveClipCommand, TrimClipCommand, DeleteClipCommand, AddClipCommand, ClipEffectsCommand, ChangeClipLoopCommand, SwapVideoCommand, SlipClipCommand, ChangeClipRecordedBpmCommand, SplitClipCommand, MuteClipCommand — all with correct execute/undo logic

**Layer Commands (5):**
- AddLayerCommand, DeleteLayerCommand, ReorderLayerCommand, GroupLayersCommand, UngroupLayersCommand — all correct

**Effect Commands (5):**
- AddEffectCommand, RemoveEffectCommand, ReorderEffectCommand, ToggleEffectCommand, ChangeEffectParamCommand — all correct

**Effect Group Commands (7):**
- GroupEffectsCommand, UngroupEffectsCommand, ToggleGroupCommand, RenameGroupCommand, ChangeGroupWetDryCommand, ReorderRackCommand, MoveEffectToRackCommand — all correct

**Envelope Commands (7):**
- AddParamEnvelopeCommand, RemoveParamEnvelopeCommand, ChangeEnvelopeADSRCommand, ChangeEnvelopeTargetNormalizedCommand, ToggleEnvelopeEnabledCommand, AddLayerEnvelopeCommand, RemoveLayerEnvelopeCommand — all correct

**Driver Commands (6):**
- AddDriverCommand, ToggleDriverEnabledCommand, ChangeDriverBeatDivCommand, ChangeDriverWaveformCommand, ToggleDriverReversedCommand, ChangeTrimCommand — all correct

**Settings Commands (12):**
- ChangeBpmCommand (two constructors: new + with_tempo_map), RestoreRecordedTempoLaneCommand, ClearTempoMapCommand, ChangeQuantizeModeCommand, ChangeResolutionCommand, ChangeFrameRateCommand, ChangeLayerMidiNoteCommand, ChangeLayerBlendModeCommand, ChangeLayerOpacityCommand, ChangeGeneratorParamsCommand, ChangeGeneratorTypeCommand, ChangeMasterOpacityCommand — all correct

**Selection Commands (1):**
- SetSelectionRegionCommand — correct

**Extra (justified):**
- RescaleBeatsForBpmChangeCommand (BPM change rescaling helper)
- ChangeEnvelopeRoutingCommand (envelope routing)

#### UndoRedoManager — FULLY CORRECT
- MAX_UNDO_HISTORY = 200 — matches Unity
- VecDeque undo + Vec redo — semantically equivalent to Unity's LinkedList + Stack
- Execute, Record, Undo, Redo, Clear — all correct
- History cap enforcement on redo — correct

#### CompositeCommand — CORRECT
- Executes all in order, undoes in reverse order

### Critical Gaps

#### GAP-EDIT-1: EditingService Missing ~15 Public Methods
**Severity:** CRITICAL — blocks most editing workflows

**Missing Selection Methods:**
- `on_clip_selected(clip)` — show in inspector
- `select_region_to(beat, layer)` — Shift+click extend
- `select_all_clips()` — Ctrl+A
- `get_effective_selected_clips()` — query from region or individual

**Missing Deletion Methods:**
- `delete_selected_clips()` — delete via region or individual selection
- `delete_selected_layers()` — delete selected layers

**Missing Clipboard Methods:**
- `copy_selected_clips()` — copy to clipboard from selection
- `cut_selected_clips()` — copy + delete

**Missing Mute:**
- `toggle_mute_selected_clips()`

**Missing Layer Management:**
- `group_selected_layers()`
- `move_clip_to_layer(clip, target_layer)`

**Missing Grid/Timing:**
- `get_current_grid_step()`
- `get_seconds_per_beat()`

**Missing Waveform Drag:**
- `on_waveform_drag_delta_beats(delta)`
- `on_waveform_drag_end(total_delta)`

**Missing Region Operations:**
- `split_clips_for_region_move(region)` — returns RegionSplitResult

#### GAP-EDIT-2: EditingHost Trait Is Dead Code
**Severity:** MEDIUM
**Details:** EditingHost trait is defined with 7 methods (current_beat, seconds_per_beat, grid_interval_beats, floor_beat_to_grid, snap_beat_to_grid, request_clip_sync, mark_compositor_dirty) but is NEVER USED in any service method. Methods don't take `host` parameter.

#### GAP-EDIT-3: No Scratch Buffers
**Severity:** MEDIUM — performance regression on hot paths
**Details:** Unity pre-allocates `_cmdScratch`, `_idScratch`, `_clipScratch` and reuses them. Rust allocates new Vec per operation. Affects delete, paste, duplicate paths.

#### GAP-EDIT-4: Project Ownership Model Changed
**Severity:** LOW (Rust borrow constraint, acceptable divergence)
**Details:** Unity EditingService owns `currentProject`. Rust passes `&mut Project` to every method. API surface completely different but functionally equivalent given Rust's ownership model.

#### GAP-EDIT-5: Command Description Case Divergence
**Severity:** COSMETIC
**Details:** Rust uses Title Case ("Move Clip"), Unity uses lowercase ("Move clip").

---

## 3. manifold-playback — Engine, Scheduling, Sync

**Verdict: ~60% functional**

### What's Correct

- **ClipScheduler::compute_sync()** — all functional parameters present, logic correct
- **All temporal constants match Unity exactly:**
  - MIN_CLIP_PLAYBACK_RATE = 0.05
  - MAX_CLIP_PLAYBACK_RATE = 8.0
  - PENDING_PAUSE_DELAY = 0.1
  - RECENTLY_STARTED_TIME = 0.1
  - LIVE_RECENTLY_STARTED_TIME = 0.02
  - COMPOSITOR_DIRTY_TIME = 0.05
  - MIN_START_REMAINING_TIME = 0.02
  - MIDI_CLOCK_TICKS_PER_BEAT = 24
  - TICKS_PER_SIXTEENTH = 6
- **SyncSource trait** — complete (is_enabled, display_name, enable, disable, toggle with default impl)
- **VideoTime calculator** — pure logic, value-level parity verified
- **ActiveTimelineClipWindow** — indexing approach sound (uses per-layer clip counts instead of reference identity, acceptable Rust divergence)

### Critical Gaps

#### GAP-PLAY-1: LiveClipHost Trait Missing 4 Methods
**Severity:** CRITICAL — blocks live performance recording
**Details:**
| Missing Method | Unity Signature | Purpose |
|---|---|---|
| `get_tempo_source_at_beat` | `TempoPointSource GetTempoSourceAtBeat(float beat)` | Recording provenance |
| `invalidate_lookahead_prewarm` | `void InvalidateLookaheadPrewarm()` | Video decoder efficiency |
| `is_playing` | `bool IsPlaying { get; }` | Quantize logic |
| `show_debug_logs` | `bool ShowDebugLogs { get; }` | Debug toggle |

#### GAP-PLAY-2: 5ms NoteOn/NoteOff Timing Guard Missing
**Severity:** CRITICAL — MIDI performance will break
**Details:** Unity's LiveClipManager ignores NoteOff within 5ms of NoteOn on the same channel. Rust has no timing guard — the constant `NOTE_ON_OFF_TIME_GUARD_SECONDS = 0.005` doesn't exist. MIDI controllers that send NoteOff quickly after NoteOn will incorrectly cancel phantom clips.

#### GAP-PLAY-3: Live Slots Not Wired in sync_clips_to_time
**Severity:** CRITICAL — live clips completely ignored during playback
**Details:** PlaybackEngine.sync_clips_to_time() passes empty `&[]` for live_slots parameter:
```rust
sync_result = self.scheduler.compute_sync(
    /* ... */
    &[],  // live_slots — wired in Phase 3D  ← TODO COMMENT
    /* ... */
);
```
Must be wired to `self.live_clip_manager.live_slots_list()`.

#### GAP-PLAY-4: PlaybackEngine Doesn't Implement LiveClipHost
**Severity:** HIGH
**Details:** In Unity, PlaybackEngine IS an ILiveClipHost. In Rust, the engine doesn't implement the trait. LiveClipManager needs to call methods on the engine at runtime but has no way to do so.

#### GAP-PLAY-5: ClipRenderer Trait Missing on_project_loaded
**Severity:** MEDIUM
**Details:** Unity's IClipRenderer has `OnProjectLoaded(Project project)`. Rust's ClipRenderer trait doesn't. Any renderer that caches per-project state has no lifecycle hook to populate that cache.

#### GAP-PLAY-6: Missing Clip Texture Query Methods
**Severity:** MEDIUM
**Details:** PlaybackEngine missing `get_clip_texture(clip_id)`, `is_clip_active(clip_id)`, `is_clip_ready(clip_id)`. Needed by compositor to query clip readiness.

#### GAP-PLAY-7: SyncArbiter Missing current_authority Instance Method
**Severity:** LOW
**Details:** Unity's SyncArbiter has a `CurrentAuthority` property reading from project settings. Rust has only a static function `current_authority(project)`.

#### GAP-PLAY-8: OSC Sync Is a Flag, Not a Trait Object
**Severity:** LOW (acceptable interim)
**Details:** OSC sender managed as `osc_sender_enabled: bool` + `osc_sender_port: i32` instead of ISyncSource trait object like Link/MidiClock.

---

## 4. manifold-renderer — Effects & Compositor

**Verdict: 16 of 46 effect types ported (35%)**

### What's Correct (Ported Effects)

All 16 ported effects verified for:
- **Parameter parity** — all param indices, names, min/max/defaults match Unity exactly
- **Pass count parity** — Bloom (4 passes), Halation (3 passes), Feedback (1+copy), all simple effects (1 pass)
- **Texture format parity** — all use Rgba16Float for intermediates (matches Unity ARGBHalf)
- **Stateful state management** — per-owner HashMap<i64, State> matches Unity Dictionary<int, State>
- **Constants verified** — Bloom: MAX_LEVELS=6, MIN_SIZE=16, PREFILTER_THRESHOLD=0.42, PREFILTER_KNEE=0.24, BLOOM_LEVELS=3, RADIUS_AT_ZERO=0.70, RADIUS_AT_ONE=1.25

**Ported effects (16):**
1. InvertColors — simple 1-param
2. ColorGrade — simple 1-param
3. Mirror — simple 1-param
4. QuadMirror — simple 1-param
5. Feedback — stateful per-owner single RT
6. Bloom — stateful per-owner pyramid (6 mips max), 4-pass
7. ChromaticAberration — simple
8. FilmGrain — simple
9. Glitch — 5-param time-based noise
10. Dither — 2-param algorithm selector
11. Halation — stateful per-owner half-res, 3-pass
12. Kaleidoscope — 2-param discrete segment count
13. EdgeStretch — 3-param direction selector
14. Strobe — 3-param rate/mode
15. CRT — CRT monitor simulation
16. StylizedFeedback — stylized feedback variant

**Shader math verified (spot-checked):**
- Bloom.wgsl: all 4 passes match BloomEffect.shader (Blur9, BrightPrefilter, Blur13, composite)
- Halation.wgsl: 3-pass structure matches (threshold+tint+blur, wide blur, composite)
- Glitch.wgsl: 5 uniform parameters match SetFloat calls

### Critical Gaps

#### GAP-FX-1: 30 Effect Types Completely Missing
**Severity:** CRITICAL — 65% of visual effects unavailable

**Not ported at all (30):**
1. PixelSort (compute)
2. InfiniteZoom (stateful 2-pass)
3. VoronoiPrism
4. BlobTracking (native blob detection)
5. FluidDistortion
6. EdgeGlow
7. Datamosh (frame-dropping glitch)
8. SlitScan
9. WireframeDepth
10. GradientMap
11. Microscope
12. Corruption
13. Infrared
14. Surveillance
15. Redaction
16. ComputeFluidEffect
17. ComputeFluidDistortionFX
18. ComputeFluidSolver
19. ComputePixelSortFX
20. ComputeSortEffect
21. BlobDetectorNative
22. DepthEstimatorNative
23-30. Plus ~8 more stateful/compute effects

**Priority for porting (by VJ usage frequency):**
- HIGH: InfiniteZoom, FluidDistortion, EdgeGlow, Datamosh, PixelSort, GradientMap
- MEDIUM: BlobTracking, WireframeDepth, Microscope, SlitScan, VoronoiPrism
- LOW: Corruption, Infrared, Surveillance, Redaction (niche effects)

#### GAP-FX-2: Compositor Architecture Divergence
**Severity:** HIGH — needs verification
**Details:** Unity's CompositorStack is monolithic: direct ping-pong buffer management, per-layer/per-clip/per-group buffers, material cache for blend modes, compute compositor optional fast path, tonemap pipeline. Rust refactored into `trait Compositor` + `LayerCompositor` abstraction. Not verified:
- Per-layer effect isolation
- Per-clip effect isolation
- Per-group wet/dry blending
- All blend modes (Normal, Add, Screen, Overlay, Multiply, etc.)
- Compute compositor fast path
- External tap point system

---

## 5. manifold-renderer — Generators

**Verdict: 16 of 18 solid, 2 broken**

### What's Correct (16 Generators)

All verified for correct parameter indices, uniforms, dispatch sizes, shader math, lifecycle:

1. **Plasma** — 6 params, 5 patterns, snap cycling ✓
2. **FractalZoom** — 2 params ✓
3. **ConcentricTunnel** — 6 params, shape toggle ✓
4. **ReactionDiffusion** — 4 params, stateful ping-pong Rgba32Float, 8 sim steps ✓
5. **BasicShapesSnap** — line-based ✓
6. **Tesseract** — line-based, 11 params, 4D geometry ✓
7. **Duocylinder** — line-based, 11 params ✓
8. **Lissajous** — line-based, 11 params, 10 presets ✓
9. **OscilloscopeXY** — line-based oscilloscope ✓
10. **WireframeZoo** — line-based geometry gallery ✓
11. **Flowfield** — stateful ping-pong, 6 params, Perlin noise ✓
12. **StrangeAttractor** — line-based, 8 params ✓
13. **MyceliumGenerator** — compute Physarum, 12 params, 4-pass pipeline ✓
14. **ComputeStrangeAttractor** — compute particles, 11 params ✓
15. **ParametricSurface** — compute volume, 5 params, 5 surface types ✓
16. **NumberStation** — 8 params, 4 display modes ✓

**Parameter registry 100% correct** — all 18 generators have correct param counts and indices.

### Broken Generators

#### GAP-GEN-1: FluidSimulation2D — 4 Known Issues
**Severity:** MEDIUM (functional but degraded)

| Issue | Details | Impact |
|---|---|---|
| Particle cap 2M vs 8M | Unity uses 8M max particles. Rust caps at 2M. | Quarter density if >2M requested |
| Wrong texture formats | Density: Rgba16Float (should be R32Float). Vector: Rgba16Float (should be Rg32Float) | 4x bandwidth waste, minor precision loss |
| Blur radius truncation | Float radius truncated instead of rounded | Off-by-one occasionally |
| Redundant buffer clear | encoder.clear_buffer() before self-clear in resolve kernel | Harmless redundancy |

#### GAP-GEN-2: FluidSimulation3D — 19 Issues (STRUCTURALLY BROKEN)
**Severity:** CRITICAL — simulation produces wrong results

**Catastrophic issues (4):**
1. Gradient magnitude 128x too strong (divides by `2.0 * texel` instead of multiplying)
2. Particles die after 3-6 seconds (should be permanent like Unity)
3. Density capping formula wrong
4. Velocity field divergence not conserved

**Major issues (9):** Documented in FLUID_SIM_AUDIT.md with Unity source line references and fix priority order.

**Moderate issues (6):** Also documented in FLUID_SIM_AUDIT.md.

**Status:** Requires line-by-line correction per the 19 issues. The audit document is the authoritative fix reference.

---

## 6. manifold-ui — Panels, Input, Coordinate System

**Verdict: ~75% structural parity**

### What's Correct

#### UIState — EXACT MATCH
Every field, every method verified:
- `selected_clip_ids: HashSet<String>`, `selection_version`, `primary_selected_clip_id`, `selected_layer_index`
- `selected_layer_ids`, `primary_selected_layer_id`, `selection_region`
- `cursor_beat`, `cursor_layer_index`, `insert_cursor_beat`, `insert_cursor_layer_index`
- `hovered_clip_id`, drag state (is_dragging, drag_clip_id, drag_start_beat/layer, drag_offset_beats)
- Trim state (is_trimming, trim_from_left, trim_clip_id, trim_original_start/duration/in_point)
- `is_scrubbing`, `current_zoom_index`
- All methods: select_clip, toggle_clip_selection, clear_selection, set_region, set_region_from_clip_bounds, clear_region, set_insert_cursor, clear_insert_cursor, select_layer, toggle_layer_selection, select_layer_range, clear_layer_selection, is_layer_active (all 4 checks), begin_drag, end_drag, begin_trim_left/right, end_trim

#### CoordinateMapper — EXACT MATCH
- beat_to_pixel, pixel_to_beat, beat_to_pixel_absolute, beat_duration_to_width, width_to_beat_duration — all formulas correct
- set_zoom_by_index (clamp 0..len-1), set_zoom (min 1.0)
- calculate_fit_zoom, get_content_width
- rebuild_y_layout — all collapse rules: expanded=140px, collapsed=48px, generator_collapsed=62px, group_collapsed=70px, child_of_collapsed=0px
- get_layer_y_offset, get_layer_height, get_layer_at_y (reverse iteration)
- Grid snapping: 16th (ppb>=16), 8th (ppb>=12), quarter (ppb>=6), bar fallback

#### Input System — COMPLETE
- PointerAction (Down/Move/Up), Modifiers (shift/ctrl/alt/command with helpers)
- Key enum (Space, Enter, Escape, arrows, A-Z, 0-9, F1-F12)
- UIEvent (Click, DoubleClick, RightClick, Scroll, PointerDown/Up, HoverEnter/Exit, DragBegin/Drag/DragEnd, KeyDown)
- DRAG_THRESHOLD_PX = 4, DOUBLE_CLICK_TIME_SEC = 0.3

#### InteractionOverlay — MOSTLY COMPLETE
- DragMode (None, Move, TrimLeft, TrimRight, RegionSelect)
- DragSnapshot (clip_id, start_beat, layer_index)
- SNAP_THRESHOLD_PX = 12, MAX_SNAP_BEATS = 0.5
- select_region_to() with correct anchor priority (insert cursor > region > primary clip > fallback)

#### 12 Panels Ported
1. HeaderPanel (project name, time display, zoom, monitor)
2. TransportPanel (play/stop/record, BPM, sync controls)
3. LayerHeaderPanel (mute/solo, blend mode, expand/collapse, folder, add-clip)
4. FooterPanel (quantization, resolution, FPS)
5. InspectorCompositePanel (master/layer/clip tabs, effect cards, gen params)
6. EffectCardPanel (per-effect card with params/drivers/envelopes)
7. GenParamPanel (generator param display)
8. MasterChromePanel (master opacity, exit path)
9. LayerChromePanel (per-layer opacity)
10. ClipChromePanel (per-clip BPM, loop, slip)
11. DropdownPanel (context menu / dropdown list)
12. PerfHUDPanel (FPS, memory, perf metrics)
13. ViewportPanel (timeline clips, playhead, ruler)

#### PanelAction Enum — COMPREHENSIVE
90+ action variants covering transport, sync, file, header/footer, layer management, effect editing, generator params, timeline, viewport, context menus.

#### Keyboard Shortcuts — COMPLETE
All major shortcuts wired in InputHandler:
- Playback: Space, Home, End
- Editing: Delete, Backspace, S (split), E/Shift+E (extend/shrink), 0 (mute)
- Navigation: Arrow keys, Up/Down (layer), Left/Right (fine nudge)
- Undo/Redo: Cmd+Z, Cmd+Shift+Z, Cmd+Y
- Clipboard: Cmd+A/C/X/V/D
- Grouping: Cmd+G, Cmd+Shift+G
- View: F (fit), ` (perf HUD)
- Export: I, O, Alt+I, Alt+O
- File: Cmd+S/O/N (delegated to legacy block for rfd window handle)

### Critical Gaps

#### GAP-UI-1: OverviewStripPanel Missing
**Severity:** HIGH — navigation feature lost
**Details:** Mini-timeline bar with clip positions, viewport indicator, playhead, export markers. Click/drag to scrub viewport.
**Unity source:** `OverviewStripPanel.cs`

#### GAP-UI-2: BrowserPopupPanel Missing
**Severity:** CRITICAL — effect/generator selection UX degraded
**Details:** Floating browser with categorized grid layout, search/filter by name, paste button. Opens on "Add Effect" or "Add Generator". Currently falls back to simple DropdownPanel (vertical list, no categories, no search).
**Unity source:** `BrowserPopupPanel.cs`

#### GAP-UI-3: EffectsListBitmapPanel Missing
**Severity:** CRITICAL — cannot drag-reorder effects
**Details:** Composite container with effect cards, rack headers, add button, drag-and-drop reordering with ghost preview. Without this, users must delete and re-add effects to reorder.
**Includes:** RackHeaderBitmapPanel, AddEffectButtonBitmapPanel
**Unity source:** `EffectsListBitmapPanel.cs`, `RackHeaderBitmapPanel.cs`, `AddEffectButtonBitmapPanel.cs`

#### GAP-UI-4: ShortcutRegistry Missing
**Severity:** LOW
**Details:** No centralized registry for UI hints or rebinding. Shortcuts hardcoded in InputHandler. No "Help > Shortcuts" introspection.
**Unity source:** `ShortcutRegistry.cs`

#### GAP-UI-5: Missing Medium-Priority Panels
**Severity:** MEDIUM (individual)
- ExportSection — export video/XML panel
- TempoLaneEditor — tempo curve editing
- ThumbnailCache + GeneratorThumbnailCache — clip/generator preview caching
- GridOverlay — visual grid lines on timeline
- ClipThumbnailBuilder — waveform thumbnail rendering
- ImportedAudioWaveformLane — percussion import waveform
- MonitorOutputSection — monitor output UI

---

## 7. manifold-io — Project Loading, Saving, Migration

**Verdict: Loader partial, Saver stub, Migrator complete**

### What's Correct

#### Migrator — PERFECT 1:1 TRANSLATION
- Version detection: `projectVersion` field with "1.0.0" default — correct
- V1.0.0 → V1.1.0 migration: nest percussion fields into `percussionImport`, nest layer generator fields into `genParams` — correct
- move_field() helper — correct
- Semver comparison — correct
- Extensibility for future migrations — correct

#### Loader — Reads Data Correctly
- V1 JSON deserialization via serde — correct
- V2 ZIP extraction of `project.json` — correct
- Post-deserialize (rebuild runtime caches) — correct
- Error handling via Result<Project, LoadError> — correct

### Critical Gaps

#### GAP-IO-1: Saver Is a Stub (31 Lines)
**Severity:** CRITICAL — projects saved in incompatible format
**Details:** Rust saver writes flat JSON only. Missing everything Unity's ProjectArchive.Save() provides:

| Feature | Unity | Rust |
|---|---|---|
| ZIP archive format | Full V2 ZIP | Flat JSON (V1) |
| Hash-based dedup | 6-char SHA256 prefix, skip on no-change | Always writes |
| Manifest | formatVersion, name, hash, timestamp, history | None |
| Snapshot history | Gzipped previous state in history/ folder | None |
| Auto-save pruning | Max 50, manual saves preserved | None |
| Atomic writes | Write to .tmp, rename on success | Direct write |

**Impact:** Rust-saved projects are V1 flat JSON, incompatible with Unity. No version history, no crash recovery, no dedup optimization.

#### GAP-IO-2: Loader Skips Post-Load Validation
**Severity:** HIGH — silent data degradation
**Details:** Three Unity post-load steps not called:
1. `PurgeOrphanedReferences()` — stale clip/MIDI references remain
2. `VideoLibrary.ValidateClips()` — missing file detection deferred to runtime
3. `PathResolver.ResolveAll()` — broken video paths can't auto-heal

#### GAP-IO-3: PathResolver Completely Missing
**Severity:** HIGH — blocks path auto-healing
**Details:** `PathResolver.cs` is marked as `missing` in PORT_STATUS.md. This handles relative→absolute path conversion and broken path fixup on project load. Without it, projects with moved video files can't auto-resolve.
**Unity source:** `Assets/Scripts/Export/PathResolver.cs`

#### GAP-IO-4: BPM Sync from TempoMap Not Performed on Load
**Severity:** MEDIUM
**Details:** Unity calls `GetBpmAtBeat(0f, project.Settings.BPM)` with Clamp(20-300) after load. Rust loader doesn't sync BPM from tempo map beat 0.

#### GAP-IO-5: Video Export Missing
**Severity:** MEDIUM (future feature)
**Details:** `VideoExporter.cs` and `MetalEncoderNative.cs` not ported. No MP4 export capability.

#### GAP-IO-6: FCPXML Export Missing
**Severity:** LOW (niche feature)
**Details:** `ResolveFcpxmlExporter.cs` not ported.

---

## 8. manifold-app — Application Layer

**Verdict: Functional but architecturally scattered**

### What's Correct

#### File I/O Methods — Mostly Ported
- `create_default_project()` — creates default project, adds layer, initializes engine ✓
- `open_project()` — rfd file dialog, remembers directory, loads via shared path ✓
- `open_recent_project()` — reads pref, checks exists, calls shared path ✓
- `open_project_from_path()` — detects V1/V2, loads, applies layout, restores playhead ✓
- `save_project()` — checks path, saves, marks clean; else calls save_as ✓
- `save_project_as()` — dialog via rfd, saves, persists path, remembers dir ✓
- `sync_project_saved_playhead()` — sets saved_playhead_time ✓

#### Infrastructure — Ported
- UserPrefs — JSON-backed persistent key-value store, correct platform dirs ✓
- DialogPathMemory — 6 contexts, legacy migration, directory persistence ✓
- Keyboard shortcuts (Cmd+S/O/N) wired correctly ✓
- Project state tracking (current_project_path, editing_service.is_dirty) ✓

### Critical Gaps

#### GAP-APP-1: ProjectIOService Not Ported as a Unit
**Severity:** HIGH — FM-14 violation
**Details:** Unity's `ProjectIOService.cs` (528 lines) owns all project lifecycle as a cohesive service. Rust scatters file I/O across app.rs as standalone methods on Application. No service struct, no single entry point for each concern.
**Impact:** Bug divergence risk — file drop handler may not match open handler. Recent files feature incomplete.

#### GAP-APP-2: File Drag-and-Drop Completely Stubbed
**Severity:** CRITICAL — primary content import broken
**Details:** All paths are log placeholders with no operations:
```rust
"mp4" | "mov" | "avi" | "mkv" | "webm" => {
    log::info!("Video file dropped: {}", path_str);
    // Future: create video clip on active layer at cursor position
}
```
**Missing:** video clip creation, layer auto-creation, non-overlap enforcement, metadata extraction, undo recording.
**Unity source:** `ProjectIOService.ProcessDroppedFiles()` (lines 246-326)

#### GAP-APP-3: MIDI File Import Missing
**Severity:** MEDIUM
**Details:** No MIDI import from dropped files. Unity routes `.mid`/`.midi` through `MidiFileParser.ParseFile()` → `MidiImportService.ImportToLayer()`.

#### GAP-APP-4: Video Metadata Extraction Missing
**Severity:** MEDIUM
**Details:** No async extraction of video duration/resolution on file drop. Unity uses VideoPlayer to probe metadata.

#### GAP-APP-5: Layer Name Off-by-One
**Severity:** COSMETIC
**Details:** Default layer name: Unity = "Layer 0", Rust = "Layer 1".

---

## 9. Cross-Cutting Issues

### Named Failure Modes Detected

| FM | Description | Where Found |
|---|---|---|
| FM-2 | Approximating instead of translating | Saver (reimagined as flat JSON) |
| FM-3 | Flattening architecture | Compositor refactored from monolithic class |
| FM-6 | Missing edge cases | Loader (skip post-load validation), LiveClipManager (no 5ms guard) |
| FM-9 | Hallucinated constraints | FluidSim2D particle cap 2M vs 8M |
| FM-10 | Substituting texture formats | FluidSim2D Rgba16Float vs R32Float/Rg32Float |
| FM-14 | Scattering service logic | ProjectIOService across app.rs |
| FM-15 | Missing cross-cutting infrastructure | PathResolver missing |

### Architecture Divergences

1. **EditingService** — project passed by ref, not owned (Rust borrow constraint)
2. **Compositor** — refactored into trait-based abstraction from Unity monolithic class
3. **OSC sync** — flag-based instead of trait object
4. **File dialogs** — rfd crate instead of native plugins (acceptable)
5. **UserPrefs** — JSON format instead of Unity's platform-specific format (acceptable)
6. **ActiveTimelineClipWindow** — clip count indexing instead of reference identity (acceptable)

---

## 10. Priority Fix List

### Tier 1 — Blocks Core Functionality

| # | Gap ID | Description | Est. LOC |
|---|---|---|---|
| 1 | GAP-CORE-1 | Port GeneratorDefinitionRegistry (param_defs, display_name) | ~300 |
| 2 | GAP-EDIT-1 | Complete EditingService (~15 missing public methods) | ~500 |
| 3 | GAP-PLAY-2 | Add 5ms NoteOn/NoteOff timing guard to LiveClipManager | ~30 |
| 4 | GAP-PLAY-3 | Wire live_slots in sync_clips_to_time | ~10 |
| 5 | GAP-PLAY-1 | Add 4 missing LiveClipHost trait methods | ~50 |
| 6 | GAP-PLAY-4 | Implement LiveClipHost on PlaybackEngine | ~80 |
| 7 | GAP-APP-2 | Implement file drag-and-drop for video | ~150 |
| 8 | GAP-IO-1 | Port ProjectArchive.Save() — V2 ZIP format | ~400 |
| 9 | GAP-CORE-2 | Fix reset_effectives() to only reset driven params | ~20 |
| 10 | GAP-IO-2 | Add post-load validation (orphan cleanup, path resolution) | ~50 |

### Tier 2 — Blocks Editing Workflow

| # | Gap ID | Description | Est. LOC |
|---|---|---|---|
| 11 | GAP-CORE-3 | Implement IParamSource / IEffectContainer traits | ~200 |
| 12 | GAP-UI-3 | Port EffectsListBitmapPanel (drag reorder) | ~400 |
| 13 | GAP-UI-2 | Port BrowserPopupPanel (effect/generator search browser) | ~500 |
| 14 | GAP-APP-1 | Consolidate ProjectIOService as a unit | ~300 |
| 15 | GAP-IO-3 | Port PathResolver | ~150 |
| 16 | GAP-GEN-2 | Fix FluidSimulation3D (19 issues) | ~300 |
| 17 | GAP-FX-1 | Port missing effects (30 types, prioritized) | ~3000+ |

### Tier 3 — Polish & Completeness

| # | Gap ID | Description | Est. LOC |
|---|---|---|---|
| 18 | GAP-CORE-4 | BeatDivision helper methods | ~80 |
| 19 | GAP-CORE-5 | ResolutionPreset helper methods | ~60 |
| 20 | GAP-CORE-6 | VideoLibrary mutation methods | ~100 |
| 21 | GAP-UI-1 | Port OverviewStripPanel | ~300 |
| 22 | GAP-PLAY-5 | Add on_project_loaded to ClipRenderer trait | ~30 |
| 23 | GAP-GEN-1 | Fix FluidSim2D (4 issues) | ~50 |
| 24 | GAP-EDIT-3 | Add scratch buffers for hot paths | ~60 |
| 25 | GAP-APP-3 | MIDI file import | ~100 |

---

## Appendix A: Files Audited

### Unity Source Files Read
- Assets/Scripts/Data/*.cs (all data models)
- Assets/Scripts/Editing/*.cs (all commands + EditingService)
- Assets/Scripts/Playback/*.cs (engine, scheduler, sync, live clips)
- Assets/Scripts/Sync/*.cs (sync sources)
- Assets/Scripts/Compositing/Effects/*.cs (all effects)
- Assets/Scripts/Playback/Generators/*.cs (all generators)
- Assets/Scripts/UI/Timeline/*.cs (UI panels)
- Assets/Scripts/UI/Bitmap/*.cs (bitmap rendering)
- Assets/Scripts/Export/*.cs (IO, serialization)
- Assets/Shaders/*.shader (HLSL shaders)
- Assets/Resources/Compute/*.compute (compute shaders)

### Rust Files Read
- crates/manifold-core/src/*.rs (all modules)
- crates/manifold-editing/src/*.rs (all modules)
- crates/manifold-playback/src/*.rs (all modules)
- crates/manifold-renderer/src/effects/*.rs (all effects)
- crates/manifold-renderer/src/generators/*.rs (all generators)
- crates/manifold-renderer/src/compositor.rs
- crates/manifold-ui/src/*.rs (all modules)
- crates/manifold-io/src/*.rs (all modules)
- crates/manifold-app/src/*.rs (all modules)

## Appendix B: Verified Constants

All temporal constants in PlaybackEngine match Unity:
```
MIN_CLIP_PLAYBACK_RATE = 0.05
MAX_CLIP_PLAYBACK_RATE = 8.0
PENDING_PAUSE_DELAY = 0.1
RECENTLY_STARTED_TIME = 0.1
LIVE_RECENTLY_STARTED_TIME = 0.02
COMPOSITOR_DIRTY_TIME = 0.05
MIN_START_REMAINING_TIME = 0.02
MIDI_CLOCK_TICKS_PER_BEAT = 24
TICKS_PER_SIXTEENTH = 6
```

All UI constants match Unity:
```
EXPANDED_TRACK_HEIGHT = 140
COLLAPSED_TRACK_HEIGHT = 48
COLLAPSED_GENERATOR_HEIGHT = 62
COLLAPSED_GROUP_HEIGHT = 70
DRAG_THRESHOLD_PX = 4
DOUBLE_CLICK_TIME_SEC = 0.3
SNAP_THRESHOLD_PX = 12
MAX_SNAP_BEATS = 0.5
```

All quantizer constants match Unity:
```
BPM_STEP = 0.01
BEAT_STEP = 0.0001
TIME_SECONDS_STEP = 0.0001
BPM_MIN = 20
BPM_MAX = 300
```

Bloom effect constants match Unity:
```
MAX_LEVELS = 6
MIN_SIZE = 16
PREFILTER_THRESHOLD = 0.42
PREFILTER_KNEE = 0.24
BLOOM_LEVELS = 3
RADIUS_AT_ZERO = 0.70
RADIUS_AT_ONE = 1.25
```
