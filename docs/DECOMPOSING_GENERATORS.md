# Decomposing Generators — A Working Guide

**Status:** Living guide, written 2026-05-21 after Plasma + Lissajous shipped and the post-decomposition audit closed every 🔴/🟠 finding. Read it before starting any generator decomposition.

This guide is the *how-to-think*, not the *how-to-add-a-primitive*. Companion docs:

- [NODE_CATALOG.md](NODE_CATALOG.md) — the settled spec for atoms / effects. Source of truth for type IDs and what exists.
- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — design rationale (port types, state model, macro shape).
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — mechanics of writing a new primitive (`primitive!` macro, parity test pattern).
- [GENERATOR_DECOMPOSITION_PLAN.md](GENERATOR_DECOMPOSITION_PLAN.md) — strategic roadmap (which generator, in what order, what infra it needs).
- [AUDIT_GRAPH_GENERATOR_HOT_PATH.md](AUDIT_GRAPH_GENERATOR_HOT_PATH.md) — the bug classes the system used to have. Everything 🔴/🟠 is closed; read it to know what *not* to reintroduce.

---

## 1. Why we decompose

The graph node generator system is not a refactor. It is the surface through which:

- **AI agents (MCP / API) author looks** by composing curated primitives. The primitive catalog is their vocabulary; bigger curated vocabulary, better authoring.
- **Users drill in** to a generator and read the wiring, change a routing, swap a sub-graph — the same way Max for Live opens up an Ableton device.
- **The renderer stays honest.** A graph plus a primitive set is far easier to audit, test, and port than 20 single-file Rust generators with their own private state.

The trap: decomposing for its own sake. A generator that is one shader doing one thing (e.g. a custom raymarch) does not get clearer by being split into a 12-node graph of `wgsl_compute_*` nodes that each contain the same shader code as a slice. **If the decomposition does not yield genuinely reusable primitives, do not decompose.** Keep it as a Rust generator and document why in `GENERATOR_DECOMPOSITION_PLAN.md`.

The win condition: the legacy Rust generator gets deleted, the JSON preset is the only path, and the primitives that fell out are useful in *other* graphs too.

**Time budgeting**: the first decomposition that uses a new primitive family (Plasma → curated procedural texture; Lissajous → curve sources; WireframeZoo → 3D wireframe pipeline) pays a one-time *inherent* tax — building the new primitives that didn't exist (`wireframe_shape`, `EdgePair`, the `edges` input on `render_lines`). That's an investment, not waste — those primitives become the vocabulary the next generator in the family reuses.

What's *not* part of that tax is extending adjacent primitives for port-shadow — that work shouldn't exist. New primitives should ship port-shadow on every numeric param by default (see §6.2). The reason WireframeZoo needed `rotate_3d` and `project_3d` extended was that they were authored before this convention was established. Future primitives should be authored to this convention so the only first-time cost is the genuinely new functionality.

Subsequent decompositions in the same family are far cheaper. Tesseract / Duocylinder will inherit the entire 3D wireframe pipeline from WireframeZoo; they'll likely be JSON-only changes plus one or two 4D primitive additions. Don't budget the second decomposition like the first.

## 2. The mental model

A generator is a sub-graph from `system.generator_input` to `final_output`.

```
system.generator_input  ──►  [your nodes]  ──►  final_output
   ▲                                              ▲
   │                                              │
 time / beat / aspect /                       Texture2D
 trigger_count /
 anim_progress
```

- **Boundary nodes are non-negotiable.** Every generator preset must have `system.generator_input` (the source of frame-context scalars) and `final_output` (the texture sink). Multi-output return values like `anim_progress` are handled via a future `system.generator_output` shape if needed; for now, primitives can write to `anim_progress` via their `extra_fields`.
- **Outer-card params are top-level controls.** The user sees `Speed`, `Scale`, `Complexity` on the right-hand UI panel. Each top-level param has one or more **bindings** to inner-node params declared in `preset_metadata.bindings`. The same outer param can fan out to many inner targets (e.g. `Speed` → `LFO.rate_x`, `LFO.rate_y`, `feedback.zoom`).
- **State lives in primitives, not in the JSON.** Per-instance state (the previous frame's texture for `Feedback`, the smoothed value for `Smoothing`, the last-emitted index for `ClipTriggerCycle`) lives in the primitive struct via the `extra_fields: { ... }` macro slot. The JSON describes wiring and defaults. State is fresh on every generator rebuild (see §9 — `trigger_count` is preserved across rebuild, *cycle state is not*; this is intentional and acceptable for authoring-time edits).
- **Port-shadows-param is the modulation pattern.** Any primitive declaring a scalar input port with the same name as one of its params (e.g. `LFO.rate`, `Gain.gain`, `Feedback.amount`) will use the wire's value when present, and fall back to the param when nothing is wired. This is what makes the same primitive useful as both a static node and a control-rate-modulated one.

The runtime walks the graph plan once per frame on the content thread, dispatching primitive `run` calls. There are no per-frame allocations on the steady-state path (confirmed in the audit) — your job as a primitive author is to not regress that.

## 2.5 Precondition: audit by analogy before workflow step 1

Every decomposition you start has a closely-shaped predecessor already in the tree. Find it before writing anything. Three concrete steps, all read-only, all required:

1. **Survey what primitives exist.** Run `rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'` (and the `pub const *_TYPE_ID` form for composite-effect primitives). One line per node telling you what it does. The [NODE_CATALOG.md](NODE_CATALOG.md) groups these by intent — read the families relevant to your generator's shape. **The trap this prevents:** proposing a "new" primitive that already exists with a different name (`sobel_edge` re-inventing `convolution_2d_9tap` with Sobel kernels, `levels_remap` re-inventing `scale_offset_texture`, a fused mesh+PBR render duplicating the atomized PBR pattern shipped in OilyFluid).
2. **Identify the nearest reference preset and read it end-to-end.** Each shipping JSON preset embodies a canonical decomposition shape:

   | Shape | Reference preset |
   |---|---|
   | Pure procedural texture (curated family) | [Plasma.json](../crates/manifold-renderer/assets/generator-presets/Plasma.json) — single `plasma_pattern_2d` node |
   | Pure procedural texture (single-purpose atom) | [StarField.json](../crates/manifold-renderer/assets/generator-presets/StarField.json) |
   | Per-pixel SDF / shape rendering | [BasicShapes.json](../crates/manifold-renderer/assets/generator-presets/BasicShapes.json) |
   | Parametric curve rendering | [Lissajous.json](../crates/manifold-renderer/assets/generator-presets/Lissajous.json) — `lfo` + `frequency_ratio` + `generate_lissajous` + `render_lines` |
   | Beat-cycled shape morph (mux'd variants) | [ConcentricTunnel.json](../crates/manifold-renderer/assets/generator-presets/ConcentricTunnel.json) — `mux_scalar` driven by `trigger_count` |
   | 3D wireframe rendering | [WireframeZoo.json](../crates/manifold-renderer/assets/generator-presets/WireframeZoo.json) — `wireframe_shape` → `rotate_3d` → `project_3d` → `render_lines` |
   | 4D wireframe rendering | [Tesseract.json](../crates/manifold-renderer/assets/generator-presets/Tesseract.json) / [Duocylinder.json](../crates/manifold-renderer/assets/generator-presets/Duocylinder.json) — same shape with `rotate_4d` / `project_4d` |
   | 3D instanced mesh with cycled poses | [NestedCubes.json](../crates/manifold-renderer/assets/generator-presets/NestedCubes.json) — `cycle_table_row` + `mux_array` + `nested_cubes_geometry` |
   | 3D instanced mesh with procedural layout | [DigitalPlants.json](../crates/manifold-renderer/assets/generator-presets/DigitalPlants.json) — `grid_uv_field` + per-instance noise + `cylinder_wrap_field` + `digital_plants_render` |
   | Particle sim with deterministic ODE | [ComputeStrangeAttractor.json](../crates/manifold-renderer/assets/generator-presets/ComputeStrangeAttractor.json) — `seed_particles` → `integrate_particles_attractor` → `scatter_particles` → `resolve_accumulator` → `reinhard_tone_map` |
   | Particle fluid sim with ping-pong + force field | [FluidSim2D.json](../crates/manifold-renderer/assets/generator-presets/FluidSim2D.json) |
   | Screen-space surface with atomized PBR | [OilyFluid.json](../crates/manifold-renderer/assets/generator-presets/OilyFluid.json) — `heightmap_to_normal` + `lambert_directional` + `matcap_two_tone` + `fresnel_rim` + `blinn_specular` summed via `mix` |
   | Volumetric / image-folder scrubbing | [MriVolume.json](../crates/manifold-renderer/assets/generator-presets/MriVolume.json) — `image_folder` × 3 + `mux_texture` |
   | Multi-atom user-style composite (effect side) | [Mandala.json](../crates/manifold-renderer/assets/effect-presets/Mandala.json), [SmearMosh.json](../crates/manifold-renderer/assets/effect-presets/SmearMosh.json) |
   | Image-to-control bridge closing a loop | [ColorCompass.json](../crates/manifold-renderer/assets/effect-presets/ColorCompass.json) — `color_sample` → `math` → `smoothing` → `affine_transform` |
   | Chroma-keyed selective effect | [EdgeStretchByColor.json](../crates/manifold-renderer/assets/effect-presets/EdgeStretchByColor.json) — `chroma_key` → effect → `masked_mix` |

   Open the JSON, read every node, follow every wire. The reference preset is the proof-of-existence for the pattern you're about to use; until you've read it, you don't know what primitives the pattern already requires.
3. **Reconcile your sketch against §1 and §2.** If your sketch invents a primitive that the reference already covers, drop the invention. If your sketch invents a primitive that's *almost* but not exactly an existing one, the right move is to extend (see §6.2), not to add. Only after the audit gives you a list of (existing primitives you'll reuse, existing primitives you'll extend, genuinely new primitives you'll build) do you start workflow step 1.

This precondition is mandatory. Skipping it produces the "argue from snippets" anti-pattern — proposing six new primitives when three are already shipped and the fourth is a one-line extension of an existing atom. **Do not propose new primitives before completing this audit.**

## 3. The workflow

The order matters. Skipping a step is how you end up with a graph that "almost works" plus six bugs.

1. **Read the legacy generator end-to-end.** Not the docstring, not the shader comments — the whole file plus its `*.wgsl`. Identify: (a) the per-frame compute passes and their order, (b) every uniform / param the shader reads, (c) every piece of state that persists across frames (ping-pong textures, particle buffers, envelope accumulators, trigger-count edge detect), (d) every place the generator reads `ctx.trigger_count` or `ctx.anim_progress` or `ctx.beat`.
2. **Sketch the graph on paper before opening any file.** Boxes for each pass, arrows for textures and scalars. Mark which boxes are existing primitives, which need to be built, and which might be a single primitive that packs a family (the way `plasma_pattern_2d` packs 8 variants behind one `pattern` enum).
3. **Inventory missing primitives.** For each "needs to be built" box, decide: is it a *single-purpose* primitive (`generate_lissajous`, `frequency_ratio`) or a *curated family* primitive (`plasma_pattern_2d`)? Single-purpose if the math has one clear use and decomposing it further would just be `value + math` plumbing. Family if there are 5+ variants with a shared param surface and the user thinks of them as one knob.
4. **Build new primitives first, in their own commit.** Each new primitive ships with: parity test against legacy math (CPU mirror inside the test module, bit-exact or numerically-bounded), unit tests for its port declarations, and a `composition_notes` string in the `primitive!` macro slot (the AI authoring surface).
5. **Author the JSON preset.** Boundary node, internal nodes, wires, outer-card params, bindings, `paramAliases` if any outer-param names are migrating from the legacy generator's positional indices.
6. **Parity-test the whole graph.** Load both the legacy generator and the JSON preset side by side in the running app, run the same canonical fixture (`Liveschool Live Show V6 LEDS.manifold`), compare visually. Where bit-exact is achievable (Tier 1 / 2 in the plan), assert it. Where it isn't (RNG ordering in particle sims, fp16 rounding across many passes), document the bound.
7. **Migration aliases for renames.** If outer-card param names are different from the legacy generator's positional param indices, add a `paramAliases` table in `PresetMetadata` and / or a `GeneratorAliasMetadata` inventory submission so old projects load unchanged.
8. **Delete the legacy Rust generator.** The JSON preset is now the only path. The Rust file's deletion ships in the same PR as the preset. (We learned this the hard way with `plasma.rs` — it lingered as shadowed-dead code until the audit.)
9. **Commit clean. `cargo clippy --workspace -- -D warnings && cargo test --workspace` green.** A completed decomposition is wide-blast-radius by definition (legacy file deletion, registry change, often adjacent-primitive extension), so the full sweep is the right scope *for this step* — not because shipping triggers it, but because the change does. During the iteration leading up to this step, use the focused tests described in §3.1.

### 3.1 Iteration loop

The parity test for the effect you're migrating is the safety net you re-run on every change to the primitive or the JSON. `cargo test -p manifold-renderer --test parity <effect>::` runs that single effect's full fixture × setup sweep in seconds. Same shape for a single new primitive: `cargo test -p manifold-renderer --lib <module_path>::gpu_tests::`. The narrow run catches the failures that matter for the work in front of you.

**For JSON preset edits specifically, run `cargo run -p manifold-renderer --bin check-presets` before relaunching the app.** Sub-second, no GPU, no app launch. Walks every preset JSON in `assets/{effect,generator}-presets/` from disk and runs the same `into_graph` + `compile` pipeline the runtime and editor take — catches `UnknownParam` (the "unknown parameter 'foo'" log spam), `UnknownTypeId`, `ParamTypeMismatch`, `InvalidWire`, `RequiredInputUnwired`, cycles, output-slot sizing, and binding-id-vs-outer-param mismatches. This bug class otherwise only surfaces as "editor canvas empty" or "first frame grey, then black" at app launch — cheap to introduce when hand-editing JSON, slow to diagnose if skipped. The parity test catches the same class but requires GPU init and pays the full test-harness cost; `check-presets` is the iteration-loop tool.

Escalate to `cargo test --workspace` when the change's blast radius exceeds one effect or one primitive — touching the parity harness, the graph runtime, `manifold-gpu`, `manifold-core` effect / generator / param types, shared WGSL headers, `Cargo.lock`, or completing a decomposition (step 9). Touching the primitive or its preset is *not* wide-blast; touching the harness that runs every primitive's tests is. If you're unsure, it's infrastructure — run the full sweep.

Pre-push is **not** a trigger by itself. Pushes happen on every change here, so "before push" collapses into "always" and defeats the scope rule. The whole point of the focused tests is that they let small changes ship in seconds; gating every push on the workspace run gives that back. The cost of this discipline is that blast-radius judgment is now load-bearing — if you misjudge what reaches the parity-tested path, the bug lands. Bias toward escalation when uncertain.

The trap to avoid: running `cargo test --workspace` between every small edit because it feels safer. It's not safer — it just rewards slower iteration with slower iteration. The focused parity test is the same correctness contract scoped to what you actually changed.

## 4. The primitive vocabulary, grouped by intent

These are the shipping primitives you'll actually reach for when decomposing a generator. The full registry is wider; this is the subset that matters for *this* job. Citations: [crates/manifold-renderer/src/node_graph/primitives/](../crates/manifold-renderer/src/node_graph/primitives/).

### Control-rate scalar plumbing

Free to evaluate (no GPU dispatch). Use these for anything modulation-shaped.

- **`value`** — constant scalar source. Every outer-card slider routes through one of these.
- **`math`** — two-input scalar math (Add/Subtract/Multiply/Divide/Min/Max/Atan2). When you find yourself writing the same `value × value` chain twice, this is the primitive.
- **`lfo`** — low-frequency oscillator with `rate_mode: Musical | Free` (Musical follows `beat`, Free uses `time × angular_rate`). Sine / Tri / Saw / Square / S&H waveforms. `min` / `max` shape the output range.
- **`beat_gate`** — beat-synced 0/amount gate. Drives strobes, clip-triggered envelopes.
- **`smoothing`** — one-pole low-pass on a scalar (stateful, via `extra_fields`).
- **`envelope_follower_ar`** — attack/release envelope from a triggered impulse (stateful).
- **`affine_scalar`** — `value * a + b` on a scalar. Use for parameter rescaling at the wire (`Speed` slider 0..4 → `LFO.rate` 0..16).
- **`frequency_ratio`** — curated 10-row harmonic ratio table (1:1, 2:1, 3:2, …), indexed by clip-trigger count with the uniqueness invariant. The shape of "snap to a musical ratio" without per-graph scaffolding.
- **`mux_scalar`** — 8-way scalar router by integer selector. Sibling: `mux_texture`. Use when a clip-trigger should pick among precomputed values.

### Procedural texture sources

Per-pixel math producing a `Texture2D`. The Plasma family is the case study.

- **`uv` / `centered_uv` / `polar_field`** — coordinate primitives. Most procedural graphs start with one of these.
- **`distance_to_point` / `sin_term` / `power_texture` / `abs_texture` / `fract_texture` / `smoothstep_texture` / `scale_offset_texture` / `trig_texture`** — per-pixel math atoms. Compose these for arbitrary procedural fields.
- **`simplex_noise_2d` / `perlin_noise_2d` / `fbm_2d` / `voronoi_2d` / `flow_field_noise`** — noise sources.
- **`checkerboard` / `plasma_pattern_2d`** — *curated family* primitives. `plasma_pattern_2d` packs 8 algorithms behind one `pattern` enum + shared `complexity/contrast/speed/scale` params. Build one of these when the family is large enough that the per-atom decomposition is just rewriting the same plumbing 8 times.

### Image-domain effects

For when a primitive operates on an incoming `Texture2D`.

- **`gaussian_blur_variable_width` / `separable_gaussian` / `bloom` / `halation`** — blur family.
- **`edge_detect` / `chromatic_offset` / `kaleido_fold` / `quad_mirror`** — single-shader effects.
- **`affine_transform` / `rotate_2d`** — UV-space transforms.
- **`color` / `color_grade` / `tone_map` / `reinhard_tone_map` / `lut1d` / `infrared`** — color.
- **`compose`** — multi-mode blend (Lerp/Screen/Add/Max/Multiply/Difference/Overlay). The standard "combine two images" primitive.
- **`wet_dry_mix` / `masked_mix`** — crossfade variants.

### Texture → scalar bridges

Closes the image-to-control loop. One-frame readback latency.

- **`luminance` / `peak` / `color_sample`** — extract scalars from a texture. Pair with `gain` / `feedback.amount` / `math` for image-driven modulation.

### Line / curve / particle / mesh

The "geometry pipeline."

- **`generate_lissajous`** — single Lissajous curve sampling. Produces `Array<LinePoint>`.
- **`generate_grid_mesh` / `generate_cube_mesh` / `generate_platonic_solid` / `generate_tesseract_vertices` / `generate_duocylinder_vertices`** — mesh sources.
- **`generate_instance_transforms`** — instance buffer for instanced rendering.
- **`rotate_3d` / `rotate_4d` / `project_3d` / `project_4d` / `displace_mesh` / `triangulate_grid`** — transform/project stages.
- **`seed_particles` / `seed_particles_from_texture` / `integrate_particles` / `integrate_particles_attractor` / `scatter_particles` / `scatter_particles_3d` / `resolve_accumulator` / `resolve_3d_accumulator`** — particle simulation chain.
- **`render_lines` / `render_3d_mesh` / `render_instanced_3d_mesh`** — final rasterizers.

### Stateful (feedback / temporal)

These primitives hold state across frames. They require the `StateStore` path — the generator runtime now plumbs it (post-audit) so you can freely compose them.

- **`feedback`** — previous-frame texture accumulation.
- **`temporal`** — ping-pong texture primitive for arbitrary temporal patterns.
- **`array_feedback`** — same shape for `Array<T>` buffers (particle systems).
- **`smoothing` / `envelope_follower_ar`** — scalar-side temporal state (see Control-rate above).

### The escape hatch — `wgsl_compute_*`

`wgsl_compute_0in_1tex`, `wgsl_compute_1tex_1tex`, `wgsl_compute_2tex_1tex`. **Read §5 before reaching for these.**

---

## 5. The WGSL escape hatch — when it's right, when it's wrong

The `wgsl_compute_*` nodes let a JSON preset embed raw WGSL. The escape hatch is a sharp tool with a narrow remit: irreducible multi-pass coupled shaders. Most things that *feel* like §5 cases aren't.

### Where the escape hatch IS right

1. **Multi-pass shaders where both the inter-pass coupling AND the per-pass texture format choices are load-bearing.** Fluid simulations are the archetype: 7 passes with hand-tuned `r32float` / `rgba16float` / `rgba32float` boundaries chosen so a long frame-to-frame feedback loop stays numerically stable. Decomposing each pass into per-pixel-math primitives would (a) explode into hundreds of nodes and (b) compound rounding across each boundary at formats the legacy pipeline never used, degrading the sim. The legacy shader code lifts verbatim into a `wgsl_compute_*` node per pass; only the inter-pass wiring becomes JSON. This is Tier 3 in the decomposition plan.
2. **One-off compute kernels where the math has zero reuse potential.** A relativistic geodesic trace, a domain-specific reaction-diffusion update, a custom FFT layout — things where decomposing would mean writing eight new primitives that nothing else will ever call.

The qualifying word in (1) is **both**. Multi-pass alone doesn't qualify — most multi-pass shaders are just sequenced operations and decompose cleanly. Coupling+formats together is what makes a pass chain irreducible.

### Where the escape hatch is WRONG

- **Single-pass shaders that internally do multiple logical operations.** A compute pass doing "Sobel + mirror + levels + temporal blend" in one shader is a *decomposition target*, not a §5 case. It looks coupled because the operations share a kernel, but each is a per-pixel transform with no precision-load-bearing format choice between them. Atomize: Sobel → mirror → levels → temporal blend, with the intermediate texture format chosen so quantization isn't a regression vs the original's single quantization at the final write. fp32 intermediates plus matching arithmetic order get byte-exact parity in almost every case; the only theoretical drift is FMA reassociation across kernel boundaries, which is ~1 ULP and well inside any sane parity epsilon. **The trap:** convincing yourself the operations "must stay fused for parity" when the real reason is decomposition is more work.
- **Per-shader primitive wraps.** Lifting each generator's individual shaders into one primitive per shader (`MyGenerator_PassA`, `MyGenerator_PassB`, …) defeats the entire point of the library. The result is generator-specific palette entries that no future generator can reuse, the catalog bloats, and the AI authoring surface is poisoned with named-after-the-source primitives. §5 is for *passes within a single irreducible chain*, not "I have N shaders, I'll wrap each."
- **A generic per-pixel transform expressible through existing primitives.** Lissajous's curve sampling pre-decomposition was tempted-but-resisted — sampling a parametric curve plus emitting an `Array<LinePoint>` is one primitive, not a WGSL paste.
- **"Too much infra for this one generator."** The new primitive is almost always worth it — it earns its keep when the next generator needs the same shape. The audit precondition (§2.5) is how you verify the shape doesn't already exist before paying that cost.
- **Dodging graph complexity.** If the graph is feeling unwieldy, the answer is "build a higher-level primitive that packs the sub-graph," not "give up and paste in WGSL." See [feedback_curated_primitives_over_wgsl](../.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/feedback_curated_primitives_over_wgsl.md).

### The test: would the next generator want this primitive?

Apply this to anything you're tempted to call a §5 case. If the answer is "no, this primitive is only useful for this one generator," the path is decomposition into atoms (which other generators *will* want), not a per-shader wrap. The only exception is the genuinely irreducible chain — and "irreducible" means the precondition test in (1) above is met: both coupling and per-pass format choices are load-bearing.

### If you do reach for `wgsl_compute_*`

Treat the embedded WGSL as the contract: format declarations are load-bearing (per the per-slot format work in D5 of the decomposition plan), uniform layouts must match what the JSON binds, and the node's `composition_notes` field must call out the precision requirements so the next reader knows why the escape hatch is here.

## 6. Keep the graph small

Three forcing functions to keep graphs from sprawling:

### 6.1 Collapse to primitives when scaffolding repeats

If your graph has the same `value → math → value → math` chain three or more times — building offsets, scaling rates, packing a parameter family — you have invented a primitive without naming it. Stop and build the primitive. The Lissajous decomposition went from a 20-node "generate the curve from raw math" sketch to a 9-node graph by collapsing the curve math into `generate_lissajous` and the harmonic-ratio plumbing into `frequency_ratio`. The Plasma decomposition went from a sketched 100-node 8-variants-each-decomposed graph to a 3-node graph by packing the 8 algorithms into `plasma_pattern_2d`.

This is a hard rule and lives in [feedback_collapse_to_primitives](../.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/feedback_collapse_to_primitives.md). Watch for it.

### 6.2 Extend before you build

When a decomposition needs new behaviour and an existing primitive is close to it, extend the existing one. Add an optional input port, a mode enum, an output port. The bar to clear: the extension must be **simple** (one or two additive changes, not a rewrite) and **contained** (doesn't pollute the existing primitive's surface with concepts that have nothing to do with its core job). Both Plasma's `clip_trigger` cycling and Lissajous's port-shadowable inputs were added this way.

**Expect adjacent extensions to cascade — but they're avoidable for *new* primitives.** WireframeZoo needed `rotate_3d` and `project_3d` extended mid-decomposition because their numeric params were static-only. If those primitives had been authored with port-shadow on every `Float` / `Int` param from day one, no extension would have been needed. **Authoring rule for new primitives: every numeric scalar param ships as a port-shadowed optional input by default.** The only params that should stay non-port-shadowable are mode selectors (`Enum`, `Bool` operation flags) where wiring doesn't make sense.

Plan for cascading extensions in the inventory step (§3 step 3) when decomposing against existing primitives — every time-varying value the legacy generator computed needs a wirable port on the consumer primitive — but treat the need to extend an adjacent primitive as a *fix for a primitive that should have shipped port-shadow originally*, not as inherent decomposition cost.

Why extend over build:

- Every new primitive is a registry entry, a `composition_notes` string, parity tests, and a slot in the user's mental palette. Cheap individually, expensive at the catalog level.
- Adjacent primitives doing nearly the same job (`render_lines` and a hypothetical `render_wireframe`) split the future work — every line-rendering improvement has to land in two places.
- Optional input ports compose cleanly: when the wire is absent, behaviour matches the old graph; when wired, the new path activates. No migration concern, no risk to existing presets.

When to build new instead:

- The existing primitive's core purpose would shift. (`render_lines` drawing lines vs. `render_volumetric` drawing volumes — different jobs.)
- The extension would add unrelated state, ports, or params that confuse the primitive's identity.
- The new behaviour needs a different type signature that can't be expressed as optional port + default-fallback.

WireframeZoo demonstrates the extend pattern: instead of a new `render_wireframe` primitive, `render_lines` grows one optional `edges: Array<EdgePair>` input port; instead of a new `generate_wireframe_topology` primitive, `generate_platonic_solid` grows an `edges` output and the existing clip-trigger pattern. Lissajous keeps working unchanged.

### 6.3 Single-purpose vs. family — choose deliberately

- **Single-purpose** (`generate_lissajous`, `frequency_ratio`, `voronoi_prism`): one clear use. Param surface is small. Composes cleanly with others. Default when in doubt.
- **Family** (`plasma_pattern_2d`, the future `cellular_pattern_2d`, `concentric_shapes_2d`): 5+ variants the user thinks of as one knob, sharing a param surface. The `pattern` enum picks the algorithm; the rest is shared. Use this when the alternative is N near-identical sub-graphs muxed at the output.

Bad middle ground: a family primitive with three barely-related variants, or a single-purpose primitive that grows a `mode` enum to cover its second use case. Both should be split or merged accordingly.

### 6.4 Constants belong inside primitives, not in graph nodes

Legacy Rust generators almost always have a handful of inline numeric constants that don't live in any visible param. WireframeZoo had `proj_scale = 0.25 * outer_scale` — that 0.25 was a screen-fit factor never exposed to the user but load-bearing for the visual size. **Static constants like this belong inside a primitive, never as a `math` node in the graph.** Math nodes are for *dynamic* arithmetic — `time × rate`, `audio_level × gain`, signals that change frame-to-frame. Putting `static × static` in a math node turns a constant into a node, growing the graph for no reason.

The codebase even has explicit precedent: the old `FloatTransform` / `FloatScaled` `ParamConvert` variant was *deliberately removed* (see `crates/manifold-core/src/effects.rs` — "their curation moved into the primitives"). Don't look for it; it doesn't exist by design.

Where the constant should live, in priority order:

1. **As a primitive default param.** Cleanest when the constant is a sensible default for that primitive's job — `project_3d.proj_scale = 0.25` was already this until our outer-card binding overrode it. If the outer-card slider doesn't need to override it, leave it as the inline default and don't bind.
2. **Baked into the upstream primitive's output.** Cleanest when the constant is intrinsic to what the primitive *produces*. WireframeZoo's 0.25 lives inside `wireframe_shape`: vertices come out at magnitude 0.25 already, so `project_3d.proj_scale = 1.0` (the user-facing "1.0 = default zoom") works directly. The 0.25 lives where it semantically belongs — inside the primitive responsible for screen-friendly vertex magnitudes.
3. **As an extra param on the consumer primitive.** Use this when (1) and (2) don't fit. Slightly less clean because it adds a param to a primitive that's specifically for one preset's UX.

The trap: missing a hidden constant and shipping a preset that's visually wrong by a multiplier. Or noticing the multiplier and reaching for a math node instead of pushing it into a primitive. Read the legacy generator's `render()` end-to-end, circle every bare numeric literal, and for each one decide which of the three homes above it belongs in — *before* authoring the JSON.

### 6.5 Outer-card params are the user surface

Every outer-card param costs UI real estate and the user's attention. Before declaring one, ask: would the user reach for this in performance? If the answer is "only when authoring the preset," the value should be baked into the JSON's defaults, not exposed. Plasma exposes 8 outer params (pattern, complexity, contrast, speed, scale, clip_trigger, plus generator_input scalars). Lissajous exposes 10. Anything past ~12 outer params is a smell — the graph is doing too much, or it should split into two presets with different defaults.

### 6.6 Name nodes for what they do, not how they're built

Node names are the user's vocabulary. They show up in the palette, the graph editor, MCP/API agent prompts, and the autocomplete a future user types into a search box. They are a UX surface, not a code surface.

The principle: **a node's name describes what it produces or transforms in user-visible terms**, not its implementation. Implementation details — algorithm names, the math behind it, internal pass structure — belong in `composition_notes` and the source file, not the type ID.

Good (already in the codebase, from the NODE_CATALOG rename pass):

| Was | Becomes | Why |
|---|---|---|
| `primitive.luminance` | `node.brightness` | sounds less like a measurement |
| `primitive.color_matrix` | `node.channel_mix` | hides the matrix math from users |
| `primitive.gradient_map` | `node.color_ramp` | matches DAW / paint-program vocabulary |
| `primitive.separable_gaussian` | `node.gaussian_blur` | the "separable" detail is an implementation choice |
| `primitive.uv_transform` | `node.transform` | "UV" is GPU jargon; users just want a transform |
| `primitive.kaleido_fold` | `node.kaleidoscope` | the math name; the user thinks "kaleidoscope" |

Patterns that are usually a smell:

- **Implementation prefixes** — `separable_*`, `convolution_*`, `compute_*`. These describe how it's coded.
- **Math terms users don't share** — `affine_*`, `convolution_2d_9tap`, `parametric_curve`. If a non-graphics user wouldn't recognise the term, it shouldn't be the primary name.
- **Acronyms** — `lfo` is fine (the user community knows it), `fbm` is borderline (signal-processing folks know it; general users don't), most others are not. When in doubt, spell it out.
- **Pass-numbered names** — `bloom_pass_1`, `blur_h`, `blur_v`. These leak the composite structure. A composite should ship as one named effect (`bloom`) with its passes as private primitives that aren't surfaced as palette nodes.

The audience is **general users**, not GPU nerds, not VJs, not generative artists who happen to know the math vocabulary. The test for a node name: would someone who has never read a graphics paper recognise the name and know roughly what to expect? If not, rename it.

- `lfo` — passes. Synth/DAW vocabulary; most music users know it.
- `lissajous`, `voronoi`, `platonic_solid`, `fbm`, `affine`, `parametric` — fail. Math jargon. Live performers loading a preset won't know these terms. Rename to what the node *produces*: `lissajous → xy_curve`, `voronoi → cell_pattern`, `platonic_solid → wireframe_shape`, `fbm → fractal_noise`, etc. (None of these are renamed yet — flagged as follow-up.)
- Individual *enum values* can stay closer to the math (Tetra/Cube/Octa/Icosa/Dodeca; Lorenz/Rössler/Aizawa attractor types). The visual preview is more informative than the label, and these names show up only after the user has already picked the broad category.

The principle: a user navigating the palette should be able to predict what a node does from its name alone. If the name only makes sense once they've already read the source, it's wrong.

Renames cost migration — add a type-ID alias and a `paramAliases` entry for outer params. Cheap, not free. So name correctly the first time, and treat any new primitive's name as a deliberate UX decision, not a side effect of the filename.

Renames cost migration — add a type-ID alias and a `paramAliases` entry for outer params. Cheap, not free. So name correctly the first time, and treat any new primitive's name as a deliberate UX decision, not a side effect of the filename.

## 7. Conventions that are invariants

Post-audit (2026-05-21), these are enforced by the runtime, the macro, or a CI test. Treat them as load-bearing.

- **`array_output_capacity`** — every primitive with an `Array<T>` output declares its capacity via the `array_output_capacity(port, params, input_capacities)` method on `EffectNode` (default impl reads `params["max_capacity"]`). Transform primitives override to inherit from input. Computed primitives override to multiply dimension params. A CI test (`every_array_output_declares_a_valid_capacity_source`) walks the registry and asserts every Array output resolves. This was a string-match convention pre-audit; it is now a port-level invariant. See [effect_node.rs:442](../crates/manifold-renderer/src/node_graph/effect_node.rs#L442).
- **`EffectNodeContext::scalar_or_param(name, default)`** — the canonical port-shadows-param read. Don't reinvent it locally; this was duplicated 8 times pre-audit and is now centralized. Use it in every primitive that has a port-shadowable scalar.
- **`extra_fields: { … }` in the `primitive!` macro** — the only place per-instance state belongs. Reset is implicit (new struct instance on rebuild). Types used here must be `pub` (sharp edge: see [render_lines.rs:64](../crates/manifold-renderer/src/node_graph/primitives/render_lines.rs#L64)).
- **`ClipTriggerCycle`** — every primitive or generator that maps a `trigger_count` to a discrete selection (`trigger_count % N`) must route it through `ClipTriggerCycle::step(raw_trigger_count, modulus)`. **Pass raw `trigger_count`, never pre-wrap.** Pre-wrapping breaks the cycle's idempotence detection (this is the 67f8db94 bug). See [clip_trigger.rs](../crates/manifold-renderer/src/generators/clip_trigger.rs).
- **`paramAliases` in `PresetMetadata` + `GeneratorAliasMetadata` inventory** — for migrating outer-card param names across releases. Project load resolves alias chains with cycle detection. Build the migration in the same commit as the rename — don't ship a shim and clean up later.
- **`composition_notes` in the `primitive!` macro slot** — the AI authoring surface. Even though no UI consumes it today (per audit §8), every primitive must populate it with a short, specific note describing when/how to compose this primitive. This is the vocabulary an MCP/API agent will read.
- **`StateStore` works for generators** — post-audit, `JsonGraphGenerator` owns a `StateStore` and dispatches through `execute_frame_with_state`. You can compose `feedback`, `array_feedback`, `temporal::*`, `smoothing`, `envelope_follower_ar` freely. No `assert!` panic at runtime, no plan-time check needed.
- **`preset_metadata` is grafted on both surfaces** — the runtime (`JsonGraphGenerator::from_def`) and the editor snapshot (`ContentThread::active_generator_graph_snapshot`) both call `graft_preset_metadata_from_bundle` before resolving bindings. An override with `preset_metadata = None` no longer silently strands every binding. This was the §9 critical asymmetry; it's closed.
- **`ParamConvert` variants are fixed at four**: `Float`, `IntRound`, `BoolThreshold`, `EnumRound`. No scaling, no offset, no enum-aware mapping. Anything that isn't a direct passthrough or one of those four conversions needs a `math` node in the graph between the outer-card slider and the inner-node target. Don't waste time looking for a `FloatScaled` convert — it doesn't exist.
- **Shared MTLBuffer is CPU + GPU visible.** Array<T> output buffers are allocated with `create_buffer_shared`, so the CPU can read them via `GpuBuffer::mapped_ptr()`. **But same-frame GPU-write → CPU-read does not see the write without an explicit fence** (the compute dispatch is queued, not completed, when the next primitive's CPU code runs). Two clean options when one primitive produces Array<T> and another consumes it CPU-side: (a) producer CPU-writes the data (works when the data is static or cheap to compute), (b) consumer reads next frame's data, accepting one-frame staleness. WireframeShape uses (a) for its edge tables. Don't try to read same-frame GPU-written data CPU-side.
- **No dead-state params or inputs.** Every declared param and every wired optional input must affect output under *every* reachable state of the primitive. This is a design rule, not a runtime workaround — there is no "warn the user the slider does nothing", because a slider that does nothing is the bug. If you find yourself wanting to write `if mode == X { /* ignore param Y */ }` or `if input.is_wired() { /* don't read param Z */ }`, the design is wrong and one of these is the fix:
  - **Find a math interpretation that makes the param active in every state.** Often available. `smoothstep_texture` used to gate `low` off in `Mode = Bipolar`; the rework dropped the mode entirely and made a bipolar curve a graph-level pattern (wire `node.math(Negate, high) → low`). Both edges are now always live, both modes still achievable, zero dead state.
  - **Split the primitive into states-as-types.** When the math has no honest interpretation that activates every param in every mode, the mode is masquerading as "two primitives sharing a name". Split them. Each new primitive has only the params it actually honors. Mode-switching at runtime becomes graph-author work via mux.
  - **Honour the input/param under every code path.** `render_lines` used to ignore `animate` / `speed` / `window` whenever the `edges` input was wired; the fix was to thread the window+fade through the topology path too. Optional inputs being *present-or-absent* is fine — what's forbidden is "wired-but-quietly-ignored".

  The performer sees outer-card sliders. A slider that the user can move while nothing changes — no log they can read on stage, no error, no visual cue — is the worst category of bug for live-show use. Designs that make this state unreachable are the only acceptable fix.

  **Mux-shaped primitives are the documented exception.** `node.mux_scalar` and `node.mux_texture` explicitly select one input; the others are inert by design and the user's mental model accommodates this ("it's a mux"). When the *selected* slot is unwired the right fix is graph-editor authoring UX (visually flag the unwired-selected port) — that's a separate piece of work, not a primitive concern. Mux is the only exception; don't borrow it for anything else.

  See [smoothstep_texture](../crates/manifold-renderer/src/node_graph/primitives/smoothstep_texture.rs) for the "drop the mode, reinterpret the math" rework, [render_lines](../crates/manifold-renderer/src/node_graph/primitives/render_lines.rs) for the "honour the param on both paths" rework, [plasma_pattern_2d](../crates/manifold-renderer/src/node_graph/primitives/plasma_pattern_2d.rs) for the clean family-primitive shape where the pattern enum picks the algorithm but every algorithm uses every shared param.

## 8. Bug classes to recognise

These are the failure modes that have actually bitten on this codebase. Watch for them in your decomposition.

- **Identical back-to-back clip-trigger outputs.** Caused by either pre-wrapping the trigger count before `ClipTriggerCycle::step` (the 67f8db94 bug) or by not using `ClipTriggerCycle` at all (the wireframe_zoo / fluid_simulation_3d bug class found in the audit). Fix: pass raw `trigger_count` to the cycle; never `% N` upstream.
- **Black frame from missing Array<T> buffer.** Caused by an Array<T> producer not declaring `array_output_capacity` correctly, so the pre-allocator skips it and downstream primitives silently see an empty wire. Fix: implement `array_output_capacity`. The CI sweep test catches new misses.
- **State that doesn't reset on export warmup.** `JsonGraphGenerator::reset_state` now walks every node and clears state via `state_store.cleanup_all()`. If you're adding state to a generator-side primitive via `extra_fields`, make sure it's reset by either being part of the StateStore or by overriding the generator's reset path. Symptom otherwise: the first frame of an exported video carries warmup residue invisible in the live preview.
- **Param-binding mismatch under override.** Editor and runtime now both graft `preset_metadata` from the bundled JSON if a per-layer override stripped it. Don't reintroduce a code path that reads `preset_metadata.bindings` without going through the graft helper.
- **Per-frame allocation regressions.** The audit confirmed the steady-state hot path has zero per-frame allocations. Don't reintroduce `Vec::new()`, `HashMap::new()`, `.to_string()`, or `Box::new()` in `Primitive::run` or anywhere the executor calls per frame. Scratch buffers live as fields and get `.clear()`'d.
- **Dead-state param or input.** Symptom: performer drags an outer-card slider and nothing changes visually, no error, no log. Caused by a code path inside a primitive's `run()` (or shader) that branches on mode / wiring / state and skips reading certain params or inputs on one branch. The render_lines `edges`-wired path was the canonical instance (wiring `edges` activated a topology code path that didn't read `animate` / `speed` / `window`); smoothstep_texture's Bipolar mode was the canonical *mode-gated* version (the slider was always interactable, but `low` was inert when `Mode = Bipolar`). The §7 invariant above ("No dead-state params or inputs") is the design rule. Review diagnostic: for every conditional branch in `run()` (and in the shader), list which params/inputs each branch reads, and check that every declared param/input shows up in *every* branch. If the answer is "no", the primitive needs the §7 rework, not a warn.

## 9. The acceptance bar

A decomposition ships when:

1. **Bit-parity, tested, not asserted — and tested on the GPU, not in CPU mirror.** When you migrate an existing native generator, the JSON preset MUST render bit-exact against the legacy generator on the canonical fixture (or numerically bounded with documented justification when bit-exact isn't achievable — e.g. RNG-seeded particle sims). **Bit-parity is not a claim you make in a commit message — it is a test that runs.** The standard the codebase uses for effects is the `gpu_tests` module pattern (see [`separable_gaussian.rs`](../crates/manifold-renderer/src/node_graph/primitives/separable_gaussian.rs) for the canonical shape, or `wireframe_shape.rs` for the Array-output variant):
   - Build a `Graph` containing the primitive under test, set params, `compile(&g)` into an ExecutionPlan.
   - For Texture2D outputs: pre-bind via `MetalBackend::pre_bind_texture_2d`, run a frame through `Executor::execute_frame_with_gpu`, read back via `copy_texture_to_buffer` + `mapped_ptr` and decode (fp16/fp32 as the format dictates). For Array<T> outputs: pre-bind via `MetalBackend::pre_bind_array`, run, and read back directly via `mapped_ptr` on the shared buffer (Array<T> buffers are CPU+GPU visible by allocation policy).
   - Compare element-wise against the legacy reference inlined as `const` data (often raw tables from the deleted Rust generator) or computed inline (e.g. `legacy_normalise(raw) * proj_scale`).
   - **Don't use CPU mirror functions as the parity test.** A CPU mirror only verifies that the test's expectations match itself. GPU parity tests verify the *shader* matches the legacy, which is the actual artefact running in production.
   - Coordinate-space conventions are part of bit-parity. If a primitive outputs in `[0, 1]` space and the consumer expects centred-at-origin, that mismatch is a bit-parity failure even when each individual primitive's math is correct in isolation. The GPU test for the producer catches its own contract; chain-level mismatches need a chain test (build the whole graph, run it, check downstream).
   - **Do not claim "bit-perfect parity" in a commit message unless an automated test in the same commit proves it.** If the parity test is missing or untested, the commit message must say "approximate parity — not yet test-verified" and the next commit should add the test. Anything else is dishonest.
2. New primitives have parity tests against the legacy math (CPU mirror inside the test module) per the above.
3. `cargo clippy --workspace -- -D warnings` is clean.
4. `cargo test --workspace` is green, including the registry sweep test for any new Array<T> primitives.
5. The legacy Rust generator file is deleted, not just shadowed.
6. `paramAliases` / `GeneratorAliasMetadata` are wired for any renamed outer params, with a project-load smoke test.
7. The preset's `composition_notes` on every new primitive describe when an AI agent would reach for that primitive.
8. The commit message is honest about what shipped, what didn't, and any known parity deltas. If the visual preview wasn't run in a browser yet, say so explicitly.

The bar is real. This system is the live show. Every shipped decomposition is one Peter loads up before going on stage — a parity bug becomes the gig.

## 10. Known limits and accepted trade-offs

Documented here so future-me doesn't try to "fix" them without context.

- **`ClipTriggerCycle` state is lost on graph-editor rebuild** for graph-defined generators. `trigger_count` is preserved, so the next emission is `trigger_count % N` — which can equal the prior instance's last emission with `1/N` probability. Accepted: the graph editor is an authoring surface, not a performance surface. A pattern flash during a graph edit is not a live-show concern. See memory [feedback_graph_editor_is_authoring_not_perform](../.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/feedback_graph_editor_is_authoring_not_perform.md).
- **Paused-layer GPU state isn't evicted.** A paused layer keeps its generator (and any FluidSim density grids, particle buffers, etc.) alive until the layer's `data_version` changes. Accepted: snap-back-on-resume is a live-show contract; bounded RAM cost is the trade.
- **The `legacy_param_aliases` / `param_aliases` / `paramAliases` / `GeneratorAliasMetadata` naming surface has four spellings of the same concept.** Not renamed yet because the surface is stable and renaming risks an alias gap. Document the alias-system entry point when grep-able naming becomes a real issue.

---

If you're starting a decomposition: re-read §3 (the workflow). If you're stuck partway: re-read §5 (escape hatch) and §6 (keep it small) — most stuck moments are one of those two. If you find a new bug class, write it up in §8 and update the post-audit log.
