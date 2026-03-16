---
name: Stateful generator patterns
description: Ping-pong state management, BlitPipeline reuse for half-res upscale, position texture upload pattern for CPU+GPU hybrid generators
type: project
---

## Stateful Generator Infrastructure (2026-03-16)

- `StatefulState` in `generators/stateful_base.rs`: ping-pong RenderTarget pair, lazy init (None until first render)
- Sim pipelines render into STATE_FORMAT (Rgba32Float or Rgba16Float), display/blit to target_format
- Half-res generators (Flowfield, StrangeAttractor) use `BlitPipeline` from `crate::blit` to upscale state to output
- `BlitPipeline::new(device, target_format)` — accepts Float{filterable:true} sources, so Rgba16Float works
- Position texture for CPU→GPU upload: Rgba32Float, write via `queue.write_texture`, bind as `Float{filterable:false}` + `textureLoad`
- Uniform structs MUST be 16-byte aligned (pad to vec4 boundaries with `_pad` fields)
- All generators use `include_str!("shaders/...")` for shader loading
- `immediate_size: 0` required on PipelineLayoutDescriptor (wgpu 28 API)
- `depth_slice: None` required on RenderPassColorAttachment

**Why:** Stateful generators need temporal feedback (previous frame state). Ping-pong avoids read-write hazard on same texture.

**How to apply:** Any new simulation generator should use `StatefulState` for temporal state management and `BlitPipeline` if internal resolution differs from output.

## Compute Pipeline Pattern (ParametricSurface)

- 3D storage textures: `TextureDimension::D3` + `STORAGE_BINDING` usage for compute write, `TEXTURE_BINDING` for fragment read
- `StorageTexture` binding: `access: WriteOnly`, `view_dimension: D3`, format must match texture
- `create_compute_pipeline` with `ComputePipelineDescriptor` (separate from render pipeline)
- `begin_compute_pass` + `dispatch_workgroups(x, y, z)` for bake
- Keep texture alive via struct field even if only view is used (GPU lifetime)
- 3D texture sampler needs `address_mode_w: ClampToEdge`
- `textureSampleLevel` required in fragment to sample 3D texture (not `textureSample`)

## Generator Registry Pattern

- All 18 generators registered in `generators/registry.rs` via match arms on `GeneratorType`
- Modules declared in `generators/mod.rs` (alphabetical order)
- Each generator: `pub struct XGenerator`, `impl XGenerator { pub fn new(device, target_format) }`, `impl Generator for XGenerator`
