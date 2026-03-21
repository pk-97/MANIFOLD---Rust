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
| `manifold-renderer` | wgpu GPU: compositor, effects, generators | `Compositor` trait, `PostProcessEffect` trait, `Generator` trait |
| `manifold-ui` | Custom bitmap UI: tree, panels, input | `UIState`, `CoordinateMapper`, `UITree`, 17 panel types |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) | `loader`, `saver`, `migrate` |
| `manifold-native` | Native plugin FFI (depth estimation) | `DepthEstimator`, `BlobDetector` |
| `manifold-app` | winit entry, Application, UIRoot, UIBridge | `Application`, `ContentThread`, `ContentPipeline` |

**Dependency direction:** `core` ← `editing`, `playback`, `renderer`, `ui`, `io` ← `app`. No backwards dependencies. Core is pure.

## ARCHITECTURE

### Two-Thread Model

- **Content thread** (owns all mutable state): `PlaybackEngine`, `EditingService`, `ContentPipeline`. Runs at project FPS (default 60).
- **UI thread** (winit event loop): Renders UI, handles input, presents GPU output.
- Communication: `ContentCommand` (UI→Content) and `ContentState` (Content→UI) via crossbeam channels.
- GPU output: macOS uses IOSurface zero-copy; other platforms use double-buffered texture swap.

### Current Patterns

- **Edition 2024** — all crates
- **Typed IDs** — `ClipId`, `LayerId`, `EffectGroupId` newtypes wrapping String (`#[serde(transparent)]`)
- **AHashMap** — on all hot-path maps (clip lookups, effect state, scheduler)
- **parking_lot** — `RwLock`/`Mutex` replacing std (no poisoning, smaller, faster)
- **Lock-free MIDI** — `AtomicClockState` packed `AtomicU64` CAS for real-time-safe MIDI clock callbacks
- **Per-owner effect cleanup** — `TickResult::stopped_clips` → `ContentPipeline::cleanup_stopped_clips()` → `EffectRegistry` → stateful effects

### Key Module Splits

- `manifold-app/src/ui_bridge/` — 7 modules: mod, transport, editing, inspector, layer, project, state_sync
- `manifold-app/src/` — `app.rs` + `app_render.rs` + `app_lifecycle.rs`
- `manifold-renderer/src/effects/` — 26 effect impls + `simple_blit_helper` + `dual_texture_blit_helper`
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

- **wgpu 28**, winit 0.30, Edition 2024, Rust stable
- `clippy.toml`: `too-many-arguments-threshold = 20`
- `rustfmt.toml`: `max_width = 100`, `use_field_init_shorthand = true`
- Release: `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`, `panic = "abort"`
- Symphonia audio codecs: `opt-level = 2` in dev profile (10-50x faster than debug)

---

## COMMIT AND PUSH

YOU MUST COMMIT AND PUSH CODE CHANGES TO THE RELEVANT REPO AFTER COMPLETING FEATURES OR FIXES.

---

## wgpu / METAL CONSTRAINTS

Metal/wgpu constraints are RUNTIME problems, NOT source-code compromises.

### Workgroup Size
- `max_compute_invocations_per_workgroup` = 256 on Metal → `@workgroup_size(4,4,4)` for 3D volumes
- 3D textures: dispatch `(res+7)/8` per axis

### Texture Format Rules
- **First, match Unity's format.** `RFloat` → `R32Float`. `RGFloat` → `Rg32Float`. `ARGBFloat` → `Rgba32Float`. `ARGBHalf` → `Rgba16Float`.
- **ALL 32-bit float formats are NOT filterable on Metal.** If shader uses `textureSample` (sampler), MUST use `Rgba16Float`. If shader uses `textureLoad` only, 32-bit formats are fine with `Float { filterable: false }` in BGL.
- `R16Float` does NOT support `STORAGE_BINDING` on Metal
- Document every format substitution in `docs/KNOWN_DIVERGENCES.md`
- **Decision rule for every texture:**
  1. `textureSample`? → MUST be filterable → `Rgba16Float` if Unity uses 32-bit float
  2. `textureLoad` only? → 32-bit float fine → `Float { filterable: false }` in BGL
  3. Both? → Two textures or `Rgba16Float` for both

### Buffer Sizes
- Match Unity's buffer sizes exactly. Runtime clamp if Metal's 128MB limit is a problem.

### Pipeline / API
- `immediate_size: 0` on `PipelineLayoutDescriptor` (wgpu 28+)
- `depth_slice: None` on `RenderPassColorAttachment`
- `textureSampleLevel` required for 3D texture sampling in fragment shaders
- 3D sampler needs `address_mode_w` set
- Separate render pipelines needed per output format even with same shader
- Separable 3D blur: after 3 passes (X,Y,Z) result is in the "temp" volume (odd number of swaps)

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

| Unity | Rust (wgpu) | Notes |
|---|---|---|
| `RFloat` | `R32Float` | Keep unless sampled via `textureSample` |
| `RGFloat` | `Rg32Float` | Keep unless sampled via `textureSample` |
| `ARGBFloat` | `Rgba32Float` | Keep unless sampled via `textureSample` |
| `ARGBHalf` | `Rgba16Float` | Always fine |
| `RHalf` | `R16Float` | No STORAGE_BINDING on Metal |

### Behavioral Invariants

- Primary time model is **beats** (`start_beat`, `duration_beats`); seconds ONLY for `in_point`, player time, delta_time, OSC, export
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
