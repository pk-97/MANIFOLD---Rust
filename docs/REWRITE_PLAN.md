# Systematic Rewrite Plan — Unity Source → Rust Translation

> **Rule:** Read the Unity .cs file first. Translate line by line. No exceptions.

## Complete File Mapping

### Layer 0: Data Models (manifold-core) — Zero dependencies

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `core/clip.rs` | `Data/TimelineClip.cs` | VERIFY — data struct, likely OK |
| `core/layer.rs` | `Data/Layer.cs` | VERIFY — data struct |
| `core/timeline.rs` | `Data/Timeline.cs` | VERIFY — data struct + FindClipById |
| `core/project.rs` | `Data/Project.cs` | VERIFY — on_after_deserialize needs checking |
| `core/settings.rs` | `Data/ProjectSettings.cs` | VERIFY — defaults matter |
| `core/effects.rs` | `Data/EffectInstance.cs`, `EffectGroup.cs`, `ParameterDriver.cs`, `ParamEnvelope.cs` | VERIFY — serde fixes applied |
| `core/generator.rs` | `Data/GeneratorParamState.cs` | VERIFY |
| `core/types.rs` | `Data/EffectType.cs`, `GeneratorType.cs`, `BlendMode.cs`, etc. | VERIFY — param_defs need checking against `EffectDefinitionRegistry.cs` and `GeneratorDefinitionRegistry.cs` |
| `core/video.rs` | `Data/VideoClip.cs`, `VideoLibrary.cs` | VERIFY |
| `core/tempo.rs` | `Data/TempoMap.cs` | VERIFY |
| `core/midi.rs` | `Data/MidiMappingConfig.cs` | VERIFY |
| `core/percussion.rs` | `Data/PercussionImportState.cs` | VERIFY |
| `core/recording.rs` | `Data/RecordingProvenance.cs` | VERIFY |
| `core/selection.rs` | `UI/Timeline/Core/UIState.cs` (SelectionRegion) | VERIFY |
| `core/color.rs` | `UI/Timeline/Core/UIConstants.cs` (Color struct) | VERIFY |
| `core/math.rs` | No direct Unity equivalent | OK (utility) |

### Layer 1: Editing Commands (manifold-editing) — Depends on core

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `editing/command.rs` | `Editing/ICommand.cs` | VERIFY |
| `editing/undo.rs` | `Editing/UndoRedoManager.cs` | REWRITE — verify against Unity |
| `editing/clipboard.rs` | `Editing/EffectClipboard.cs` | VERIFY |
| `editing/service.rs` | `UI/Timeline/EditingService.cs` (1476 LOC) | **REWRITE** — core mutation gateway |
| `editing/commands/clip.rs` | `Editing/ClipCommands.cs` | REWRITE from Unity source |
| `editing/commands/layer.rs` | `Editing/LayerGroupCommands.cs` | REWRITE from Unity source |
| `editing/commands/settings.rs` | `Editing/SettingsCommands.cs` | REWRITE from Unity source |
| `editing/commands/effects.rs` | `Editing/SettingsCommands.cs` (effect section) | REWRITE from Unity source |
| `editing/commands/effect_target.rs` | (routing logic in EditingService) | VERIFY |
| `editing/commands/effect_groups.rs` | `Editing/EffectGroupCommands.cs` | REWRITE from Unity source |
| `editing/commands/drivers.rs` | `Editing/SettingsCommands.cs` (driver section) | REWRITE from Unity source |
| `editing/commands/envelopes.rs` | `Editing/SettingsCommands.cs` (envelope section) | REWRITE from Unity source |
| `editing/commands/selection.rs` | (no direct Unity command) | OK |

### Layer 2: Playback (manifold-playback) — Depends on core + editing

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `playback/engine.rs` | `Playback/PlaybackController.cs`, `PlaybackEngine.cs` | **REWRITE** — core playback loop |
| `playback/scheduler.rs` | `Playback/ClipScheduler.cs` | REWRITE from Unity source |
| `playback/renderer.rs` | `Playback/IClipRenderer.cs` | VERIFY (trait definition) |
| `playback/video_time.rs` | (part of ClipScheduler) | VERIFY |
| `playback/active_window.rs` | (optimization, verify against Unity) | VERIFY |
| `playback/live_clip_manager.rs` | `Playback/LiveClipManager.cs` | REWRITE from Unity source |
| `playback/sync.rs` | `Sync/SyncArbiter.cs` | VERIFY |
| `playback/sync_source.rs` | `Sync/ISyncSource.cs` | VERIFY (trait only) |

### Layer 3: IO (manifold-io) — Depends on core

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `io/loader.rs` | `Export/ProjectSerializer.cs`, `Export/ProjectArchive.cs` | VERIFY (recently fixed) |
| `io/migrate.rs` | `Export/ProjectJsonMigrator.cs` | VERIFY |
| `io/saver.rs` | `Export/ProjectSerializer.cs` (save path) | VERIFY |

### Layer 4: UI System (manifold-ui) — Depends on core

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `ui/node.rs` | Custom (no direct Unity equivalent) | OK (Rust-specific UI tree) |
| `ui/tree.rs` | Custom | OK |
| `ui/input.rs` | `UI/Bitmap/UIInputSystem.cs` | **REWRITE** from Unity source |
| `ui/color.rs` | `UI/Timeline/Core/UIConstants.cs` | VERIFY constants match |
| `ui/layout.rs` | `UI/Timeline/Core/WidgetLayout.cs`, `InspectorLayout.cs` | **REWRITE** from Unity source |
| `ui/slider.rs` | `UI/Bitmap/BitmapSlider.cs` | **REWRITE** from Unity source |
| `ui/snap.rs` | `UI/Timeline/InteractionOverlay.cs` (MagneticSnapBeat) | VERIFY (pure function) |
| `ui/trim.rs` | `UI/Timeline/InteractionOverlay.cs` (trim section) | VERIFY (pure function) |
| `ui/cursor_nav.rs` | `UI/Timeline/InputHandler.cs` (arrow nav section) | VERIFY (pure function) |
| `ui/text.rs` | Custom | OK |
| `ui/panels/transport.rs` | `UI/Bitmap/TransportPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/header.rs` | `UI/Bitmap/HeaderPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/footer.rs` | `UI/Bitmap/FooterPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/layer_header.rs` | `UI/Bitmap/LayerHeaderPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/viewport.rs` | `UI/Timeline/InteractionOverlay.cs` + `ClipHitTester.cs` + `ViewportManager.cs` | **REWRITE** — most critical |
| `ui/panels/inspector.rs` | `UI/Bitmap/InspectorCompositeBitmapPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/effect_card.rs` | `UI/Bitmap/EffectCardBitmapPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/gen_param.rs` | `UI/Bitmap/GenParamBitmapPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/master_chrome.rs` | `UI/Bitmap/MasterChromeBitmapPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/layer_chrome.rs` | `UI/Bitmap/LayerChromeBitmapPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/clip_chrome.rs` | `UI/Bitmap/ClipChromeBitmapPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/dropdown.rs` | `UI/Bitmap/DropdownPanel.cs` | **REWRITE** from Unity source |
| `ui/panels/mod.rs` | (enum definitions) | VERIFY |

### Layer 5: App Glue (manifold-app) — Depends on everything

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `app/app.rs` | `UI/Timeline/WorkspaceController.cs` (3418 LOC) | **REWRITE** — main loop, refresh, undo |
| `app/ui_bridge.rs` | `UI/Timeline/WorkspaceController.cs` (dispatch section) | **REWRITE** from Unity source |
| `app/ui_root.rs` | `UI/Bitmap/UIBitmapRoot.cs` | **REWRITE** from Unity source |
| `app/text_input.rs` | `UI/Bitmap/BitmapTextInput.cs` | **REWRITE** from Unity source |
| `app/frame_timer.rs` | Custom | OK |
| `app/window_registry.rs` | Custom | OK |

### Layer 6: Renderer (manifold-renderer) — Depends on core + ui

| Rust file | Unity source | Status |
|-----------|-------------|--------|
| `renderer/compositor.rs` | `Compositing/CompositorStack.cs` | **REWRITE** from Unity source |
| `renderer/layer_compositor.rs` | `Compositing/CompositorStack.cs` (BlitClip section) | **REWRITE** |
| `renderer/effect_chain.rs` | `Compositing/CompositorStack.cs` (ApplyEffectChain) | **REWRITE** |
| `renderer/effect.rs` | `Compositing/Effects/IPostProcessEffect.cs` | VERIFY |
| `renderer/effect_registry.rs` | `Compositing/EffectProcessorRegistry.cs` | VERIFY |
| `renderer/effects/*.rs` | `Compositing/Effects/*.cs` + `Shaders/FX_*.shader` | **REWRITE** each from Unity source |
| `renderer/generator.rs` | `Playback/Generators/IGenerator.cs` | VERIFY |
| `renderer/generator_context.rs` | `Playback/Generators/GeneratorContext.cs` | VERIFY |
| `renderer/generator_renderer.rs` | `Playback/Generators/GeneratorRenderer.cs` | **REWRITE** |
| `renderer/generators/*.rs` | `Playback/Generators/*.cs` + `Shaders/Generator*.shader` | **REWRITE** each from Unity source |
| `renderer/blit.rs` | Custom (wgpu-specific) | OK |
| `renderer/gpu.rs` | Custom (wgpu-specific) | OK |
| `renderer/surface.rs` | Custom (wgpu-specific) | OK |
| `renderer/render_target.rs` | Custom (wgpu-specific) | OK |
| `renderer/ui_renderer.rs` | Custom (wgpu text rendering) | OK |
| `renderer/wet_dry_lerp.rs` | `Shaders/GroupWetDryLerp.shader` | VERIFY |

---

## Rewrite Order (bottom-up dependency chain)

### Batch 1: Data Models (manifold-core)
Read each Unity .cs, verify/fix the Rust struct. Focus on:
- Correct field names/types/defaults matching Unity serialization
- `on_after_deserialize` matching Unity's `OnAfterDeserialize`
- `EffectDefinitionRegistry.cs` → verify all effect param_defs
- `GeneratorDefinitionRegistry.cs` → verify all generator param_defs

### Batch 2: Editing Commands (manifold-editing)
Read each Unity command .cs, rewrite the Rust command. Focus on:
- `EditingService.cs` → the mutation gateway (overlap enforcement, split, paste, etc.)
- Each command's execute/undo behavior matching exactly

### Batch 3: Playback (manifold-playback)
Read Unity playback .cs files, rewrite. Focus on:
- `PlaybackController.cs` → tick loop, SyncClipsToTime
- `ClipScheduler.cs` → which clips start/stop when
- `LiveClipManager.cs` → phantom clip lifecycle

### Batch 4: UI Input + State (manifold-ui core)
Read Unity UI input files, rewrite. Focus on:
- `UIInputSystem.cs` → pointer state machine (THE most critical file)
- `UIBitmapRoot.cs` → panel routing, z-order, dropdown dismiss
- `UIState.cs` → selection model, drag state, cursor state

### Batch 5: UI Panels (manifold-ui panels)
Read each Unity panel .cs, rewrite. Focus on:
- `InteractionOverlay.cs` → clip click/drag/trim/region (THE most buggy area)
- `ViewportManager.cs` → scroll, zoom, auto-scroll
- `InputHandler.cs` → keyboard shortcuts
- All inspector/chrome panels

### Batch 6: App Glue (manifold-app)
Read `WorkspaceController.cs`, rewrite app.rs + ui_bridge.rs. Focus on:
- Main loop: event → dispatch → sync → rebuild → push_state → render
- Undo/redo refresh flow
- File operations

### Batch 7: Compositor + Effects (manifold-renderer)
Read `CompositorStack.cs`, rewrite. Focus on:
- Ping-pong buffer management
- Per-clip → per-layer → master effect chain
- Blend mode application
- Each effect shader: read HLSL, translate to WGSL

### Batch 8: Generators (manifold-renderer/generators)
Read each Unity generator .cs + .shader, verify/rewrite. Focus on:
- Param index mapping matching exactly
- Shader math matching HLSL line for line
- State management (ping-pong, compute buffers)

---

## Progress Tracking

Each file gets marked when rewritten:
- [ ] = not started
- [V] = verified correct (no changes needed)
- [R] = rewritten from Unity source
- [F] = fixed (minor corrections from Unity source)
