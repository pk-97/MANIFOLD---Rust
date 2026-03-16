# MANIFOLD Rust Port — Agent Contract

## THE CARDINAL RULE

**The Unity source code is the SINGLE SOURCE OF TRUTH for ALL behavior.**

You MUST NOT:
- Synthesize new logic from descriptions, summaries, or documentation
- Write code "inspired by" or "approximating" Unity behavior
- Guess at implementation details based on what a feature should do
- Use audit documents, interaction contracts, or user guides as the primary source

You MUST:
- **Read the Unity .cs file FIRST** before writing ANY Rust code
- **Translate line by line** — same logic flow, same variable names, same edge cases
- **Read the Unity .shader/.compute file FIRST** before writing ANY WGSL
- **Translate each line of HLSL to WGSL** — same math, same variable names, same order
- If you haven't read the Unity source file, you are NOT READY to write the Rust equivalent

This is a MECHANICAL TRANSLATION, not a reimplementation. The architecture maps:
- C# class → Rust struct
- Unity MonoBehaviour → trait impl
- HLSL shader → WGSL shader
- C# method → Rust method (same name, same logic, same order)

## PORTING WORKFLOW (mandatory for every file)

1. **Read** the Unity .cs file completely
2. **Map** each field, method, and constant to Rust equivalents
3. **Translate** line by line, preserving logic flow and variable names
4. **Verify** against the Unity source — every branch, every edge case
5. **Never skip complexity** — if Unity does something, Rust must do the same thing

## Unity Source Locations

The Unity project is at: `/Users/peterkiemann/MANIFOLD - Render Engine/`

Key directories:
```
Assets/Scripts/Data/                    — Data models (Project, Timeline, Layer, Clip, Effects, etc.)
Assets/Scripts/Editing/                 — Commands, EditingService, UndoRedoManager
Assets/Scripts/Playback/                — PlaybackController, ClipScheduler, Generators/
Assets/Scripts/Playback/Generators/     — ALL generator implementations
Assets/Scripts/UI/Timeline/             — WorkspaceController, InputHandler, InteractionOverlay
Assets/Scripts/UI/Timeline/Core/        — UIState, UIConstants, CoordinateMapper
Assets/Scripts/UI/Bitmap/               — UIInputSystem, UIBitmapRoot, all panel classes
Assets/Scripts/Compositing/             — CompositorStack, Effects/
Assets/Scripts/Sync/                    — Link, MidiClock, OSC sync sources
Assets/Scripts/Export/                  — VideoExporter, ProjectArchive, ProjectSerializer
Assets/Shaders/                         — ALL HLSL shaders
Assets/Resources/Compute/               — ALL compute shaders
```

## INVARIANTS (from Unity project)

- Primary time model is beats (startBeat, durationBeats)
- Seconds only for: InPoint, player.time, deltaTime, OSC, export
- SyncClipsToTime() is the sole idempotent authority for playback state
- Overlap is a write-time invariant on Layer
- EditingService is the SOLE mutation gateway
- All data mutations flow through EditingService — no direct model writes from UI
- UI translates gestures into service calls; service handles command creation + undo recording

## METAL/WGPU CONSTRAINTS (learned the hard way)

- `max_compute_invocations_per_workgroup` = 256 on Metal. Use @workgroup_size(4,4,4) for 3D, not (8,8,8)
- `R16Float` does NOT support `STORAGE_BINDING` on Metal. Use `Rgba16Float` for ALL storage textures
- `R32Float` is NOT filterable on Metal. Use `Rgba16Float` for textures that need `textureSample`
- `max_storage_buffer_binding_size` = 128MB on Metal. Cap particle buffers to 2M × 48 bytes = 96MB
- Compute shaders: use `textureLoad` (no sampler). Fragment shaders: use `textureSample` (with filtering sampler)
- `Rgba16Float` is the universal safe format: supports STORAGE_BINDING + filtering + RENDER_ATTACHMENT

## PROJECT STRUCTURE

```
crates/manifold-core/       — Data models, types, enums
crates/manifold-editing/    — Commands, undo, EditingService
crates/manifold-playback/   — PlaybackEngine, ClipScheduler, SyncArbiter
crates/manifold-io/         — Project loading (V1 JSON + V2 ZIP), saving, migration
crates/manifold-renderer/   — wgpu GPU: compositor, effects, generators, UI renderer
crates/manifold-ui/         — Custom bitmap UI: tree, panels, input, layout
crates/manifold-app/        — winit entry point, Application, UIRoot, UIBridge
```

## KEY DOCS IN THIS REPO

```
docs/UNITY_PARITY_AUDIT.md     — Gap analysis
docs/INTERACTION_CONTRACT.md   — Behavioral spec (extracted from Unity source)
docs/PORTING_STRATEGY.md       — Testing strategy
docs/MIGRATION_PLAN.md         — Phase execution plan
```
