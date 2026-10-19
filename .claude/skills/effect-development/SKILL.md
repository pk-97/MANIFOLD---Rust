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
1. manifold-nodes/src/effects/my_effect.rs    (NEW FILE — impl + registration)
2. manifold-nodes/src/effects/mod.rs           pub mod my_effect;
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

Compute work lives in a primitive (`docs/ADDING_PRIMITIVES.md`): its `run` method binds the source, target and uniform bytes and submits one dispatch on the `GpuEncoder`. Pad uniform structs to 16 bytes.

**WGSL compute layout:**
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

**Key patterns:**
- **Function constants**: Create specialized pipelines per mode at init time:
  `device.create_specialized_compute_pipeline(SHADER, "cs_main", &[("uniforms.mode", "0u")], label)`
- **Per-owner state**: `AHashMap<i64, State>` keyed by `owner_key` from `EffectContext`
- **Texture pooling**: `RenderTarget::new_pooled(pool, w, h, format, label)` — auto-recycled
- **State cleanup**: Implement `StatefulEffect` trait for `cleanup_owner()` / `clear_state_for_owner()`
- **Each dispatch uses its own uniform values** — never share a single buffer across passes

## Dispatch

Workgroup dispatch is always `[width.div_ceil(16), height.div_ceil(16), 1]`.
