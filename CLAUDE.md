# MANIFOLD — Agent Contract

YOU MUST read this file completely before any action. Every rule is load-bearing.

## WHAT MANIFOLD IS

A Visual DAW with live performance capabilities. Users create, arrange, and compose video and generative visual content in beats, bars, and arrangements, then perform live. It bridges the deliberate studio workflow of a DAW (Ableton) with the real-time visual performance of a VJ tool (Resolume). Both are equally important.

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

**Dependency direction:** `core` ← `gpu` ← `editing`, `playback`, `renderer`, `ui`, `io` ← `app`. No backwards dependencies. Core is pure. GPU is Metal-only on content thread.

## ARCHITECTURE

### Two-Thread Model

- **Content thread** (owns all mutable state): `PlaybackEngine`, `EditingService`, `ContentPipeline`. Runs at project FPS (default 60).
- **UI thread** (winit event loop): Renders UI, handles input, presents GPU output.
- Communication: `ContentCommand` (UI→Content) and `ContentState` (Content→UI) via crossbeam channels.
- GPU output: macOS uses IOSurface zero-copy triple-buffer with atomic front_index.

### Thread Boundary

| Direction | Type | Channel | Capacity | Notes |
|---|---|---|---|---|
| UI→Content | `ContentCommand` | crossbeam bounded | 64 | `try_send`, logs on full |
| Content→UI | `ContentState` | crossbeam bounded | 4 | `try_send`, UI drains all + keeps latest |
| GPU output | IOSurface | Triple-buffer | 3 | Atomic `front_index`, zero-copy kernel memory |
| OSC background→Content | `PendingWrite` | `Arc<Mutex<Vec>>` | unbounded | Only shared mutable state in the system |

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
- **Per-owner effect cleanup** — `TickResult::stopped_clips` → `ContentPipeline::cleanup_stopped_clips()` → `EffectRegistry` → stateful effects
- **Native Metal GPU** — content thread uses `manifold-gpu` crate (`metal` crate directly, zero wgpu). UI thread uses wgpu on separate device.
- **All-compute effect pipeline** — all effects use compute dispatches via `ComputeBlitHelper` / `ComputeDualBlitHelper`, eliminating TBDR tile load/store overhead from render passes
- **Async compute** — independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- **Texture pool** — frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- **Function constants** — specialized Metal pipelines per effect mode (bloom 4, compositor 13 blend modes, etc.)
- **MTLBinaryArchive** — compiled pipeline cache on disk, near-instant startup on subsequent launches
- **`set_fast_math_enabled(true)`** — globally on all Metal pipeline compile options

### Key Module Splits

- `manifold-app/src/ui_bridge/` — 7 modules: mod, transport, editing, inspector, layer, project, state_sync
- `manifold-app/src/` — `app.rs` + `app_render.rs` + `app_lifecycle.rs`
- `manifold-renderer/src/effects/` — 21 effect impls + `compute_blit_helper` + `compute_dual_blit_helper`
- `manifold-renderer/src/generators/` — 13 generator impls + shared infrastructure (registry, line_pipeline, compute_common, stateful_base, generator_math)

---

## BEHAVIORAL INVARIANTS

These invariants govern how the system works. Violating them causes subtle, hard-to-diagnose bugs.

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

- `#[serde(rename_all = "camelCase")]` on all serialized structs (project file compatibility)
- `#[serde(transparent)]` on typed IDs (`ClipId`, `LayerId`, `EffectGroupId`)
- `#[serde(skip)]` for runtime-only fields
- Field names in JSON must match camelCase format — getting this wrong silently breaks project loading

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

### All-Compute Effect Pipeline
- **All effects use compute dispatches** via `ComputeBlitHelper` (single source) or `ComputeDualBlitHelper` (dual source)
- Compute writes directly to storage textures — no TBDR tile memory load/store overhead
- The compute encoder stays alive across dispatches, eliminating per-pass encoder creation cost
- Render passes (`draw_fullscreen`) are only used for non-effect paths: output presenter blit, UI atlas blit, line/dot rendering

### Metal Constraints
- `max_compute_invocations_per_workgroup` = 256 → `@workgroup_size(16,16)` for 2D, `@workgroup_size(4,4,4)` for 3D
- `R32Float` NOT filterable — use `Rgba16Float` if `textureSample` needed
- `R16Float` does NOT support `STORAGE_BINDING`
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

### VSync & Frame Pacing

See `docs/VSYNC_AND_FRAME_PACING.md` for full architecture and hard-won lessons.

- **Three independent CVDisplayLinks** — content thread, output presenter, UI thread. Each callback is <1μs. **NEVER put heavy GPU work in a vsync callback that serves as a timing source for other consumers** — CVDisplayLink skips the next callback if the current one overruns the vsync interval.
- **Content thread VSync** (`GpuVsyncSignal` in `manifold-gpu`): CVDisplayLink → condvar notify → content thread wakes. Frame divisor snaps project FPS to nearest clean display divisor.
- **Output presenter** (`DisplayLinkPresenter`): fullscreen = callback does blit (Direct Display, must present every vsync). Windowed = callback sets flag, main thread does blit with `presentsWithTransaction` + `commit_and_wait_scheduled` for compositor sync.
- **Hz from CVTimeStamp** — `CVDisplayLinkGetActualOutputVideoRefreshPeriod` returns 0 before the first callback. Always derive Hz from `video_time_scale / video_refresh_period` in the callback's CVTimeStamp.
- **IOSurface triple buffer** — content thread renders to IOSurface, GPU completion handler publishes `front_index` asynchronously. Presenter reads current `front_index` — never waits for content thread.
- **`presentsWithTransaction = true` only works on the main thread** — CA transactions don't exist on CVDisplayLink background threads. Presents from background threads are silently discarded.

---

## HOW TO ADD A NEW EFFECT

Touch exactly 7 locations:

1. **`manifold-core/src/effect_type_id.rs`** — Add `pub const MY_EFFECT: Self = Self(Cow::Borrowed("MyEffect"));` + update `from_legacy_discriminant()` if needed
2. **`manifold-core/src/effect_type_registry.rs`** — Add to `build_registry()` vec: `reg(E::MY_EFFECT, "My Effect", POST_PROCESS, true)`
3. **`manifold-core/src/effect_definition_registry.rs`** — Add `EffectDef` in `build_definitions()` with param defs (use `pd()`, `pd_osc()`, `pd_whole()`, `pd_whole_labels()`, `pd_toggle()` helpers)
4. **`manifold-core/src/effect_category_registry.rs`** — (Optional) Add category if not POST_PROCESS
5. **`manifold-renderer/src/effects/my_effect.rs`** — NEW FILE: implement `PostProcessEffect` trait. Use `ComputeBlitHelper` (1 input) or `ComputeDualBlitHelper` (2 inputs). See `bloom.rs` as template.
6. **`manifold-renderer/src/effects/mod.rs`** — Add `pub mod my_effect;`
7. **`manifold-renderer/src/effect_registry.rs`** — Add `Box::new(MyEffectFX::new(device))` in `EffectRegistry::new()`

## HOW TO ADD A NEW GENERATOR

Touch exactly 6 locations:

1. **`manifold-core/src/generator_type_id.rs`** — Add `pub const MY_GEN: Self = Self(Cow::Borrowed("MyGen"));` + update `from_legacy_discriminant()` if needed
2. **`manifold-core/src/generator_type_registry.rs`** — Add to `build_registry()` vec: `reg(G::MY_GEN, "My Generator", true)`
3. **`manifold-core/src/generator_definition_registry.rs`** — Add `GeneratorDef` in `build_definitions()` via `create_def("My Generator", is_line_based, "osc_prefix", params)`
4. **`manifold-renderer/src/generators/my_gen.rs`** — NEW FILE: implement `Generator` trait. See `plasma.rs` (compute) or `lissajous.rs` (line-based) as template.
5. **`manifold-renderer/src/generators/mod.rs`** — Add `pub mod my_gen;`
6. **`manifold-renderer/src/generators/registry.rs`** — Add to `prewarm_all()` array AND `create()` if-else chain

---

## REFERENCE

### Texture Format Mapping

| Unity | Rust (manifold-gpu / Metal) | Notes |
|---|---|---|
| `RFloat` | `R32Float` | Keep unless sampled via `textureSample` |
| `RGFloat` | `Rg32Float` | Keep unless sampled via `textureSample` |
| `ARGBFloat` | `Rgba32Float` | Keep unless sampled via `textureSample` |
| `ARGBHalf` | `Rgba16Float` | Always fine |
| `RHalf` | `R16Float` | No STORAGE_BINDING on Metal |

### Math Gotchas

| Operation | Correct Rust | Trap |
|---|---|---|
| Round to int | `x.round() as i32` | NOT truncation (`as i32` alone) |
| Lerp | `a + (b - a) * t.clamp(0.0, 1.0)` | Lerp CLAMPS t |
| Repeat(t, len) | `t - (t / len).floor() * len` | NOT `t % len` (negative values differ) |
| Sign(0) | `1.0` (match Unity) | NOT `0.0` |

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
