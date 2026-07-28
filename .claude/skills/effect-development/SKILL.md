---
name: effect-development
description: Complete pattern for adding or modifying effects — Bloom (multi-pass) and Mirror (simple) as templates. Invoke before any effect work alongside the mandatory docs/DECOMPOSING_GENERATORS.md read.
---
# Effect Development Guide

Two patterns: **simple** (stateless, single dispatch) and **complex** (stateful, multi-pass).
Always read the actual shader code before modifying. Never synthesize from descriptions.

## Registration (inventory-based, as of 2026-04-09)

Touch exactly **2 files** to add a new effect:

```
1. manifold-renderer/src/effects/my_effect.rs    (NEW FILE — impl + registration)
2. manifold-renderer/src/effects/mod.rs           pub mod my_effect;
```

All metadata and factory registration lives in the implementation file via `inventory::submit!`:

```rust
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;
use crate::effects::registration::EffectFactory;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::MY_EFFECT,  // or EffectTypeId::new("MyEffect")
        display_name: "My Effect",
        category: "Post-Process",     // "Spatial", "Post-Process", "Filmic", "Surveillance"
        available: true,
        osc_prefix: "myEffect",
        legacy_discriminant: None,     // Some(N) only for old project compat
        params: &[
            ParamSpec::continuous("Amount", 0.0, 1.0, 0.5, "F2", ""),
            ParamSpec::whole_labels("Mode", 0.0, 2.0, 0.0,
                &["Normal", "Inverted", "Additive"], "Mode"),
        ],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::MY_EFFECT,
        create: |device| Box::new(MyEffectFX::new(device)),
    }
}
```

ParamSpec is shared between generators and effects — same helpers:
`ParamSpec::continuous`, `ParamSpec::toggle`, `ParamSpec::whole`, `ParamSpec::whole_labels`.

If other code needs to reference this effect's type ID, add a const to
`effect_type_id.rs` — but this is optional for new effects.

## Pattern A: Simple Stateless Effect (Mirror template)

```rust
use manifold_gpu::{GpuDevice, GpuEncoder, GpuTexture, GpuComputePipeline};
use crate::effects::compute_blit_helper::ComputeBlitHelper;

const SHADER: &str = include_str!("shaders/my_effect.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MyUniforms {
    amount: f32,
    mode: u32,
    _pad: [f32; 2],    // Pad to 16-byte boundary
}

pub struct MyEffectFX {
    helper: ComputeBlitHelper,
}

impl MyEffectFX {
    pub fn new(device: &GpuDevice) -> Self {
        Self { helper: ComputeBlitHelper::new(device, SHADER, "My Effect") }
    }
}

impl PostProcessEffect for MyEffectFX {
    fn effect_type(&self) -> &EffectTypeId { &EffectTypeId::MY_EFFECT }

    fn apply(&mut self, gpu: &mut GpuEncoder, source: &GpuTexture, target: &GpuTexture,
             fx: &EffectInstance, ctx: &EffectContext) {
        let amount = fx.param_values.first().copied().unwrap_or(0.5);
        let mode = fx.param_values.get(1).copied().unwrap_or(0.0).round() as u32;

        let uniforms = MyUniforms { amount, mode, _pad: [0.0; 2] };

        self.helper.dispatch(
            gpu, source, target,
            bytemuck::bytes_of(&uniforms),
            "My Effect", ctx.width, ctx.height,
        );
    }
}
```

**Matching WGSL shader:**
```wgsl
struct Uniforms { amount: f32, mode: u32, _pad0: f32, _pad1: f32 }

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    // ... effect logic ...
    textureStore(output_tex, id.xy, result);
}
```

## Pattern B: Stateful Multi-Pass Effect (Bloom template)

Additional elements beyond Pattern A:

```rust
pub struct BloomFX {
    helper: ComputeDualBlitHelper,    // Dual-source (two input textures)
    pipeline_prefilter: GpuComputePipeline,   // Specialized per mode
    pipeline_downsample: GpuComputePipeline,
    pipeline_upsample: GpuComputePipeline,
    pipeline_composite: GpuComputePipeline,
    states: AHashMap<i64, BloomState>,  // Per-owner state
    width: u32, height: u32,
}
```

**Key patterns:**
- **Function constants**: Create specialized pipelines per mode at init time:
  `device.create_specialized_compute_pipeline(SHADER, "cs_main", &[("uniforms.mode", "0u")], label)`
- **Per-owner state**: `AHashMap<i64, State>` keyed by `owner_key` from `EffectContext`
- **Texture pooling**: `RenderTarget::new_pooled(pool, w, h, format, label)` — auto-recycled
- **State cleanup**: Implement `StatefulEffect` trait for `cleanup_owner()` / `clear_state_for_owner()`
- **Each dispatch uses its own uniform values** — never share a single buffer across passes

## ComputeBlitHelper Dispatch

```rust
// Single-source (read source, write target):
helper.dispatch(gpu, source, target, uniform_bytes, label, width, height);

// With specific pipeline (for specialized variants):
helper.dispatch_with(&pipeline, gpu, source, target, uniform_bytes, label, w, h);
```

**ComputeDualBlitHelper** adds a second source texture:
```rust
// Dual-source (read source_a + source_b, write target):
helper.dispatch_with(&pipeline, gpu, source_a, source_b, target, uniform_bytes, label, w, h);
```

Workgroup dispatch is always `[width.div_ceil(16), height.div_ceil(16), 1]`.
