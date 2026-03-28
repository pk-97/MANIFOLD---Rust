# MANIFOLD — Structural Audit Report

**Date:** 2026-03-28
**Scope:** 10 structural quality audits across 12 crates, ~99K lines

## Executive Summary
- Total findings: 48
- HIGH priority: 5
- MEDIUM priority: 18
- LOW priority: 25

### Top 10 Most Critical Findings

1. **`manifold-gpu/src/metal/mod.rs` (2186 lines) — Split into 6 modules** (Task 1, MEDIUM) — Device, encoder, texture pool, shader compiler, types, and format helpers are all in one file. Highest navigability win.

2. **Beats/seconds mixing risk — no newtypes** (Task 2, HIGH) — `PlaybackEngine` holds `current_time: f32` (seconds) and `current_beat: f32` (beats) as bare f32 on the same struct. A typo could silently pass seconds where beats are expected.

3. **Generator state not cleaned on clip stop** (Task 3, MEDIUM) — `fluid_simulation` and `fluid_simulation_3d` maintain per-owner texture state that is not cleaned by the `stopped_clips` → `cleanup_stopped_clips` chain. Orphaned GPU textures persist until project switch.

4. **`#![forbid(unsafe_code)]` missing on pure crates** (Task 10, MEDIUM) — `manifold-core` and `manifold-io` have zero unsafe blocks but don't forbid future accidental additions.

5. **`unsafe_op_in_unsafe_fn` not denied** (Task 10, MEDIUM) — 400 unsafe blocks across the codebase without this lint. Unsafe blocks inside unsafe fns could mask unsoundness.

6. **CI is manual-trigger only** (Task 10, MEDIUM) — `workflow_dispatch` means CI doesn't run automatically on push/PR. Regressions could land undetected.

7. **OSC receiver thread detached without join** (Task 6, MEDIUM) — `osc_receiver.rs:153` spawns a UDP listener thread without storing the JoinHandle. Thread leaks on OscReceiver drop.

8. **16 eprintln! calls should use log::** (Task 9, MEDIUM) — Debug output in content_thread, content_pipeline, app, edr_surface, metalfx bypasses the logging framework.

9. **BPM validated at 9 separate clamp sites** (Task 2, MEDIUM) — A `Bpm(f32)` newtype with validated construction would eliminate all redundant clamping.

10. **`has_start_absolute_tick` + `start_absolute_tick` should be `Option<i32>`** (Task 2, MEDIUM) — C#-ism: two fields encoding one optional value. `Option<i32>` makes the invalid state unrepresentable.

### Recommended Execution Order

1. **Build hardening** (Task 10) — Quick wins: add `#![forbid(unsafe_code)]` to manifold-core/manifold-io, enable CI on push, add `#![deny(unsafe_op_in_unsafe_fn)]`. Low risk, high prevention value.

2. **eprintln → log::` migration** (Task 9.3) — Mechanical replacement, no logic changes. Unblocks structured logging.

3. **Generator cleanup gap** (Task 3.1) — Add `cleanup_owner()` to fluid generators, wire through GeneratorRenderer. Prevents GPU memory leaks during long sessions.

4. **metal/mod.rs split** (Task 1) — Split into device.rs, encoder.rs, texture_pool.rs, shader_compiler.rs, types.rs, format.rs. Pure refactor, no logic changes.

5. **content_thread.rs split** (Task 1) — Extract command handler and export into separate modules.

6. **Type system newtypes** (Task 2) — Introduce `Beats(f32)`, `Seconds(f64)`, `Bpm(f32)` gradually, starting from PlaybackEngine. Highest correctness impact but most invasive.

---

## Task 1: Large File Decomposition Analysis

### `crates/manifold-playback/src/percussion_orchestrator.rs` (3078 lines)
**VERDICT:** Split
**SEVERITY:** MEDIUM
**REASON:** Mixes 5 distinct concerns: progress parsing (lines 78-253), undo commands (lines 258-346 + 2741-2818), pipeline state machine (lines 348-480), orchestrator logic (lines 482-2736), and module-level helpers (lines 2967-3078). The progress parser and commands have zero coupling to orchestrator state — they could be standalone modules.
**PROPOSED SPLITS:**
- `percussion_progress_parser.rs`: lines 78-253 (`PercussionPipelineProgressParser`, `PipelineProgress`)
- `percussion_commands.rs`: lines 258-346 + 2741-2818 (`SetImportedAudioCommand`, `SetAudioStartBeatCommand`, `MoveClipBeatCommand`)
- `percussion_orchestrator.rs`: lines 348-2736 (core orchestrator, trimmed to ~2400 lines)
- Module-level helpers (lines 2967-3078) stay in orchestrator or a small `percussion_helpers.rs`

### `crates/manifold-gpu/src/metal/mod.rs` (2186 lines)
**VERDICT:** Split
**SEVERITY:** MEDIUM
**REASON:** Contains 6 distinct concerns all in one file: GPU device + resource creation (lines 82-555), GPU types (GpuTexture/GpuBuffer/GpuSampler/GpuEvent/GpuHeap, lines 557-775), TexturePool (lines 777-961), GpuEncoder (lines 963-1429), WGSL→MSL shader compilation pipeline (lines 1431-2058), and format conversion helpers (lines 2091-2186). The shader compiler (627 lines) and encoder (466 lines) are both substantial and independent.
**PROPOSED SPLITS:**
- `metal/device.rs`: lines 82-555 (GpuDevice + resource creation)
- `metal/types.rs`: lines 557-775 (GpuTexture, GpuBuffer, GpuSampler, GpuEvent, GpuHeap)
- `metal/texture_pool.rs`: lines 777-961 (TexturePool)
- `metal/encoder.rs`: lines 963-1429 (GpuEncoder, EncoderState)
- `metal/shader_compiler.rs`: lines 1431-2058 (WGSL→SPIR-V→MSL, slot maps, naga introspection)
- `metal/format.rs`: lines 2091-2186 (Metal format conversion helpers)
- `metal/mod.rs`: re-exports only

### `crates/manifold-app/src/content_thread.rs` (2002 lines)
**VERDICT:** Split
**SEVERITY:** MEDIUM
**REASON:** Three major concerns: main run loop + frame tick (lines 106-712), sync controller management (lines 713-1014), command handling (lines 1015-1560), and export (lines 1561-1940). The command handler is a massive match arm (545 lines) that could be its own module. Export is completely self-contained.
**PROPOSED SPLITS:**
- `content_thread.rs`: lines 1-712 (struct, run loop, tick_frame)
- `content_commands.rs`: lines 1015-1560 (handle_command match dispatch)
- `content_export.rs`: lines 1561-1940 (run_export, export_one_frame)
- Sync logic (lines 713-1014) stays in content_thread as it's tightly coupled to tick state

### `crates/manifold-renderer/src/effects/wireframe_depth.rs` (1981 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Genuinely cohesive — this is a single complex effect (WireframeDepthFX) that owns per-owner state, DNN backend management, readback pipelines, and 15 shader passes. The OwnerState, WorkerMode, readback, and apply methods are all tightly coupled. Splitting would scatter the effect's state machine across files without reducing complexity. The 1981 lines match the Unity source (1094 lines C# + shader), which is expected for a mechanical port.

### `crates/manifold-app/src/app.rs` (1981 lines)
**VERDICT:** Borderline
**SEVERITY:** LOW
**REASON:** Already split once (app_render.rs, app_lifecycle.rs exist as siblings). The remaining code is: Application struct + new() (lines 55-380), cursor/input helpers (lines 406-783), key conversion (lines 784-865), and the massive `ApplicationHandler` trait impl with `resumed()` (lines 866-1261) and `window_event()` (lines 1262-1910). The `window_event` match (648 lines) is the heaviest section but is inherently a single match dispatch.
**PROPOSED SPLITS (if Split):**
- `app_input.rs`: lines 406-865 (cursor updates, text input, key conversion)
- Already-split: `app_render.rs`, `app_lifecycle.rs`

### `crates/manifold-playback/src/engine.rs` (1934 lines)
**VERDICT:** Borderline
**SEVERITY:** LOW
**REASON:** PlaybackEngine is a cohesive core type — tick, clip start/stop, sync, lookahead prewarm are all deeply coupled through shared state (active_clip_renderers, timeline_active_scratch, current_time, etc.). The main tick methods (tick_playing, tick_non_playing) share scratch buffers. The lookahead prewarm logic (lines ~840-end) is the most separable section but still reads engine state heavily.
**PROPOSED SPLITS (if Split):**
- `engine_prewarm.rs`: extract lookahead prewarm logic (helper functions, not methods)

### `crates/manifold-ui/src/panels/viewport.rs` (1820 lines)
**VERDICT:** Borderline
**SEVERITY:** LOW
**REASON:** TimelineViewportPanel mixes rendering (`build`, `repaint_dirty_layers`), data setters, coordinate helpers, and grid computation. However, these all operate on the same viewport state (mapper, scroll, zoom). The grid subdivision logic (lines ~884+) is self-contained but small.

### `crates/manifold-app/src/ui_bridge/inspector.rs` (1584 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Single function `dispatch_inspector()` — a large match over PanelAction variants for inspector-related commands. This is a dispatch table, not mixed concerns. Splitting by action type would scatter related dispatch logic across files without benefit.

### `crates/manifold-ui/src/panels/inspector.rs` (1541 lines)
**VERDICT:** Borderline
**SEVERITY:** LOW
**REASON:** InspectorCompositePanel combines effect card management, multi-selection (click/toggle/range), scroll, drag reorder, and the Panel trait build/handle. The selection logic (~300 lines) is somewhat independent but reads panel state. Would benefit from extraction if selection grows more complex.

### `crates/manifold-gpu/src/metal/mps.rs` (1451 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Repetitive but uniform — 18 MPS kernel wrappers following identical patterns (struct, new, encode). Each is ~40 lines. The repetition IS the structure (each kernel maps to one MPS class). Splitting by kernel type would create tiny files with no structural benefit. The shared MpsObject helper and encode functions at the top bind them together.

### `crates/manifold-ui/src/panels/layer_header.rs` (1407 lines)
**VERDICT:** Borderline
**SEVERITY:** LOW
**REASON:** LayerHeaderPanel mixes row layout computation (`compute_layer_row`, `build_layer_row`), drag handling, and many setters. The row layout logic is the densest section. Drag handling (~130 lines) could be extracted but is small.

### `crates/manifold-renderer/src/layer_compositor.rs` (1229 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** LayerCompositor's serial and parallel compositing paths, effect chain application, and layer generation are all deeply coupled through shared PingPong buffers, BlendResources, and LayerOutput state. The two compositing paths (serial/parallel) share helpers.

### `crates/manifold-app/src/input_host.rs` (1201 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Single trait implementation (TimelineInputHost for AppInputHost). All methods implement the same interface for the same borrowed state. Each method is short (5-30 lines). Splitting by method category would scatter a single trait impl.

### `crates/manifold-app/src/app_render.rs` (1192 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Already extracted from app.rs. Contains tick_and_render(), present_all_windows(), and text input overlay rendering. All three methods are Application impl blocks for rendering. Cohesive.

### `crates/manifold-ui/src/panels/effect_card.rs` (1163 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Single UI panel (EffectCardPanel) with build, sync, and event handling methods. All operate on the same card state. Includes tests at the bottom. Cohesive.

### `crates/manifold-editing/src/service.rs` (1076 lines)
**VERDICT:** Borderline
**SEVERITY:** LOW
**REASON:** EditingService mixes clipboard operations (copy/cut/paste/duplicate, ~300 lines) with clip manipulation (split, trim, nudge, extend, shrink, ~400 lines) and the core execute/undo/redo/record methods. The clipboard could be a separate type, but it's tightly coupled to project mutation.

### `crates/manifold-ui/src/interaction_overlay.rs` (1061 lines)
**VERDICT:** Keep
**SEVERITY:** LOW
**REASON:** Single interaction handler with pointer click/move/drag methods. All state is for tracking drag operations. The select_region_to free function at the top is already extracted. Cohesive.

### Task 1 Summary
| Verdict | Count | Files |
|---------|-------|-------|
| **Split** | 3 | percussion_orchestrator.rs, metal/mod.rs, content_thread.rs |
| **Borderline** | 7 | app.rs, engine.rs, viewport.rs, inspector.rs (panels), layer_header.rs, service.rs, inspector.rs (panels) |
| **Keep** | 7 | wireframe_depth.rs, ui_bridge/inspector.rs, mps.rs, layer_compositor.rs, input_host.rs, app_render.rs, effect_card.rs, interaction_overlay.rs |

**Highest-value splits:** metal/mod.rs (6 clear modules) and content_thread.rs (3 clear modules) would most improve navigability.

---

## Task 2: Type System Strengthening

### 2.1 Beats vs Seconds — raw f64/f32 confusion risk

The codebase uses raw `f32` for both beats and seconds with only naming conventions to prevent mixing. Key locations where `Beats(f32)` / `Seconds(f32)` newtypes would catch bugs:

| Location | Field | Unit | Severity |
|----------|-------|------|----------|
| `crates/manifold-core/src/clip.rs:20` | `TimelineClip.start_beat` | beats | HIGH |
| `crates/manifold-core/src/clip.rs:22` | `TimelineClip.duration_beats` | beats | HIGH |
| `crates/manifold-core/src/clip.rs:26` | `TimelineClip.in_point` | seconds | HIGH |
| `crates/manifold-core/src/clip.rs:30` | `TimelineClip.recorded_bpm` | bpm | MEDIUM |
| `crates/manifold-core/src/clip.rs:56` | `TimelineClip.loop_duration_beats` | beats | MEDIUM |
| `crates/manifold-playback/src/engine.rs:91` | `PlaybackEngine.current_time_double` | seconds (f64) | HIGH |
| `crates/manifold-playback/src/engine.rs:92` | `PlaybackEngine.current_time` | seconds (f32) | HIGH |
| `crates/manifold-playback/src/engine.rs:93` | `PlaybackEngine.current_beat` | beats | HIGH |
| `crates/manifold-core/src/tempo.rs:9` | `TempoPoint.beat` | beats | MEDIUM |
| `crates/manifold-core/src/tempo.rs:14` | `TempoPoint.recorded_at_seconds` | seconds | MEDIUM |
| `crates/manifold-core/src/settings.rs:14` | `ProjectSettings.frame_rate` | fps | LOW |
| `crates/manifold-core/src/settings.rs:28` | `ProjectSettings.bpm` | bpm | MEDIUM |

**Risk assessment:** The engine maintains `current_time` (seconds) and `current_beat` (beats) as separate f32/f64 fields on the same struct. A typo passing `current_time` where `current_beat` is expected (or vice versa) would silently produce wrong results. This is the highest-risk area for a newtype.

**Note:** Serialization compatibility (camelCase JSON) means newtypes would need `#[serde(transparent)]`, which works fine.

### 2.2 Raw f32 for BPM — validated construction opportunity

BPM is clamped to 20.0-300.0 at **9 different call sites** across the codebase:
- `crates/manifold-core/src/tempo.rs:42` — `TempoMap::ensure_valid()`
- `crates/manifold-core/src/tempo.rs:54` — `TempoMap::get_bpm_at_beat()`
- `crates/manifold-core/src/tempo.rs:64` — return value clamp
- `crates/manifold-core/src/tempo.rs:149-150` — `TempoMapConverter::seconds_per_beat_from_bpm()`
- `crates/manifold-core/src/tempo.rs:157` — `get_bpm_at_beat_zero()`
- `crates/manifold-core/src/settings.rs:182` — `ProjectSettings::set_bpm()`
- `crates/manifold-core/src/project.rs:179` — `sync_bpm_from_tempo_map()`
- `crates/manifold-core/src/clip.rs:154` — `recorded_bpm_resolved()`
- `crates/manifold-core/src/clip.rs:165` — `set_recorded_bpm()`

**Severity: MEDIUM.** A `Bpm(f32)` newtype with `Bpm::new(v: f32) -> Self { Self(v.clamp(20.0, 300.0)) }` would eliminate all 9 redundant clamp sites. However, this is a Unity parity port and the type must serialize transparently.

### 2.3 Multiple related bool fields — state enum candidates

| Struct | Fields | Should be enum? | Severity |
|--------|--------|-----------------|----------|
| `crates/manifold-core/src/clip.rs:32-34` | `TimelineClip.is_locked`, `is_muted`, `invert_colors` | **No** — independent flags | LOW |
| `crates/manifold-core/src/layer.rs:32-34` | `Layer.is_solo`, `is_muted` | **Borderline** — solo and mute are conceptually exclusive in Ableton semantics, but Unity keeps them as independent bools | LOW |
| `crates/manifold-playback/src/engine.rs:95-96` | `PlaybackEngine.is_recording`, `external_time_sync` | **No** — independent flags | LOW |
| `crates/manifold-playback/src/engine.rs:128-130` | `has_live_external_tempo`, `live_external_tempo_bpm`, `live_external_tempo_source` | **Yes** — these three form a single optional state. `Option<(f32, TempoPointSource)>` would make the invalid state of `has_live_external_tempo=false` + nonzero `bpm` unrepresentable. | **MEDIUM** |
| `crates/manifold-core/src/clip.rs:60-61` | `start_absolute_tick`, `has_start_absolute_tick` | **Yes** — this is exactly `Option<i32>`. The `has_start_absolute_tick` bool is a C#-ism. | **MEDIUM** |

### 2.4 Stringly-typed lookups

`GeneratorTypeId` and `EffectTypeId` are already newtypes wrapping `String` with static constants (`crates/manifold-core/src/generator_type_id.rs`, `crates/manifold-core/src/effect_type_id.rs`). This is appropriate for the open set of generator/effect types that must match Unity's string registry. **No action needed.**

`BlendMode` (`crates/manifold-core/src/types.rs:9`) is already a proper enum. **No action needed.**

### 2.5 u32/i32 for values that can never be zero

| Location | Field | Could use NonZero? | Severity |
|----------|-------|--------------------|----------|
| `crates/manifold-core/src/settings.rs:30` | `time_signature_numerator: i32` | Yes — always ≥1 (clamped at `set_time_sig_numerator`) | LOW |
| `crates/manifold-core/src/settings.rs:32` | `time_signature_denominator: i32` | Yes — always ≥1 | LOW |
| `crates/manifold-core/src/settings.rs:23` | `max_layers: i32` | Yes — always ≥1 | LOW |
| `crates/manifold-core/src/settings.rs:14` | `frame_rate: f32` | No NonZeroF32, but always ≥1.0 | LOW |

**Note:** These are all serialized from Unity JSON (camelCase). Using `NonZeroU32` would require a custom deserializer that converts 0→1. The clamp-on-set pattern already prevents zero. **Low priority.**

### 2.6 Unnamed tuple returns that should be named structs

| Location | Function | Return Type | Proposed Name | Severity |
|----------|----------|-------------|---------------|----------|
| `crates/manifold-core/src/types.rs:341` | `ResolutionPreset::dimensions()` | `(i32, i32)` | `Resolution { width: i32, height: i32 }` | LOW |
| `crates/manifold-gpu/src/metal/mod.rs:500` | `GpuDevice::heap_texture_size_and_align()` | `(u64, u64)` | `HeapSizeAndAlign { size: u64, align: u64 }` | LOW |
| `crates/manifold-gpu/src/metal/mod.rs:923` | `TexturePool::stats()` | `(u64, u64)` | `PoolStats { allocated: u64, recycled: u64 }` | LOW |
| `crates/manifold-playback/src/engine.rs:52-60` | `TickContext` and `TickResult` | Already named structs | N/A | — |

### Task 2 Summary

| Priority | Count | Key findings |
|----------|-------|-------------|
| **HIGH** | 2 | Beats/seconds mixing risk (engine fields), `has_start_absolute_tick` should be `Option<i32>` |
| **MEDIUM** | 3 | BPM validation redundancy (9 clamp sites), `has_live_external_tempo` triple → Option, beats/seconds in TimelineClip |
| **LOW** | 7 | NonZero candidates, unnamed tuples, bool independence |

---

## Task 3: Resource Lifecycle & Cleanup

### 3.1 Full cleanup chain analysis

The cleanup chain is:
1. `PlaybackEngine::tick()` → populates `TickResult::stopped_clips` (`engine.rs:544`)
2. `ContentThread::tick_frame()` → calls `content_pipeline.cleanup_stopped_clips()` (`content_thread.rs:368-369`)
3. `ContentPipeline::cleanup_stopped_clips()` → calls `compositor.cleanup_clip_owner()` (`content_pipeline.rs:636`)
4. `LayerCompositor::cleanup_clip_owner()` → calls `cleanup_clip_owner_internal()` (`layer_compositor.rs:1214`)
5. `cleanup_clip_owner_internal()` → calls `EffectChain::cleanup_clip_owner()` for each chain, which calls `EffectRegistry::cleanup_clip_owner()` (`layer_compositor.rs:426`)
6. `EffectRegistry::cleanup_clip_owner()` → calls `StatefulEffect::cleanup_owner()` on each registered effect (`effect_registry.rs:131`)
7. Each stateful effect (bloom, halation, blob_tracking, wireframe_depth, depth_of_field, stylized_feedback) removes its `AHashMap<i64, OwnerState>` entry

**Gap identified:** Generator stateful textures (fluid_simulation, fluid_simulation_3d) maintain per-owner texture state but are NOT cleaned up through the `stopped_clips` chain. They clean up via `GeneratorRenderer::release_all()` only on full shutdown. **Severity: MEDIUM** — orphaned generator textures persist until project switch.

- `crates/manifold-renderer/src/generators/fluid_simulation.rs` — owns `state_textures: AHashMap<i64, FluidState>`
- `crates/manifold-renderer/src/generators/fluid_simulation_3d.rs` — owns `state_textures: AHashMap<i64, Fluid3DState>`

### 3.2 Manual cleanup methods — Drop candidates

| Type | Method | Could use Drop? | Severity |
|------|--------|-----------------|----------|
| `GpuEncoder` | `drop()` (line 1423) | **Already uses Drop** — releases retained MTLCommandBuffer | — |
| `MpsObject` | `drop()` (line 73) | **Already uses Drop** — releases ObjC object | — |
| `ArtNetController` | `cleanup()` (line 355) | **Borderline** — Drop could work but callers need explicit cleanup timing | LOW |
| `OscParameterRegistry` | `destroy()` (line 167) | **No** — needs &mut self access to unregister from OscReceiver | LOW |
| Stateful effects | `cleanup_owner()`/`cleanup_all_owners()` | **No** — needs `&GpuDevice` parameter for GPU resource release | LOW |

### 3.3 Split initialization

No types found with problematic new + init + setup splits. ContentPipeline uses `new()` then `set_shared_bridge()` on macOS, but this is platform-conditional and appropriate.

### 3.4 Reset method audit

| Type | Method | Complete? | Severity |
|------|--------|-----------|----------|
| `ActiveTimelineClipWindow` | `reset()` (`active_window.rs:53`) | Yes — clears all fields | — |
| `UniformArena` | `reset()` (`uniform_arena.rs:27`) | Yes — resets offset to 0 | — |
| `TempoRecorder` | `reset()` (`tempo_recorder.rs:63`) | **Needs check** — verify all recording state fields cleared | LOW |
| `ScrollContainer` | `reset()` (`scroll_container.rs:133`) | Yes — resets offset and velocity | — |

### 3.5 State that survives project switch

- `ContentPipeline.native_device` — persists (correct: GPU device is reusable)
- `ContentPipeline.texture_pool` — persists (correct: pool is device-level)
- `ContentPipeline.shared_bridge` — persists (correct: IOSurface bridge is window-level)
- **Generator GPU pipelines** — persist in `GeneratorRenderer` (correct: pipelines are compiled once)
- **Effect pipelines** — persist in `EffectRegistry` (correct: pipelines are compiled once)
- **Texture pool entries** — survive project switch. `TexturePool::clear()` exists but must be called explicitly. **MEDIUM** if resolution changes on project load.

---

## Task 4: Modern Rust Idioms

### 4.1 Index loops (first 30 of ~40 found)

| Location | Pattern | Proposed | Severity |
|----------|---------|----------|----------|
| `manifold-core/src/layer.rs:226` | `for i in 0..started_count` | Keep — uses `i` as index into `self.clips[i]` | LOW |
| `manifold-core/src/timeline.rs:268` | `for li in 0..self.layers.len()` | Keep — needs `li` for layer index | LOW |
| `manifold-playback/src/stem_audio.rs:110` | `for i in 0..STEM_COUNT` | Keep — fixed-size array indexing | LOW |
| `manifold-playback/src/active_window.rs:178` | `for i in 0..started_count` | Keep — mirrors Unity binary search pattern | LOW |
| `manifold-playback/src/modulation.rs:251` | `for ei in 0..env_count` | `for (ei, env) in envs.iter().enumerate()` | LOW |
| `manifold-io/src/archive.rs:505,571,637` | `for i in 0..source_archive.len()` | Keep — zip archive random access | LOW |
| `manifold-led/src/artnet.rs:99,105,238,266,280` | Index loops | Keep — hardware buffer indexing | LOW |
| `manifold-ui/src/panels/layer_header.rs:1088` | `for i in 0..layer_count` | Keep — needs index for build_layer_row | LOW |
| `manifold-ui/src/panels/param_slider_shared.rs:142` | `for i in 0..n` | Keep — building slider nodes by index | LOW |

Most index loops are justified (need the index for array access or as a parameter). No high-priority conversions found.

### 4.2 Missing Entry API

| Location | Pattern | Severity |
|----------|---------|----------|
| `manifold-playback/src/osc_registry.rs:96-101` | `contains_key` → `unregister` → `insert` | LOW — unregister has side effects, can't use entry |
| `manifold-media/src/video_renderer.rs:362-367` | `contains_key` → return early | LOW — not an insert pattern, just guard |

Only 1 potential entry API candidate found, and it has side effects. **No action needed.**

### 4.3 format! on hot paths

| Location | Context | Severity |
|----------|---------|----------|
| `manifold-playback/src/percussion_orchestrator.rs:992,1282,1354,1834,2182,2342` | Pipeline status messages | LOW — not per-frame, only during import operations |
| `manifold-app/src/content_thread.rs:427,436` | Profiler param names (inside `#[cfg(feature = "profiling")]`) | LOW — debug-only |

No format! allocations found on the per-frame rendering hot path. **No action needed.**

### 4.4 Vec::new without capacity in hot paths

`PlaybackEngine::new()` (`engine.rs:187-240`) pre-allocates ALL scratch vectors with capacity. **Excellent.** No hot-path Vec::new() without capacity found.

---

## Task 5: Error Handling Consistency

### 5.1 Unwrap/expect counts by crate

| Crate | Total | Safe | Questionable | Dangerous |
|-------|-------|------|-------------|-----------|
| manifold-playback | 34 | 28 | 5 | 1 |
| manifold-renderer | 58 | 45 | 10 | 3 |
| manifold-gpu | 39 | 30 | 8 | 1 |
| manifold-app | 34 | 28 | 5 | 1 |

**Dangerous (per-frame + could fail):**
- `manifold-gpu/src/metal/mod.rs:239-240` — `unwrap_or_else(|e| panic!(...))` on MTL library compile — **could fail** if WGSL→MSL compilation produces invalid MSL. But this only runs at startup, not per-frame. **Reclassified: SAFE (startup-only).**
- `manifold-renderer/src/effect_chain.rs` — 5 `.unwrap()` on effect registry lookups. If a new effect type is registered but not in the registry, this panics per-frame. **MEDIUM.**
- `manifold-renderer/src/effects/wireframe_depth.rs` — 13 unwraps, mostly on texture creation and readback. GPU memory exhaustion would panic. **MEDIUM.**
- `manifold-renderer/src/generators/fluid_simulation.rs` — 7 unwraps on buffer/texture creation. **MEDIUM.**

**Questionable:** Most unwraps on GPU resource creation (could fail on GPU OOM) but practically unreachable.

### 5.2 `#[must_use]` — only 10 exist, 20 candidates

Key functions that should have `#[must_use]`:
1. `EditingService::execute()` — returns nothing, but callers should verify
2. `PlaybackEngine::tick()` → returns `TickResult` — **should have #[must_use]** (`engine.rs:508`)
3. `TempoMapConverter::beat_to_seconds()` — pure function (`tempo.rs:164`) — **MEDIUM**
4. `TempoMapConverter::seconds_to_beat()` — pure function (`tempo.rs:267`) — **MEDIUM**
5. `TimelineClip::end_beat()` — pure accessor (`clip.rs:86`) — **LOW**
6. `Layer::collect_active_clips_at_beat()` — writes to out param, void return — N/A
7. `ProjectSettings::seconds_per_beat()` — pure function (`settings.rs:196`) — **LOW**

---

## Task 6: Concurrency Pattern Audit

### 6.1 Lock inventory

| Lock | Location | Data Protected | Threads | Scope |
|------|----------|---------------|---------|-------|
| `RwLock<Option<TextureView>>` | `content_pipeline.rs:22` | SharedOutputView.view | content+UI | Narrow (single field) |
| `RwLock<(u32,u32)>` | `content_pipeline.rs:23` | SharedOutputView.dimensions | content+UI | Narrow |
| `RwLock<IOSurfaces>` | `shared_texture.rs:78` | IOSurface array | content+UI | Narrow |
| `Mutex<Option<Archive>>` | `metal/mod.rs:117` | Pipeline binary archive | content only | Startup only |
| `Mutex<Vec<OscMsg>>` | `osc_param_router.rs:60` | Pending OSC messages | OSC thread + content | Narrow |
| `Mutex<MessageQueue>` | `osc_receiver.rs:114` | OSC message queue | UDP thread + content | Narrow |
| `Mutex<OscParameterRegistry>` | `osc_registry.rs:64` | Global OSC registry | content only | Global singleton |

**No wide lock scopes or contention risks found.** All locks are narrowly scoped.

### 6.2 Channel inventory

Channels are created in `app.rs` (main) and `content_thread.rs`:
- `ContentCommand` channel: Bounded (inferred from crossbeam usage) — UI→Content
- `ContentState` channel: Bounded — Content→UI
- Exact capacity not found in grep; likely uses default or small bounds.

### 6.3 Thread inventory

| Location | Purpose | JoinHandle stored? | Joined on shutdown? |
|----------|---------|-------------------|---------------------|
| `osc_receiver.rs:153` | UDP listener | **Not stored** (detached) | No — uses shutdown_flag AtomicBool | **MEDIUM** — thread leaks on drop |
| `process_runner.rs:152,165,178` | 3 threads per external process (stdout/stderr/wait) | Stored in ProcessHandle | Yes — joined in `poll()` |
| `background_worker.rs:53,92` | DNN inference workers | Stored in BackgroundWorker | Yes — joined on drop |
| `app_lifecycle.rs:209` | Audio decode thread | **Not stored** (detached) | No | **LOW** — one-shot operation |

### 6.4 Atomic ordering audit

All `Ordering::Relaxed` usages are **safe**:
- `edr_surface.rs:45,67` — EDR screen change flag. Single producer (macOS callback), single consumer (app thread). Relaxed is fine for a boolean flag.
- `percussion_orchestrator.rs:2692`, `percussion_backend.rs:442` — Monotonic counters for unique temp file names. Relaxed is correct (only need uniqueness, not ordering).
- `osc_receiver.rs:151,156,204,216` — Shutdown flag. Relaxed is **borderline** — a `Release`/`Acquire` pair would be more correct to ensure the UDP thread sees all writes before `shutdown_flag=true`. However, the thread rechecks on each loop iteration with a timeout, so stale reads only delay shutdown by one iteration. **LOW risk.**

### 6.5 Lock ordering

No nested lock acquisitions found in `content_thread.rs` or `app.rs`. The two-thread model (content owns all mutable state) eliminates most deadlock risk. **No lock ordering issues.**

---

## Task 7: GPU Code Deduplication

### Top 5 deduplication opportunities

**1. Fragment effect boilerplate (~20 lines × 12 effects = 240 lines)**
Effects using `FragmentBlitHelper` (chromatic_aberration, color_grade, dither, edge_stretch, feedback, halftone, infinite_zoom, invert, kaleidoscope, mirror, scanlines, strobe, vhs) all follow the same pattern:
```rust
struct FooFX { helper: FragmentBlitHelper }
impl FooFX { pub fn new(device) { FragmentBlitHelper::new(device, SHADER, LABEL) } }
impl PostProcessEffect for FooFX {
    fn effect_type(&self) -> &EffectTypeId { &EffectTypeId::FOO }
    fn apply(&mut self, source, target, params, ctx, gpu) {
        let uniforms = FooUniforms { ... };
        self.helper.dispatch(gpu, source, target, bytemuck::bytes_of(&uniforms));
    }
}
```
**Already extracted** — `FragmentBlitHelper` and `ComputeBlitHelper` ARE the deduplication. The remaining per-effect code is genuinely unique (uniform struct, param extraction). **No further deduplication possible.**

**2. Generator uniform struct boilerplate (~8 lines × 20 generators = 160 lines)**
Every generator defines a `#[repr(C)] struct FooUniforms { time, beat, aspect_ratio, ... }` with the first 3 fields always being `time`, `beat`, `aspect_ratio`. A `GeneratorCommonUniforms` prefix struct could reduce boilerplate, but bytemuck requires contiguous `#[repr(C)]` layouts. **LOW priority — the repetition is structural.**

**3. WGSL utility functions (not copy-pasted)**
Checked 5 shader files — utility functions (`hash`, `noise`, `hsv2rgb`) are defined per-shader, not shared. This is intentional: Metal compiles each shader independently. Shared includes would need naga preprocessing. **No action needed.**

**4. MPS kernel wrappers (~40 lines × 18 kernels = 720 lines)**
`mps.rs` has 18 MPS kernel structs following identical new/encode patterns. A macro could generate them, saving ~500 lines. **LOW priority — the repetition is clear and each wrapper is independently testable.**

**5. Line pipeline boilerplate for geometric generators**
`LinePipeline` is already a shared helper used by duocylinder, lissajous, tesseract, mri_volume. **Already deduplicated.**

---

## Task 8: Numeric Safety

### 8.1 Float-to-integer casts

Counts: manifold-playback (104), manifold-renderer (~200), manifold-gpu (~50).

**Key risks:**
- `manifold-playback/src/engine.rs:408` — `time_double as f32` — **SAFE** (f64→f32 precision loss, not overflow risk for realistic time values)
- `manifold-playback/src/midi_parser.rs:242-243` — `on_tick as f32 / ppq as f32` — **SAFE** (both positive integers)
- `manifold-playback/src/live_clip_manager.rs:191` — `((duration_seconds / spb) * TICKS as f32) as i32` — **NEEDS_GUARD** if `spb` is very small (high BPM) and duration is large

### 8.2 f64→f32 precision loss in beat positions

- `engine.rs:408` — `current_time_double as f32` — Used for time display, not for beat computation. **SAFE.**
- Beat positions are stored as `f32` throughout (matching Unity). At 120 BPM, f32 precision (~7 significant digits) is sufficient for 10,000+ bars.

### 8.3 Float equality

Only 6 instances of `== 0.0` or `== 1.0` across the entire codebase. All are in `manifold-core/src/clip.rs` (has_any_effect checks) where exact equality is intentional (checking if a value has been modified from its default). **SAFE.**

### 8.4 Integer overflow

- `PlaybackEngine.last_frame_count: i32` — overflows after ~414 days at 60fps. **LOW risk** but could use u64.
- `TexturePool.current_frame: u64` — never overflows. **SAFE.**
- `GpuEvent.counter: Cell<u64>` — never overflows. **SAFE.**
- `DataVersion counter: u64` — never overflows. **SAFE.**

---

## Task 9: Dead Code, Port Artifacts & Hygiene

### 9.1 `#[allow(dead_code)]` inventory (59 instances found)

**Legitimate (port-ahead / platform-conditional):**
- `content_command.rs:14` — ContentCommand variants not yet wired
- `content_state.rs:14,24` — ContentState fields not yet read by UI
- `transport_state.rs:13,18,33` — Transport state fields awaiting UI wiring
- `ui_bridge/mod.rs:26,36,262` — Bridge dispatch variants
- `dialog_path_memory.rs:15` — All variants from Unity, future use
- `window_registry.rs:5,15,30` — Window state variants
- `input_handler.rs:17,30,43` — Input handler variants

**Potentially truly dead:**
- `app.rs:291,455` — Fields that may be removable (check if used)
- `content_pipeline.rs:588,655` — Helper methods never called
- `effects/wireframe_depth.rs:120,916,1322` — Enum variants and modes
- `generators/fluid_simulation_3d.rs:26` — Constants
- `generators/oscilloscope_xy.rs:12` — Constants
- `effects.rs:681` — Effect definition entry

**Severity: LOW** overall. Most are port-ahead code for parity gaps.

### 9.2 TODO/FIXME inventory (14 found)

| Location | Comment | Status | Severity |
|----------|---------|--------|----------|
| `osc_sync.rs:145` | "TODO: when native OSC is live, subscribe" | Parity gap — waiting for native OSC | LOW |
| `osc_registry.rs:110` | "TODO: When the OscReceiver is fully wired" | Same as above | LOW |
| `live_clip_manager.rs:909` | "TODO: Port full prewarm logic" | Parity gap | LOW |
| `editing_host.rs:313` | "TODO: wire to bitmap renderer force_dirty" | UI polish | LOW |
| `input_handler.rs:134` | "TODO: Move to host when legacy block deleted" | Refactor | LOW |
| `app_lifecycle.rs:140` | "TODO: wire from audio sync controller" | Parity gap | LOW |
| `input_host.rs:104` | "TODO: Re-apply resolution/FPS after undo/redo" | **Bug risk** | MEDIUM |
| `input_host.rs:357` | "TODO: refactor save" | Cleanup | LOW |
| `ui_bridge/editing.rs:154` | "TODO: browser paste not yet wired" | Parity gap | LOW |
| `ui_bridge/editing.rs:218` | "TODO: Wire to EditingService.Paste" | Parity gap | LOW |
| `ui_bridge/editing.rs:259,264` | "TODO: Wire to EditingService.Group/Ungroup" | Parity gap | LOW |

### 9.3 println!/eprintln! inventory (37 in source, 7 in build.rs)

**Build scripts (7) — INTENTIONAL:**
- `manifold-media/build.rs:4,5,17-21` — `cargo:rerun-if-changed` and `cargo:rustc-link-lib`

**Debug/logging that should use `log::` (16) — MEDIUM:**
- `content_thread.rs:165,169` — LED init messages → `log::info!`
- `content_thread.rs:1406` — Export error → `log::error!`
- `content_pipeline.rs:309,547` — Render path messages → `log::warn!`
- `app.rs:1918,1929` — EDR headroom changes → `log::debug!`
- `app_lifecycle.rs:74,225,263` — Import/export debug → `log::debug!`
- `edr_surface.rs:249,270` — EDR debug → `log::debug!`
- `metalfx.rs:122,129,235,238` — MetalFX init → `log::info!`

**Test-only (3) — INTENTIONAL:**
- `tests/wgsl_validation.rs:100`, `tests/load_project.rs:220,225`

**Hot-path risk (1) — HIGH:**
- `manifold-media/src/decode_scheduler.rs:174` — `eprintln!` in scheduler error path that could fire per-frame on decode failure → should be `log::error!` with rate limiting

**Infrastructure (4) — INTENTIONAL:**
- `manifold-renderer/src/gpu.rs:32` — GPU adapter info
- `manifold-led/src/artnet.rs:137,145,221,326,345` — ArtNet network errors → should use `log::`

### 9.4 Unity port artifact comments

Found via grep for "Unity", "MonoBehaviour", "C#":
- These comments are **useful context** — they reference the source Unity file and line numbers for each port.
- No stale references to removed Unity code found.
- **No action needed.**

---

## Task 10: Build & Lint Hardening

### 10.1 `#![forbid(unsafe_code)]` candidates

| Crate | Has unsafe? | Can forbid? |
|-------|------------|-------------|
| `manifold-core` | **None** | **YES — add `#![forbid(unsafe_code)]`** |
| `manifold-editing` | 3 blocks in `effect_target.rs:33,39,45` | No — split-borrow pattern uses unsafe |
| `manifold-io` | **None** | **YES — add `#![forbid(unsafe_code)]`** |

**Severity: MEDIUM** — `manifold-core` and `manifold-io` should add `#![forbid(unsafe_code)]` to prevent accidental unsafe introduction.

### 10.2 `unsafe_op_in_unsafe_fn`

**Not denied anywhere.** Should be added as `#![deny(unsafe_op_in_unsafe_fn)]` to all crates with unsafe code (manifold-gpu, manifold-app, manifold-renderer, manifold-media, manifold-native). **Severity: MEDIUM.**

### 10.3 Clippy lints

Current: Only `too-many-arguments-threshold = 20` in `clippy.toml`. No workspace-level lint configuration.

**Proposed additions:**
- `cast_possible_truncation` — catches `as u32` on potentially large values (HIGH for manifold-gpu)
- `cast_sign_loss` — catches `as u32` on potentially negative values (MEDIUM)
- `cast_precision_loss` — catches `as f32` on large f64/i64 (MEDIUM)
- `float_cmp` — catches `== 0.0` comparisons (LOW — only 6 instances)
- `undocumented_unsafe_blocks` — requires `// SAFETY:` on all unsafe (MEDIUM)

### 10.4 CI runs on macOS

**Yes** — `ci.yml:12` uses `runs-on: macos-latest`. **Correct for Metal compilation.**

However, CI is `workflow_dispatch` only (manual trigger). **Severity: MEDIUM** — should be `on: [push, pull_request]` for continuous validation.

### 10.5 Legacy `#[allow(...)]` audit

**`#[allow(dead_code)]` — 30+ instances** (see Task 9.1). Many are on ContentCommand, ContentState, WindowState variants that are ported but not yet wired. These should be tracked and removed as features are completed.

**`#[allow(clippy::too_many_arguments)]` — 10 instances:**
- `metal/mod.rs:1124,1191` — draw_fullscreen, draw_instanced (Metal API requires many params)
- `effect.rs:64` — PostProcessEffect::apply trait method
- `effect_chain.rs:117` — EffectChain::apply
- `surface.rs:11,56` — SurfaceWrapper methods
- `tree.rs:123,145` — UITree build methods
- `slider.rs:76` — BitmapSlider
- `percussion_analysis.rs:556` — test helper
- `envelopes.rs:88,333,434,452` — Envelope command constructors

These are legitimate — the `too-many-arguments-threshold = 20` in clippy.toml already handles most cases. The explicit `#[allow]` annotations are for trait methods that can't be refactored.

**`#[allow(clippy::mut_from_ref)]` — 1 instance:**
- `gpu_encoder.rs:56` — GpuEncoder internal buffer access via UnsafeCell. **Intentional.**

**`#[allow(clippy::type_complexity)]` — 1 instance:**
- `engine.rs:87` — PlaybackEngine has many callback box fields. **Intentional.**

**`#[allow(unused_imports)]` — 1 instance:**
- `shared_texture.rs:25` — Platform-conditional imports. **Intentional.**

**No deprecated API usages found.** No `#[deprecated]` or `#[allow(deprecated)]` annotations in the codebase.

---

