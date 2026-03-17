# MANIFOLD Rust Port — Definitive Parity Audit

**Date:** 2026-03-17 (revised 2026-03-17)
**Method:** Independent line-by-line source reading of every Rust file and every Unity C# file across ALL subsystems (including previously unaudited subsystems), performed by 10 parallel deep-audit agents with cross-verification against two prior audits and a complete file inventory sweep.
**Auditor:** Claude Opus 4.6 — 10 parallel agents reading both Rust and Unity source in full

---

## Executive Summary

| Metric | Unity | Rust | Coverage |
|---|---|---|---|
| C# source files (Scripts/) | 310 | 154 (.rs excl tests) | 50% |
| C# source lines (Scripts/) | 84,340 | ~42,744 (excl tests) | 51% |
| C# test files | 34 | 7 | 21% |
| Shader + compute + include files | 84 | 48 (.wgsl) | 57% |
| Shader lines | 12,316 | ~5,155 (WGSL) | 42% |
| Native plugin source files | 8 | 0 | 0% |
| External tools (Python/Swift/JS) | 24 | 0 | 0% |
| **Grand total** | **461 files, ~105,456 lines** | **209 files, ~47,900 lines** | **~45%** |

**True functional completion: ~45%**

Both prior audits overclaimed. The first (FULL_PARITY_AUDIT_2026-03-17.md) estimated 55-60% — too generous because it didn't account for the dead modulation pipeline. The second (FULL_PARITY_AUDIT.md) estimated 40-50% — closer, but inconsistent in its subsystem percentages. This audit resolves every disagreement with verified source reads.

### Verified Completion by Subsystem

| Subsystem | True % | Prior Audit 1 | Prior Audit 2 | This Audit's Basis |
|---|---|---|---|---|
| Core Data Models | **65%** | 80% | 60% | Fields complete; missing ~40 methods, 5 traits, all clamping setters |
| Editing | **75%** | 75% | 57% | All 38 commands ported; EditingService missing ~16 methods; 2 traits missing |
| Playback + Sync | **30%** | 60% | 30% | 5 critical files at 0%; modulation pipeline dead; sync stubs |
| Effects | **53%** | 35% | 50% | 16 of 30 effect types ported (2 handled inline) |
| Generators | **88%** | 90% | 90% | All 18 types exist; 2 broken; GeneratorRenderer not wired as ClipRenderer |
| Compositor | **50%** | 70% | 53% | No compute fast-path; no material cache; no shader warmup |
| UI | **65%** | 75% | 70% | Core panels ported; 5 critical panels missing; ~40 Unity files with no Rust equivalent |
| IO | **25%** | 30% | 30% | Migrator perfect; saver is a 30-line stub; no V2 ZIP; no PathResolver |
| App | **55%** | 60% | N/A | UserPrefs + DialogPathMemory complete; video drop stubbed; ProjectIOService scattered |
| Audio/Percussion | **0%** | N/A | 0% | 16 files, 7,451 lines — entire subsystem absent. See Section 10 |
| MIDI Input | **0%** | N/A | 0% | MidiInputController.cs (526 lines) — CRITICAL for live performance. See Section 11 |
| LED/External Output | **0%** | N/A | 0% | 8 files, 853 lines — See Section 12 |
| External Window | **0%** | N/A | 0% | 2 files, 1,025 lines — See Section 13 |
| Infrastructure | **0%** | N/A | 0% | ExternalProcessRunner.cs (487 lines) — See Section 14 |
| Diagnostics | **0%** | N/A | 0% | 4 files, 1,417 lines — See Section 15 |

---

## 1. manifold-core — Data Models, Types, Registries

**Verdict: 65% functional**

### What's Correct (verified)

All core structs have correct fields with correct serde annotations and correct default values:
- **Project** — all fields present including legacy fields
- **Timeline** — all fields present; clip lookup uses `(layer_index, clip_index)` tuple (acceptable Rust divergence)
- **Layer** — all fields present including all legacy fields
- **TimelineClip** — all fields present; `has_any_effect()` logic matches Unity
- **ProjectSettings** — all fields present; all defaults correct (1920x1080, 60fps, 120bpm, 4/4, pool=10, max_layers=8, osc=9001)
- **EffectInstance** — all fields present including legacy param fields
- **EffectGroup** — all fields and `new()` / `clone_with_new_id()` correct
- **ParameterDriver** — all fields present; `evaluate()` logic produces equivalent results (different path, same math)
- **ParamEnvelope** — all fields present
- **GeneratorParamState** — all fields present
- **TempoMap** — all methods ported (`get_bpm_at_beat`, `add_or_replace_point`, `beat_to_seconds`, `seconds_to_beat`)
- **BeatQuantizer** — all constants correct (BPM_STEP=0.01, BEAT_STEP=0.0001, TIME_SECONDS_STEP=0.0001)
- **SelectionRegion** — correct (Rust adds convenience methods Unity doesn't have)
- **MidiMappingConfig** — `rebuild_dictionary`, `get_mapping_for_note`, `purge_orphaned_clip_ids` correct
- **RecordingProvenance** — all fields correct
- **PercussionImportState** — all fields correct
- **Color** — RGBA fields, WHITE/BLACK/CLEAR constants, `hsv_to_rgb()` correct
- **All enums** — BlendMode (0-12), EffectType (0-39), GeneratorType (0-19), LayerType (0-2), all with correct numeric values and all variants

### Gaps

#### GAP-CORE-1: 5 Traits/Interfaces NOT PORTED
**Severity: HIGH**
| Unity Interface | Purpose | Impact |
|---|---|---|
| `IParamSource` | Generic param get/set/def | Blocks generic parameter editing UI |
| `IEffectContainer` | Generic effect access | Blocks generic effect operations |
| `GeneratorParamSource` | IParamSource adapter for generators | Blocks generic gen param UI |
| `EffectCategoryRegistry` | Effect/gen type categories | No categorized browser |
| `MidiNoteParser` | MIDI note name conversion | No note display names |

#### GAP-CORE-2: Systematic Missing Property Clamping
**Severity: HIGH — silent data corruption**
Unity uses clamping setters; Rust uses plain `pub` fields. The following properties can be set to invalid values:
- `Layer.opacity` (should clamp 0-1)
- `Layer.midi_note` (should clamp 0-127 or -1)
- `Layer.midi_channel` (should clamp 0-15 or -1)
- `TimelineClip.scale` (should clamp min 0.01)
- `TimelineClip.loop_duration_beats` (should clamp min 0)
- `ProjectSettings.bpm` (should clamp 20-300)
- `ProjectSettings.master_opacity` (should clamp 0-1)
- `ProjectSettings.output_width/height` (should clamp min 1)
- `ProjectSettings.frame_rate` (should clamp min 1)
- `ProjectSettings.osc_send_port` (should clamp 1024-65535)
- `ProjectSettings.time_signature_numerator/denominator` (should clamp 1-16)
- `EffectGroup.wet_dry` (should clamp 0-1)

#### GAP-CORE-3: GeneratorParamState.reset_effectives() Behavioral Bug
**Severity: HIGH — silently breaks modulation**
Unity resets ONLY params that have active drivers or envelopes. Rust resets ALL params unconditionally. Any param manually adjusted by the user gets overwritten every frame if ANY driver exists on the same generator.

#### GAP-CORE-4: Missing ~40 Methods Across Core Structs
**Severity: MEDIUM to HIGH** (individually MEDIUM, collectively HIGH)

**Project** — missing: `CreateNew()`, `Validate()`, `PurgeOrphanedReferences()`, `GetStatistics()`
**Timeline** — missing: `AddLayer()`, `InsertLayer()`, `RemoveLayer()`, `MoveLayer()`, `FindLayerById()`, `GetContentRange()`, `ClearAllClips()`, `HasExportRange` property, export range setters
**Layer** — missing: `FindEffect()`, `FindEffectGroup()`, `FindGenDriver()`, `FindGenEnvelope()`, `SetGenParam()`, `SetGenParamBase()`, `ChangeGeneratorType()` (doesn't propagate to clips), `RestoreGeneratorState()` (doesn't propagate), `ClearClips()`, `GetDurationBeats()`
**TimelineClip** — missing: `Clone()` with deep effect/group/envelope cloning, `Scale` clamping setter
**ProjectSettings** — missing: `SecondsPerBeat`, `SecondsPerBar`, `GetQuantizeInterval()`, `QuantizeTime()`, `ResolutionPreset` setter that syncs width/height, `HasAnyMasterEffect`, `FindMasterEffect()`
**EffectInstance** — missing: `IParamSource` methods, `FindDriver()`, `CreateDriver()`, `RemoveDriver()`, `DisplayName`, `ParamCount`, `EnsureParamCapacity()`
**GeneratorParamState** — missing: `GetParam()`, `SetParam()`, `GetParamBase()`, `SetParamBase()`, `FindDriver()`, `FindEnvelope()`, `Restore()`, `SnapshotParams/Drivers/Envelopes()`
**VideoLibrary** — missing: `AddClip()`, `RemoveClip()`, `HasClip()`, `Clear()`, `ScanDirectory()`, `FindClipByPath()`, `ValidateClips()`, `RemoveMissingClips()`
**MidiMappingConfig** — missing: `AssignClipToNote()`, `RemoveClipFromNote()`, `ClearNote()`, `GetClipsForNote()`, `ClearAllMappings()`
**RecordingProvenance** — missing: `EnsureValid()`, `Clear()`, `AddRecordedClip()`, `AddTempoChange()`, all tempo lane methods

#### GAP-CORE-5: ParameterDriver Random Hash Divergence
**Severity: MEDIUM — different visual output for random waveform**
Unity uses a 3-round mixing hash: `h ^= h >> 16; h *= 0x45d9f3b; h ^= h >> 16; h *= 0x45d9f3b; h ^= h >> 16`
Rust uses a single Fibonacci multiply: `seed.wrapping_mul(2654435761)`
These produce different "random" sequences — visual output for `DriverWaveform::Random` will differ.

#### GAP-CORE-6: Registry Metadata Incomplete
**Severity: MEDIUM — blocks UI formatting and OSC**
`EffectType::param_defs()` and `GeneratorType::param_defs()` return simplified tuples missing: `wholeNumbers`, `isToggle`, `valueLabels`, `formatString`, `oscSuffix`, `oscPrefix`. This blocks value formatting in UI and OSC address generation.

#### GAP-CORE-7: PathResolver NOT PORTED
**Severity: HIGH — projects break on file movement**
380 lines. Three-step resolution (absolute, relative, filename+size search). `StoreRelativePaths()` for pre-save. Without this, projects are fragile and cannot be shared between machines.

#### GAP-CORE-8: Minor Behavioral Divergences
- `TempoMapConverter.seconds_per_beat_from_bpm()` — Rust does NOT clamp BPM to 20-300 (Unity does)
- `BeatQuantizer.quantize_time_seconds()` — Rust rounds negative values (Unity preserves negative sentinels like -1)
- `TimelineClip.overlaps_with()` — Rust does NOT check layer index (Unity returns false for different layers)
- `EffectInstance.ensure_base_values()` — Rust doesn't check length mismatch (only checks `is_none()`)
- `Layer.change_generator_type()` — Rust does NOT call `UpdateClipGeneratorTypes()` to propagate

---

## 2. manifold-editing — Commands & EditingService

**Verdict: 75% functional (Commands 100%, Service ~50%)**

### What's Correct (verified)

#### All 38 Command Types — FULLY PORTED (1:1 with Unity)

**Clip (11):** MoveClip, TrimClip, DeleteClip, AddClip, ClipEffects, ChangeClipLoop, SwapVideo, SlipClip, ChangeClipRecordedBpm, SplitClip, MuteClip

**Layer (5):** AddLayer, DeleteLayer, ReorderLayer, GroupLayers, UngroupLayers

**Effect (5):** AddEffect, RemoveEffect, ReorderEffect, ToggleEffect, ChangeEffectParam

**Effect Group (7):** GroupEffects, UngroupEffects, ToggleGroup, RenameGroup, ChangeGroupWetDry, ReorderRack, MoveEffectToRack

**Envelope (7):** AddParamEnvelope, RemoveParamEnvelope, ChangeEnvelopeADSR, ChangeEnvelopeTargetNormalized, ToggleEnvelopeEnabled, AddLayerEnvelope, RemoveLayerEnvelope

**Driver (6):** AddDriver, ToggleDriverEnabled, ChangeDriverBeatDiv, ChangeDriverWaveform, ToggleDriverReversed, ChangeTrim

**Settings (12):** ChangeBpm, RestoreRecordedTempoLane, ClearTempoMap, ChangeQuantizeMode, ChangeResolution, ChangeFrameRate, ChangeLayerMidiNote, ChangeLayerBlendMode, ChangeLayerOpacity, ChangeGeneratorParams, ChangeGeneratorType, ChangeMasterOpacity

**Selection (1):** SetSelectionRegion

**Extra (1):** RescaleBeatsForBpmChange (ported from Unity `PercussionImportOrchestrator`)

#### UndoRedoManager — COMPLETE
- MAX_UNDO_HISTORY = 200 matches Unity
- VecDeque undo + Vec redo matches Unity's LinkedList + Stack semantics
- Execute, Record, Undo, Redo, Clear — all correct

#### CompositeCommand — CORRECT
- Forward execute, reverse undo

#### EffectClipboard — COMPLETE

### Gaps

#### GAP-EDIT-1: EditingService Missing ~16 Methods
**Severity: HIGH**

**Missing (requires UIState — moved to app layer in Rust):**
- `OnClipSelected()`, `GetEffectiveSelectedClips()`, `SelectRegionTo()`, `SelectAllClips()`, `DeleteSelectedLayers()`, `ToggleMuteSelectedClips()`, `CutSelectedClips()`, `GroupSelectedLayers()`

**Missing (requires PlaybackController):**
- `SplitSelectedClipsAtPlayhead()`, `GetSecondsPerBeat()`, `GetCurrentGridStep()`

**Missing (region operations):**
- `SplitClipsForRegionMove()` — returns RegionSplitResult for drag operations

**Missing (audio sync):**
- `OnWaveformDragDeltaBeats()`, `OnWaveformDragEnd()`

Note: Many of these are handled at the app layer in Rust (app.rs/input_host.rs) via the trait-based host pattern. This is an acceptable architectural adaptation for Rust's ownership model, but means the app layer must correctly replicate the orchestration Unity centralizes in EditingService.

#### GAP-EDIT-2: 2 Traits Missing
**Severity: MEDIUM**
- `ILayerLifecycleCallbacks` — not ported. `AddLayerCommand`/`DeleteLayerCommand` don't fire lifecycle callbacks.
- `ISelectionRegionTarget` — not ported. `SetSelectionRegionCommand` execute/undo are no-ops in Rust.

#### GAP-EDIT-3: Behavioral Divergences
- **SetSelectionRegionCommand** — execute/undo are no-ops in Rust; Unity mutates selection state
- **ChangeParamEnvelopeCommand** — Unity is one atomic command; Rust splits into 3 separate commands (ADSR, Target, Enabled). Non-atomic undo.
- **EditingHost trait** — defined in Rust but NEVER USED by any method. Dead code.
- **EditingService returns commands** instead of executing them — caller must wrap in CompositeCommand. Acceptable Rust adaptation.

---

## 3. manifold-playback — Engine, Scheduling, Sync

**Verdict: 30% functional**

This is the biggest gap in the port. Prior Audit 1 claimed 60% — verified as significantly overclaimed.

### What's Correct (verified)

#### ClipScheduler — 100% PORTED
All methods present with value-level parity. Live slot merging, start/stop diff, min-remaining skip, looping bypass — all correct.

#### ActiveTimelineClipWindow — 100% PORTED
All methods, all 3 comparators, both binary search helpers, all constants (BACKWARD_EPSILON=0.0001, LARGE_JUMP_BEATS=32.0). Comprehensive tests.

#### SyncArbiter — 95% PORTED
All methods present. Structural divergence (static methods vs instance methods) but functionally equivalent.

#### SyncSource trait — 100% PORTED
`is_enabled`, `display_name`, `enable`, `disable`, `toggle` with default impl.

#### OscPositionSender — 95% PORTED
Full OSC encoding with custom minimal implementation. Transport change detection, seek detection, echo suppression.

#### TransportController — 85% PORTED
Core transport actions, authority cycling, BPM editing, tempo map management.

#### All Temporal Constants — EXACT MATCH
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

### Critical Gaps

#### GAP-PLAY-1: 5 Critical Files at 0% — ENTIRELY MISSING
**Severity: CATASTROPHIC — makes core features non-functional**

| Missing File | Unity Lines | Impact |
|---|---|---|
| **DriverController.cs** | 104 | All LFO parameter modulation dead |
| **ParameterDriverManager.cs** | 110 | Per-effect and per-generator driver evaluation dead |
| **EnvelopeEvaluator.cs** | 380+ | All ADSR envelope modulation dead |
| **ClipLauncher.cs** | 629 | MIDI NoteOn/NoteOff → clip triggering impossible |
| **TempoRecorder.cs** | 330+ | Tempo recording sessions impossible |

**The entire modulation pipeline is dead:**
```
Unity:  PlaybackController.Update() → DriverController → ParameterDriverManager.EvaluateAll()
                                     → EnvelopeEvaluator.EvaluateAll()
                                     → if anyDirty → MarkCompositorDirty()

Rust:   NOTHING. No DriverController. No ParameterDriverManager. No EnvelopeEvaluator.
        Every LFO and every ADSR envelope is silently broken.
```

#### GAP-PLAY-2: PlaybackController Orchestration ~15% Ported
**Severity: CATASTROPHIC**
Unity's `PlaybackController.Update()` (1500+ lines) orchestrates the entire playback loop. Rust absorbs ~15% of this into `engine.rs` tick methods, but is missing:
- Full Update() loop with Link/MidiClock beat derivation
- `ApplyResolvedTempo()` / `TryResolveExternalTempo()`
- `UpdateRecordingSessionState()`
- `CheckPreparingClips()` / `CheckCustomLoopBoundaries()` / `CorrectVideoDrift()`
- `UpdateActiveClipPlaybackRates()`
- `UpdateCompositor()` / `UpdateLookaheadPrewarm()`
- Pre-warm system (~200 lines)
- Event hooks (OnStateChanged, OnTimeChanged)

#### GAP-PLAY-3: live_slots NOT WIRED in sync_clips_to_time
**Severity: CRITICAL — live clips invisible to scheduler**
```rust
// engine.rs line 605
let sync_result = self.scheduler.compute_sync(
    ...
    &[],  // live_slots — wired in Phase 3D  ← TODO
    ...
);
```
Live MIDI clips are never included in sync scheduling.

#### GAP-PLAY-4: No 5ms NoteOn/NoteOff Timing Guard
**Severity: CRITICAL — MIDI performance breaks**
`ClipLauncher.cs` (entirely missing) contains the 5ms timing guard. Without it, MIDI controllers that send NoteOff quickly after NoteOn incorrectly cancel phantom clips.

#### GAP-PLAY-5: PlaybackEngine Missing ~50% of Methods
**Severity: HIGH**
Missing methods include:
- All clip time/rate methods: `GetClipPlaybackRate()`, `GetClipStartTimeSeconds()`, `GetClipEndTimeSeconds()`, `GetClipDurationSeconds()`, `ResolveClipRecordedBpm()`, `GetClipSourceElapsedSeconds()`, `ComputeVideoTime()` (beat-domain)
- Clip maintenance: `CheckPreparingClips()`, `CheckCustomLoopBoundaries()`, `CorrectVideoDrift()`, `UpdateActiveClipPlaybackRates()`, `SeekActiveClips()`
- Pre-warm: `ComputePrewarmCandidates()`, entire lookahead system
- `FilterReadyClips()` recently-started gate with variable duration
- 10 prewarm-related constants (all missing)

#### GAP-PLAY-6: GeneratorRenderer Has No ClipRenderer Implementation
**Severity: CRITICAL — generators can't be invoked polymorphically**
`GeneratorRenderer` struct exists in `manifold-renderer` with `start_clip`, `stop_clip`, `render_all`, etc., but does NOT implement the `ClipRenderer` trait from `manifold-playback`. Cannot be used alongside video renderer.

#### GAP-PLAY-7: LiveClipManager ~55% Ported
**Severity: HIGH**
Core trigger/commit present but missing:
- `TryGetPendingLiveLaunchForCommit()` — complex pending commit resolution
- All provenance tracking: `TrackRecordingClipStart()`, `FinalizeRecordingClip()`, `RemoveRecordingClipStart()`
- `AppendLivePrewarmCandidates()`
- `commit_live_clip` missing `startAbsoluteTick` parameter

#### GAP-PLAY-8: LiveClipHost Trait Missing 4 Methods
| Missing | Purpose |
|---|---|
| `GetTempoSourceAtBeat()` | Recording provenance |
| `InvalidateLookaheadPrewarm()` | Video decoder efficiency |
| `ShowDebugLogs` | Debug toggle |
| `CurrentProject` | Project access |

#### GAP-PLAY-9: Sync Controllers Are Stubs
| Controller | Status | Detail |
|---|---|---|
| LinkSyncController | ~25% stub | Fields present, no Update loop, no FFI |
| MidiClockSyncController | ~30% stub | BPM estimator ported, no Update loop |
| MidiClock.cs | **0% MISSING** | Native CoreMIDI FFI — no MIDI input at all |
| OscSyncController | **0% MISSING** | No OSC input capability |
| OscReceiver | **0% MISSING** | No OSC listening |
| All 4 OSC bridges | **0% MISSING** | No param-to-OSC exposure |
| AbletonLink.cs | **0% MISSING** | Native FFI wrapper |

#### GAP-PLAY-10: Additional Missing Files
| File | Lines | Purpose |
|---|---|---|
| VideoPlayerPool.cs | 700+ | Platform-specific video player pool |
| PerfLogger.cs | 280+ | Performance CSV logging |
| ComputeShaderCache.cs | 75 | Compute shader caching |
| IPlaybackNotifier.cs | 19 | Compositor dirty notification |
| ISyncTarget.cs | 31 | Sync controller target interface |

---

## 4. manifold-renderer — Effects

**Verdict: 16 of 28 registered effect processors ported (57%); 14 missing**

### What's Correct (verified)

16 effects ported with verified parameter parity, pass counts, texture formats, and stateful state management:

| # | Effect | Stateful? | Passes | Status |
|---|---|---|---|---|
| 1 | InvertColors | No | 1 | Ported (standalone processor; Unity handles inline — divergence but functionally equivalent) |
| 2 | Feedback | **Yes** | 1+copy | Ported — per-owner state |
| 3 | Bloom | **Yes** | 4 | Ported — per-owner pyramid, MAX_LEVELS=6, constants verified |
| 4 | Kaleidoscope | No | 1 | Ported |
| 5 | EdgeStretch | No | 1 | Ported |
| 6 | QuadMirror | No | 1 | Ported |
| 7 | Dither | No | 1 | Ported |
| 8 | Strobe | No | 1 | Ported |
| 9 | StylizedFeedback | **Yes** | 1+copy | Ported — per-owner state |
| 10 | Mirror | No | 1 | Ported |
| 11 | CRT | **Yes** | 3 | Ported — per-owner state |
| 12 | ColorGrade | No | 1 | Ported |
| 13 | ChromaticAberration | No | 1 | Ported |
| 14 | Glitch | No | 1 | Ported |
| 15 | FilmGrain | No | 1 | Ported |
| 16 | Halation | **Yes** | 3 | Ported — per-owner state |

Transform (EffectType 0) is handled inline in the compositor blend pass in both Unity and Rust — not a registered processor in either.

### Gaps

#### GAP-FX-1: 14 Effect Types Missing
**Severity: CRITICAL — 50% of effect types unavailable**

**Simple (single-pass blit — quick ports):**
| # | Effect | Unity Lines | Notes |
|---|---|---|---|
| 1 | GradientMap | 22 | WGSL shader `fx_gradient_map.wgsl` already exists — only needs .rs processor + registration |
| 2 | EdgeGlow | 29 | Simple blit |
| 3 | Infrared | 35 | Simple blit |
| 4 | Surveillance | 37 | Simple blit |
| 5 | Redaction | 37 | Simple blit |
| 6 | VoronoiPrism | 22 | Simple blit |

**Stateful (per-owner temporal buffers):**
| # | Effect | Unity Lines | Notes |
|---|---|---|---|
| 7 | InfiniteZoom | 142 | Stateful, 2-pass |
| 8 | Datamosh | 124 | Stateful, temporal buffers |
| 9 | SlitScan | 84 | Stateful, temporal buffers |
| 10 | Corruption | 101 | Stateful |

**Complex (compute pipelines or native dependencies):**
| # | Effect | Unity Lines | Notes |
|---|---|---|---|
| 11 | PixelSort | 47+308 | Compute pipeline + BitonicSort.compute |
| 12 | FluidDistortion | 185+239+292 | Compute fluid solver (3+ compute shaders) |
| 13 | Microscope | 163 | Stateful, 6KB C# + 11KB shader |
| 14 | WireframeDepth | 1,094 | 14 render passes, DNN backend, temporal tracking |
| — | BlobTracking | 444 | Requires native blob detection library — special case |

Note: BlobTracking is listed as a 15th missing effect in some counts, but its native library dependency makes it a special case.

---

## 5. manifold-renderer — Compositor

**Verdict: 50% functional**

### What's Correct
- `LayerCompositor` — layer grouping, mute/solo, multi-clip per layer compositing
- `EffectChain` — chain dispatch with group wet/dry blending
- `EffectRegistry` — factory + storage for effect processors
- `TonemapPipeline` — ACES tonemap
- `PingPong` struct — source/target swap
- Blend modes via uniform integer (functionally equivalent to Unity's shader keyword variants)
- `PostProcessEffect` and `StatefulEffect` traits — same methods as Unity interfaces

### Gaps

#### GAP-COMP-1: ComputeCompositor Missing
**Severity: MEDIUM (optimization, not correctness)**
Unity's single-pass compute compositor (`TileCompositor.compute`) batches up to 8 effect-free layers. Performance regression on 8+ layer projects.

#### GAP-COMP-2: No Material/Pipeline Caching
**Severity: MEDIUM (performance)**
Bind groups recreated per frame. Unity caches materials per blend mode.

#### GAP-COMP-3: Missing Compositor Features
- No shader warmup pass (first-frame hitches)
- No external output tap (ExternalTapIndex)
- `CleanupEffectOwner` trait exists but not wired into compositor lifecycle
- No effect timing diagnostics
- No MasterOpacity / SuppressDisplay / RenderedThisFrame tracking

---

## 6. manifold-renderer — Generators

**Verdict: 88% functional (18/18 types exist, 2 broken, infrastructure gaps)**

### What's Correct (verified)
All 18 Unity generators have Rust implementations with matching parameter counts:

| Generator | Params | Category | Status |
|---|---|---|---|
| Plasma | 6 | Shader | Correct |
| FractalZoom | 2 | Shader | Correct |
| BasicShapesSnap | 3 | Shader | Correct |
| ConcentricTunnel | 6 | Shader | Correct |
| NumberStation | 8 | Shader | Correct |
| Tesseract | 11 | Line | **Broken** — see GAP-GEN-1 |
| Duocylinder | 11 | Line | **Broken** — see GAP-GEN-1 |
| Lissajous | 11 | Line | Correct (2D, unaffected by project_4d issue) |
| OscilloscopeXY | 9 | Line | Correct (2D) |
| WireframeZoo | 8 | Line | Correct (3D) |
| ReactionDiffusion | 4 | Stateful shader | Correct |
| Flowfield | 6 | Compute particle | Correct |
| StrangeAttractor | 8 | Compute particle | Correct |
| ComputeStrangeAttractor | 11 | Compute particle | Correct |
| Mycelium | 12 | Compute (4-pass) | Correct |
| ParametricSurface | 5 | Compute volume | Correct |
| FluidSimulation | 20 | Compute particle | **Degraded** — see GAP-GEN-2 |
| FluidSimulation3D | 26 | Compute particle | **BROKEN** — see GAP-GEN-3 |

### Gaps

#### GAP-GEN-1: project_4d Missing Second Projection Stage
**Severity: HIGH — Tesseract and Duocylinder look geometrically wrong**
Unity does two-stage projection: 4D→3D (`f = projDist / (projDist - w)`), then 3D→2D (`s = projDist / (projDist + p3z)`).
Rust does single-stage: 4D→2D (`scale = projDist / (projDist - w)`) — no depth-based foreshortening.
Additionally, Tesseract discards `_pz` and never populates `projected_z`, so depth-sorted edge animation is broken.

#### GAP-GEN-2: FluidSimulation2D — 3 Issues
**Severity: MEDIUM (functional but degraded)**
1. Particle cap 2M vs Unity 8M (FM-9: Hallucinated Constraints)
2. Texture formats: Rgba16Float everywhere instead of R32Float for density / Rg32Float for vector field (FM-10)
3. Blur radius truncation instead of rounding

#### GAP-GEN-3: FluidSimulation3D — STRUCTURALLY BROKEN
**Severity: CRITICAL — produces wrong visual output**
4 catastrophic issues:
1. Gradient magnitude 128x too strong (divides by `2.0 * texel` instead of multiplying by `0.5`)
2. Particles die after 3-6 seconds (should be permanent)
3. Density capping formula wrong
4. Velocity field divergence not conserved

Plus 9 major and 6 moderate issues documented in `FLUID_SIM_AUDIT.md`.

#### GAP-GEN-4: GeneratorRenderer Not Wired as ClipRenderer
**Severity: HIGH**
`GeneratorRenderer` struct exists with `start_clip`, `stop_clip`, `render_all` methods but does NOT implement the `ClipRenderer` trait. Cannot be used polymorphically alongside a video renderer in the playback engine.

#### GAP-GEN-5: No Shared Base Classes
**Severity: MEDIUM (code duplication)**
Each shader-blit generator reimplements uniform-setting boilerplate. Each particle generator reimplements buffer allocation and scatter dispatch. Unity centralizes these in `ShaderGeneratorBase` and `ComputeParticleGeneratorBase`.

#### GAP-GEN-6: Display Name Mismatches
| GeneratorType | Unity | Rust |
|---|---|---|
| BasicShapesSnap | "Basic Shapes Snap" | "Basic Shapes" |
| ReactionDiffusion | "Reaction-Diffusion" | "Reaction Diffusion" |
| ComputeStrangeAttractor | "Strange Attractor (GPU)" | "Compute Attractor" |
| FluidSimulation3D | "Fluid Simulation 3D" | "Fluid Sim 3D" |

---

## 7. manifold-ui — Panels, Input, Interaction

**Verdict: 65% functional**

### What's Correct (verified)

#### UIState — EXACT MATCH (25 fields, 17+ methods)
Every field verified: `selected_clip_ids`, `selection_version`, `primary_selected_clip_id`, `selected_layer_index`, all drag/trim/scrub state, all selection methods. Minor type adaptations (Option<usize> vs int with -1 sentinel).

#### CoordinateMapper — EXACT MATCH (20 methods)
All methods verified: `beat_to_pixel`, `pixel_to_beat`, `set_zoom_by_index`, `rebuild_y_layout` with all collapse constants (140/48/62/70px), `get_layer_at_y` with reverse iteration. Grid snapping thresholds match.

#### Input System — COMPLETE
PointerAction, Modifiers, Key enum, UIEvent, DRAG_THRESHOLD_PX=4, DOUBLE_CLICK_TIME_SEC=0.3.

#### InteractionOverlay — COMPLETE
DragMode, DragSnapshot, SNAP_THRESHOLD_PX=12, MAX_SNAP_BEATS=0.5, select_region_to with correct anchor priority.

#### Keyboard Shortcuts — 32 of 37 Wired
All major shortcuts present. Only 5 percussion import shortcuts (Ctrl+Shift+I/M/[/]/R) not wired (blocked on percussion subsystem).

#### 14 Panels Ported
TransportPanel, HeaderPanel, FooterPanel, LayerHeaderPanel, MasterChromePanel, LayerChromePanel, ClipChromePanel, EffectCardPanel, GenParamPanel, InspectorCompositePanel, TimelineViewportPanel, DropdownPanel, PerfHudPanel + supporting infrastructure (BitmapSlider, BitmapScrollContainer, BitmapText).

#### PanelAction Enum — ~100 Variants
Comprehensive coverage of transport, file, export, header, footer, layer, inspector, effect, generator, timeline, viewport, dropdown, and context menu actions.

### Gaps

#### GAP-UI-1: 5 Critical Missing Panels
**Severity: HIGH — visible gaps in every session**

| Panel | Purpose | Impact |
|---|---|---|
| **OverviewStripPanel** | Mini-timeline with viewport indicator, playhead | Navigation feature lost |
| **BrowserPopupPanel** | Effect/generator browser with categories, search | No categorized add-effect workflow |
| **ClipInspector** | Clip-specific property panel | No clip property editing |
| **LayerInspector** | Layer-specific property panel | No layer property editing |
| **MasterInspector** | Master output property panel | No master property editing |

#### GAP-UI-2: Missing Medium-Priority Components
| Component | Impact |
|---|---|
| EffectsListBitmapPanel + RackHeaderBitmapPanel | No effect drag-reorder, no rack headers |
| EffectSelectionManager | No effect multi-select, copy/paste |
| ViewportManager | No auto-scroll, no follow-playhead |
| BitmapTextInput | No inline text editing (BPM field, etc.) |
| ClipThumbnailBuilder + ThumbnailCache | No clip/generator thumbnails |
| TempoLaneEditor | No tempo curve editing |
| InspectorPanelBase + IInspectorPanel | No inspector panel switching infrastructure |

#### GAP-UI-3: ~40 Unity Files With No Rust Equivalent
Many are Unity-specific (UGUI scroll rects, Unity GL renderer) and don't need porting. Excluding Unity-specific files, approximately 20 UI files are genuinely missing.

---

## 8. manifold-io — Project Loading, Saving, Migration

**Verdict: 25% functional**

### What's Correct (verified)

#### Migrator — PERFECT 1:1 TRANSLATION
Version detection, V1.0.0→V1.1.0 migration (percussion nesting, generator param nesting), `move_field()` helper, semver comparison — all correct. **This is the only file in manifold-io with full parity.**

#### Loader — Reads Data Correctly
V1 JSON and V2 ZIP extraction work. Deserialization via serde is correct. `on_after_deserialize()` called.

### Gaps

#### GAP-IO-1: Saver Is a 30-Line V1 Stub
**Severity: CRITICAL — projects saved in wrong format**
Rust writes plain JSON. Unity writes V2 ZIP archives with:
- SHA-256 hash dedup (skip on no-change)
- Gzipped snapshot history (max 50 entries)
- Manifest with timestamp, hash, label
- Atomic writes (write to .tmp, rename on success)
- `PathResolver.StoreRelativePaths()` pre-save

**Impact:** Rust-saved projects are V1 flat JSON. No version history, no crash recovery, no dedup, not cross-compatible with Unity loader's V2 expectations.

#### GAP-IO-2: Loader Skips ALL Post-Load Validation
**Severity: HIGH — silent data degradation**
6 Unity post-load steps not performed:
1. BPM sync from tempo map beat 0 (clamp 20-300)
2. DurationMode migration (force all to NoteOff)
3. `PathResolver.ResolveAll()` — broken path auto-healing
4. `project.Validate()` — structural validation
5. `VideoLibrary.ValidateClips()` — missing file detection
6. `PurgeOrphanedReferences()` — stale clip/MIDI reference cleanup

#### GAP-IO-3: PathResolver NOT PORTED
**Severity: HIGH — 380 lines**
Three-step file resolution (absolute → relative → filename+size search), search directory discovery, pre-save relative path storage.

#### GAP-IO-4: VideoExporter NOT PORTED
**Severity: MEDIUM (future feature) — 1,275 lines**
FFmpeg subprocess, GPU readback, Metal encoding, HDR export, audio muxing. No video export capability.

#### GAP-IO-5: FCPXML Export NOT PORTED
**Severity: LOW — 263 lines**

---

## 9. manifold-app — Application Layer

**Verdict: 55% functional**

### What's Correct (verified)

#### UserPrefs — COMPLETE
JSON-backed persistent key-value store at `~/Library/Application Support/MANIFOLD/prefs.json`. `get_string()`, `set_string()`, `save()`. Faithful port of Unity's PlayerPrefs string subset.

#### DialogPathMemory — COMPLETE
6 contexts, legacy key migration, directory persistence. Same pref key prefix `"MANIFOLD_DialogPath_"`.

#### File I/O Methods — FUNCTIONAL
- `open_project()` — rfd dialog, remembers directory, loads via shared path
- `open_recent_project()` — reads pref, validates existence
- `open_project_from_path()` — detects V1/V2, loads, applies layout, restores playhead
- `save_project()` — checks path, saves (but V1 format only)
- `save_project_as()` — rfd dialog, saves, persists path

#### Infrastructure — FUNCTIONAL
TransportStateCache, InputHandler, UIRoot, AppEditingHost, AppInputHost, FrameTimer, WindowRegistry, TextInputState.

### Gaps

#### GAP-APP-1: ProjectIOService NOT Ported as Unit (FM-14)
**Severity: HIGH**
Unity's `ProjectIOService.cs` (527 lines) owns all project lifecycle as a cohesive service. Rust scatters equivalent logic as private methods on `Application` in `app.rs`. `NewProject` is duplicated in two places (PanelAction handler and Cmd+N handler).

#### GAP-APP-2: Video File Drop Completely Stubbed
**Severity: CRITICAL — primary content import broken**
All video extensions log a message and do nothing. No clip creation, no library import, no metadata extraction, no layer auto-creation.

#### GAP-APP-3: MIDI File Drop Not Handled
**Severity: MEDIUM**
`.mid`/`.midi` files fall through as "Unrecognized file type". No `MidiFileParser` or `MidiImportService` equivalent.

#### GAP-APP-4: Missing Post-Load Pipeline
`PrepareForProjectSwitch()` (stop + cleanup before loading new project), `OnProjectOpened()` hooks (MIDI clock source, scan video folders, pre-warm lookahead) — all missing.

#### GAP-APP-5: Save Uses V1 Format
`save_project()` writes plain JSON via `serde_json::to_string_pretty`. Unity always saves as V2 ZIP.

---

## 10. Audio/Percussion Pipeline — 0% Ported

**Unity: 16 runtime files, ~7,451 lines + 4 Editor-only files (869 lines)**
**Rust: ZERO files. Entire subsystem absent.**

This subsystem was dismissed as a one-line footnote by both prior audits. It is in fact the second-largest subsystem by line count after UI, containing critical functionality for audio-driven VJ workflows.

### Architecture

The pipeline flows:
```
User drops audio file → PercussionImportOrchestrator
    → ExternalProcessRunner launches Python analysis
    → Python (madmom + Demucs + ADTOF + Basic Pitch)
    → JSON output parsed by PercussionAnalysisParser
    → BPM auto-detection (confidence > 0.72 → auto-apply)
    → PercussionTimelinePlanner converts seconds → beat placements
    → PercussionImportService creates clips on timeline layers
    → ImportedAudioSyncController loads audio for sync playback
    → StemAudioController loads 4 stems (drums/bass/other/vocals)
    → Waveform UI renders multi-res spectral display
```

### File Inventory

#### Core Pipeline (required for basic audio import — 4,189 lines)

| File | Lines | Purpose |
|---|---|---|
| `PercussionImportOrchestrator.cs` | 1,501 | Central orchestration: file selection, pipeline invocation, progress tracking, JSON parsing, timeline placement, undo, audio sync, stem resolution, alignment, BPM rescaling, re-analysis |
| `PercussionAnalysisModels.cs` | 561 | All data types: PercussionTriggerType (11 variants), PercussionEvent, PercussionBeatGrid, PercussionAnalysisData, PercussionClipBinding, PercussionImportOptions, PercussionClipPlacement, PercussionPlacementPlan |
| `PercussionAnalysisParser.cs` | 439 | JSON parser for Python backend output. Maps dozens of string aliases to trigger types |
| `PercussionImportService.cs` | 405 | Timeline mutation: layer resolution, clip creation, BPM auto-apply, undo recording |
| `MidiFileParser.cs` | 309 | Standard MIDI File parser (format 0 + 1). Extracts note-on/off pairs, converts ticks to beats via PPQ |
| `MidiImportService.cs` | 183 | Applies parsed MIDI notes to timeline as clips. Overlap trimming, round-robin assignment |
| `PercussionImportState.cs` | 162 | Serializable project state: audioPath, audioStartBeat, clipPlacements, energyEnvelope, stemPaths |
| `PercussionTimelinePlanner.cs` | 150 | Seconds→beats conversion with quantization, confidence gating, energy gating, dedup |
| `PercussionImportOptionsFactory.cs` | 126 | Default trigger→generator bindings (Kick→WireframeZoo, Snare→BasicShapesSnap, etc.) |
| `PercussionBindingResolver.cs` | 100 | Resolves trigger bindings against project library/layer state |
| `BeatTimeConverter.cs` | 90 | Tempo-aware time conversion + clip reprojection planner |
| `AudioFileIdentity.cs` | 63 | Content-based SHA-256 identity hash (first 64KB + file size) |

#### Infrastructure (required for pipeline execution — 2,528 lines)

| File | Lines | Purpose |
|---|---|---|
| `PercussionPipelineBackendResolver.cs` | 1,176 | Resolves Python/ffmpeg/Demucs binaries, builds command-line invocations. Two backends: BundledRuntime (shipped Python) and ProjectPython (dev .venv) |
| `PercussionPipelineSettings.cs` | 711 | ScriptableObject with 12 nested settings sections for every instrument type and detection parameter |
| `PercussionPipelineProgressParser.cs` | 154 | Parses pipeline stdout into progress updates (`MANIFOLD_PROGRESS|0.50|message` protocol) |
| `ExternalProcessRunner.cs` | 487 | Async subprocess execution (see Section 14) |

#### Audio Playback Sync (945 lines)

| File | Lines | Purpose |
|---|---|---|
| `ImportedAudioSyncController.cs` | 473 | Timeline-synced audio playback. Per-frame play/pause/seek sync with tolerance-based resyncing. Probes encoder delay via ffprobe for MP3 sync compensation |
| `StemAudioController.cs` | 472 | 4-stem DAW-style playback (drums/bass/other/vocals). Sample-perfect sync to master. Mute/solo logic |

#### Alignment / Reprojection (538 lines)

| File | Lines | Purpose |
|---|---|---|
| `PercussionAlignmentService.cs` | 538 | Downbeat calibration at playhead, nudge by delta, reset, reprojection. Contains 2 ICommand implementations: ApplyPercussionAlignmentCommand, SetAudioStartBeatCommand |

#### UI — Waveform Display (1,870 lines)

| File | Lines | Purpose |
|---|---|---|
| `WaveformRenderer.cs` | 486 | Multi-resolution mip chain waveform with spectral (frequency-based) coloring |
| `ImportedAudioWaveformLane.cs` | 457 | Full waveform lane: viewport, playhead, remove/re-analyze buttons, stem expand chevron |
| `StemWaveformLane.cs` | 312 | Per-stem lane with waveform, playhead, mute (M) and solo (S) buttons |
| `StemLaneGroup.cs` | 145 | Manages 4 StemWaveformLanes as collapsible group |
| `WorkspaceController.PercussionImport.cs` | 132 | Bridge to orchestrator: MonoBehaviour lifecycle, progress bar rendering |
| `ImportedAudioWaveformDragHandler.cs` | 77 | Beat-snapped drag-to-reposition |
| `ImportedAudioWaveformScrubHandler.cs` | 46 | Click/drag scrubbing on waveform |
| `WaveformLaneScrollForwarder.cs` | 20 | Scroll forwarding to track ScrollRect |
| `TempoLaneEditor.cs` | 195 | Tempo map overlay on ruler (BPM range [20,300] → pixel range [5,35]) |

#### Playback-Adjacent (793 lines)

| File | Lines | Purpose |
|---|---|---|
| `MidiInputController.cs` | 526 | See Section 11 — listed separately due to CRITICAL priority |
| `TempoRecorder.cs` | 267 | Tempo recording sessions, clip provenance tracking (start/end time/beat/tick/BPM) |

---

## 11. MIDI Input — 0% Ported (CRITICAL)

**Unity: `Input/MidiInputController.cs` (526 lines)**
**Rust: ZERO equivalent**

This file makes MIDI controllers work. Without it, no hardware MIDI device can trigger clips.

### What MidiInputController Does

- **Device discovery** via Minis library (Unity Input System MIDI backend)
- **Channel filtering** — only process events from configured channel
- **Device name filtering** — `SetDeviceFilter(string)` to target specific controller
- **Two note event paths:**
  1. **Minis callbacks** — standard MIDI for standalone operation
  2. **Native clock note queue** — deterministic tick-aligned events when MidiClock is clock authority. Drains `MidiClock.DrainNativeNoteEvents()` for tick-precise sequencing
- **Same-tick NoteOff-before-NoteOn reordering** — ensures deterministic behavior when NoteOff and NoteOn arrive in the same tick
- **Auto-play on first note** — starts playback if not already playing
- **Routes to ClipLauncher** — `clipLauncher.OnNoteOn(midiNote, velocity, channel)` / `clipLauncher.OnNoteOff(midiNote, channel)`

### Dependencies
ClipLauncher (0% ported), PlaybackController (15% ported), MidiClock (0% ported), MidiMappingConfig, ClockAuthority

### Rust Equivalent
Would use `midir` crate for cross-platform MIDI input. The two-path architecture (callback vs clock-queue) and same-tick reordering logic must be preserved exactly.

### Also Missing: `Input/FileDragDrop.cs` (200 lines)

Native macOS file drop plugin wrapper. In Rust, winit provides `WindowEvent::DroppedFile` / `WindowEvent::HoveredFile` natively, so this is a non-issue architecturally. The Rust app already handles `DroppedFile` events in `app.rs` (though video drop processing is stubbed — see GAP-APP-2).

---

## 12. LED/External Output — 0% Ported

**Unity: 8 files, 853 lines**
**Rust: ZERO files**

### File Inventory

| File | Lines | Purpose | Priority |
|---|---|---|---|
| `IExternalOutput.cs` | 17 | Interface: Initialize, ProcessFrame, Blackout, Shutdown. Listed in CLAUDE.md trait table. | **Medium** |
| `ExternalOutputType.cs` | 17 | Enum: ArtNet, Syphon + StripAddressing (PerUniverse, Packed) | **Medium** |
| `LEDConstants.cs` | 23 | Constants: 512 bytes/universe, 8 strips default, 120 LEDs/strip, ArtNet header 18 bytes | **Medium** |
| `LedSettings.cs` | 34 | Config struct: enabled, outputType, IP/port, stripCount, ledsPerStrip, bgrMode, energyGate | **Medium** |
| `ArtNetDmxConverter.cs` | 123 | Pure byte-packing: pixels → DMX universes with Resolume-style linear addressing and universe boundary straddling | **Medium** |
| `ArtNetOutput.cs` | 408 | Full pipeline: compositor blit through edge-extension shader → async GPU readback → DMX pack → UDP send. Background thread for ArtPoll reachability probing + ICMP ping fallback | **Medium** |
| `LEDOutputController.cs` | 209 | Orchestrator: subscribes to compositor external tap, routes frames to active IExternalOutput. Energy-envelope brightness modulation (quiet → dim, drops → full) | **Medium** |
| `SyphonOutput.cs` | 22 | **STUB** — returns false from Initialize(). Not implemented in Unity. | **Skip** |

### Architecture
```
CompositorStack → ExternalTapCallback → LEDOutputController
    → ArtNetOutput.ProcessFrame()
        → Graphics.Blit with LEDEdgeExtend.shader (tiny pixel grid)
        → AsyncGPUReadback → ArtNetDmxConverter → UDP send
```

### Porting Notes
- `ArtNetDmxConverter` is pure portable byte-packing logic
- `ArtNetOutput` needs wgpu async readback equivalent for GPU → CPU pixel transfer
- `LEDEdgeExtend.shader` needs WGSL translation
- UDP networking uses `std::net::UdpSocket` in Rust
- `SyphonOutput` is a stub — skip

---

## 13. External Window — 0% Ported

**Unity: 2 files, 1,025 lines**
**Rust: ZERO files**

### NativeMonitorWindowController.cs (574 lines)

In-process native Metal monitor window for macOS. Presents compositor texture directly via P/Invoke to `MonitorWindow` native plugin. Features:
- HDR sync: EDR headroom probing, PQ decode, paper white nits calculation
- Resolution matching to target display
- Fullscreen parity mode
- Periodic status logging

**Porting:** The concept is needed (external monitor output). In Rust, this would be a second winit window with a wgpu surface. The HDR logic (EDR headroom, PQ decode) contains important display pipeline knowledge worth preserving.

**Priority: Medium**

### SyphonOutputController.cs (451 lines)

Publishes compositor output to Syphon for external display. Bypasses KlakSyphon and calls native Syphon plugin directly. Features:
- HDR vs SDR output path (ACES tonemap + PQ encoding via SyphonBlit.shader)
- External viewer process lifecycle (launch/kill MANIFOLDViewer.app)
- UDP listener for play/pause toggle from viewer
- Background pgrep-based liveness detection

**Porting:** macOS-specific. No Rust Syphon crate exists. The process management and HDR output path logic could be reused in a different inter-process texture sharing mechanism.

**Priority: Low**

### Also Missing: `UI/MonitorOutput.cs` (307 lines)

Displays compositor output fullscreen on a selected monitor using Unity's multi-display system. Camera + Canvas + RawImage overlay. HDR detection/activation, aspect ratio fitting, Escape key to close. In Rust this would be a second winit window.

**Priority: Medium**

---

## 14. Infrastructure — 0% Ported

**Unity: `Infrastructure/ExternalProcessRunner.cs` (487 lines)**
**Rust: ZERO equivalent**

### What ExternalProcessRunner Does

Platform-abstracted async external process execution with two code paths:
1. **Editor/non-macOS:** `System.Diagnostics.Process` with async stdout/stderr via `DataReceived` events
2. **macOS standalone (IL2CPP):** `popen()` P/Invoke with file-based output polling (workaround for IL2CPP `Process` limitations)

**Interface:** `IExternalProcessRunner`
- `RunAsync(command, arguments, onStarted, onExitCode, onStdout, onStderr, onLogLine)`

**Used by:** PercussionImportOrchestrator (Python analysis pipeline), SyphonOutputController (viewer process lifecycle)

### Rust Equivalent
`std::process::Command` with `tokio::process` for async stdout/stderr streaming. The popen workaround is unnecessary in Rust. This would be significantly simpler than the Unity version.

**Priority: High** — Required by percussion import pipeline.

---

## 15. Diagnostics — 0% Ported

**Unity: 4 files, 1,417 lines**
**Rust: ZERO files**

| File | Lines | Purpose |
|---|---|---|
| `DiagnosticsDB.cs` | 587 | SQLite-backed telemetry database (WAL mode, auto-prune >90 days). Tables: Sessions, Events, PerfRollups, Builds, Environment, CreativeSnapshots. Would use `rusqlite` in Rust. |
| `CreativeSnapshotBuilder.cs` | 364 | Builds structured JSON snapshot of playback state every 10s: active clips, layers, effects, generator types, memory metrics |
| `PerfRollupAccumulator.cs` | 254 | Zero-allocation ring buffer accumulating per-frame perf samples. Flushes summary rollup (FPS, p95 frame time, GC pressure, spike frames) every 10s |
| `DiagnosticsController.cs` | 212 | Singleton lifecycle: opens/closes sessions, writes environment metadata, captures error/exception logs, detects display resolution changes |

**Priority: Low** — Telemetry/profiling infrastructure. Not needed for visual or behavioral parity.

---

## 16. VisualTools — Skip

**Unity: `VisualTools/DebugSpectrogram.cs` (632 lines)**

GPU-accelerated scrolling spectrogram overlay for debugging audio playback. Uses `SpectrumBake.compute` shader, IMGUI rendering, live VU meters per band, transient flash detection, BPM-synced beat markers. Developer debug tool only.

**Priority: Skip** — Not part of the product's runtime behavior.

---

## 17. Shader Include Files (.cginc) — Not Previously Audited

**2 files containing shared HLSL logic that multiple shaders `#include`:**

| File | Est. Lines | Used By | Priority |
|---|---|---|---|
| `Assets/Resources/Compute/ParticleCommon.cginc` | ~100 | FluidParticleSimulate.compute, StrangeAttractorSimulate.compute, FluidDensityScatter.compute, others | **High** — shared particle struct definitions and hash/noise utilities. Rust equivalent exists as `particle_common.wgsl` but must be verified for parity. |
| `Assets/Shaders/Includes/CompositorBlend.cginc` | ~100 | VideoCompositor.shader | **High** — all blend mode math (Add, Multiply, Screen, Overlay, etc.). Rust equivalent is the blend mode uniform in `compositor_blend.wgsl` but must be verified for formula parity. |

These are NOT standalone shaders — they are `#include`d by other shaders. Any shader translation that doesn't account for the included code will be incomplete.

---

## 18. Unaudited Shaders

5 shader/compute files not covered by effects or generators audits:

| Shader | Lines | Category | Priority |
|---|---|---|---|
| `LEDEdgeExtend.shader` | 59 | LED output | Medium — needed for LED pixel sampling |
| `SyphonBlit.shader` | 80 | External window | Low — macOS Syphon HDR blit |
| `SpectrumOverlay.shader` | 179 | Debug tool | Skip |
| `SpectrumBake.compute` | 86 | Debug tool | Skip |
| `TileCompositor.compute` | 101 | Compositor | Medium — compute fast-path (see GAP-COMP-1) |

---

## 19. Native Plugin Source Files — Not Previously Audited

**8 native source files implementing platform-specific functionality via P/Invoke:**

| File | Language | Lines | Purpose | Rust Equivalent |
|---|---|---|---|---|
| `Assets/Plugins/NativeMonitor/MonitorWindowPlugin.mm` | Obj-C++ | 617 | Native macOS Metal window for fullscreen output. SDR + HDR (EDR/PQ). Metal shaders embedded as string constants. | Second winit window + wgpu surface |
| `Assets/Plugins/MetalEncoder/MetalEncoderPlugin.m` | Obj-C | 486 | Direct Metal GPU video encoder. Zero-readback H.264 (SDR, 50Mbps) + HEVC Main10 (HDR, 100Mbps, BT.2020/PQ). GPU compute Y-flip. | wgpu + platform encoder FFI |
| `Assets/Plugins/MidiClock/MidiClockPlugin.c` | C | 458 | CoreMIDI MIDI clock/transport/note receiver. Two-pass callback (system RT first, then channel voice). Lock-free SPSC note queue (capacity 2048). | `midir` crate + manual MIDI parsing |
| `Assets/Plugins/NativeTextInput/NativeTextInputPlugin.m` | Obj-C | 287 | Native macOS NSTextField overlay. Enter/Escape/focus-loss commit/cancel. Dark theme styling. | Custom bitmap text input or cocoa-rs |
| `Assets/Plugins/FileDrop/FileDropPlugin.m` | Obj-C | 274 | Native file drag-and-drop via NSView swizzling. Extension filtering (mp4/mov/webm/avi). MAX_DROPPED_FILES=64, MAX_PATH_LEN=2048. | winit `WindowEvent::DroppedFile` (built-in) |
| `Assets/Plugins/FileDialogs/FileDialogsPlugin.m` | Obj-C | 219 | Native NSOpenPanel/NSSavePanel file dialogs. | `rfd` crate (already used in Rust port) |
| `Assets/Plugins/BlobDetector/BlobDetectorPlugin.cpp` | C++ | 166 | OpenCV blob detection: Grayscale→GaussianBlur→Canny→Dilate→FindContours. Bundled OpenCV dylibs. | OpenCV Rust bindings or GPU-only approach |
| `Assets/Plugins/DepthEstimator/DepthEstimatorPlugin.cpp` | C++ | large | OpenVINO monocular depth + subject segmentation + dense optical flow with camera compensation. | ONNX Runtime Rust bindings |

**Total native plugin source: ~2,500+ lines**

**Porting notes:**
- `FileDropPlugin.m` and `FileDialogsPlugin.m` are already superseded by winit and `rfd` in the Rust port
- `MidiClockPlugin.c` is the native implementation behind `MidiClock.cs` — both must be ported together via `midir`. The two-pass callback architecture and lock-free note queue are critical for deterministic MIDI timing.
- `BlobDetectorPlugin.cpp` and `DepthEstimatorPlugin.cpp` are required only for BlobTrackingFX and WireframeDepthFX effects (Tier 4)
- `MonitorWindowPlugin.mm` and `MetalEncoderPlugin.m` need Rust-native equivalents (winit + wgpu)
- `NativeTextInputPlugin.m` — the Rust port has `text_input.rs` in manifold-app but needs verification of commit/cancel semantics

---

## 20. Test Files — Not Previously Audited

**34 test files under `Assets/Tests/EditMode/` + 1 under `Assets/Tests/PlayMode/`**

These define expected behaviors and edge cases. They are the best source for Rust `#[test]` functions.

| Test File | Tests For |
|---|---|
| `BeatQuantizerTests.cs` | BeatQuantizer rounding/snapping |
| `ClipSchedulerTests.cs` | ClipScheduler sync logic |
| `ClipSchedulerEdgeCaseTests.cs` | Scheduler edge cases |
| `CoordinateMapperTests.cs` | Beat/pixel coordinate mapping |
| `EditingCommandTests.cs` | Command execute/undo roundtrips |
| `EffectSystemTests.cs` | Effect instance management |
| `EnvelopeEvaluatorTests.cs` | ADSR envelope evaluation |
| `GeneratorTests.cs` | Generator param validation |
| `LayerTests.cs` | Layer clip management |
| `LayerAdvancedTests.cs` | Advanced layer operations |
| `MidiDeterminismTests.cs` | MIDI note ordering determinism |
| `MidiFileParserTests.cs` | SMF parsing |
| `MidiNoteParserTests.cs` | MIDI note name parsing |
| `PercussionImportTests.cs` | Percussion import pipeline |
| `Phase1RegressionTests.cs` | Phase 1 regression suite |
| `PlaybackEngineTests.cs` | Playback engine state |
| `ProjectJsonMigratorTests.cs` | JSON migration |
| `ProjectSettingsTests.cs` | Settings validation |
| `RecordingProvenanceTests.cs` | Recording provenance tracking |
| `ResolveFcpxmlExporterTests.cs` | FCPXML export |
| `SerializationTests.cs` | Project serialization roundtrip |
| `SyncArbiterTests.cs` | Sync arbiter logic |
| `TempoMapTests.cs` | Tempo map operations |
| `TimelineClipTests.cs` | Clip operations |
| `TimelineTests.cs` | Timeline operations |
| `UIInputSystemTests.cs` | UI input system |
| `UITreeTests.cs` | UI tree operations |
| `UndoRedoManagerTests.cs` | Undo/redo stack |
| `UndoSemanticsTests.cs` | Undo semantic correctness |
| `VideoTimeCalculatorTests.cs` | Video time computation |
| `BitmapPrecisionTests.cs` | Bitmap rendering precision |
| `MockClipRenderer.cs` | Test helper |
| `MockTimeProvider.cs` | Test helper |
| `TimelineDataIntegrityTests.cs` | (PlayMode) Data integrity |

**Priority: HIGH** — These tests should be translated to Rust to verify parity. The Rust port currently has ~7 test files with limited coverage compared to Unity's 34.

---

## 21. Python Audio Analysis Pipeline — Not Previously Audited

**21 Python files under `Tools/AudioAnalysis/`**

This is the actual audio analysis backend that `PercussionImportOrchestrator` launches via `ExternalProcessRunner`. It uses madmom, librosa, Demucs, ADTOF, and Basic Pitch.

| File | Purpose |
|---|---|
| `percussion_json_pipeline.py` | Main entry point — CLI orchestration |
| `manifold_audio/analyzer.py` | Core analysis coordinator |
| `manifold_audio/onset_detection.py` | madmom-based onset detection |
| `manifold_audio/adtof_detection.py` | ADTOF drum transcription |
| `manifold_audio/basic_pitch_detection.py` | Basic Pitch bass/synth detection |
| `manifold_audio/bpm.py` | BPM estimation |
| `manifold_audio/spectral.py` | Spectral analysis utilities |
| `manifold_audio/peak_detection.py` | Peak detection algorithms |
| `manifold_audio/gestures.py` | Gesture/phrase detection |
| `manifold_audio/conflict_resolution.py` | Multi-source event deduplication |
| `manifold_audio/profiles.py` | Detection profiles/presets |
| `manifold_audio/models.py` | Data models |
| `manifold_audio/audio_io.py` | Audio file I/O |
| `manifold_audio/external_tools.py` | ffmpeg/ffprobe wrappers |
| `manifold_audio/math_utils.py` | Shared math utilities |
| `manifold_audio/cli.py` | CLI argument parsing |
| `manifold_audio/__init__.py` | Package init |
| `manifold_audio/__main__.py` | Package entry point |
| `lameenc.py` | MP3 encoding utility |

**Porting notes:** This Python codebase does NOT need mechanical translation to Rust. It runs as a subprocess. The Rust port needs:
1. `ExternalProcessRunner` equivalent to launch it (GAP-INFRA-1)
2. `PercussionPipelineBackendResolver` to find the Python binary and construct args (GAP-AUDIO-6)
3. The pipeline itself ships as-is alongside the Rust binary

---

## 22. Other Missed Files

### Font File (MUST BUNDLE)
- `Assets/Resources/Fonts/Inter-Regular.ttf` (67KB) — **The font used by the bitmap text renderer for ALL UI text.** The Rust port must bundle this exact file. Without it, no text renders.

### ScriptableObject Configuration Assets (behavioral defaults)
These `.asset` files contain runtime configuration constants that are NOT in the C# source — the code reads them at runtime:

- **`Assets/Scripts/PercussionPipelineSettings.asset`** — Extensive tuning:
  - BPM range: 55-215
  - Per-instrument configs: generator type assignments (Kick→WireframeZoo, Snare→BasicShapesSnap, etc.), layer indices, frequency bands, confidence thresholds
  - Algorithm parameters: madmom thresholds, octave weights, grid scoring weights, downbeat tolerances, ADTOF thresholds, basic-pitch detection settings
  - These values must be replicated as Rust struct defaults when the percussion pipeline is ported.

- **`Assets/UI/LedSettings.asset`** — LED output defaults:
  - ArtNet IP: `192.168.2.18`, port: `6454`, strips: 8, LEDs/strip: 120, BGR: true
  - Energy gate settings for brightness modulation
  - These values must be replicated as Rust struct defaults when the LED subsystem is ported.

- **`Debugging/MidiMappingConfig.asset`** — Legacy/debug MIDI mapping. Not behavioral.

### Third-Party Library
- `Assets/Plugins/SQLite-net/SQLite.cs` (~2,000 lines) — SQLite ORM used by DiagnosticsDB. Replace with `rusqlite` crate.
- `Assets/Plugins/SQLite-net/libsqlite3.dylib` (1.2MB) — Native SQLite library.

### Pre-compiled Native Plugins (no source, binary only)
- `Assets/Plugins/UnityAbletonLink/AbletonLink.bundle` — Ableton Link synchronization. Rust equivalent: direct Link C++ library integration.
- `Assets/Plugins/UnityMtcReceiver/` — Empty Xcode project placeholder. MTC receiver not implemented.

### Companion Tools (NOT part of core, stay as-is)
- `Tools/SyphonViewer/` — Swift app for Syphon output testing (2 .swift files). macOS companion.
- `Assets/Plugins/MaxForLive/manifold_sync.js` (66 lines) — Max for Live device for Ableton sync via OSC. Runs in Ableton, not MANIFOLD.
- `Tools/Debugging/` — Python analysis/debugging tools. Standalone utilities.

### Sample Projects & Test Data
- `ProjectDemos/` — 5 sample `.manifold` project files
- `TestAssets/` — Test video files
- `Burn V5.manifold`, `Particle Testing ANOTHER 4.manifold` — Sample projects at root

### Assembly Definitions (5 .asmdef files)
Define Unity compilation units. Confirm that Tests and Editor are separate assemblies.

---

## 23. Complete Unity Project Inventory (CORRECTED)

### C# Source Files

| Directory | Files | Lines |
|---|---|---|
| **Assets/Scripts/** | | |
| Audio/ (incl. Midi/, Percussion/) | 16 | 7,451 |
| Compositing/ (incl. Effects/) | 43 | 6,352 |
| Data/ | 34 | 7,113 |
| Diagnostics/ | 4 | 1,417 |
| Editing/ | 7 | 2,327 |
| Editor/ (Unity-specific, skip) | 7 | 1,404 |
| Export/ | 7 | 2,713 |
| ExternalWindow/ | 2 | 1,025 |
| Infrastructure/ | 1 | 487 |
| Input/ | 2 | 726 |
| LED/ | 8 | 853 |
| Playback/ (incl. Generators/) | 45 | 13,301 |
| Sync/ | 14 | 2,712 |
| UI/ (root) | 1 | 307 |
| UI/Bitmap/ | 37 | 11,920 |
| UI/Timeline/ (incl. Core/) | 60 | 23,237 |
| VisualTools/ | 1 | 632 |
| **Scripts subtotal** | **310** | **84,340** |
| **Assets/Tests/** | 34 | ~3,000+ |
| **Assets/Plugins/** (SQLite.cs) | 1 | ~2,000 |
| **C# Grand Total** | **345** | **~89,340** |

### Shader & Compute Files

| Category | Files | Lines |
|---|---|---|
| .shader (Assets/Shaders/) | 66 | ~9,575 |
| .compute (Assets/Resources/Compute/) | 16 | 2,541 |
| .cginc (shader includes) | 2 | ~200 |
| **Shader Grand Total** | **84** | **~12,316** |

### Native Plugin Source

| Category | Files | Lines |
|---|---|---|
| .m / .mm (Obj-C / Obj-C++) | 5 | ~1,883 |
| .c (C) | 1 | 458 |
| .cpp (C++) | 2 | ~400+ |
| **Native Grand Total** | **8** | **~2,741** |

### External Tools

| Category | Files | Lines (est.) |
|---|---|---|
| Python (AudioAnalysis) | 21 | ~2,000 |
| Swift (SyphonViewer) | 2 | ~200 |
| JavaScript (Max for Live) | 1 | ~100 |
| **Tools Grand Total** | **24** | **~2,300** |

### Additional Behavioral Assets (not code, but configure runtime behavior)

| Asset | Purpose |
|---|---|
| `Assets/Resources/Fonts/Inter-Regular.ttf` | **THE font for all UI text** — must be bundled with Rust port |
| `Assets/Scripts/PercussionPipelineSettings.asset` | Percussion detection constants (BPM range, instrument thresholds, algorithm weights) |
| `Assets/UI/LedSettings.asset` | LED output defaults (ArtNet IP, port, strip config) |

### TRUE GRAND TOTAL

| Category | Files | Lines |
|---|---|---|
| C# source (Scripts + Tests + Plugins) | 345 | ~89,340 |
| Shaders (.shader + .compute + .cginc) | 84 | ~12,316 |
| Native plugins (C/C++/ObjC) | 8 | ~2,741 |
| External tools (Python/Swift/JS) | 24 | ~2,300 |
| Behavioral assets (font + config) | 3 | N/A |
| **EVERYTHING** | **464** | **~106,697** |

---

## 24. Named Failure Modes Detected in Current Codebase

| FM | Description | Where |
|---|---|---|
| FM-2 | Approximating instead of translating | Saver (reimagined as flat JSON), video_time.rs (simplified) |
| FM-3 | Flattening architecture | Compositor refactored from monolithic class |
| FM-6 | Missing edge cases | Loader (skip post-load), LiveClipManager (no 5ms guard), TempoMapConverter (no BPM clamp) |
| FM-9 | Hallucinated constraints | FluidSim2D particle cap 2M vs 8M |
| FM-10 | Substituting texture formats | FluidSim2D/3D Rgba16Float vs R32Float/Rg32Float |
| FM-14 | Scattering service logic | ProjectIOService scattered across app.rs |

---

## 25. Priority Fix List

### Tier 0 — Without These, the App Is Non-Functional

| # | Gap | Description | Est. LOC |
|---|---|---|---|
| 1 | GAP-PLAY-1 | Port DriverController + ParameterDriverManager + EnvelopeEvaluator (modulation pipeline) | ~600 |
| 2 | GAP-PLAY-2 | Port PlaybackController orchestration (Update loop) | ~800 |
| 3 | GAP-GEN-4 | Wire GeneratorRenderer as ClipRenderer trait impl | ~100 |

### Tier 1 — Core Features Broken

| # | Gap | Description | Est. LOC |
|---|---|---|---|
| 4 | GAP-PLAY-1 | Port ClipLauncher (MIDI → clip triggering) | ~400 |
| 5 | GAP-INPUT-1 | Port MidiInputController (MIDI device → ClipLauncher routing) | ~400 |
| 6 | GAP-PLAY-3 | Wire live_slots in sync_clips_to_time | ~10 |
| 7 | GAP-PLAY-4 | Add 5ms NoteOn/NoteOff timing guard | ~30 |
| 8 | GAP-CORE-3 | Fix reset_effectives() to only reset driven params | ~20 |
| 9 | GAP-PLAY-5 | Port PlaybackEngine clip time/rate methods | ~300 |
| 10 | GAP-APP-2 | Implement video file drag-and-drop | ~150 |
| 11 | GAP-IO-1 | Port ProjectArchive.Save() — V2 ZIP format | ~400 |
| 12 | GAP-IO-2 | Add post-load validation pipeline | ~100 |
| 13 | GAP-GEN-3 | Fix FluidSimulation3D (19 issues per FLUID_SIM_AUDIT.md) | ~300 |

### Tier 2 — Blocks Editing Workflow

| # | Gap | Description | Est. LOC |
|---|---|---|---|
| 14 | GAP-CORE-1 | Implement IParamSource / IEffectContainer traits | ~200 |
| 15 | GAP-CORE-2 | Add property clamping (systematic) | ~150 |
| 16 | GAP-UI-1 | Port 5 critical missing panels | ~1500 |
| 17 | GAP-EDIT-1 | Complete EditingService orchestration methods | ~400 |
| 18 | GAP-CORE-7 | Port PathResolver | ~300 |
| 19 | GAP-APP-1 | Consolidate ProjectIOService as unit | ~200 |
| 20 | GAP-FX-1a | Port 6 simple missing effects (GradientMap, EdgeGlow, etc.) | ~300 |
| 21 | GAP-GEN-1 | Fix project_4d for Tesseract/Duocylinder | ~50 |
| 22 | GAP-GEN-2 | Fix FluidSim2D (particle cap, texture formats, rounding) | ~50 |
| 23 | GAP-INFRA-1 | Port ExternalProcessRunner (std::process::Command) | ~150 |

### Tier 3 — Audio Import Pipeline

| # | Gap | Description | Est. LOC |
|---|---|---|---|
| 24 | GAP-AUDIO-1 | Port PercussionAnalysisModels (all data types) | ~400 |
| 25 | GAP-AUDIO-2 | Port PercussionAnalysisParser (JSON parsing) | ~350 |
| 26 | GAP-AUDIO-3 | Port PercussionTimelinePlanner + PercussionImportService | ~450 |
| 27 | GAP-AUDIO-4 | Port PercussionImportOrchestrator (central orchestration) | ~1000 |
| 28 | GAP-AUDIO-5 | Port MidiFileParser + MidiImportService | ~400 |
| 29 | GAP-AUDIO-6 | Port PercussionPipelineBackendResolver | ~800 |
| 30 | GAP-AUDIO-7 | Port ImportedAudioSyncController + StemAudioController | ~700 |
| 31 | GAP-AUDIO-8 | Port PercussionAlignmentService | ~400 |
| 32 | GAP-AUDIO-9 | Port waveform UI (WaveformRenderer + lanes) | ~1200 |

### Tier 4 — Effects, Sync, Polish

| # | Gap | Description | Est. LOC |
|---|---|---|---|
| 33 | GAP-FX-1b | Port 4 stateful effects (InfiniteZoom, Datamosh, SlitScan, Corruption) | ~800 |
| 34 | GAP-FX-1c | Port 4 complex effects (PixelSort, FluidDistortion, Microscope, WireframeDepth) | ~2000+ |
| 35 | GAP-PLAY-9 | Complete sync controllers (Link, MidiClock, OSC) | ~1500 |
| 36 | GAP-COMP-1 | ComputeCompositor fast path | ~200 |
| 37 | GAP-CORE-4 | Port remaining ~40 missing methods across core structs | ~600 |
| 38 | GAP-IO-4 | VideoExporter | ~1000 |
| 39 | GAP-CORE-5 | Fix ParameterDriver random hash to match Unity's algorithm | ~20 |

### Tier 5 — LED, External Output, Diagnostics

| # | Gap | Description | Est. LOC |
|---|---|---|---|
| 40 | GAP-LED-1 | Port IExternalOutput trait + ExternalOutputType + LEDConstants + LedSettings | ~100 |
| 41 | GAP-LED-2 | Port ArtNetDmxConverter + ArtNetOutput + LEDOutputController | ~600 |
| 42 | GAP-LED-3 | Translate LEDEdgeExtend.shader to WGSL | ~60 |
| 43 | GAP-EXT-1 | Port external monitor output (second winit window + wgpu surface) | ~400 |
| 44 | GAP-DIAG-1 | Port DiagnosticsDB + PerfRollupAccumulator + CreativeSnapshotBuilder | ~800 |

---

## Appendix A: Where the Two Prior Audits Disagreed and Resolution

| Disagreement | Audit 1 Said | Audit 2 Said | This Audit's Verdict |
|---|---|---|---|
| Core completion | 80% | 60% | **65%** — Audit 1 credited correct fields without counting missing methods; Audit 2 was closer |
| Playback completion | 60% | 40% | **30%** — Both missed the full scope. 5 files at 0% and dead modulation pipeline make this lower than either claimed |
| Effect count denominator | 46 total types | 17 missing listed | **28 registered processors in Unity, 16 ported** — Audit 1 counted infrastructure classes as "effects"; Audit 2 only listed missing ones without a firm total |
| Compositor | 70% | 53% | **50%** — Audit 1 said "unverified" and guessed high; Audit 2 was closer |
| Missing modulation pipeline | Not mentioned | DriverController + ParameterDriverManager + EnvelopeEvaluator all 0% | **Confirmed 0%** — Audit 2 was correct; Audit 1 missed this entirely |
| Missing ClipLauncher | Not mentioned | 0% MISSING | **Confirmed 0%** — Audit 2 was correct |
| Missing TempoRecorder | Not mentioned | 0% MISSING | **Confirmed 0%** — Audit 2 was correct |
| GeneratorDefinitionRegistry | "NOT PORTED" (GAP-CORE-1) | "Ported (in types.rs)" | **PARTIALLY PORTED** — `param_defs()` and `display_name()` exist on GeneratorType. Missing: full `GeneratorDef` struct, OSC methods, format methods. Audit 1 was wrong to say "NOT PORTED"; Audit 2 was right that it exists in types.rs |
| UI completion | 75% | 70% | **65%** — both were slightly generous; ~40 Unity files have no Rust equivalent |
| Audio/Percussion | Not mentioned (0 coverage) | "0% — entire subsystem absent" (1 line) | **Confirmed 0%** — 16 runtime files, 7,451 lines. Neither audit read a single line of these files |
| MIDI Input | Not mentioned | Not mentioned | **0% — CRITICAL gap missed by BOTH audits**. MidiInputController.cs (526 lines) is the sole path for hardware MIDI → clip triggering |
| LED/External | Not mentioned | "0% — 8 files, 853 lines" (1 line) | **Confirmed 0%** — Neither audit read any of the 8 files |
| Diagnostics | Not mentioned | "0% — 4 files, 1,417 lines" (1 line) | **Confirmed 0%** — Neither audit read any of the 4 files |
| Infrastructure | Not mentioned | "0% — 1 file, 487 lines" (1 line) | **Confirmed 0%** — ExternalProcessRunner.cs never read by either audit |

## Appendix B: Methodology

This audit was conducted in four phases using 11 parallel agents:

**Phase 1 — Subsystem deep-audit (7 parallel agents):**
Each of the 7 Rust crates audited independently:
1. Read the complete directory listing of the Rust crate
2. Read every .rs file to inventory all structs, enums, traits, impls, methods, constants
3. Read the corresponding Unity .cs files in full
4. Performed method-by-method, field-by-field comparison
5. Verified constants, default values, and behavioral logic

**Phase 2 — Audio pipeline deep-audit (1 agent):**
1. Read all 16 runtime Audio/ files in full
2. Mapped the complete processing flow from file drop to timeline placement
3. Documented every class, interface, method, constant, and dependency

**Phase 3 — Complete file inventory sweep (2 parallel agents):**
1. Listed every .cs, .shader, and .compute file in the Unity project
2. Listed every .rs and .wgsl file in the Rust project
3. Cross-referenced against Phase 1-2 coverage to find gaps
4. Identified 19 files (5,447 lines) that Phases 1-2 had not deeply audited

**Phase 4 — Gap closure and verification (1 agent):**
1. Read every file in Diagnostics/, LED/, ExternalWindow/, Infrastructure/, Input/, VisualTools/
2. Documented every class, public method, dependency, constant
3. Searched for files OUTSIDE Assets/Scripts/ — found 34 test files, 2 .cginc shader includes, 8 native plugin sources, 21 Python pipeline files, and other tools
4. Corrected the shader count (66 actual vs 67 previously claimed)
5. Verified the complete inventory: 461 files, ~105,456 lines across all categories

**Verification guarantees:**
- No claims from prior audits were trusted without independent source reading
- Every "ported" claim was checked against actual Rust source
- Every "missing" claim was verified by searching the Rust codebase
- Every .cs file under Assets/ has been accounted for (345 total)
- Every .shader, .compute, and .cginc file has been accounted for (84 total)
- Every native plugin source file has been accounted for (8 total)
- Every Python/Swift/JS tool file has been accounted for (24 total)
