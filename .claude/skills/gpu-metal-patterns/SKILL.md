---
name: gpu-metal-patterns
description: Correct GPU patterns for the content thread — manifold-gpu types, compute dispatch, texture management. Invoke when writing or reviewing GPU-side code.
---
# GPU / Metal Patterns Guide

ALL threads use `manifold-gpu` (native `metal` crate). Zero wgpu anywhere in the codebase.
UI thread rendering (ui_renderer.rs, layer_bitmap_gpu.rs, native_text.rs) also uses `manifold-gpu`.

## Core Types

```rust
use manifold_gpu::{
    GpuDevice,           // Metal device wrapper
    GpuEncoder,          // Wraps MTLCommandBuffer — one per frame
    GpuTexture,          // Metal texture
    GpuBuffer,           // Metal buffer
    GpuComputePipeline,  // Compiled compute PSO
    GpuRenderPipeline,   // Compiled render PSO
    GpuSampler,          // Texture sampler
    GpuBinding,          // Binding for dispatch
    TexturePool,         // Frame-stamped texture recycling
};
```

## Creating Pipelines

```rust
// Standard compute pipeline (from WGSL source)
let pipeline = device.create_compute_pipeline(WGSL_SOURCE, "cs_main", "Label");

// Specialized with function constants (Metal dead-code elimination)
let pipeline = device.create_specialized_compute_pipeline(
    WGSL_SOURCE, "cs_main",
    &[("uniforms.mode", "0u"), ("uniforms.variant", "1u")],
    "Label",
);
```

All pipelines cached in `MTLBinaryArchive` — near-instant startup after first launch.
`set_fast_math_enabled(true)` is set globally on all pipeline compile options.

## Compute Dispatch

```rust
gpu.native_enc.dispatch_compute(
    &pipeline,
    &[
        GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
        GpuBinding::Texture { binding: 1, texture: &source },
        GpuBinding::Sampler { binding: 2, sampler: &sampler },
        GpuBinding::Texture { binding: 3, texture: &target },  // storage write
    ],
    [width.div_ceil(16), height.div_ceil(16), 1],   // threadgroup count
    "Dispatch Label",
);
```

**Workgroup sizes:**
- 2D: `@workgroup_size(16, 16)` = 256 (Metal max per workgroup)
- 3D: `@workgroup_size(4, 4, 4)` = 64 (safe) or `@workgroup_size(8, 8, 4)` = 256 (max)
- NEVER exceed 256 total invocations

## Uniform Struct Rules

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MyUniforms {
    value_a: f32,        // offset 0
    value_b: f32,        // offset 4
    mode: u32,           // offset 8
    _pad: f32,           // offset 12 — pad to 16-byte boundary
    vector: [f32; 4],    // offset 16 — vec4 starts on 16-byte boundary
}
// Total: 32 bytes ✓ (multiple of 16)
```

**Rules:**
- `#[repr(C)]` always
- Total size MUST be multiple of 16 bytes
- `vec3<f32>` in WGSL = 16 bytes (not 12) — pad in Rust
- Field ORDER in Rust MUST match WGSL struct exactly
- Verify with: `assert!(std::mem::size_of::<MyUniforms>() % 16 == 0);`
- Pass via: `bytemuck::bytes_of(&uniforms)`

## Texture Format Reference

| Use case | Format | Notes |
|----------|--------|-------|
| HDR color (default) | `Rgba16Float` | Always safe, supports sampling + storage |
| Full precision float | `Rgba32Float` | Only if 16-bit isn't enough |
| Single channel storage | `R32Float` | NOT filterable — no `textureSample` |
| Single channel (needs sampling) | `Rgba16Float` | Use this if you need `textureSample` |
| Single channel read-only | `R16Float` | NO `STORAGE_BINDING` — read-only |

## Texture Pool Usage

```rust
// Allocate from pool (frame-stamped, auto-recycled)
let tex = pool.get(device, width, height, format, "Label");

// Or via RenderTarget helper
let rt = RenderTarget::new_pooled(pool, w, h, Rgba16Float, "Label");

// Release happens automatically when RenderTarget drops
// Pool delays reuse by N frames (N = frames in flight) to prevent GPU aliasing
```

**Persistent state textures** (feedback buffers, accumulation) are NOT pooled — create with
`device.create_texture()` directly and manage lifetime manually.

## Async Compute (Parallel Layers)

```rust
// Each layer gets its own command buffer
let layer_cb = device.create_command_buffer("Layer 0");

// Encode all generator + effect work
// ...

// Signal event when done
layer_cb.encode_signal_event(&event, signal_value);
layer_cb.commit();  // MUST commit before compositor waits

// Compositor waits for all layer events
compositor_cb.encode_wait_event(&event, signal_value);
// Now safe to read layer textures
```

**Deadlock trap:** The signaling CB must be committed BEFORE the waiting CB. If you encode
`encodeSignalEvent` on a CB that hasn't been committed, Metal deadlocks.

## WGSL Shader Contract (Compute Effects)

```wgsl
// Standard binding layout for ComputeBlitHelper
struct Uniforms { /* matches Rust struct exactly */ }

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }  // Bounds check

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    // ... effect logic ...

    textureStore(output_tex, id.xy, result);
}
```

**Sampling rules:**
- `textureSampleLevel(tex, sampler, uv, 0.0)` — required in compute shaders
- `textureSample(tex, sampler, uv)` — preferred in fragment shaders (more efficient)
- `textureLoad(tex, coord, 0)` — no sampler, integer coords, no filtering

## Math Parity with Unity

```rust
// Rounding
x.round() as i32           // NOT: x as i32 (truncates)

// Lerp (clamp t)
a + (b - a) * t.clamp(0.0, 1.0)

// Repeat (negative-safe)
t - (t / len).floor() * len  // NOT: t % len

// Sign(0)
if x >= 0.0 { 1.0 } else { -1.0 }  // NOT: x.signum() (returns 0 for 0)
```
