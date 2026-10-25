# Development Reference

<!-- index: Quick reference grab-bag: texture formats, math gotchas, module layout, and the ui_bridge split. Facts you reach for mid-task. -->

## Crates

| Crate | Role |
|---|---|
| `manifold-foundation` | Shared primitive vocabulary; zero-dependency leaf crate |
| `manifold-core` | Data models, types, registries (no GPU) |
| `manifold-editing` | Commands, undo/redo, EditingService |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC |
| `manifold-gpu` | GPU backend — native Metal today; Vulkan approved, not built |
| `manifold-node-engine` | Graph loading, execution, freeze compiler and engine primitives |
| `manifold-nodes-image` / `manifold-nodes-scene` | Image and scene primitive implementations |
| `manifold-fluids` | Safe owned access to the pinned FLIP Fluids CPU reference engine |
| `manifold-physics` | Small owned Rust interface to the pinned Box3D C library |
| `manifold-nodes-physics` | Physics graph nodes over the shared engine vocabulary: the GPU side of the force lattice water, matter and swarms sample, the tick-sampled registration the physics history replay finds them by, and the swarm nodes. Names no liquid solver |
| `manifold-nodes-water` | Water registration: links the six water crates; owns migrations, bundled water presets, the physics scene, the runtime extension and cross-solver harnesses |
| `manifold-water-rigid` | Box3D rigid-body graph adapter and the native pair contract that coupled liquids implement |
| `manifold-water-liquid` | The liquid seam every solver shares: clock, lattice, bodies, coupling, fields (impulses and per-tick force lattices), frame ring, roles and the shared atoms |
| `manifold-water-gpu-flip` | The GPU FLIP liquid solver: step, pressure solve, clock, bodies, sheeting and its own particle atoms |
| `manifold-water-gpu-mpm` | The GPU MLS-MPM liquid solver (Matter): domain, particle-to-grid, grid update, grid-to-particle, body reaction and its frame |
| `manifold-water-whitewater` | The whitewater step: foam, spray and bubble emitters, potentials, lifecycle and the CPU handoff, seeded from a liquid frame |
| `manifold-water-surface` | The liquid surface mesher: particle volume, lattice bricks, the welded mesh, its smoothing and normals, the liquid frame and blob bounds |
| `manifold-compositor` | Layer composition, generator rendering and preset thumbnails |
| `manifold-ui-paint` | GPU painting for the bitmap UI |
| `manifold-nodes` | Bundled presets, registration and catalog contracts. See `docs/NODE_CATALOG.md` |
| `manifold-media` | Audio/video decode, Metal-accelerated encode, export |
| `manifold-ui` | Custom bitmap UI: tree, panels, input |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) |
| `manifold-native` | Native plugin FFI (`DepthEstimator`, `BlobDetector`) |
| `manifold-profiler` | Profiling and instrumentation |
| `manifold-led` | DMX/Art-Net LED output |
| `manifold-audio` | Audio capture behind one `CaptureBackend` trait → lock-free ring + off-RT analysis worker (`docs/AUDIO_INFRASTRUCTURE.md` section 11 (Output taps), `docs/AUDIO_MODULATION_DESIGN.md`) |
| `manifold-spectral` | Spectral DSP and spectrogram rendering |
| `manifold-recording` | Live recording of compositor output and optional audio input to QuickTime |
| `manifold-app` | winit entry, Application, ContentThread, ContentPipeline |

Workspace dependencies (from each crate's `[dependencies]`): `foundation`, `native` and `profiler` have none; `core`, `gpu`, `physics` and `ui` on `foundation`; `fluids` on `foundation`+`physics`; `editing` and `io` on `core`; `playback` on `core`+`editing`; `audio` on `core`+`spectral`; `spectral` on `gpu`; `recording` on `audio` (+`gpu` on macOS); `node-engine` on `core`+`foundation`+`gpu`+`playback`; `nodes` on `core`+`gpu`+`node-engine`+`nodes-image`+`nodes-physics`+`nodes-scene`+`nodes-water`+`water-gpu-mpm`+`water-liquid`+`water-whitewater`; `nodes-image` on `core`+`foundation`+`gpu`+`native`+`node-engine`; `nodes-scene` on `core`+`foundation`+`gpu`+`node-engine`; `nodes-physics` on `gpu`+`node-engine`+`physics`; `nodes-water` on `core`+`fluids`+`foundation`+`gpu`+`node-engine`+`nodes-physics`+`physics`+`water-gpu-flip`+`water-gpu-mpm`+`water-liquid`+`water-rigid`+`water-surface`+`water-whitewater`; `water-liquid` on `core`+`fluids`+`gpu`+`node-engine`+`nodes-physics`+`physics`+`water-rigid`; `water-gpu-flip` and `water-gpu-mpm` on `core`+`gpu`+`node-engine`+`nodes-physics`+`physics`+`water-liquid`+`water-rigid`; `water-whitewater` on `core`+`fluids`+`gpu`+`node-engine`+`nodes-physics`+`physics`+`water-liquid`; `water-surface` on `core`+`gpu`+`node-engine`+`water-liquid`; `water-rigid` on `core`+`gpu`+`node-engine`+`physics`; `compositor` on `core`+`gpu`+`node-engine`+`playback`; `ui-paint` on `foundation`+`gpu`+`ui`; `media` on `core`+`playback` (+`gpu` on macOS); `led` on `gpu`; `app` on `audio`+`compositor`+`core`+`editing`+`gpu`+`io`+`led`+`media`+`node-engine`+`nodes`+`nodes-image`+`nodes-scene`+`nodes-water`+`playback`+`profiler`+`recording`+`spectral`+`ui`+`ui-paint`.

`ui` depends on `foundation` only: mutations leave as `PanelAction` values translated to commands app-side, and UI-reachable shared types go in `foundation`.

## Texture Format Mapping

| Format | Metal (manifold-gpu) | Notes |
|---|---|---|
| Single-channel 32-bit | `R32Float` | NOT filterable — can't use `textureSample` |
| Two-channel 32-bit | `Rg32Float` | NOT filterable |
| Four-channel 32-bit | `Rgba32Float` | NOT filterable |
| Four-channel 16-bit | `Rgba16Float` | Always fine, filterable + storage |
| Single-channel 16-bit | `R16Float` | No STORAGE_BINDING on Metal |

## Math Gotchas

| Operation | Correct Rust | Trap |
|---|---|---|
| Round to int | `x.round() as i32` | NOT truncation (`as i32` alone) |
| Lerp | `a + (b - a) * t.clamp(0.0, 1.0)` | Lerp CLAMPS t |
| Repeat(t, len) | `t - (t / len).floor() * len` | NOT `t % len` (negative values differ) |
| Sign(0) | `1.0` | NOT `0.0` |

## UI Renderer Invariant

A draw command's clip (and depth) is **bound at enqueue, never inferred at flush**. All four command types carry it per command: rect `RectCommand::clip`, line `LineCommand::clip`, image `ImageCommand::clip`, text `clip_bounds`. Batches are DERIVED in `prepare()` by run-scanning consecutive equal `(clip, depth)` commands — there is no "pending run" whose scissor gets decided later. History: BUG-060 (hardest bug in repo history) existed because rect scissors were stamped per batch at flush time, and a transform/depth boundary mid-traversal flushed pending tree rects under the immediate clip (`None`); depth had the same class of bug earlier. If you add a new command type, give it `clip` + `depth` fields captured at the push site.

Corollary (2026-07-10, sibling of BUG-060): a tree node's **text/icon clip is the tree clip intersected with the node's own rect**, bound in `draw_node` at enqueue — a label longer than its widget cuts at the edge instead of overrunning the neighbour. Containment is structural, not a per-call-site elide. Proof: `crates/manifold-ui-paint/tests/contracts/text_clip_to_node_bounds.rs` (pixel-asserted both ways).

## Key Module Splits

- `manifold-app/src/ui_bridge/` — 8 modules: mod, transport, editing, inspector, layer, project, state_sync, marker
- `manifold-app/src/` — `app.rs` + `app_render.rs` + `app_lifecycle.rs`
- `crates/manifold-nodes/src/` — catalog registration (`registry.rs`), bundled effect/generator preset lookup, and catalog generation; preset assets live in `crates/manifold-nodes/assets/` and are loaded from disk.
- `crates/manifold-node-engine/src/` — graph vocabulary and runtime; `load/`, `exec/`, `freeze/`, `runtime/`, and `gpu/` own loading, execution, compilation, playback state and GPU dispatch. Generic engine atoms live in `primitives/` and `atomic/`.
- Water is seven crates, layered so Cargo refuses an upward reach (`crates/manifold-app/tests/crate_layering.rs` pins the edges):
  - `crates/manifold-water-rigid/src/` — Box3D rigid bodies as graph nodes and the native pair contract (`node.rs`); the bottom, names no liquid.
  - `crates/manifold-water-liquid/src/` — the liquid seam every solver shares: clock, lattice, bodies, coupling, fields (impulses and per-tick force lattices), frame ring, whitewater grid vocabulary and the shared atoms in `primitives/`; on rigid.
  - `crates/manifold-water-gpu-flip/src/`, `manifold-water-gpu-mpm/src/`, `manifold-water-whitewater/src/`, `manifold-water-surface/src/` — the leaves: GPU FLIP, GPU MLS-MPM (Matter), the whitewater step and the surface mesher; each on liquid, none on another leaf.
  - `crates/manifold-nodes-water/src/` — registration: links the six, owns migrations, bundled water presets, the physics scene, the runtime extension and the cross-solver test harnesses. No image or scene family dependency.
- `crates/manifold-nodes-image/src/node_graph/` — image primitives and retained composite builders; image-specific helpers live in the crate's `effects/` and `generators/` directories.
- `crates/manifold-nodes-scene/src/node_graph/` — scene primitives, glTF import, scene exposure and scene-modifier support; scene-specific generator helpers live in the crate's `generators/` directory.
- `crates/manifold-compositor/src/` — layer composition, generator rendering, presentation, preset thumbnails and upscaling.
- `crates/manifold-ui-paint/src/` and `tests/contracts/` — bitmap UI GPU painting and its clipping/drawing proofs, mounted by `tests/main.rs`.
- `crates/manifold-nodes/tests/contracts/` — catalog and engine integration contracts, mounted by `tests/main.rs`; GPU proof modules are mounted by `tests/gpu_proofs/main.rs`.
- `crates/manifold-app/tests/contracts/` — compositor and application-dependent graph contracts, mounted by `tests/renderer_contracts.rs`; application-dependent GPU proofs live in `tests/renderer_contracts/gpu_proofs/`.

## Effect Pipeline

Effects run through the node graph: every preset is a `ChainGraph` of typed primitives, walked by an `ExecutionPlan` once per frame. The graph runtime is the sole dispatcher.

Primitives use compute dispatches via the `Primitive` trait (each primitive's `run` method binds inputs/outputs/params and submits its work to a `GpuEncoder`). Render passes (`draw_fullscreen`) are only for non-effect paths: output presenter blit, UI atlas blit, instanced line rendering (`node.render_lines`).

- Async compute: independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- Texture pool: frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- Function constants: specialized Metal pipelines per effect mode
- MTLBinaryArchive: compiled pipeline cache on disk
- `set_fast_math_enabled(true)` globally
- Skip-passthrough via slot aliasing: when an effect's skip condition is met (e.g. amount=0), no GPU work runs and the output slot aliases to the input — zero-cost bypass

## Resolution Scaling

All generators render at full output resolution. If a specific generator needs internal downscaling for performance, it allocates its own reduced-resolution intermediate inside `render()` and stretches to the output — same pattern Bloom uses for its mip chain.

The pipeline-wide `render_scale` setting (FSR / MetalFX full-frame upscaling) is separate and still active.

## Recorder soundcheck

The day before a gig, on the rig, run the pre-gig soak — the instrument check for the live
show recorder:

```
cargo run --release -p manifold-recording --features recording-proofs --bin recording-soak
```

With no flags this drives the actual show configuration through the real
`LiveRecordingSession` API into the real native AVAssetWriter encoder: 4K60 SDR ProRes, 20
media-minutes (72,000 frames, ~17.5 GB), synthetic 48kHz stereo audio, encoding as fast as the
hardware allows (100% encoder duty — the video pool is never throttled to wall clock). At the
end it prints exactly one line:

```
SOAK PASS: 72000 frames, 0 dropped, PTS monotonic, gap-free indices, 17.4 GB, audio 1200.0s
```

or, if something's wrong:

```
SOAK FAIL: <first failed check, with numbers>
```

**A PASS means the recorder survives a full take on this machine, this OS build, this disk** —
not "the code compiles," not "stop() returned Ok" (two of the three worst live-recording
failures this system has actually produced returned `Ok` from `stop()` while the file was
unusable). macOS updates have already changed hardware encoder behavior once without any
MANIFOLD change being involved — this is the check that catches the next one before it costs a
show, the same way you'd line-check the LEDs before a set.

Useful flags:

- `--width`/`--height`/`--fps`/`--minutes` — override the take size (a shorter run at a lower
  resolution is a fast sanity check, not a substitute for the full-scale run before a real gig).
- `--no-audio` — video only.
- `--realtime` — paces submissions to wall clock instead of running flat-out; a true dress
  rehearsal, but gates on file validity only (drops are reported, not failed on — keep-up under
  a loaded rig is a separate concern from "did the file come out valid").
- `--keep` — keep the output file even on PASS (default: PASS deletes it, FAIL always keeps it
  and prints its path — the failed file is the evidence).
- `--output <path>` — write somewhere other than a temp path.
- `--hdr` — refuses immediately; HDR live recording is BUG-053 (structurally broken today).

Rust's own frame and sample counters can be optimistic under real backpressure, so the soak's PASS
decision is anchored to what ffprobe actually decodes out of the file, not to those counters.
