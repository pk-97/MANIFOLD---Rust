# MANIFOLD — Agent Contract

A Visual DAW for live video performance. Users create, arrange, and compose video and generative visual content in beats/bars/arrangements, then perform live. Studio workflow (Ableton) meets real-time VJ tool (Resolume).

The Rust codebase is the complete, authoritative implementation. The Unity codebase at `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived reference only.

## CRATE STRUCTURE

| Crate | Role |
|---|---|
| `manifold-core` | Data models, types, registries (pure, no GPU) |
| `manifold-editing` | Commands, undo/redo, EditingService |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC |
| `manifold-gpu` | Native Metal GPU backend (`metal` crate, zero wgpu) |
| `manifold-renderer` | Compositor, 22 effects, 16 generators (uses `manifold-gpu`) |
| `manifold-media` | Audio/video decoding, Metal-accelerated encoding, export |
| `manifold-ui` | Custom bitmap UI: tree, 20+ panel types, input |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) |
| `manifold-native` | Native plugin FFI (`DepthEstimator`, `BlobDetector` traits) |
| `manifold-profiler` | Performance profiling and instrumentation |
| `manifold-led` | DMX/Art-Net LED output |
| `manifold-audio` | Audio (stub — placeholder for future work) |
| `manifold-app` | winit entry, Application, ContentThread, ContentPipeline |

**Dependencies:** `core` has no deps. `gpu` has no deps. `editing`/`playback`/`ui`/`io` depend on `core`. `renderer` depends on `core`, `gpu`, `native`, `playback`, `ui`. `media` depends on `core`, `playback`, `gpu`. `led` depends on `gpu`. `app` depends on all.

## ARCHITECTURE

### Two-Thread Model

- **Content thread** (owns all mutable state): `PlaybackEngine`, `EditingService`, `ContentPipeline`. Runs at project FPS (default 60).
- **UI thread** (winit event loop): Renders UI, handles input, presents GPU output.
- Communication: `ContentCommand` (UI->Content, bounded 64) and `ContentState` (Content->UI, bounded 4) via crossbeam.
- GPU output: IOSurface zero-copy triple-buffer with atomic `front_index`.
- **Project owned exclusively by content thread.** UI gets `Arc<Project>` snapshots via `ContentState`.
- **All mutations** via `ContentCommand::Execute(Box<dyn Command>)` or `ContentCommand::MutateProject(Box<dyn FnOnce(&mut Project)>)`.
- **NEVER** create new `Arc<Mutex<>>` or `Arc<RwLock<>>` shared state without approval.

### Key Patterns

- **Edition 2024**, Rust stable, winit 0.30
- **Typed IDs** — `ClipId`, `LayerId`, `EffectGroupId` newtypes wrapping String (`#[serde(transparent)]`)
- **Typed time** — `Beats(pub f64)`, `Seconds(pub f64)`, `Bpm(pub f32)`. Never raw `f32`/`f64` for time in function signatures.
- **AHashMap** on all hot-path maps. **parking_lot** `RwLock`/`Mutex` replacing std.
- **Native Metal GPU** — all threads use `manifold-gpu` (`metal` crate directly, zero wgpu). See `docs/MANIFOLD_GPU_ARCHITECTURE.md`.
- **VSync** — three independent CVDisplayLinks. See `docs/VSYNC_AND_FRAME_PACING.md`.

## BEHAVIORAL INVARIANTS

- Primary time model is **beats**; `Seconds` only for `in_point`, player time, delta_time, OSC, export
- `sync_clips_to_time()` is the SOLE authority for playback state
- `EditingService` is the SOLE mutation gateway — no direct model writes from UI
- All mutations: `EditingService` -> `UndoRedoManager` -> `Command`. Undo stack capped at 200.
- Overlap is a write-time invariant on Layer (`enforce_non_overlap()`)
- Phantom clips: created on NoteOn, committed on NoteOff. Time guard: 5ms. Channel filtering: same channel.

## DEVELOPMENT RULES

### Performance

- **No per-frame allocations on hot paths** (engine tick, sync, rendering)
- Pre-allocated scratch buffers, `AHashMap` for ID lookups, static sort functions, dirty-checking via `DataVersion`

### Code Style

- `snake_case` functions, `PascalCase` types, `SCREAMING_SNAKE_CASE` constants
- `pub(crate)` over `pub` for internal API
- `#[derive(Clone, Debug, Serialize, Deserialize)]` on data models
- `unwrap()`/`expect("reason")` for impossible states; `Option<T>` for nullable
- Do NOT over-engineer error handling

### Serialization

- `#[serde(rename_all = "camelCase")]` on all serialized structs
- `#[serde(transparent)]` on typed IDs, `#[serde(skip)]` for runtime-only fields

### GPU Constraints

- Uniform structs: 16-byte aligned, `_pad` fields, `#[repr(C)]`, field order matches WGSL
- WGSL `vec3<f32>` has 16-byte alignment in storage buffers — Rust structs MUST pad
- `R32Float` NOT filterable. `R16Float` no `STORAGE_BINDING`. `@workgroup_size(16,16)` for 2D, `(4,4,4)` for 3D.
- **NEVER** introduce wgpu dependencies

### Build & Git

- `cargo clippy --workspace -- -D warnings` + `cargo test --workspace` before commit
- `clippy.toml`: `too-many-arguments-threshold = 20`
- `rustfmt.toml`: `max_width = 100`, `use_field_init_shorthand = true`
- Release: `lto = "thin"`, `codegen-units = 1`, `strip = "symbols"`, `panic = "abort"`

## EXTERNAL DEPENDENCIES

### AbletonOSC patch (perform-mode HUD)

Perform-mode's PLAY-group track HUD requires two AbletonOSC endpoints that are NOT in the stock release:

- `/live/track/get/arrangement_clips/end_time`
- `/live/track/get/arrangement_clips/muted`

Patch lives at `assets/abletonosc-patches/`. Install with `./scripts/install-abletonosc-patch.sh` (idempotent, backs up to `track.py.bak`). Restart Ableton Live after installing. Without the patch, the perform-mode track display will be wrong for looped clips and will not honor clip-level mute. The cue-points HUD works without the patch.

## COMMIT AND PUSH

YOU MUST COMMIT AND PUSH CODE CHANGES AFTER COMPLETING FEATURES OR FIXES.

## REFERENCE DOCS (read on-demand, not preloaded)

| Doc | When to read |
|---|---|
| `docs/MANIFOLD_GPU_ARCHITECTURE.md` | Working on GPU, effects, generators, textures, compute |
| `docs/VSYNC_AND_FRAME_PACING.md` | Working on frame pacing, display links, presentation |
| `docs/ADDING_EFFECTS_AND_GENERATORS.md` | Adding new effects or generators |
| `docs/DEVELOPMENT_REFERENCE.md` | Texture formats, math gotchas, module layout |

## DEBUGGING

Runtime bugs (callbacks, event ordering, timing): add `println!`/`eprintln!` after 1-2 min of reading, ask user to reproduce, read logs, fix. Static analysis is for compile errors only.

## AGENTS

Write code directly in the main context by default. Only spawn an agent for genuinely large, isolated tasks. Tell the user if you spawn one and why.
