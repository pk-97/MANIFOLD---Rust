# Adding Effects and Generators

Registration uses the `inventory` crate for distributed self-registration.
Each processor declares its metadata and factory in its implementation file.
No central registry editing required.

## New Effect — 2 files

1. **`manifold-renderer/src/effects/my_effect.rs`** — NEW FILE: implement `PostProcessEffect` trait + add two `inventory::submit!` blocks (EffectMetadata + EffectFactory)
2. **`manifold-renderer/src/effects/mod.rs`** — Add `pub mod my_effect;`

```rust
// At the top of my_effect.rs, after imports:
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::ParamSpec;
use crate::effects::registration::EffectFactory;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::new("MyEffect"),
        display_name: "My Effect",
        category: "Post-Process",  // "Spatial", "Post-Process", "Filmic", "Surveillance"
        available: true,
        osc_prefix: "myEffect",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("Amount", 0.0, 1.0, 0.5, "F2", ""),
        ],
    }
}

inventory::submit! {
    EffectFactory {
        id: EffectTypeId::new("MyEffect"),
        create: |device| Box::new(MyEffectFX::new(device)),
    }
}
```

## New Generator — 2 files

1. **`manifold-renderer/src/generators/my_gen.rs`** — NEW FILE: implement `Generator` trait + add two `inventory::submit!` blocks (GeneratorMetadata + GeneratorFactory)
2. **`manifold-renderer/src/generators/mod.rs`** — Add `pub mod my_gen;`

```rust
// At the top of my_gen.rs, after imports:
use manifold_core::GeneratorTypeId;
use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};
use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::new("MyGen"),
        display_name: "My Generator",
        is_line_based: false,
        available: true,
        osc_prefix: "myGen",
        legacy_discriminant: None,
        params: &[
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
        ],
        string_params: &[],
    }
}

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::new("MyGen"),
        create: |device| Box::new(MyGenGenerator::new(device)),
    }
}
```

## ParamSpec Helpers

All `const fn`, usable in static contexts:
- `ParamSpec::continuous(name, min, max, default, fmt, osc_suffix)` — float slider
- `ParamSpec::toggle(name, min, max, default, osc_suffix)` — on/off
- `ParamSpec::whole(name, min, max, default, osc_suffix)` — integer
- `ParamSpec::whole_labels(name, min, max, default, &labels, osc_suffix)` — integer with named values

## Optional: Type ID Constants

If other code needs to reference the type ID (e.g., compositor, project loading),
add a const to the appropriate type ID file:

```rust
// generator_type_id.rs
pub const MY_GEN: Self = Self(Cow::Borrowed("MyGen"));

// effect_type_id.rs
pub const MY_EFFECT: Self = Self(Cow::Borrowed("MyEffect"));
```

This is optional — new processors can use `GeneratorTypeId::new("MyGen")` inline.

## How It Works

The `inventory` crate collects all `submit!` blocks across the entire binary at link time.
At startup, the registries in `manifold-core` iterate `inventory::iter::<GeneratorMetadata>`
to build the definition and type registration maps. The factories in `manifold-renderer`
do the same with `GeneratorFactory`/`EffectFactory` to build the creation maps.
