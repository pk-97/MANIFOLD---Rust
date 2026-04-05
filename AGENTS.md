# MANIFOLD — Codex Agent Contract

Read this file completely before any action. Every rule is load-bearing.

## WHAT MANIFOLD IS

A Visual DAW with live performance capabilities. Users create, arrange, and compose video and generative visual content in beats, bars, and arrangements, then perform live. It bridges the deliberate studio workflow of a DAW (Ableton) with the real-time visual performance of a VJ tool (Resolume).

The Rust codebase is the complete, authoritative implementation. The Unity codebase at `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived as historical reference only — do not consult it for new development.

## CRATE STRUCTURE

| Crate | Role | Key Types |
|---|---|---|
| `manifold-core` | Data models, types, registries (pure, no GPU) | `Project`, `Timeline`, `Layer`, `TimelineClip`, `ClipId`, `LayerId`, `EffectGroupId` |
| `manifold-editing` | Commands, undo/redo, EditingService | `Command` trait, `EditingService`, `UndoRedoManager` |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC | `PlaybackEngine`, `ClipScheduler`, `SyncArbiter`, `ClipRenderer` trait |
| `manifold-gpu` | Native Metal GPU backend (`metal` crate). Zero wgpu. | `GpuDevice`, `GpuEncoder`, `GpuTexture`, `GpuBuffer`, `GpuComputePipeline`, `GpuRenderPipeline`, `TexturePool` |
| `manifold-renderer` | Compositor, effects, generators (uses `manifold-gpu`) | `Compositor` trait, `PostProcessEffect` trait, `Generator` trait |
| `manifold-media` | Audio/video decoding, Metal-accelerated encoding, export | `ExportSession`, `MetalEncoder`, `DecodeScheduler` |
| `manifold-ui` | Custom bitmap UI: tree, panels, input | `UIState`, `CoordinateMapper`, `UITree`, 17 panel types |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) | `loader`, `saver`, `migrate` |
| `manifold-native` | Native plugin FFI (depth estimation, blob detection) | `DepthEstimator`, `BlobDetector` |
| `manifold-profiler` | Performance profiling and instrumentation | Frame timing, GPU timing |
| `manifold-led` | DMX/Art-Net LED output | LED mapping, blit operations |
| `manifold-app` | winit entry, Application, UIRoot, UIBridge | `Application`, `ContentThread`, `ContentPipeline` |

**Dependency direction:** `core` <- `gpu` <- `editing`, `playback`, `renderer`, `ui`, `io` <- `app`. No backwards dependencies. Core is pure. GPU is Metal-only on content thread.

## ARCHITECTURE

### Two-Thread Model

- **Content thread** (owns all mutable state): `PlaybackEngine`, `EditingService`, `ContentPipeline`. Runs at project FPS (default 60).
- **UI thread** (winit event loop): Renders UI, handles input, presents GPU output.
- Communication: `ContentCommand` (UI->Content) and `ContentState` (Content->UI) via crossbeam channels.
- GPU output: macOS uses IOSurface zero-copy triple-buffer with atomic front_index.

### Thread Boundary

| Direction | Type | Channel | Capacity |
|---|---|---|---|
| UI->Content | `ContentCommand` | crossbeam bounded | 64 |
| Content->UI | `ContentState` | crossbeam bounded | 4 |
| GPU output | IOSurface | Triple-buffer | 3 |
| OSC background->Content | `PendingWrite` | `Arc<Mutex<Vec>>` | unbounded |

- **Project is owned exclusively by the content thread.** UI gets `Arc<Project>` snapshots via `ContentState` only when `data_version` changes.
- **All project mutations** go through `ContentCommand::Execute(Box<dyn Command>)` or `ContentCommand::MutateProject(Box<dyn FnOnce(&mut Project)>)`.
- **NEVER** create new `Arc<Mutex<>>` or `Arc<RwLock<>>` shared state without explicit approval.

### Current Patterns

- **Edition 2024** — all crates
- **Typed IDs** — `ClipId`, `LayerId`, `EffectGroupId` newtypes wrapping String (`#[serde(transparent)]`)
- **Typed time** — `Beats(pub f64)`, `Seconds(pub f64)`, `Bpm(pub f32)` newtypes in `manifold-core`. All timing function signatures use these types. Extract to `f32` only at GPU uniform boundaries (`.as_f32()`), serialized `f32` fields, or legacy f32 APIs. Never use raw `f32`/`f64` for time in function signatures.
- **AHashMap** — on all hot-path maps (clip lookups, effect state, scheduler)
- **parking_lot** — `RwLock`/`Mutex` replacing std (no poisoning, smaller, faster)
- **Lock-free MIDI** — `AtomicClockState` packed `AtomicU64` CAS for real-time-safe MIDI clock callbacks
- **Per-owner effect cleanup** — `TickResult::stopped_clips` -> `ContentPipeline::cleanup_stopped_clips()` -> `EffectRegistry` -> stateful effects
- **Native Metal GPU** — all threads use `manifold-gpu` crate (`metal` crate directly, zero wgpu)
- **All-compute effect pipeline** — all effects use compute dispatches via `ComputeBlitHelper` / `ComputeDualBlitHelper`, eliminating TBDR tile load/store overhead from render passes
- **Async compute** — independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- **Texture pool** — frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- **Function constants** — specialized Metal pipelines per effect mode (bloom 4, compositor 13 blend modes, etc.)
- **MTLBinaryArchive** — compiled pipeline cache on disk, near-instant startup on subsequent launches
- **`set_fast_math_enabled(true)`** — globally on all Metal pipeline compile options

## BEHAVIORAL INVARIANTS

These invariants govern how the system works. Violating them causes subtle, hard-to-diagnose bugs.

- Primary time model is **beats** (`start_beat`, `duration_beats` as `Beats`); `Seconds` only for `in_point`, player time, delta_time, OSC, export
- `sync_clips_to_time()` is the SOLE idempotent authority for playback state
- `EditingService` is the SOLE mutation gateway — no direct model writes from UI
- All mutations: `EditingService` -> `UndoRedoManager` -> `Command`. Undo stack capped at 200.
- Overlap is a write-time invariant on Layer (`enforce_non_overlap()`)
- Selection is region-based: `{ start_beat, end_beat, start_layer, end_layer }`
- Phantom clips: created on NoteOn, committed on NoteOff only
- NoteOn auto-commits existing phantom on same layer
- Time guard: ignore NoteOff within 5ms of NoteOn
- Channel filtering: only process NoteOff from same channel as NoteOn

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

- `#[serde(rename_all = "camelCase")]` on all serialized structs (project file compatibility)
- `#[serde(transparent)]` on typed IDs (`ClipId`, `LayerId`, `EffectGroupId`)
- `#[serde(skip)]` for runtime-only fields
- Field names in JSON must match camelCase format

### GPU Constraints — Native Metal

All threads use `manifold-gpu` with the `metal` crate directly. **Zero wgpu anywhere in the codebase.** NEVER introduce wgpu dependencies.

- `max_compute_invocations_per_workgroup` = 256 -> `@workgroup_size(16,16)` for 2D, `@workgroup_size(4,4,4)` for 3D
- `R32Float` NOT filterable — use `Rgba16Float` if `textureSample` needed
- `R16Float` does NOT support `STORAGE_BINDING`
- Uniform structs: 16-byte aligned, `_pad` fields, `#[repr(C)]`, field order matches WGSL
- `textureSampleLevel` required for 3D textures in fragment shaders
- `set_fast_math_enabled(true)` is set globally on all pipeline compile options

### Build & Validation

- `cargo clippy --workspace -- -D warnings` (before commit)
- `cargo test --workspace` (before commit)
- **manifold-gpu** (native `metal` crate on macOS, zero wgpu), winit 0.30, Edition 2024, Rust stable
- `clippy.toml`: `too-many-arguments-threshold = 20`
- `rustfmt.toml`: `max_width = 100`, `use_field_init_shorthand = true`

## HOW TO ADD A NEW EFFECT

Touch exactly 7 locations:

1. **`manifold-core/src/effect_type_id.rs`** — Add `pub const MY_EFFECT: Self = Self(Cow::Borrowed("MyEffect"));`
2. **`manifold-core/src/effect_type_registry.rs`** — Add to `build_registry()` vec
3. **`manifold-core/src/effect_definition_registry.rs`** — Add `EffectDef` in `build_definitions()`
4. **`manifold-core/src/effect_category_registry.rs`** — (Optional) Add category if not POST_PROCESS
5. **`manifold-renderer/src/effects/my_effect.rs`** — NEW FILE: implement `PostProcessEffect` trait
6. **`manifold-renderer/src/effects/mod.rs`** — Add `pub mod my_effect;`
7. **`manifold-renderer/src/effect_registry.rs`** — Add `Box::new(MyEffectFX::new(device))` in `EffectRegistry::new()`

## HOW TO ADD A NEW GENERATOR

Touch exactly 6 locations:

1. **`manifold-core/src/generator_type_id.rs`** — Add `pub const MY_GEN: Self = Self(Cow::Borrowed("MyGen"));`
2. **`manifold-core/src/generator_type_registry.rs`** — Add to `build_registry()` vec
3. **`manifold-core/src/generator_definition_registry.rs`** — Add `GeneratorDef` in `build_definitions()`
4. **`manifold-renderer/src/generators/my_gen.rs`** — NEW FILE: implement `Generator` trait
5. **`manifold-renderer/src/generators/mod.rs`** — Add `pub mod my_gen;`
6. **`manifold-renderer/src/generators/registry.rs`** — Add to `prewarm_all()` array AND `create()` if-else chain

## REFERENCE DOCS

- `docs/MANIFOLD_GPU_ARCHITECTURE.md` — Full GPU architecture details
- `docs/VSYNC_AND_FRAME_PACING.md` — VSync, CVDisplayLink, frame pacing

## DEBUGGING

When a bug involves runtime state (callbacks, event ordering, timing):
1. Add targeted `println!`/`eprintln!` after at most 1-2 minutes of code reading
2. Ask user to reproduce and paste output
3. Read logs, identify root cause, fix

## COMMIT AND PUSH

Commit and push code changes after completing features or fixes.
