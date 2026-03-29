# MANIFOLD — Agent Contract

YOU MUST read this file completely before any action. Every rule is load-bearing.

## WHAT MANIFOLD IS

A Visual DAW with live performance capabilities. Users create, arrange, and compose video and generative visual content in beats, bars, and arrangements, then perform live. It bridges the deliberate studio workflow of a DAW (Ableton) with the real-time visual performance of a VJ tool (Resolume). Both are equally important.

The Rust codebase is the authoritative implementation. The Unity codebase at `/Users/peterkiemann/MANIFOLD - Render Engine/` serves as the behavioral specification for features originally ported from it. For remaining parity gaps, Unity source is the source of truth. For new features and improvements, Rust is the primary codebase.

## CRATE STRUCTURE

| Crate | Role | Key Types |
|---|---|---|
| `manifold-core` | Data models, types, registries (pure, no GPU) | `Project`, `Timeline`, `Layer`, `TimelineClip`, `ClipId`, `LayerId`, `EffectGroupId` |
| `manifold-editing` | Commands, undo/redo, EditingService | `Command` trait, `EditingService`, `UndoRedoManager` |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC | `PlaybackEngine`, `ClipScheduler`, `SyncArbiter`, `ClipRenderer` trait |
| `manifold-gpu` | Native Metal GPU backend (`metal` crate). Zero wgpu. | `GpuDevice`, `GpuEncoder`, `GpuTexture`, `GpuBuffer`, `GpuComputePipeline`, `GpuRenderPipeline`, `TexturePool` |
| `manifold-renderer` | Compositor, effects, generators (uses `manifold-gpu`) | `Compositor` trait, `PostProcessEffect` trait, `Generator` trait |
| `manifold-ui` | Custom bitmap UI: tree, panels, input | `UIState`, `CoordinateMapper`, `UITree`, 17 panel types |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) | `loader`, `saver`, `migrate` |
| `manifold-native` | Native plugin FFI (depth estimation) | `DepthEstimator`, `BlobDetector` |
| `manifold-app` | winit entry, Application, UIRoot, UIBridge | `Application`, `ContentThread`, `ContentPipeline` |

**Dependency direction:** `core` ← `gpu` ← `editing`, `playback`, `renderer`, `ui`, `io` ← `app`. No backwards dependencies. Core is pure. GPU is Metal-only on content thread.

## ARCHITECTURE

### Two-Thread Model

- **Content thread** (owns all mutable state): `PlaybackEngine`, `EditingService`, `ContentPipeline`. Runs at project FPS (default 60).
- **UI thread** (winit event loop): Renders UI, handles input, presents GPU output.
- Communication: `ContentCommand` (UI→Content) and `ContentState` (Content→UI) via crossbeam channels.
- GPU output: macOS uses IOSurface zero-copy; other platforms use double-buffered texture swap.

### Current Patterns

- **Edition 2024** — all crates
- **Typed IDs** — `ClipId`, `LayerId`, `EffectGroupId` newtypes wrapping String (`#[serde(transparent)]`)
- **Typed time** — `Beats(pub f64)`, `Seconds(pub f64)`, `Bpm(pub f32)` newtypes in `manifold-core`. All timing function signatures use these types. Extract to `f32` only at GPU uniform boundaries (`.as_f32()`), serialized `f32` fields, or legacy f32 APIs. Never use raw `f32`/`f64` for time in function signatures.
- **AHashMap** — on all hot-path maps (clip lookups, effect state, scheduler)
- **parking_lot** — `RwLock`/`Mutex` replacing std (no poisoning, smaller, faster)
- **Lock-free MIDI** — `AtomicClockState` packed `AtomicU64` CAS for real-time-safe MIDI clock callbacks
- **Per-owner effect cleanup** — `TickResult::stopped_clips` → `ContentPipeline::cleanup_stopped_clips()` → `EffectRegistry` → stateful effects
- **Native Metal GPU** — content thread uses `manifold-gpu` crate (`metal` crate directly, zero wgpu). UI thread uses wgpu on separate device.
- **Hybrid render pipeline** — single-pass effects use fragment shaders (TBDR tile memory), multi-pass/stateful effects use compute dispatches
- **Async compute** — independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- **Texture pool** — frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- **Function constants** — specialized Metal pipelines per effect mode (bloom 4, compositor 13 blend modes, etc.)
- **MTLBinaryArchive** — compiled pipeline cache on disk, near-instant startup on subsequent launches
- **`set_fast_math_enabled(true)`** — globally on all Metal pipeline compile options

### Key Module Splits

- `manifold-app/src/ui_bridge/` — 7 modules: mod, transport, editing, inspector, layer, project, state_sync
- `manifold-app/src/` — `app.rs` + `app_render.rs` + `app_lifecycle.rs`
- `manifold-renderer/src/effects/` — 26 effect impls + `fragment_blit_helper` (single-pass) + `compute_blit_helper` (multi-pass) + `compute_dual_blit_helper`
- `manifold-renderer/src/generators/` — 20+ generator impls + shared infrastructure

---

## DEVELOPMENT RULES

### Performance Invariants

- **No per-frame allocations on hot paths** (engine tick, sync, rendering)
- Pre-allocated scratch buffers (`stopped_this_tick`, `timeline_active_scratch`, scheduler internals)
- `AHashMap` for all clip/effect/generator ID lookups
- Static comparison functions for sorting (no per-frame closures)
- Dirty-checking: cache previous values, only update UI on change (`DataVersion` counter)

### Code Style

- `snake_case` for functions/variables, `PascalCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants
- `pub(crate)` over `pub` for internal API
- `#[derive(Clone, Debug, Serialize, Deserialize)]` on data models
- Comments only where logic isn't self-evident
- `unwrap()`/`expect("reason")` for impossible states; `Option<T>` for nullable
- Do NOT over-engineer error handling beyond what the logic requires

### Serialization Convention

- `#[serde(rename_all = "camelCase")]` on all serialized structs (Unity JSON compatibility)
- `#[serde(transparent)]` on typed IDs (`ClipId`, `LayerId`, `EffectGroupId`)
- `#[serde(skip)]` for runtime-only fields
- Field names in JSON must match Unity's camelCase format — getting this wrong silently breaks project loading

### Uniform Struct Alignment

- Uniform structs MUST be 16-byte aligned (pad with `_pad` fields to vec4 boundaries)
- WGSL `vec3<f32>` has 16-byte alignment in storage buffers — Rust structs MUST pad to match
- Field order in Rust struct must match field order in WGSL struct

### Git Workflow

1. `cargo clippy --workspace -- -D warnings` (before commit)
2. `cargo test --workspace` (before commit)
3. Commit to main with descriptive message
4. Push
5. CI confirms (GitHub Actions: check, clippy, test, fmt on macos-latest)

### Build

- **manifold-gpu** (native `metal` crate on macOS), **wgpu 28** (UI thread only), winit 0.30, Edition 2024, Rust stable
- `clippy.toml`: `too-many-arguments-threshold = 20`
- `rustfmt.toml`: `max_width = 100`, `use_field_init_shorthand = true`
- Release: `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`, `panic = "abort"`
- Symphonia audio codecs: `opt-level = 2` in dev profile (10-50x faster than debug)

---

## COMMIT AND PUSH

YOU MUST COMMIT AND PUSH CODE CHANGES TO THE RELEVANT REPO AFTER COMPLETING FEATURES OR FIXES.

---

## GPU ARCHITECTURE — NATIVE METAL

The content thread uses `manifold-gpu` with the `metal` crate directly. **Zero wgpu on the content thread.** wgpu is only used on the UI thread (separate device). See `docs/MANIFOLD_GPU_ARCHITECTURE.md` for full details.

### Content Thread GPU Types
- ALL GPU types from `manifold-gpu`: `GpuDevice`, `GpuEncoder`, `GpuTexture`, `GpuBuffer`, `GpuComputePipeline`, `GpuRenderPipeline`
- **NEVER** use `wgpu::*` types on the content thread
- UI thread files (`ui_renderer.rs`, `tonemap_blit.rs`, `layer_bitmap_gpu.rs`, `app_render.rs`) use wgpu — don't migrate these

### Hybrid Render Pipeline
- **Single-pass effects → fragment shaders** via `FragmentBlitHelper` — benefits from TBDR tile memory on Apple Silicon
- **Multi-pass/stateful effects → compute dispatches** via `ComputeBlitHelper` — avoids per-pass tile load/store
- **Rule:** If `num_passes == 1` AND effect is not stateful → use fragment shader. Otherwise → compute.

### Metal Constraints
- `max_compute_invocations_per_workgroup` = 256 → `@workgroup_size(16,16)` for 2D, `@workgroup_size(4,4,4)` for 3D
- `R32Float` NOT filterable — use `Rgba16Float` if `textureSample` needed
- `R16Float` does NOT support `STORAGE_BINDING`
- Match Unity's texture formats first, substitute only when Metal requires it
- Uniform structs: 16-byte aligned, `_pad` fields, `#[repr(C)]`, field order matches WGSL
- `textureSampleLevel` required for 3D textures in fragment shaders
- `textureSample` (implicit LOD) preferred in fragment shaders — more efficient than `textureSampleLevel`
- `set_fast_math_enabled(true)` is set globally on all pipeline compile options
- Separable 3D blur: after 3 passes (X,Y,Z) result is in the "temp" volume (odd number of swaps)

### Async Compute
- Independent layers generate in parallel `MTLCommandBuffer`s
- Compositor `MTLCommandBuffer` waits on all layer `MTLEvent` signals before blending
- Single-layer fast path skips multi-command-buffer overhead

### Texture Pool
- Frame-stamped recycling: textures released to pool, only reused after N frames (N = frames in flight)
- Zero per-frame allocations after 3-frame warmup
- Persistent state textures (feedback, stylized_feedback) are NOT pooled

### Resolution Scaling
- Controlled by `project.settings.upscale_mode` (`UpscaleMode` enum). **Default is `Native`.**
- `Native` (default) → all generators render at full resolution (`scaling_enabled = false`)
- `MetalFxSpatial` / `MpsLanczos` → generators with `internal_resolution_scale() < 1.0` render at reduced resolution, upscaled via MetalFX Spatial or MPS Lanczos
- Four generators have sub-1.0 overrides (only active in non-Native mode): `FluidSimulation` (0.5×), `FluidSimulation3D` (0.5×), `Mycelium` (0.5×), `ParametricSurface` (0.75×)

---

## UNITY PARITY (for remaining gaps)

For features ported from Unity, the Unity source is the behavioral specification. When closing parity gaps, this workflow is mandatory.

### Porting Workflow

1. **READ** the Unity .cs source completely — HALT if you haven't read it
2. **MAP** every field → Rust type, method → signature, interface → trait, dependency → crate
3. **TRANSLATE** line by line — same logic, same edge cases, same order, same constants
4. **SELF-AUDIT** — did you skip methods? simplify logic? change signatures? add abstractions?
5. **VERIFY** value-level parity — every constant, format, math op, param index matches exactly
6. **UPDATE** `docs/parity_tracker.json` + `docs/PORT_STATUS.md`

### Failure Modes

| ID | Name | Rule |
|---|---|---|
| FM-1 | Synthesizing from docs | ONLY translate from .cs source, never from descriptions |
| FM-2 | Approximating | Line-by-line translation, not "roughly the same thing" |
| FM-3 | Flattening architecture | If Unity keeps classes separate, Rust keeps them separate |
| FM-4 | Rustifying semantics | Match Unity's mutation/ownership model, don't functionalize |
| FM-5 | Over-engineering errors | If Unity crashes, use `unwrap()`. If null, use `Option`. |
| FM-6 | Missing edge cases | Every `if`, early return, guard, bounds check → preserved |
| FM-7 | Inventing state | Only add state Unity has. Document Rust-specific additions. |
| FM-8 | Wrong abstraction level | Concrete impls, not generic frameworks |
| FM-9 | Hallucinated constraints | NEVER invent platform limits. Use Unity's exact values. |
| FM-10 | Texture format substitution | `RFloat`→`R32Float`, NOT `Rgba16Float` (see Metal exception) |
| FM-11 | Changing constants | Every constant, limit, threshold must match Unity EXACTLY |
| FM-12 | Math operation drift | `RoundToInt`→`.round()`, `Lerp` clamps t, `Repeat`≠modulo |
| FM-13 | Param value drift | Match every param index, uniform name, default from registries |
| FM-14 | Scattering services | Port services as WHOLE UNITS, not scattered inline |
| FM-15 | Missing infrastructure | Port dependencies BEFORE features that use them |
| FM-16 | Stale stubs | Remove "not yet wired" stubs when actions get wired |

### Effect / Generator Porting (Highest Risk)

1. Read `SetUniforms()` / `Apply()` — exact param-to-shader mapping
2. Read the HLSL shader — translate line by line to WGSL (same math, same names)
3. Count passes: if Unity has 3, Rust has 3. NEVER approximate multi-pass as single-pass.
4. Count textures: 2 = `DualTextureBlitHelper`, 1 = `SimpleBlitHelper`
5. Texel sizes from SOURCE texture, not target
6. Discrete params: `.round()` before `as u32`
7. Stateful effects: per-owner `AHashMap<i64, T>`
8. Read the registry entry for param definitions (index, name, min, max, default, format)

### Math Operation Reference

| Unity | Rust | Notes |
|---|---|---|
| `Mathf.RoundToInt(x)` | `x.round() as i32` | NOT truncation |
| `Mathf.FloorToInt(x)` | `x.floor() as i32` | |
| `Mathf.CeilToInt(x)` | `x.ceil() as i32` | |
| `Mathf.Clamp01(x)` | `x.clamp(0.0, 1.0)` | |
| `Mathf.Lerp(a, b, t)` | `a + (b - a) * t.clamp(0.0, 1.0)` | Lerp CLAMPS t |
| `Mathf.LerpUnclamped(a, b, t)` | `a + (b - a) * t` | |
| `Mathf.InverseLerp(a, b, v)` | `((v - a) / (b - a)).clamp(0.0, 1.0)` | |
| `Mathf.Repeat(t, len)` | `t - (t / len).floor() * len` | NOT `t % len` |
| `Mathf.Sign(x)` | check Unity: returns 1 for 0 | NOT 0 for 0 |

### Texture Format Mapping

| Unity | Rust (manifold-gpu / Metal) | Notes |
|---|---|---|
| `RFloat` | `R32Float` | Keep unless sampled via `textureSample` |
| `RGFloat` | `Rg32Float` | Keep unless sampled via `textureSample` |
| `ARGBFloat` | `Rgba32Float` | Keep unless sampled via `textureSample` |
| `ARGBHalf` | `Rgba16Float` | Always fine |
| `RHalf` | `R16Float` | No STORAGE_BINDING on Metal |

### Behavioral Invariants

- Primary time model is **beats** (`start_beat`, `duration_beats` as `Beats`); `Seconds` only for `in_point`, player time, delta_time, OSC, export. `Bpm` for all tempo values. See **Typed time** in Current Patterns.
- `sync_clips_to_time()` is the SOLE idempotent authority for playback state
- `EditingService` is the SOLE mutation gateway — no direct model writes from UI
- All mutations: `EditingService` → `UndoRedoManager` → `Command`. Undo stack capped at 200.
- Overlap is a write-time invariant on Layer (`enforce_non_overlap()`)
- Selection is region-based: `{ start_beat, end_beat, start_layer, end_layer }`
- Phantom clips: created on NoteOn, committed on NoteOff only
- NoteOn auto-commits existing phantom on same layer
- Time guard: ignore NoteOff within 5ms of NoteOn
- Channel filtering: only process NoteOff from same channel as NoteOn

### Unity Source → Rust Crate Mapping

Unity project: `/Users/peterkiemann/MANIFOLD - Render Engine/`

| Unity Directory | Rust Crate |
|---|---|
| `Assets/Scripts/Data/` | `manifold-core` |
| `Assets/Scripts/Editing/` + `EditingService.cs` | `manifold-editing` |
| `Assets/Scripts/Playback/` + `Assets/Scripts/Sync/` | `manifold-playback` |
| `Assets/Scripts/Compositing/` + `Effects/` | `manifold-renderer` |
| `Assets/Scripts/Playback/Generators/` | `manifold-renderer` (generators/) |
| `Assets/Shaders/` + `Assets/Resources/Compute/` | `manifold-renderer` (*.wgsl) |
| `Assets/Scripts/UI/Timeline/` + `UI/Bitmap/` | `manifold-ui` |
| `Assets/Scripts/Export/` | `manifold-io` |
| `WorkspaceController.cs` + `PlaybackController.cs` | `manifold-app` |

Full file-level mapping: `docs/PORT_STATUS.md`
Interface→trait and base class→pattern tables: `docs/TRAIT_AND_PATTERN_MAP.md`

---

## TRACKING

| Doc | Purpose | Mutability |
|---|---|---|
| `docs/parity_tracker.json` | Live status of all 44 gaps (Tiers 0-5) | Update on gap completion |
| `docs/PORT_STATUS.md` | File-level parity tracker | Update on port/verify |
| `docs/KNOWN_DIVERGENCES.md` | Approved intentional divergences | Add when diverging |
| `docs/DEFINITIVE_PARITY_AUDIT.md` | Canonical gap inventory (1310 lines) | FROZEN — DO NOT EDIT |

**Parity gap workflow:** check tracker → read audit section → read Unity source → port → update tracker + PORT_STATUS

---

## AVAILABLE SKILLS

| Skill | Purpose |
|---|---|
| `/rust-port [file]` | Mechanical translation of a Unity file to Rust |
| `/rust-verify [file]` | Compare Rust implementation against Unity source |
| `/pre-port [file]` | Dependency analysis before porting |
| `/audit-parity [files]` | Batch post-port verification |

---

## DEBUGGING

When a bug involves runtime state (callbacks, event ordering, timing):
1. Add targeted `println!`/`eprintln!` after at most 1-2 minutes of code reading
2. Ask user to reproduce and paste output
3. Read logs, identify root cause, fix

Static analysis is for compile errors and obvious logic bugs. For runtime issues — instrument and observe.

## AGENTS

- Write code directly in the main context by default
- Only spawn an agent for genuinely large, isolated tasks
- Tell the user if you decide to spawn an agent and why
