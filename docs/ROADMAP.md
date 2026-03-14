# MANIFOLD Rust Port — Master Roadmap

## Context

Full port of MANIFOLD (84K LOC C#, 66 shaders, 16 compute shaders) from Unity to standalone Rust. Phase 1 proved the architecture ports cleanly. This roadmap covers every phase from library to shipping app.

**Source inventory:** 310 C# files, 66 HLSL shaders, 16 compute shaders, 5 external packages (Minis MIDI, Syphon, OSC-Jack, Link, RoundedCorners).

---

## Dependency Graph

```
Phase 1 ✅
  └─ Phase 2 (Domain)
       ├─ Phase 3 (Window + GPU) ─┬─ Phase 4 (Compositor + Generators)
       │                          │    └─ Phase 5 (Effects)
       │                          └─ Phase 6 (Video Decode)
       ├─ Phase 7 (MIDI + Sync)      ← independent, can parallel Phase 3-6
       ├─ Phase 8 (Audio)            ← independent, can parallel Phase 3-6
       └─ Phase 9 (UI)              ← needs Phase 3 minimum
            └─ Phase 10 (Export + Outputs) ← needs Phase 4-6
```

---

## Phase 1: Core Domain ✅ DONE

Data models, PlaybackEngine, ClipScheduler, SyncArbiter, 7 commands, UndoRedoManager, V1 project loading. 4 crates, 33 tests, pushed to GitHub.

---

## Phase 2: Complete Domain Core

**Goal:** Feature-complete domain library. All editing operations work, MIDI phantom clips work, everything testable without GPU.

**Scope:**
- Port remaining 38 commands (layer, effect, driver, envelope, settings, clip properties)
- EditingService (1,476 LOC C# → Rust): mutation gateway, clipboard, overlap enforcement, region operations
- LiveClipManager (971 LOC C# → Rust): phantom clip lifecycle, quantized launch queuing
- SyncSource trait definition (no concrete implementations yet)
- PlaybackNotifier trait

**New crate work:** All in manifold-editing + manifold-playback (no new crates)

**Key Rust patterns:**
- Commands store identifiers (clip_id, layer_index), resolve via `&mut Project`
- EffectTarget enum routes effect commands to clip/layer/master
- LiveClipManager commit takes `&mut Project` explicitly (avoids aliased borrows)

**Exit criteria:** ~80+ tests, `cargo clippy` clean, all commands undo-roundtrip tested

**C# references:** EditingService.cs, ClipCommands.cs, SettingsCommands.cs, EffectGroupCommands.cs, LayerGroupCommands.cs, LiveClipManager.cs

---

## Phase 3: Window + GPU Bootstrap

**Goal:** A window that renders a solid color, driven by the PlaybackEngine frame loop.

**Scope:**
- New crate: `manifold-renderer` (wgpu backend)
- winit window creation + event loop
- wgpu device/queue/surface/adapter setup
- Basic render pipeline (clear screen, blit texture to surface)
- Render target management (create, resize, format selection)
- Frame loop: winit event → PlaybackEngine.tick() → present
- HiDPI / Retina handling (macOS `scale_factor`)

**Key decisions:**
- winit 0.30+ for windowing (cross-platform, well-maintained)
- wgpu for GPU abstraction (Metal primary, Vulkan secondary)
- Frame pacing: `requestAnimationFrame`-style via winit `ControlFlow::Poll`

**Dependencies:** winit, wgpu, raw-window-handle

**Exit criteria:** Window opens, renders solid color, resizes cleanly, engine ticks at target framerate, no frame drops

---

## Phase 4: Compositor + Generator Pipeline

**Goal:** Load a project with generator layers and see them playing back in real-time.

**Scope:**
- CompositorStack port: ping-pong render targets, layer compositing loop
- 13 blend modes as wgpu render pipelines (Normal, Additive, Multiply, Screen, Overlay, etc.)
- Generator renderer (ClipRenderer implementation for wgpu)
- ComputeShaderCache equivalent for wgpu
- Port all 19 generator types (HLSL → WGSL):
  - Simple fragment (Plasma, Tesseract, Lissajous, Flowfield, etc.) — ~12 shaders
  - Line-based (WireframeZoom, ConcentricTunnel, Duocylinder) — geometry + fragment
  - Compute-based (FluidSim, FluidSim3D, Particles, StrangeAttractor, Mycelium, Physarum) — ~6 compute pipelines
  - Volume (Raymarch, ParametricSurface) — compute bake + fragment display
- Shared particle splat shader (GeneratorParticleSplat → WGSL)
- GeneratorDefinitionRegistry + param system
- HDR pipeline: ACES tonemap + ST.2084 PQ encoding

**Key challenges:**
- HLSL → WGSL translation (keyword variants → pipeline permutations)
- Compute shader atomics (InterlockedAdd in density scatter)
- RWTexture synchronization (fluid sim ping-pong)
- Volume bake coordinate space must match raymarch mapping

**C# references:** CompositorStack.cs (1,338 LOC), GeneratorRenderer.cs (442 LOC), all 29 generator files (5,912 LOC), BlendMaterialCache.cs, ComputeShaderCache.cs

**Exit criteria:** Load any generator-only project, see all 19 generator types render and composite correctly with blend modes

---

## Phase 5: Effects Pipeline

**Goal:** Full effect chain processing — any effect on any clip/layer/master.

**Scope:**
- Effect processor registry (EffectType → pipeline mapping)
- SimpleBlitEffect base pattern → wgpu render pass
- Port all 44 effect shaders (HLSL → WGSL):
  - Simple blit effects (~30): ChromaticAberration, FilmGrain, Glitch, GradientMap, Kaleidoscope, Mirror, etc.
  - Stateful effects (~5): Feedback, StylizedFeedback, SlitScan, Datamosh, InfiniteZoom (temporal buffer)
  - Compute effects (~5): PixelSort (bitonic sort), FluidDistortion, ComputeFluidEffect, ComputeSort
  - Complex (~4): WireframeDepth (depth buffer), BlobTracking (native plugin), Bloom (multi-pass), CRT
- Effect groups: wet/dry lerp (GroupWetDryLerp shader)
- Driver system: ParameterDriverManager (LFO evaluation already in Rust, need GPU-side application)
- Envelope evaluator (ADSR already in Rust, need GPU-side application)
- TileCompositor compute shader (batching 2+ effect-free layers)

**C# references:** All 44 effect files (6,352 LOC), EffectProcessorRegistry.cs, EffectContext.cs

**Exit criteria:** All 44 effects render correctly, drivers/envelopes modulate parameters, effect groups with wet/dry work

---

## Phase 6: Video Decode + Playback

**Goal:** Load and play back video clips alongside generators.

**Scope:**
- ffmpeg integration (ffmpeg-next crate or custom FFI)
- Video decoder: file → decoded frames → GPU texture upload
- Video player pool (reusable decoders, bounded pool size)
- Video clip renderer (ClipRenderer implementation)
- Looping, in-point, playback rate, seeking
- Pending pause pattern (Play briefly for decoder init, Pause after 40ms)
- Recently-started exclusion (50ms compositor delay)
- Thumbnail generation (parallel via rayon — 5-10x speedup over Unity)
- VideoLibrary path resolution

**Key decisions:**
- ffmpeg-next vs gstreamer: ffmpeg-next preferred (lighter, more control)
- Texture upload strategy: staging buffer → GPU copy (wgpu `write_texture`)
- Decode threading: rayon thread pool for parallel multi-clip decode

**C# references:** VideoPlayerPool.cs (695 LOC), VideoClip.cs, VideoLibrary.cs (360 LOC), ClipThumbnailBuilder.cs, video_time.rs (already ported)

**Exit criteria:** Video clips play back in sync with timeline, looping works, thumbnails generate, pool recycles correctly under load

---

## Phase 7: MIDI + Sync

**Goal:** Full MIDI performance and external sync.

**Scope:**
- midir crate for MIDI input (cross-platform)
- MIDI input routing: note-on/off → LiveClipManager triggers
- MIDI channel filtering, time guards (5ms NoteOff debounce)
- Ableton Link: FFI binding to official C++ library (link already has Rust bindings)
- MIDI Clock: MidiClock state machine (360 LOC C#), MidiClockSyncController
- OSC: rosc crate, OscReceiver, OscPositionSender, parameter bridges
- Wire SyncArbiter to concrete sync sources
- MidiMappingConfig: note → layer mapping

**Can be developed in parallel with Phases 3-6** (MIDI/sync is independent of rendering).

**C# references:** MidiInputController.cs (526 LOC), MidiClock.cs (360 LOC), all Sync/ files (2,712 LOC)

**Exit criteria:** MIDI notes trigger live clips, Link syncs tempo/phase, MIDI Clock follows external clock, OSC parameters route correctly

---

## Phase 8: Audio Pipeline

**Goal:** Audio playback and percussion analysis.

**Scope:**
- Stem playback: cpal (audio output) + symphonia (decode) or rodio
- ImportedAudioSyncController equivalent: sync audio playback to timeline beats
- Percussion analysis: decision point —
  - Option A: Keep Python subprocess (percussion_json_pipeline.py) — least work, proven accuracy
  - Option B: Native Rust (tract/candle for ML inference) — 1.5x faster, no Python dependency
- Waveform rendering (peak data → GPU texture for timeline display)
- Beat-indexed energy envelope for LED gating

**Can be developed in parallel with Phases 3-6.**

**C# references:** StemAudioController.cs (472 LOC), ImportedAudioSyncController.cs (473 LOC), PercussionImportOrchestrator.cs (1,501 LOC), all Audio/ files (7,451 LOC)

**Exit criteria:** Stems play in sync with timeline, percussion analysis produces correct onset/BPM data

---

## Phase 9: UI

**Goal:** Fully interactive application — load projects, edit timelines, control playback.

**Scope:**
- Decision: custom bitmap renderer (port existing UITree/UIBitmapRoot) vs egui vs iced
  - Existing bitmap system is 11K LOC of custom GL rendering — could port to wgpu
  - egui has immediate-mode simplicity but limited visual customization
  - Recommendation: **start with egui for rapid iteration**, replace with custom bitmap later if needed
- Window layout: header, transport bar, timeline viewport, inspectors, footer
- Timeline viewport: clip rects, drag, selection region, waveform/thumbnail overlays, ruler, playhead
- Inspector panels: clip, layer, master (effect racks, generator params, driver/envelope rows)
- Keyboard shortcuts (ShortcutRegistry equivalent)
- File dialogs (rfd crate for native open/save)
- Context menus, dropdowns, sliders
- Coordinate mapping: beat ↔ pixel, zoom, scroll
- Viewport virtualization (only render visible clips)

**Largest phase by LOC** (~35K LOC in C# UI). However, much is UGUI boilerplate that doesn't apply.

**C# references:** WorkspaceController.cs (3,418 LOC), all UI/ files (35,822 LOC)

**Exit criteria:** Can load project, see timeline, play/pause/seek, select/move/trim clips, edit effects, use inspectors

---

## Phase 10: Export + External Outputs

**Goal:** Ship video, drive LEDs, send Syphon/NDI.

**Scope:**
- Frame capture: GPU readback (wgpu `map_async` on buffer)
- Video encoding:
  - Option A: ffmpeg subprocess pipe (proven, cross-platform)
  - Option B: Metal VideoToolbox (macOS native, hardware accelerated)
- HDR10 HEVC export (BT.2020/PQ, 10-bit)
- Dual frame-pacing: generator-only → offline capture, has video → real-time
- FCP XML export (ResolveFcpxmlExporter — 263 LOC, pure string generation)
- LED/ArtNet: raw UDP (artnet-protocol crate or manual packets)
- Energy gating: PercussionAnalysisData.EnergyAtBeat → DMX brightness
- Syphon output: macOS native FFI (IOSurface sharing)
- NDI: alternative to Syphon (ndi crate or FFI)
- External monitor window: second winit window on secondary display

**C# references:** VideoExporter.cs (1,275 LOC), ArtNetOutput.cs (408 LOC), SyphonOutputController.cs (451 LOC), LEDOutputController.cs (209 LOC)

**Exit criteria:** Export video file, ArtNet DMX output drives LEDs, Syphon/NDI sends to resolume/OBS

---

## Effort Estimates

| Phase | Scope | Effort | Cumulative |
|-------|-------|--------|------------|
| 1 ✅ | Core domain | Done | Done |
| 2 | Complete domain | 1-2 weeks | 1-2 weeks |
| 3 | Window + GPU | 1-2 weeks | 2-4 weeks |
| 4 | Compositor + Generators | 4-6 weeks | 6-10 weeks |
| 5 | Effects | 3-4 weeks | 9-14 weeks |
| 6 | Video decode | 2-3 weeks | 11-17 weeks |
| 7 | MIDI + Sync | 1-2 weeks | (parallel) |
| 8 | Audio | 2-3 weeks | (parallel) |
| 9 | UI | 4-6 weeks | 15-23 weeks |
| 10 | Export + Outputs | 2-3 weeks | 17-26 weeks |

**Critical path:** 1 → 2 → 3 → 4 → 5 → 6 → 9 → 10 ≈ 17-26 weeks
**Phases 7 + 8 can run in parallel** with the critical path (save ~3-5 weeks)

---

## New Crates (projected)

| Crate | Phase | Purpose |
|-------|-------|---------|
| manifold-core | 1 ✅ | Data models, types, math |
| manifold-playback | 1 ✅ | Engine, scheduler, sync |
| manifold-editing | 1 ✅ | Commands, undo, service |
| manifold-io | 1 ✅ | Project loading, migration |
| manifold-renderer | 3 | wgpu backend, compositor, generators, effects |
| manifold-video | 6 | ffmpeg decode, player pool, thumbnails |
| manifold-midi | 7 | midir input, MIDI clock, Link FFI |
| manifold-sync | 7 | OSC, sync source implementations |
| manifold-audio | 8 | Stem playback, percussion analysis |
| manifold-ui | 9 | UI framework, panels, timeline viewport |
| manifold-export | 10 | Video export, ArtNet, Syphon/NDI |
| manifold-app | 3+ | Binary crate: winit event loop, wires everything together |

---

## Key Technical Decisions (to revisit per phase)

1. **Shader translation strategy:** HLSL → WGSL manual rewrite vs naga/SPIR-V transpilation (Phase 4)
2. **UI framework:** egui (fast iteration) vs custom bitmap (visual fidelity) vs iced (Phase 9)
3. **Percussion analysis:** Keep Python subprocess vs native Rust ML inference (Phase 8)
4. **Video encoding:** ffmpeg pipe vs platform-native hardware encoder (Phase 10)
5. **Threading model:** rayon for CPU parallelism, tokio for async I/O, or keep single-threaded with wgpu async (Phase 3)
