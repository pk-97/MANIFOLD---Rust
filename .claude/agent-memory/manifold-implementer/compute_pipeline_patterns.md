---
name: Compute Pipeline Patterns
description: Key patterns for multi-pass compute+fragment generators in manifold-renderer
type: project
---

## Multi-pass Generator Architecture

Generators with compute+fragment passes follow the Mycelium pattern:
- All pipelines/BGLs created in `new()` at startup
- GPU resources (buffers, RenderTargets) lazy-init'd in `init_resources()` on first `render()`
- `resize()` sets `initialized = false` to force reinit
- `frame_count` tracked for deterministic RNG seeding in shaders

## Key API Patterns

- `RenderTarget::new(device, w, h, format, label)` — creates texture with RENDER_ATTACHMENT | TEXTURE_BINDING | STORAGE_BINDING | COPY_SRC | COPY_DST
- `compute_common::Particle` — 48-byte struct matching WGSL Particle (position vec3, velocity vec3, life f32, age f32, color vec4)
- `compute_common::FIXED_POINT_SCALE` = 4096.0 for atomic scatter
- Compute pass: `encoder.begin_compute_pass()` → `set_pipeline` → `set_bind_group` → `dispatch_workgroups`
- Fragment fullscreen blit: draw 0..3, 0..1 with vertex shader generating fullscreen triangle from vertex_index

## BGL Helper Pattern

Extract BGL entry creation into helpers (`bgl_uniform`, `bgl_storage_rw`, `bgl_storage_ro`, `bgl_texture`, `bgl_texture_filterable`, `bgl_sampler`) to reduce boilerplate. First used in FluidSimulation rewrite.

## Texture Format Constraints

When using separable blur across multiple texture formats (e.g., R32Float for density, Rg32Float for vector field), need separate render pipelines per output format even if using the same shader — pipeline target format must match render attachment.

## Blur Ping-Pong Without Extra RT

For density blur with limited RTs: H-blur from full-res density_rt into half-res blur_density_rt (implicit downsample), then V-blur from blur_density_rt back into density_rt (implicit upsample). Gradient/simulate then read density_rt.

## 3D Volume Textures

- `TextureDimension::D3` with `STORAGE_BINDING | TEXTURE_BINDING | COPY_DST` for compute write + sample read
- `Volume3D` helper struct: holds `_texture` (for GPU lifetime), `view`, `_res`
- 3D compute dispatch: `@workgroup_size(8,8,8)` → `dispatch_workgroups((res+7)/8, (res+7)/8, (res+7)/8)`
- Separable 3D blur: 3 passes (X, Y, Z axis) ping-ponging between volume and blur_temp volume
- After 3-pass separable blur, result ends up in the "temp" volume (odd number of swaps). Track which volume holds the final result.
- `bgl_storage_texture_3d`: `StorageTextureAccess::WriteOnly`, `view_dimension::D3`
- `bgl_texture_3d`: `Float{filterable:false}` for textureLoad; `bgl_texture_3d_filterable` for textureSampleLevel
- 3D sampler needs `address_mode_w: Repeat` (or ClampToEdge depending on use case)

## Cross-Module Struct Access

Private structs in other generator modules (e.g., `BlurUniforms` in `fluid_simulation.rs`) cannot be referenced from sibling modules. Use hardcoded byte size or define local struct instead.

## Parameter Reading Pattern

Use `fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32` helper to eliminate repetitive bounds-checking boilerplate. Underscore-prefix unused param bindings (e.g., `let _scale = ...`).
