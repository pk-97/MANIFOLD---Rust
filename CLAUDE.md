# MANIFOLD Rust Port — Agent Contract

YOU MUST read this file completely before any action. Every rule is load-bearing.

## THE CARDINAL RULE — STRUCTURAL FIDELITY

**The Unity codebase is the SINGLE SOURCE OF TRUTH for ALL behavior AND ALL architecture.**

The Unity project at `/Users/peterkiemann/MANIFOLD - Render Engine/` was deliberately designed with interfaces, base classes, OOP, SOLID principles, and dependency inversion so that a **direct structural port** would be feasible. A huge amount of work was spent optimising that architecture. The Rust port MUST preserve it.

This is NOT a reimplementation. This is NOT "Rust-inspired-by-Unity." This is a **mechanical structural translation**.

### The Architecture Maps Directly

| Unity (C#) | Rust | Notes |
|---|---|---|
| `interface IFoo` | `trait Foo` | Same methods, same signatures, same contract |
| `abstract class FooBase` | `trait FooBase` + shared state struct | Preserve the inheritance hierarchy as trait + composition |
| `class Foo : FooBase` | `struct Foo` + `impl FooBase for Foo` | Same fields, same method bodies |
| `class FooService` (plain C#) | `struct FooService` | Same public API surface, same internal logic |
| `class FooController : MonoBehaviour` | `struct FooController` with lifecycle methods | `Awake()` → `new()`, `Update()` → `tick()`, `OnDestroy()` → `Drop` |
| `enum FooType` | `enum FooType` | Same variants, same numeric values |
| HLSL shader | WGSL shader | Same math, same variable names, same order |
| ComputeShader | wgpu compute pipeline | Same dispatch sizes, same buffer layouts |
| `List<T>` | `Vec<T>` | Same semantics |
| `Dictionary<K,V>` | `HashMap<K,V>` | Same semantics |
| `HashSet<T>` | `HashSet<T>` | Same semantics |
| `event Action<T>` | callback / channel | Same firing semantics |
| `[NonSerialized]` field | `#[serde(skip)]` | Same runtime-only semantics |
| `[SerializeField]` field | `#[serde(rename = "...")]` | Same JSON field names |
| nullable reference | `Option<T>` | Same null-check semantics |

### What You MUST Preserve

1. **Every interface** → becomes a Rust trait with the SAME methods
2. **Every base class** → becomes a trait + shared state struct; concrete subclasses implement the trait
3. **Every service class** → becomes a Rust struct with the SAME public method surface
4. **Every data model** → becomes a Rust struct with the SAME fields, SAME relationships
5. **Every command** → becomes a Rust struct implementing the Command trait with SAME execute/undo logic
6. **Every registry** → becomes a Rust module with the SAME lookup functions
7. **Method names** → same names (snake_case conversion is fine: `SyncClipsToTime` → `sync_clips_to_time`)
8. **Field names** → same names (snake_case conversion is fine: `startBeat` → `start_beat`)
9. **Logic flow** → same branches, same edge cases, same order of operations
10. **Constants** → same values, same names

### What You MUST NOT Do

- **DO NOT** flatten inheritance hierarchies into "simpler" Rust patterns
- **DO NOT** merge classes that Unity keeps separate
- **DO NOT** rename things to be "more Rusty" beyond snake_case conversion
- **DO NOT** lose interface/trait boundaries
- **DO NOT** collapse service layers
- **DO NOT** change method signatures to be "more idiomatic"
- **DO NOT** add Rust idioms that change semantics (e.g., replacing mutable state with functional transforms)
- **DO NOT** synthesize code from descriptions, summaries, audit docs, or documentation
- **DO NOT** write code "inspired by" or "approximating" Unity behavior
- **DO NOT** guess at implementation details based on what a feature "should" do
- **DO NOT** add error handling beyond what Unity does (if Unity doesn't check for it, Rust shouldn't either — use `unwrap()` / `expect()` for "impossible" states)
- **DO NOT** over-abstract or add generic type parameters that Unity doesn't have
- **DO NOT** use `Arc<Mutex<T>>` where Unity uses a plain mutable field — match the ownership model

---

## PORTING WORKFLOW — MANDATORY FOR EVERY FILE

Every porting task follows this exact sequence. No shortcuts. No skipping steps.

### Step 1: READ the Unity Source

```
READ /Users/peterkiemann/MANIFOLD - Render Engine/Assets/Scripts/{path}.cs
```

Read the ENTIRE file. Every field. Every method. Every constant. Every comment that reveals intent.

**HALT if you haven't read the Unity source.** You are NOT READY to write Rust.

### Step 2: MAP the Structure

Before writing any code, list:
- Every field → Rust type equivalent
- Every method → Rust signature
- Every interface implemented → which Rust trait
- Every base class → which trait + shared state struct
- Every dependency → which crate provides it

### Step 3: TRANSLATE Line by Line

Write the Rust code as a mechanical translation:
- Same logic flow
- Same variable names (snake_case'd)
- Same edge cases
- Same order of operations
- Same branching structure

### Step 4: SELF-AUDIT

After writing, answer these questions:
1. Did I skip any methods? List them.
2. Did I simplify any logic? Where and why?
3. Did I change any signatures? Which ones?
4. Did I add any abstractions Unity doesn't have? Which?
5. Did I lose any interface/trait boundary? Which?

**If any answer is "yes" and it wasn't explicitly approved, go back and fix it.**

### Step 5: VERIFY — VALUE-LEVEL PARITY

Re-read the Unity source. Walk through the Rust code line by line and confirm 1:1 correspondence.

**Structural check:**
- All fields present with same types
- All methods present with same logic
- All edge cases preserved (every `if`, every early return, every guard)
- Interface/trait surface matches exactly

**Value-level check (CRITICAL — this is where most bugs hide):**
- Every constant matches Unity's value EXACTLY (buffer sizes, timeouts, thresholds, epsilons)
- Every texture format matches Unity's format (RFloat → R32Float, NOT Rgba16Float)
- Every math operation matches (RoundToInt → round(), NOT truncation)
- Every param index matches the registry definition
- Every shader uniform name matches
- Every default value matches
- Every min/max range matches
- Every dispatch size / workgroup size matches Unity's compute dispatch

**If ANY value differs from Unity without explicit approval, fix it before proceeding.**

---

## EFFECT / GENERATOR PORTING WORKFLOW — MANDATORY

This is the most error-prone area. Effects and generators have been hyper-optimised in Unity. Every parameter, every pass, every texture format, every constant exists for a concrete visual reason. Synthesis drift here means the project looks wrong.

### Step 1: Read the Unity C# Class FIRST

Read the ENTIRE effect/generator .cs file:
- `SetUniforms()` / `Apply()` / `Render()` — this is the **exact param-to-shader mapping**. Every `material.SetFloat`, `SetVector`, `SetTexture` call tells you what the shader expects.
- Constructor / `Initialize()` — what resources are allocated, what formats, what sizes.
- Count the **passes**. If Unity has 3 passes, Rust has 3 passes. NEVER approximate multi-pass as single-pass.
- Count the **textures**. 2 input textures = `DualTextureBlitHelper` pattern. 1 = `SimpleBlitHelper`. Never duplicate boilerplate.

### Step 2: Read the Registry Entry

Read `EffectDefinitionRegistry.cs` or `GeneratorDefinitionRegistry.cs` for the exact param definitions:
- Param count
- Every param index, name, min, max, default value, format string
- `unwrap_or()` defaults in Rust MUST match `paramDefs` defaults in Unity's `types.rs`
- Discrete params (those with `wholeNumbers = true`) need `.round()` before `as u32`

### Step 3: Read the HLSL Shader

```
READ /Users/peterkiemann/MANIFOLD - Render Engine/Assets/Shaders/{name}.shader
READ /Users/peterkiemann/MANIFOLD - Render Engine/Assets/Resources/Compute/{name}.compute
```

Read EVERY pass, EVERY function, EVERY property. Then:
- Translate line by line to WGSL — same math, same variable names, same order
- Same coordinate spaces (UV origin, NDC conventions)
- Texel sizes come from the **SOURCE** texture, not the target
- Same texture sampling modes (point vs bilinear vs trilinear)

### Step 4: Match Pass Architecture

- If Unity does H-blur then V-blur, Rust does H-blur then V-blur (same ping-pong)
- If Unity uses a temporary RT at half resolution, Rust uses a temporary RT at half resolution
- If Unity blits through an intermediate buffer, Rust blits through an intermediate buffer
- If Unity reads from `_MainTex` and writes to `_TempTex`, Rust reads from source and writes to temp

### Step 5: Stateful Effect Rules

For effects that implement `IStatefulEffect` (per-owner temporal state):
- Rust needs a per-owner `HashMap<i32, T>` matching Unity's `Dictionary<int, T>`
- Owner key encoding: `0 = master`, `layer_index + 1 = layer`, `clip_id.hash() = clip`
- `clear_state(owner_key)` must clear ONLY that owner's state
- `cleanup_all_owners()` must release ALL per-owner resources

### Step 6: VERIFY

After writing:
- Every param index matches the registry
- Every default value matches
- Every shader uniform name matches
- Pass count matches
- Texture count matches
- Texel size source matches (source texture, not target)
- Rounding on discrete params present
- Stateful effects have per-owner maps if Unity does

---

## NAMED FAILURE MODES — RECOGNIZE AND STOP

These are the specific mistakes that keep happening. If you catch yourself doing any of these, STOP and correct immediately.

### FM-1: Synthesizing from Documentation
**Pattern:** Reading an audit doc, interaction contract, or user guide, then writing Rust code based on that description instead of reading the actual Unity source.
**Why it fails:** Docs describe behavior. Unity source IS behavior. Translation from description is lossy.
**Fix:** Close the doc. Open the .cs file. Translate from source.

### FM-2: Approximating Instead of Translating
**Pattern:** Writing code that "roughly does the same thing" instead of preserving exact logic flow.
**Why it fails:** Edge cases, state machines, refresh flows, timing guards — all lost in approximation.
**Fix:** Line-by-line translation. If Unity has 47 lines in a method, your Rust should have ~47 lines.

### FM-3: Flattening Architecture
**Pattern:** Combining two Unity classes into one Rust struct because "it's simpler in Rust."
**Why it fails:** The separation exists for a reason — testability, dependency inversion, reuse. Flattening breaks the architecture.
**Fix:** If Unity has `IClipRenderer` as an interface with `VideoPlayerRenderer` and `GeneratorRenderer` as implementations, Rust has `trait ClipRenderer` with `VideoPlayerRenderer` and `GeneratorRenderer` as implementations. Period.

### FM-4: Rustifying Semantics
**Pattern:** Replacing mutable state management with functional patterns, replacing callbacks with channels, replacing nullable fields with Result types.
**Why it fails:** Changes the behavior model. Makes it impossible to verify parity with Unity.
**Fix:** Match Unity's ownership and mutation model. `Option<T>` for nullable. Mutable fields for mutable state. Keep it mechanically equivalent.

### FM-5: Over-Engineering Error Handling
**Pattern:** Adding `Result<T, E>` return types, custom error enums, and propagation chains where Unity just crashes or returns null.
**Why it fails:** Changes control flow. Adds code paths Unity doesn't have. Makes divergence harder to track.
**Fix:** If Unity would throw/crash, use `unwrap()`/`expect("reason")`. If Unity returns null, use `Option<T>`.

### FM-6: Missing Edge Cases
**Pattern:** Porting the "happy path" of a method and ignoring guards, early returns, bounds checks, and special cases.
**Why it fails:** The edge cases ARE the behavior. A scheduler without its timing guards is broken.
**Fix:** Every `if` in Unity → an `if` in Rust. Every early return → an early return. Every bounds check → a bounds check.

### FM-7: Inventing State
**Pattern:** Adding fields, caches, or tracking state that doesn't exist in Unity because "Rust needs it."
**Why it fails:** Creates divergence. Now the Rust version has state Unity doesn't, making parity verification impossible.
**Fix:** Only add state that Unity has. If Rust's ownership model genuinely requires different state management, document it explicitly and keep the logical behavior identical.

### FM-8: Wrong Abstraction Level
**Pattern:** Creating generic frameworks, builder patterns, or type-level abstractions for things Unity implements concretely.
**Why it fails:** Unity's concrete implementation IS the spec. Abstract frameworks produce different code paths.
**Fix:** If Unity has a concrete `BloomFX` class that reads 4 params and blits a shader, Rust has a concrete `BloomFX` struct that reads 4 params and dispatches a pipeline. No generic "EffectFramework<T>".

### FM-9: Hallucinated Constraints
**Pattern:** The agent invents platform limits, buffer size caps, or format substitutions based on what "sounds right" rather than verified facts. Example: writing `MAX_PARTICLES = 2_000_000` with a comment "Metal max_storage_buffer_binding_size is 128MB" — a fabricated justification for a number the agent chose because 8M "felt too big."
**Why it fails:** These are degradations dressed up as engineering decisions. The 2M cap means 1/4 the particle density, changing the entire visual character. The agent understood the code but didn't trust the specific values.
**Root cause:** An agent that understands the code is MORE dangerous than one that doesn't, because it feels confident enough to "improve" things during translation. The mechanical translation rule exists precisely to prevent this.
**Fix:** NEVER invent platform limits. If Unity uses 8M particles, write `8_000_000`. If a real platform limit exists, you will hit it at runtime — do not preemptively work around imagined limits. If you genuinely know a hard platform constraint, add a runtime clamp with a comment referencing the Unity value, but the source constant stays at Unity's value.

### FM-10: Substituting Texture Formats
**Pattern:** Unity uses `RFloat` (32-bit single-channel) for density and `RGFloat` (32-bit two-channel) for vector fields. The Rust port uses `Rgba16Float` everywhere because "it's the safe universal format."
**Why it fails:** Half the precision AND 4x the bandwidth. The Unity formats were chosen deliberately — 32-bit precision for accumulation pipelines, and only the channels actually needed. "Convenient" substitution wastes GPU bandwidth and loses precision simultaneously.
**Fix:** Match Unity's texture format EXACTLY:
- `RFloat` → `R32Float`
- `RGFloat` → `Rg32Float`
- `RHalf` → `R16Float`
- `ARGBHalf` → `Rgba16Float`
- `ARGBFloat` → `Rgba32Float`
Never use `Rgba16Float` as a "universal" format — it wastes bandwidth and changes precision. If Metal doesn't support a format for a specific usage, that's a runtime problem to solve at runtime (format fallback, capability query) — NOT a compile-time compromise baked into the source.

### FM-11: Changing Constants and Limits
**Pattern:** Unity says `maxParticles = 8_000_000`. The Rust port says `MAX_PARTICLES = 2_000_000` because "Metal's max_storage_buffer_binding_size is 128MB."
**Why it fails:** The Unity value IS the spec. Platform constraints are runtime concerns, not source-code compromises. If the value needs to be clamped at runtime on certain hardware, do that at runtime.
**Fix:** Every constant, limit, threshold, buffer size, timeout, epsilon, and magic number must match Unity EXACTLY. If you need a platform-specific runtime cap, add it as a separate runtime clamp — don't change the source constant.

### FM-12: Math Operation Drift
**Pattern:** Unity does `Mathf.RoundToInt(radius)`. Rust does `radius as i32` (truncation). Unity does `Mathf.Lerp`. Rust does a manual lerp with different clamping behavior. Unity clamps with `Mathf.Clamp01`. Rust uses `.min(1.0).max(0.0)`.
**Why it fails:** Rounding modes, clamping behavior, and precision semantics affect visual output. These effects have been hyper-optimised in Unity. A truncation where Unity rounds changes blur kernel sizes, blend weights, and timing thresholds.
**Fix:** Match the EXACT math operation:
- `Mathf.RoundToInt(x)` → `x.round() as i32`
- `Mathf.FloorToInt(x)` → `x.floor() as i32`
- `Mathf.CeilToInt(x)` → `x.ceil() as i32`
- `Mathf.Clamp01(x)` → `x.clamp(0.0, 1.0)`
- `Mathf.Clamp(x, min, max)` → `x.clamp(min, max)`
- `Mathf.Lerp(a, b, t)` → `a + (b - a) * t.clamp(0.0, 1.0)` (Lerp clamps t!)
- `Mathf.LerpUnclamped(a, b, t)` → `a + (b - a) * t`
- `Mathf.InverseLerp(a, b, v)` → `((v - a) / (b - a)).clamp(0.0, 1.0)`
- `Mathf.SmoothStep(a, b, t)` → Hermite interpolation with clamped t
- `Mathf.Abs(x)` → `x.abs()`
- `Mathf.Sign(x)` → check Unity's exact behavior (returns 1 for 0, NOT 0)
- `Mathf.Repeat(t, len)` → `t - (t / len).floor() * len` (NOT `t % len` which has different sign behavior)

### FM-13: Shader Uniform / Parameter Value Drift
**Pattern:** Unity sets `material.SetFloat("_Radius", 3.5f)`. Rust passes `3.0` or a differently-named uniform. Unity reads `paramValues[2]` for blur radius. Rust reads `params[1]` (wrong index).
**Why it fails:** Every parameter index, uniform name, default value, and range was tuned by hand in Unity. Wrong indices mean wrong visual behavior. Wrong defaults mean the effect looks different on first use.
**Fix:** When porting any effect or generator:
1. Read the Unity `EffectDefinitionRegistry` / `GeneratorDefinitionRegistry` entry for param definitions
2. Match EVERY param index, name, min, max, default, format string
3. Match EVERY shader uniform name and value
4. Match EVERY `material.SetFloat/SetVector/SetTexture` call and its parameter

### FM-14: Scattering Service Logic Across Event Handlers
**Pattern:** Unity has `ProjectIOService` as a single class that owns all project load/save logic — open dialog, open recent, file drop, save, save-as all route through `OpenProjectFromPath()`. The Rust port implements each entry point ad-hoc: `open_project()` inline in `app.rs`, then the `DroppedFile` handler copy-pastes the same 20 lines, then `open_recent_project()` would be copy #3.
**Why it fails:** DRY violation. The copies diverge immediately — one gets bug fixes, the others don't. The drop handler was already buggy (never persisted the path) because that concern didn't exist when it was written, and nobody went back to update it.
**Fix:** Port services as UNITS, not individual methods. If Unity has `ProjectIOService`, Rust has a `ProjectIOService` struct with the same public methods. Every code path that loads a project calls `project_io_service.open_project_from_path()`. No inline load logic in event handlers. Ever.

### FM-15: Missing Cross-Cutting Infrastructure
**Pattern:** `DialogPathMemory` and `PlayerPrefs`-equivalent persistence are infrastructure that multiple features depend on. The Rust port builds the features (open, save, save-as) without porting the infrastructure they sit on. Dialogs always open to system default directory, paths never remembered across sessions.
**Why it fails:** Invisible at the feature level. Each method "works" in isolation. The missing persistence only shows up as a UX regression noticed after weeks of use.
**Fix:** Before porting feature methods, identify and port the shared infrastructure they depend on. Read the Unity service class and list its dependencies. Port dependencies FIRST, then the service, then the feature entry points. If Unity's `OpenRecent()` reads from `UserPrefs`, port `UserPrefs` before porting `OpenRecent()`.

### FM-16: Stale Stubs Masking Missing Functionality
**Pattern:** A catch-all match arm logs "not yet wired" for 5 actions, but 4 of them ARE wired elsewhere. The stub silently claims `OpenRecent` is "not yet wired" alongside actions that work — making it impossible to tell which ones are genuinely missing without reading two files.
**Why it fails:** Stale code obscures actual gaps by mixing done and not-done in one bucket.
**Fix:** When wiring an action in the intercept loop (`app.rs`), immediately remove or update any corresponding stub. Never leave a "not yet wired" log message on an action that IS wired. Match arms for unimplemented actions should have `todo!("reason")` or `unimplemented!()`, not silent logging.

---

## PORTING STRATEGY — SERVICES AS UNITS

This section addresses HOW to approach porting at the module level, not just the file level.

### Rule 1: Port Services as Whole Units

When Unity has a service class (e.g., `ProjectIOService`, `EditingService`, `DialogPathMemory`, `VideoLibrary`), port the ENTIRE service as a Rust module/struct first. Do NOT scatter its methods inline across `app.rs` or event handlers.

The service boundary exists because multiple code paths converge through it. Breaking that boundary creates copy-paste DRY violations that immediately diverge.

### Rule 2: Dependency-First Ordering

Before porting feature methods, read the Unity service class and list its dependencies:
- What other services does it call?
- What infrastructure does it need? (persistence, file dialogs, user prefs)
- What shared state does it read/write?

Port dependencies FIRST, then the service, then the feature entry points that call the service.

### Rule 3: Single Entry Point for Each Concern

If Unity routes all project loading through `OpenProjectFromPath()`, Rust routes all project loading through `project_io.open_project_from_path()`. No exceptions. File drop handlers, menu actions, recent file clicks, CLI arguments — all call the same method.

### Rule 4: Stub Hygiene

- When an action gets wired: remove its "not yet wired" stub immediately
- Use `todo!("description")` for genuinely unimplemented paths (crashes loudly)
- Use `log::warn!("not yet implemented: {}", action)` ONLY for paths that should degrade gracefully
- Never mix implemented and unimplemented actions in the same catch-all match arm

### Rule 5: Audit Before Porting a Feature

Before implementing any feature that touches multiple code paths, answer:
1. What Unity service class owns this logic?
2. Does the Rust port have that service as a unit?
3. What infrastructure does it depend on? Is that ported?
4. Are there existing inline implementations that should be consolidated?

If the service doesn't exist in Rust yet, port it as a whole unit FIRST.

---

## UNITY SOURCE LOCATIONS

The Unity project is at: `/Users/peterkiemann/MANIFOLD - Render Engine/`

```
Assets/Scripts/Data/                        → Data models (Project, Timeline, Layer, Clip, Effects)
Assets/Scripts/Data/IEffectContainer.cs     → Shared effect container interface
Assets/Scripts/Data/IParamSource.cs         → Parameter source abstraction
Assets/Scripts/Data/EffectDefinitionRegistry.cs
Assets/Scripts/Data/GeneratorDefinitionRegistry.cs
Assets/Scripts/Editing/                     → Commands, UndoRedoManager
Assets/Scripts/UI/Timeline/EditingService.cs → Mutation gateway (plain C#, not MonoBehaviour)
Assets/Scripts/Playback/                    → PlaybackEngine, ClipScheduler
Assets/Scripts/Playback/IClipRenderer.cs    → Clip renderer interface
Assets/Scripts/Playback/ILiveClipHost.cs    → Live recording host interface
Assets/Scripts/Playback/Generators/         → ALL generator implementations
Assets/Scripts/Playback/Generators/IGenerator.cs
Assets/Scripts/Playback/Generators/ShaderGeneratorBase.cs
Assets/Scripts/Playback/Generators/LineGeneratorBase.cs
Assets/Scripts/Playback/Generators/StatefulShaderGeneratorBase.cs
Assets/Scripts/Playback/Generators/ComputeVolumeGeneratorBase.cs
Assets/Scripts/Playback/Generators/ComputeParticleGeneratorBase.cs
Assets/Scripts/Compositing/                 → CompositorStack, Effects
Assets/Scripts/Compositing/Effects/IPostProcessEffect.cs
Assets/Scripts/Compositing/Effects/IStatefulEffect.cs
Assets/Scripts/Compositing/Effects/SimpleBlitEffect.cs
Assets/Scripts/UI/Timeline/                 → WorkspaceController, InputHandler
Assets/Scripts/UI/Timeline/Core/            → UIState, UIConstants, CoordinateMapper
Assets/Scripts/UI/Timeline/IInspectorPanel.cs
Assets/Scripts/UI/Bitmap/                   → UIInputSystem, UIBitmapRoot, panels
Assets/Scripts/Sync/                        → ISyncSource, LinkSync, MidiClockSync, OscSync
Assets/Scripts/Export/                      → VideoExporter, ProjectArchive
Assets/Scripts/LED/                         → IExternalOutput, ArtNetOutput
Assets/Shaders/                             → ALL HLSL shaders
Assets/Resources/Compute/                   → ALL compute shaders
Assets/Tests/EditMode/                      → Tests
```

---

## UNITY → RUST CRATE MAPPING

| Unity Directory | Rust Crate | What It Contains |
|---|---|---|
| `Assets/Scripts/Data/` | `manifold-core` | Project, Timeline, Layer, TimelineClip, EffectInstance, EffectGroup, enums, registries |
| `Assets/Scripts/Editing/` + `EditingService.cs` | `manifold-editing` | Commands, UndoRedoManager, EditingService |
| `Assets/Scripts/Playback/` | `manifold-playback` | PlaybackEngine, ClipScheduler, LiveClipManager, SyncArbiter |
| `Assets/Scripts/Sync/` | `manifold-playback` | ISyncSource impls (LinkSync, MidiClockSync, OscSync) |
| `Assets/Scripts/Compositing/` | `manifold-renderer` | CompositorStack, effect chain, blend materials |
| `Assets/Scripts/Compositing/Effects/` | `manifold-renderer` (effects/) | IPostProcessEffect impls (BloomFX, FeedbackFX, etc.) |
| `Assets/Scripts/Playback/Generators/` | `manifold-renderer` (generators/) | IGenerator impls (all generator types) |
| `Assets/Shaders/` | `manifold-renderer` (*.wgsl) | All HLSL → WGSL translations |
| `Assets/Resources/Compute/` | `manifold-renderer` (*.wgsl) | All compute shader translations |
| `Assets/Scripts/UI/Timeline/` + `UI/Bitmap/` | `manifold-ui` | UIState, CoordinateMapper, panels, input, interaction |
| `Assets/Scripts/Export/` | `manifold-io` | Project serialization, save/load, migration |
| `WorkspaceController.cs` + `PlaybackController.cs` | `manifold-app` | Application entry, UIRoot, UIBridge (winit equivalents) |

---

## KEY INTERFACES → TRAITS (Must Exist in Rust)

These Unity interfaces MUST have Rust trait equivalents with the SAME method surface:

| Unity Interface | Rust Trait | Crate | Key Methods |
|---|---|---|---|
| `IClipRenderer` | `ClipRenderer` | `manifold-playback` | `can_handle`, `start_clip`, `stop_clip`, `get_texture`, `is_clip_ready`, `pre_render`, `resize`, `release_all` |
| `IPostProcessEffect` | `PostProcessEffect` | `manifold-renderer` | `effect_type`, `initialize`, `apply`, `clear_state`, `resize`, `cleanup` |
| `IStatefulEffect` | `StatefulEffect: PostProcessEffect` | `manifold-renderer` | `clear_state_for_owner`, `cleanup_owner`, `cleanup_all_owners` |
| `IGenerator` | `Generator` | `manifold-renderer` | `generator_type`, `is_line_based`, `initialize`, `render`, `resize`, `cleanup` |
| `IEffectContainer` | `EffectContainer` | `manifold-core` | `effects()`, `effect_groups()`, `has_modular_effects()`, `envelopes()` |
| `IParamSource` | `ParamSource` | `manifold-core` | `param_count`, `get_param`, `set_param`, `get_base_param`, `set_base_param`, `get_param_def` |
| `ISyncSource` | `SyncSource` | `manifold-playback` | `is_enabled`, `display_name`, `enable`, `disable`, `toggle` |
| `ILiveClipHost` | `LiveClipHost` | `manifold-playback` | `current_project`, `current_time`, `current_beat`, `is_recording`, `mark_sync_dirty`, `record_command` |
| `ICommand` | `Command` | `manifold-editing` | `description`, `execute`, `undo` |
| `IInspectorPanel` | `InspectorPanel` | `manifold-ui` | `is_active`, `panel_height`, `show_empty`, `refresh_after_undo`, `sync_from_data` |
| `IExternalOutput` | `ExternalOutput` | (future) | `initialize`, `process_frame`, `blackout`, `shutdown` |

---

## BASE CLASSES → TRAIT + SHARED STATE (Must Exist in Rust)

| Unity Base Class | Rust Pattern | Key Responsibilities |
|---|---|---|
| `ShaderGeneratorBase` | `trait ShaderGenerator` + helper fns | Material lifecycle, standard uniforms (_Time2, _Beat, _AspectRatio, etc.) |
| `LineGeneratorBase` | `trait LineGenerator` + line mesh utils | Line drawing with LineMeshUtil |
| `StatefulShaderGeneratorBase` | Extends `ShaderGenerator` + `StatefulState` struct | Ping-pong RT pair for temporal feedback |
| `ComputeVolumeGeneratorBase` | `trait VolumeGenerator` + volume state | 3D compute + raymarch display |
| `ComputeParticleGeneratorBase` | `trait ParticleGenerator` + particle state | Compute particles + scatter splat |
| `SimpleBlitEffect` | `trait SimpleBlitEffect` + blit helper | Single-pass shader blit for effects |

---

## INVARIANTS (from Unity — must hold in Rust)

- Primary time model is **beats** (`start_beat`, `duration_beats`)
- Seconds ONLY for: `in_point`, player time, delta_time, OSC, export
- `sync_clips_to_time()` is the SOLE idempotent authority for playback state
- `EditingService` is the SOLE mutation gateway — no direct model writes from UI
- All data mutations flow through `EditingService` → `UndoRedoManager` → `Command`
- Undo stack capped at 200 entries (oldest discarded)
- Overlap is a write-time invariant on Layer (`enforce_non_overlap()`)
- Selection is region-based: `{ start_beat, end_beat, start_layer, end_layer }`
- Clipboard stores relative patterns, not clip identity
- Paste target = click position (beat + layer), not playhead
- `DataVersion` counter incremented on every mutation; UI polls to detect changes
- Phantom clips: created on NoteOn, committed on NoteOff only
- NoteOn auto-commits existing phantom on same layer (Minis delivers NoteOn BEFORE NoteOff)
- Time guard: ignore NoteOff within 5ms of NoteOn
- Channel filtering: only process NoteOff from same channel as NoteOn
- No per-frame allocations on hot paths
- Pre-allocated scratch buffers for iteration during mutation
- Static comparison delegates for sorting (no per-frame closures)

## WGPU / METAL CONSTRAINTS

**CRITICAL PRINCIPLE:** Metal/wgpu constraints are RUNTIME problems, NOT source-code compromises. The Rust source must match Unity's values, formats, and limits exactly. If Metal can't handle something at runtime, add a runtime fallback/clamp — but the source of truth stays identical to Unity.

### Workgroup Size
- `max_compute_invocations_per_workgroup` = 256 on Metal → `@workgroup_size(4,4,4)` for 3D volumes
- 3D textures: dispatch `(res+7)/8` per axis

### Texture Format Rules
- **First, match Unity's format.** `RFloat` → `R32Float`. `RGFloat` → `Rg32Float`. `ARGBFloat` → `Rgba32Float`. `ARGBHalf` → `Rgba16Float`.
- **Then handle Metal limitations as runtime fallbacks:**
  - `R16Float` does NOT support `STORAGE_BINDING` on Metal
  - `R32Float` is NOT filterable on Metal (can't use `textureSample`)
  - If Unity uses `RFloat` for storage+sample, you need a runtime format selection or separate read/write textures
  - Document the workaround in a comment referencing the Unity format
- Compute shaders: `textureLoad` (no sampler). Fragment shaders: `textureSample` (needs filtering)
- `Float { filterable: true }` for `textureSample` sources; `Float { filterable: false }` for `textureLoad`

### Buffer Sizes
- Match Unity's buffer sizes exactly. If Metal's 128MB limit is a problem, add a runtime clamp, not a source-code change
- `max_storage_buffer_binding_size` = 128MB on Metal — relevant for large particle buffers

### Pipeline / API
- Uniform structs MUST be 16-byte aligned (pad with `_pad` fields to vec4 boundaries)
- `immediate_size: 0` required on `PipelineLayoutDescriptor` (wgpu 28+)
- `depth_slice: None` required on `RenderPassColorAttachment`
- `textureSampleLevel` required for 3D texture sampling in fragment shaders
- 3D sampler needs `address_mode_w` set (Repeat or ClampToEdge depending on Unity's usage)
- Separate render pipelines needed per output format even with same shader
- Separable 3D blur: after 3 passes (X,Y,Z) result is in the "temp" volume (odd number of swaps)

## RUST CRATE STRUCTURE

```
crates/manifold-core/       → Data models, types, enums, registries (pure, no GPU)
crates/manifold-editing/    → Commands, UndoRedoManager, EditingService
crates/manifold-playback/   → PlaybackEngine, ClipScheduler, SyncArbiter, LiveClipManager
crates/manifold-io/         → Project loading (V1 JSON + V2 ZIP), saving, migration
crates/manifold-renderer/   → wgpu GPU: compositor, effects, generators, shaders
crates/manifold-ui/         → Custom bitmap UI: tree, panels, input, layout, state
crates/manifold-app/        → winit entry point, Application, UIRoot, UIBridge
```

**Dependency direction:** `core` ← `editing`, `playback`, `renderer`, `ui`, `io` ← `app`
No backwards dependencies. Core is pure.

## DEBUGGING RULE — LOGS FIRST, NOT STATIC ANALYSIS

When a bug involves runtime state (callbacks, event ordering, null refs, timing):
1. Add targeted `println!`/`eprintln!` calls (with context) after at most 1-2 minutes of code reading
2. Ask the user to reproduce and paste output
3. Read the logs, identify root cause, fix it

Static analysis is for compile errors and obvious logic bugs. For anything involving runtime dispatch, event ordering, or "why does X fire twice" — instrument and observe.

## AGENTS — USE SPARINGLY

- Write code directly in the main context by default
- Only spawn an agent when the task is genuinely large and isolated
- TELL THE USER if you decide to spawn an agent and why

## COMMIT AND PUSH

YOU MUST COMMIT AND PUSH CODE CHANGES TO THE RELEVANT REPO AFTER COMPLETING FEATURES OR FIXES.

## PERFORMANCE INVARIANTS

- No per-frame allocations on hot paths (engine tick, sync, rendering)
- No closures in sort comparisons on hot paths — use static/cached comparison functions
- Pre-allocated buffers for safe HashMap iteration during removal
- Dirty-checking: cache previous values, only update UI on change

## CODE STYLE

- Snake_case for functions and variables (mechanical conversion from Unity's camelCase)
- PascalCase for types and traits (same as Unity)
- `SCREAMING_SNAKE_CASE` for constants
- Prefer `pub(crate)` over `pub` for internal API
- `#[derive(Clone, Debug, Serialize, Deserialize)]` on data models
- `serde(rename = "camelCase")` on serialized fields to match Unity JSON format
- Comments only where logic isn't self-evident — same standard as Unity
