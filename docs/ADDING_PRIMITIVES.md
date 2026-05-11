# Adding a primitive

Phase 4a authoring guide. Companion to [`PRIMITIVE_LIBRARY_DESIGN.md`](PRIMITIVE_LIBRARY_DESIGN.md).

## When to add one

The `≥2-use` filter applies (design doc §1.2):

- **Yes**: the math appears in 2+ existing effects/generators, OR the primitive replaces an existing effect 1:1 (the "the effect IS the primitive" case).
- **No**: speculative future need, or only one caller and not a 1:1 effect replacement.

## Files you touch per primitive

| File | Why |
|---|---|
| `crates/manifold-renderer/src/node_graph/primitives/<bucket>.rs` | The `primitive!` declaration + `Primitive::run` body |
| `crates/manifold-renderer/src/node_graph/primitives/shaders/<name>.wgsl` | Compute shader |
| `crates/manifold-renderer/src/node_graph/primitives/mod.rs` | Public re-export |
| `crates/manifold-renderer/tests/parity_<effect>.rs` | Parity test vs the legacy effect this replaces (only when replacing an effect) |

That's it. The macro generates the `EffectNode` impl, type-id constants, `PrimitiveSpec` metadata, and the AI-surface `PrimitiveDescription`.

## Skeleton

```rust
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Invert,
    type_id: "primitive.invert",
    purpose: "Inverts RGB channels, blended against the source by intensity.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "intensity",
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 replacement for legacy InvertColors effect.",
    examples: ["preset.effect.invert"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

impl Primitive for Invert {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let intensity = match ctx.params.get("intensity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let Some(in_tex) = ctx.inputs.texture_2d("in") else { return };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else { return };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/invert.wgsl"),
                "cs_main",
                "primitive.invert",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = InvertUniforms { intensity, _pad: [0.0; 3] };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Texture { binding: 3, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "primitive.invert",
        );
    }
}
```

The `primitive!` declaration generates:

- `pub struct Invert { pub pipeline: ..., pub sampler: ... }` with `new()` and `Default`.
- `impl PrimitiveSpec for Invert` with all const arrays, AI metadata, and a `cached_type_id()` backed by a per-primitive `OnceLock`.
- The blanket `impl<P: Primitive> EffectNode for P` in `node_graph::primitive` then provides the full `EffectNode` surface — no further trait impl needed.

## Macro reference

```text
primitive! {
    name: <StructName>,                 // PascalCase struct identifier
    type_id: "primitive.<name>",        // stable string; renaming breaks saved graphs
    purpose: "<one sentence>",          // shown in editor + AI surface
    inputs: {
        <port_name>: <PortType> [required|optional],
        ...
    },
    outputs: {
        <port_name>: <PortType>,
        ...
    },
    params: [
        ParamDef { name: ..., label: ..., ty: ParamType::..., default: ParamValue::..., range: ..., enum_values: &[] },
        ...
    ],
    composition_notes: "<optional>",    // when to choose this over alternatives
    examples: [ "<preset_id>", ... ],   // preset graphs that use this primitive
    extra_fields: {
        <field>: <type> = <init expr>,  // additional struct fields beyond pipeline/sampler
        ...
    },
}
```

- `<PortType>` is one of `Texture2D`, `Texture3D`. Scalar ports use the `Scalar(<sub>)` variant — write them via a manual `ParamDef` for now.
- Inputs default to `required`; mark optional with the `optional` keyword.
- `composition_notes`, `examples`, and `extra_fields` are all optional. Omit the keyword entirely if you don't need it.

## Parity test (only if you're replacing a legacy effect)

`crates/manifold-renderer/tests/parity_invert.rs`:

```rust
mod parity;

use manifold_core::EffectTypeId;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

#[test]
fn invert_decomposes_pixel_exactly_across_all_fixtures() {
    let mut h = ParityHarness::new();
    let fx = make_default_effect(EffectTypeId::INVERT_COLORS);
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);
        let legacy = h.run_legacy(&fx, &input, &ctx);
        let decomposed = h.run_primitive_graph::<Invert>(&fx, &input, &ctx);
        assert_bytewise_equal(
            &format!("invert/{:?} legacy vs primitive", fixture),
            &legacy,
            &decomposed,
        );
    }
}
```

(`run_primitive_graph` will be added to the harness when the first migration lands.)

## What NOT to do

- **Don't write `impl EffectNode for <Name>` by hand.** The blanket impl in `node_graph::primitive` covers it. Hand-written `EffectNode` impls in `primitives/*.rs` predate the macro and will be migrated in place.
- **Don't generate the WGSL.** Author it directly; `cargo test` runs `tests/wgsl_validation.rs` which validates every shader via naga.
- **Don't skip the parity test when replacing an existing effect.** Strict bit-equality is the gate.
- **Don't add a primitive for speculative future use.** The `≥2-use` filter is enforced at review time.

## When fusion compiler arrives

Per design doc §1, five effects ship as fused composite primitives today (EdgeDetect, Glitch, Strobe, VoronoiPrism, ChromaticOffset) because their multi-pass decomposition would break bit-equality. Once the fusion compiler can re-merge adjacent pixel-local primitives into one dispatch:

1. Add the atomic primitives (`BlockDisplace`, `Scanline`, etc.) as new entries.
2. Author preset graphs that compose them (`Glitch = Hash → BlockDisplace → Scanline → ChromaticOffset`).
3. Keep the fused composite primitives for backward compatibility — old projects keep loading.
4. New presets opt into the atomic chain; the fusion compiler restores single-pass perf.

Decomposition is purely additive. No flag-day migration of old presets is required.
