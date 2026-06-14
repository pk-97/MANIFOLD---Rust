# Adding Effects and Generators

Effects and generators ship through the same path: a JSON preset file. Both went through the JSON migration (generators are fully migrated — zero Rust generators remain), and both load from disk at startup, not from the compiled binary.

---

## Adding an Effect — drop a JSON file

A new effect is one file: `crates/manifold-renderer/assets/effect-presets/<TypeId>.json`. The preset loader (`preset_loader.rs`) scans that directory at startup and builds the catalog at runtime — the binary embeds no preset JSON, there's no `build.rs` codegen, no central registry edit, and no Rust to write. While the app is running, edits to a preset JSON hot-reload live (no rebuild, no restart) via the catalog's `ArcSwap` snapshot + file watcher.

If your effect can be expressed by composing primitives that already exist, that's the whole step. If it needs a new atomic operation (new shader, new shape of compute work), add a primitive first ([ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md)) and then reference it from your JSON.

### Minimal preset JSON

```json
{
  "version": 2,
  "name": "InvertColors",
  "description": "Per-pixel colour invert.",
  "presetMetadata": {
    "id": "InvertColors",
    "displayName": "Invert",
    "category": "Color",
    "oscPrefix": "invert",
    "available": true,
    "params": [
      {
        "id": "amount",
        "name": "Amount",
        "min": 0.0, "max": 1.0, "defaultValue": 1.0,
        "wholeNumbers": false, "isToggle": false,
        "formatString": "F2"
      }
    ],
    "bindings": [
      {
        "id": "amount",
        "label": "Amount",
        "defaultValue": 1.0,
        "target": { "kind": "handleNode", "handle": "invert", "param": "intensity" },
        "convert": { "type": "Float" }
      }
    ],
    "skipMode": { "kind": "onZero", "paramId": "amount" }
  },
  "nodes": [
    { "id": 0, "typeId": "system.source", "handle": "source" },
    { "id": 1, "typeId": "node.invert", "handle": "invert",
      "params": { "intensity": { "type": "Float", "value": 1.0 } } },
    { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
  ],
  "wires": [
    { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
    { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
  ]
}
```

### Schema cheat sheet

- **`version`** — `2` for current schema.
- **`name`** — internal name, kebab/PascalCase. Match the filename stem.
- **`presetMetadata.id`** — the `EffectTypeId` (must equal the filename stem).
- **`presetMetadata.category`** — one of `Spatial | Color | Stylize | Filmic | Diagnostic`. Drives the effect-browser grouping.
- **`presetMetadata.oscPrefix`** — kebab/snake-case prefix for OSC routing (`/manifold/<oscPrefix>/<paramId>`).
- **`presetMetadata.available`** — set `false` to hide from the picker but still load saved projects.
- **`presetMetadata.params`** — the card-UI slider list. Each entry is one effect-card slider.
- **`presetMetadata.bindings`** — how each slider routes to inner state. `target.kind: "handleNode"` is the common case; `handle` is the inner node's `handle` string and `param` is the param name on that primitive.
- **`presetMetadata.skipMode`** — optimisation hint. `{"kind": "onZero", "paramId": "..."}` enables zero-cost skip-passthrough when that slider is at zero. Omit if every slider is load-bearing.
- **`nodes[].typeId`** — must reference a registered primitive (browseable at `crates/manifold-renderer/src/node_graph/primitives/`) or one of the system nodes (`system.source`, `system.final_output`).
- **`nodes[].handle`** — a string label used by bindings and wires. Must be unique within the preset.
- **`nodes[].params`** — initial param values for this instance. Format is the same tagged-enum used everywhere: `{"type": "Float", "value": 0.5}`, `{"type": "Enum", "value": 2}`, `{"type": "Int", "value": 24}`, `{"type": "Vec2", "value": [0.5, 0.2]}`.
- **`wires`** — connections between ports. `fromPort` / `toPort` must exist on the referenced primitive's port list.

### ParamConvert variants

The `convert` field on each binding tells the runtime how to map the card-slider float into the inner-node param value. Four variants:

- **`Float`** — direct passthrough (slider 0.5 → param 0.5).
- **`IntRound`** — round-to-int (slider 4.7 → param 5).
- **`EnumRound`** — round-to-int into an enum index (same wire shape, separate variant for typed clarity).
- **`BoolThreshold`** — `value >= 0.5 → 1`, else `0`.

Static (registry) and user (per-instance exposed) bindings both run through the same enum — see §7.11 of [EFFECT_RUNTIME_UNIFICATION.md](EFFECT_RUNTIME_UNIFICATION.md).

### Validation

The loader does structural checks only — every file must parse and carry a `version`. Deeper validation runs when a preset is instantiated:

- Every `typeId` in `nodes` references a registered primitive.
- Every `bindings.target.handle` resolves to a node in the graph.
- Every `wires` endpoint references a valid `(node, port)` pair with matching types.
- The graph is a DAG (no cycles).

The test `every_bundled_preset_loads_validates_and_compiles` in `bundled_presets.rs` runs all bundled presets through `validate(&graph)` and the Metal pipeline build — if you add a preset that's structurally broken, that test catches it before any user sees it.

### Tests

- **Cheap:** `cargo test -p manifold-renderer --lib bundled_preset` — loads + validates + compiles every preset.
- **Per-preset GPU parity** (optional): the `composites/` Rust builders carry pixel-exact parity tests against legacy fused shaders for the 6 grandfathered presets. New JSON presets don't need this unless they're replacing a legacy fused shader.

### Real examples to crib from

| File | Pattern |
|---|---|
| `InvertColors.json` | Minimal one-primitive preset |
| `ChromaticAberration.json` | Decomposed UV-warp: `radial_offset_field → math → chromatic_displace → mix`; multi-slider with `EnumRound` for the mode |
| `EdgeGlow.json` | Two-stage chain: EdgeDetect → Threshold → Mix |
| `StylizedFeedback.json` | Stateful (Feedback) loop: `feedback → affine_transform → gain → vignette → mix` — the canonical feedback-trail preset |
| `Glitch.json` | Scalar-wire-driven control: `node.value` fans `amount`/`speed` into `block_displace_field` + `scanline_jitter_field` + the chromatic split |
| `ColorCompass.json` | Four texture→scalar bridges driving AffineTransform translate ports |
| `Strobe.json` | Decomposed: `node.beat_gate` (reads `FrameTime.beats`) → `node.flash` (3-mode brightness modulator) — no fused primitive |

---

## Adding a Generator — inventory::submit! (legacy path)

Generators have **not** yet migrated to the JSON workflow. They still ship through the original `inventory::submit!` pattern, with one `.rs` file per generator plus a `pub mod` line in `generators/mod.rs`.

### Two files

1. **`crates/manifold-renderer/src/generators/<name>.rs`** — implement the `Generator` trait + two `inventory::submit!` blocks (`GeneratorMetadata` + `GeneratorFactory`).
2. **`crates/manifold-renderer/src/generators/mod.rs`** — `pub mod <name>;` so the file is part of the crate.

### Template

```rust
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

### ParamSpec helpers

All `const fn`, usable in static contexts:

- `ParamSpec::continuous(name, min, max, default, fmt, osc_suffix)` — float slider
- `ParamSpec::toggle(name, min, max, default, osc_suffix)` — on/off
- `ParamSpec::whole(name, min, max, default, osc_suffix)` — integer
- `ParamSpec::whole_labels(name, min, max, default, &labels, osc_suffix)` — integer with named values

### Optional: type ID constants

If other code needs to reference the type ID (e.g., compositor branching, project loading), add a `const` in `generator_type_id.rs`:

```rust
pub const MY_GEN: Self = Self(Cow::Borrowed("MyGen"));
```

Optional — new generators can use `GeneratorTypeId::new("MyGen")` inline.

### How it works

The `inventory` crate collects all `submit!` blocks across the entire binary at link time. At startup, `manifold-core` iterates `inventory::iter::<GeneratorMetadata>` to build the definition map. The `manifold-renderer` factory registry does the same with `GeneratorFactory` to build the creation map.

### Future migration

Generators will eventually follow effects onto a JSON-authoritative workflow under `assets/generator-presets/` once the per-frame state shapes and Buffer-port story are finalised. Don't pre-architect for that — write generators using today's pattern; the migration will be a coordinated sweep, not a per-generator burden.

---

## References

- [NODE_GRAPH_SYSTEM.md](NODE_GRAPH_SYSTEM.md) — graph runtime and preset architecture
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — authoring a new primitive (the atoms JSON presets reference)
- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — primitive catalog, decomposition recipes
- [EFFECT_RUNTIME_UNIFICATION.md](EFFECT_RUNTIME_UNIFICATION.md) §7.11 — bindings unification (one ResolvedBinding, one ParamConvert)
- `crates/manifold-renderer/src/preset_loader.rs` — disk scan, catalog build, fail-loud rules, hot-reload watcher
- `crates/manifold-renderer/src/node_graph/bundled_presets.rs` — thin lookup over the disk-loaded catalog + the `every_bundled_preset_loads_validates_and_compiles` test
