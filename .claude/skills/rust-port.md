# Port Unity File to Rust

Mechanically translate a Unity C# file (or shader) to Rust, preserving exact architecture, values, and logic flow.

User request: $ARGUMENTS

## Context

The Rust codebase uses: Edition 2024, typed IDs (`ClipId`/`LayerId`/`EffectGroupId`), `AHashMap` on hot paths, `parking_lot` mutexes. Use these patterns in ported code. All serialized structs need `#[serde(rename_all = "camelCase")]`.

## Mandatory Workflow

### 1. Identify the Unity Source

Determine which Unity file(s) to port from the user's request. The Unity project is at:
`/Users/peterkiemann/MANIFOLD - Render Engine/`

### 2. Read the Unity Source COMPLETELY

Read the entire .cs file. For shaders, read the .shader and/or .compute file.
For effects/generators, ALSO read:
- The registry entry in `EffectDefinitionRegistry.cs` or `GeneratorDefinitionRegistry.cs`
- The `SetUniforms()` / `Apply()` / `Render()` method for param-to-shader mapping

### 3. Identify the Target Crate

| Unity Directory | Rust Crate |
|---|---|
| `Assets/Scripts/Data/` | `manifold-core` |
| `Assets/Scripts/Editing/` | `manifold-editing` |
| `Assets/Scripts/Playback/` | `manifold-playback` |
| `Assets/Scripts/Compositing/` | `manifold-renderer` |
| `Assets/Scripts/Playback/Generators/` | `manifold-renderer` (generators/) |
| `Assets/Shaders/` | `manifold-renderer` (*.wgsl) |
| `Assets/Scripts/UI/` | `manifold-ui` |

### 4. Translate

- Line by line. Same logic, same names (snake_case'd), same edge cases.
- Same constants (EXACT values). Same texture formats. Same math operations.
- Same pass count. Same parameter indices. Same defaults.
- Interfaces → traits. Base classes → trait + shared state. Services → service structs.

### 5. Verify

Re-read Unity source. Walk Rust code. Confirm:
- All fields present. All methods present. All edge cases preserved.
- All constants match. All formats match. All math ops match.
- All param indices match registry. All shader uniforms match.

## Guardrails

- NEVER synthesize from descriptions — only translate from source
- NEVER change constants, formats, or limits for "platform reasons"
- NEVER approximate multi-pass as single-pass
- NEVER add abstractions Unity doesn't have
- Port services as whole units, not scattered methods
- Port dependencies before features that depend on them
