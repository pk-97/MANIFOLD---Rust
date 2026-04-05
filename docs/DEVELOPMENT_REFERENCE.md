# Development Reference

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
- `manifold-renderer/src/effects/` — 22 effect impls + `compute_blit_helper` + `compute_dual_blit_helper`
- `manifold-renderer/src/generators/` — 16 generator impls + shared infrastructure (registry, line_pipeline, compute_common, stateful_base, generator_math)

## Effect Pipeline

Effects use compute dispatches via `ComputeBlitHelper` (single source) or `ComputeDualBlitHelper` (dual source). Render passes (`draw_fullscreen`) are only for non-effect paths: output presenter blit, UI atlas blit, line/dot rendering.

- Async compute: independent layers generate in parallel `MTLCommandBuffer`s, compositor waits via `MTLEvent`
- Texture pool: frame-stamped recycling, zero per-frame allocations after 3-frame warmup
- Function constants: specialized Metal pipelines per effect mode
- MTLBinaryArchive: compiled pipeline cache on disk
- `set_fast_math_enabled(true)` globally

## Resolution Scaling

Controlled by `project.settings.upscale_mode` (`UpscaleMode` enum). Default is `Native`.

- `Native` — all generators render at full resolution
- `MetalFxSpatial` / `MpsLanczos` — generators with `internal_resolution_scale() < 1.0` render reduced, upscaled
- Sub-1.0 overrides (non-Native only): FluidSimulation (0.5x), FluidSimulation3D (0.5x), Mycelium (0.5x), ParametricSurface (0.75x)
