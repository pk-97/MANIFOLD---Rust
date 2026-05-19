# Adding a primitive

Authoring guide for the `primitive!` macro. Companion to [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) (design rationale + decomposition recipes) and [NODE_CATALOG.md](NODE_CATALOG.md) (the catalog of what's shipping today).

Primitives auto-register via `inventory::submit!` from inside the macro — dropping a file under `crates/manifold-renderer/src/node_graph/primitives/` is the only step required. Nothing else has to be edited to register a new primitive; `cargo build` picks it up and the palette + bundled-preset loader see it on next startup.

## When to add one

The `≥2-use` filter applies (design doc §1.2):

- **Yes**: the math appears in 2+ existing effects/generators, OR the primitive replaces an existing effect 1:1 (the "the effect IS the primitive" case).
- **No**: speculative future need, or only one caller and not a 1:1 effect replacement.

## Files you touch per primitive

| File | Why |
|---|---|
| `crates/manifold-renderer/src/node_graph/primitives/<name>.rs` | The `primitive!` declaration + `Primitive::run` body |
| `crates/manifold-renderer/src/node_graph/primitives/shaders/<name>.wgsl` | Compute shader (only if your primitive runs GPU work — control-rate primitives like `value`/`math`/`lfo` don't need shaders) |
| `crates/manifold-renderer/src/node_graph/primitives/mod.rs` | `pub mod <name>;` to include the file |
| `crates/manifold-renderer/tests/parity_<effect>.rs` | Parity test vs the legacy effect this replaces (only when replacing a legacy fused shader) |

That's it. The macro generates the `EffectNode` impl, type-id constants, `PrimitiveSpec` metadata, the AI-surface `PrimitiveDescription`, and the `inventory::submit!` registration for the auto-populated palette.

## Skeleton

```rust
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: Invert,
    type_id: "node.invert",
    purpose: "Invert RGB channels, blended against the source by intensity. The `intensity` input port is the standard port-shadow — wire any scalar producer to drive the blend in real time.",
    inputs: {
        in: Texture2D required,
        intensity: ScalarF32 optional,
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
    composition_notes: "1:1 replacement for the legacy InvertColors effect.",
    examples: ["preset.effect.invert"],
    picker: { label: "Invert", category: Color },
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
    type_id: "node.<name>",             // stable string; renaming breaks saved graphs
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
    picker: { label: "<Display>", category: <Color|Spatial|Stylize|Filmic|Driver|Math|Source|Diagnostic> },
    extra_fields: {
        <field>: <type> = <init expr>,  // additional struct fields beyond pipeline/sampler
        ...
    },
}
```

- `<PortType>` is one of `Texture2D`, `Texture3D`, `ScalarF32`, `ScalarV2`, `ScalarV3`. Scalar input ports are first-class — use them directly in the macro (no manual `ParamDef` workaround needed).
- Inputs default to `required`; mark optional with the `optional` keyword.
- **Port-shadows-param convention.** If you declare a scalar input port with the same name as a `ParamDef` (e.g. `gain` in both `inputs:` and `params:`), the wire wins when present, the param is the fallback. Standard pattern for any control-rate modulation. The graph editor disables the expose checkbox + value cell on wire-driven rows automatically.
- `picker: { label, category }` declares how the palette and effect-card UI surface this primitive. Categories used today: `Color`, `Spatial`, `Stylize`, `Filmic`, `Driver` (texture→scalar bridges), `Math` (scalar arithmetic / LFO / BeatGate), `Source` (constants / generators), `Diagnostic`.
- `composition_notes`, `examples`, `picker`, and `extra_fields` are optional. Omit the keyword entirely if you don't need it.

### Stateful primitives

Per-frame state (a previous frame's texture, a one-pole filter's running value, a background worker handle) lives in the `StateStore` keyed by `(owner_key, node_id)`. Two patterns:

- **Single-instance state on the primitive struct** — use `extra_fields:` to add fields. Fine for state that doesn't survive an effect-chain rebuild. `Feedback`'s pipeline / sampler caches use this.
- **StateStore-backed state** — implement `run` with `EffectNodeContext::state_mut::<MyState>()`. State survives chain rebuilds as long as `owner_key` is stable. `Feedback`'s `prev: RenderTarget` and `Smoothing`'s `previous: f32` use this. The chain runtime threads the `StateStore` through `execute_frame_with_state`.

Stateful primitives must also wire up `clear_state` so seek / layer-idle resets clear properly — see [EFFECT_CHAIN_LIFECYCLE.md](EFFECT_CHAIN_LIFECYCLE.md).

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
