---
name: parameter-wiring
description: End-to-end parameter flow from definition to shader — the full chain for effects and generators. Invoke when a param doesn't reach the shader or when adding params.
---
# Parameter Wiring Reference

Complete chain for how a parameter goes from definition → UI → content thread → GPU shader.

## The Full Chain (Effect Example: Bloom "Amount")

```
1. DEFINITION (manifold-core/src/effect_definition_registry.rs)
   pd("Amount", 0.0, 5.0, 0.187)
   → ParamDef { name: "Amount", min: 0.0, max: 5.0, default_value: 0.187 }

2. PROJECT FILE (JSON, camelCase)
   "baseParamValues": [0.187]    ← user's saved value
   "paramValues": [0.187]        ← runtime copy (may be modulated)

3. UI (effect card slider)
   User drags slider → writes to base_param_values[0] via ContentCommand
   ⚠ MUST write to base_param_values, NOT param_values

4. CONTENT THREAD (playback engine tick)
   sync_clips_to_time() copies base_param_values → param_values
   (with any OSC/automation modulation applied on top)

5. EFFECT APPLY (manifold-renderer/src/effects/bloom.rs)
   let amount = fx.param_values.first().copied().unwrap_or(0.187);
   → feeds into uniform struct → GPU dispatch

6. SHADER (bloom_compute.wgsl)
   uniforms.intensity = amount (mapped in Rust before dispatch)
```

## Parameter Index Mapping

Parameters are accessed by INDEX, not by name. The index matches the order in `param_defs`:

```rust
// Definition order determines index
param_defs: vec![
    pd("Amount", ...),   // index 0
    pd("Rate", ...),     // index 1
    pd("Mode", ...),     // index 2
],

// Access in effect apply:
let amount = fx.param_values[0];   // "Amount"
let rate = fx.param_values[1];     // "Rate"
let mode = fx.param_values[2];     // "Mode"

// Access in generator render:
let speed = ctx.params[0];         // first param
let scale = ctx.params[1];         // second param
```

**If you reorder param_defs, all existing projects break.** Add new params at the end only.

## Effect vs Generator Parameter Differences

### Effect Parameters
```rust
// Definition helpers (effect_definition_registry.rs) — NO fmt/osc args:
pd("Amount", 0.0, 1.0, 0.5)
pd_whole_labels("Mode", 0.0, 2.0, 0.0, &["A", "B", "C"])

// Access at runtime:
fn apply(&mut self, ..., fx: &EffectInstance, ctx: &EffectContext) {
    let val = fx.param_values[INDEX];
    // OR with default:
    let val = fx.param_values.get(INDEX).copied().unwrap_or(DEFAULT);
}
```

### Generator Parameters
```rust
// Definition helpers (generator_definition_registry.rs) — HAS fmt/osc:
pd("Speed", 0.1, 5.0, 1.0, Some("F1"), "speed")
//                           ^fmt string  ^osc suffix
pd_toggle("Snap", 0.0, 1.0, 0.0, "snap")
pd_whole_labels("Mode", 0.0, 3.0, 0.0, &["A","B","C","D"], "mode")

// Access at runtime:
fn render(&mut self, ..., ctx: &GeneratorContext) -> f32 {
    let val = ctx.params[INDEX];
    // With bounds check:
    let val = if ctx.param_count > INDEX as u32 { ctx.params[INDEX] } else { DEFAULT };
}
```

## OSC Address Mapping

OSC addresses are auto-generated from the prefix:

```
Effect:    /manifold/effects/{osc_prefix}/{param_name}
           /manifold/effects/bloom/Amount     → param_values[0]

Generator: /manifold/generators/{osc_prefix}/{osc_suffix}
           /manifold/generators/plasma/speed  → params[0]
```

**Effect params use the param NAME in the OSC address.**
**Generator params use the osc_suffix field.**

## Serialization Convention

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectInstance {
    pub effect_type: EffectTypeId,        // → "effectType" in JSON
    pub base_param_values: Vec<f32>,      // → "baseParamValues"
    #[serde(skip)]
    pub param_values: Vec<f32>,           // NOT serialized (runtime copy)
}
```

- `rename_all = "camelCase"` on ALL serialized structs
- `#[serde(transparent)]` on typed IDs (`ClipId`, `LayerId`)
- `#[serde(skip)]` for runtime-only fields like `param_values`
- Getting camelCase wrong silently breaks project loading

## Common Mistakes

1. **Writing to `param_values` from UI** — gets overwritten next tick. Use `base_param_values`.
2. **Reordering param_defs** — breaks all existing projects. Add new params at end.
3. **Mismatched param_count** — `param_count` field must equal `param_defs.len()`.
4. **Using effect pd() signatures for generators** — generators need fmt and osc args.
5. **Integer params not rounded** — use `.round() as u32` when reading "whole" params.
