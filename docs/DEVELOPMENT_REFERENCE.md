# Development Reference

<!-- index: Quick reference grab-bag: texture formats, math gotchas, module layout, and the ui_bridge split. Facts you reach for mid-task. -->

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

## Key Module Splits

- `manifold-app/src/ui_bridge/` — 8 modules: mod, transport, editing, inspector, layer, project, state_sync, marker
- `manifold-app/src/` — `app.rs` + `app_render.rs` + `app_lifecycle.rs`
- `manifold-renderer/src/node_graph/` — the graph runtime: `primitive.rs`, `graph.rs`, `execution_plan.rs`, `state_store.rs`, `metal_backend.rs`, `bundled_presets.rs`, plus the `primitives/`, `atomic/`, and `composites/` subdirectories
- `manifold-renderer/src/node_graph/primitives/` — ~30 primitives (one file per primitive, auto-registered via `inventory::submit!`)
- `manifold-renderer/src/node_graph/atomic/` — irreducible complex kernels (FluidSim2D, FluidSim3D, Plasma, Glitch)
- `manifold-renderer/src/node_graph/composites/` — 6 Rust composite builders (Bloom, Halation, Infrared, Mirror, SoftFocus, StrobeOpacity) retained as dev fixtures for parity tests; new composites ship as JSON
- `crates/manifold-renderer/assets/effect-presets/` — 29 JSON-authoritative presets, codegened into `BUNDLED_PRESETS` by `build.rs`
- `manifold-renderer/src/effects/` — 6 legacy monolithic effect impls retained because their primitives wrap them (auto_gain, blob_tracking, depth_of_field, infrared, quad_mirror, wireframe_depth) + `compute_blit_helper` + `compute_dual_blit_helper`
- `manifold-renderer/src/generators/` — 23 generator impls + shared infrastructure (registry, line_pipeline, compute_common, stateful_base, generator_math). Still on the legacy `inventory::submit! { GeneratorMetadata, GeneratorFactory }` workflow; JSON migration pending.

## Effect Pipeline

Effects run through the node graph: every preset is a `ChainGraph` of typed primitives, walked by an `ExecutionPlan` once per frame. The graph runtime is the sole dispatcher; the legacy linear chain dispatcher was deleted in the May 2026 migration.

Primitives use compute dispatches via the `Primitive` trait (each primitive's `run` method binds inputs/outputs/params and submits its work to a `GpuEncoder`). The legacy `ComputeBlitHelper` (single source) and `ComputeDualBlitHelper` (dual source) helpers still back the 6 retained legacy effect impls. Render passes (`draw_fullscreen`) are only for non-effect paths: output presenter blit, UI atlas blit, line/dot rendering.

- Async compute: independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- Texture pool: frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- Function constants: specialized Metal pipelines per effect mode
- MTLBinaryArchive: compiled pipeline cache on disk
- `set_fast_math_enabled(true)` globally
- Skip-passthrough via slot aliasing: when an effect's skip condition is met (e.g. amount=0), no GPU work runs and the output slot aliases to the input — zero-cost bypass

## Resolution Scaling

All generators render at full output resolution. The per-generator `internal_resolution_scale()` trait method and the `UpscaleMode` enum were removed (the infrastructure was wired but the default `Native` mode disabled it, so it was dead code in practice). If a specific generator needs internal downscaling for performance, it allocates its own reduced-resolution intermediate inside `render()` and stretches to the output — same pattern Bloom uses for its mip chain.

The pipeline-wide `render_scale` setting (FSR / MetalFX full-frame upscaling) is separate and still active.
