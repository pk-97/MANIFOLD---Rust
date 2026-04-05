# Adding Effects and Generators

## New Effect — 7 locations

1. **`manifold-core/src/effect_type_id.rs`** — Add `pub const MY_EFFECT: Self = Self(Cow::Borrowed("MyEffect"));` + update `from_legacy_discriminant()` if needed
2. **`manifold-core/src/effect_type_registry.rs`** — Add to `build_registry()` vec: `reg(E::MY_EFFECT, "My Effect", POST_PROCESS, true)`
3. **`manifold-core/src/effect_definition_registry.rs`** — Add `EffectDef` in `build_definitions()` with param defs (use `pd()`, `pd_osc()`, `pd_whole()`, `pd_whole_labels()`, `pd_toggle()` helpers)
4. **`manifold-core/src/effect_category_registry.rs`** — (Optional) Add category if not POST_PROCESS
5. **`manifold-renderer/src/effects/my_effect.rs`** — NEW FILE: implement `PostProcessEffect` trait. Use `ComputeBlitHelper` (1 input) or `ComputeDualBlitHelper` (2 inputs). See `bloom.rs` as template.
6. **`manifold-renderer/src/effects/mod.rs`** — Add `pub mod my_effect;`
7. **`manifold-renderer/src/effect_registry.rs`** — Add `Box::new(MyEffectFX::new(device))` in `EffectRegistry::new()`

## New Generator — 6 locations

1. **`manifold-core/src/generator_type_id.rs`** — Add `pub const MY_GEN: Self = Self(Cow::Borrowed("MyGen"));` + update `from_legacy_discriminant()` if needed
2. **`manifold-core/src/generator_type_registry.rs`** — Add to `build_registry()` vec: `reg(G::MY_GEN, "My Generator", true)`
3. **`manifold-core/src/generator_definition_registry.rs`** — Add `GeneratorDef` in `build_definitions()` via `create_def("My Generator", is_line_based, "osc_prefix", params)`
4. **`manifold-renderer/src/generators/my_gen.rs`** — NEW FILE: implement `Generator` trait. See `plasma.rs` (compute) or `lissajous.rs` (line-based) as template.
5. **`manifold-renderer/src/generators/mod.rs`** — Add `pub mod my_gen;`
6. **`manifold-renderer/src/generators/registry.rs`** — Add to `prewarm_all()` array AND `create()` if-else chain
