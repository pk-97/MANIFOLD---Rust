# Generator Decomposition Plan

**Status:** Roadmap, kept current with shipping reality. Per-generator decomposition of Rust `inventory::submit!` generators into JSON-authored graphs using the shipping primitive vocabulary. Companion to [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) (the how-to-think guide) and [NODE_CATALOG.md](NODE_CATALOG.md) (what exists).

## Why generators decompose

Generators were originally one Rust file per algorithm. The JSON-graph form lets:

- **AI agents author new generators** by composing curated primitives. Each new shipped JSON adds a reference shape future agents can adapt.
- **Users drill into a generator** in the graph editor and read the wiring — the Max-for-Live analogue.
- **The renderer stays auditable.** A graph plus a primitive set is far easier to test, port, and tune than 20 monolithic Rust generators with private state.

Don't decompose for its own sake — see DECOMPOSING_GENERATORS §1 for the "monolithic Rust is sometimes correct" rule and §5 for what irreducible looks like.

---

## State of play

- **JSON-defined generators:** 17 (full list in [NODE_CATALOG.md §6.1](NODE_CATALOG.md)). Cover the procedural-texture, parametric-curve, mux'd-variant, particle-sim, screen-space PBR, 3D-mesh PBR-IBL, 3D / 4D wireframe, instanced-mesh, 2D/3D fluid-sim, and volumetric-scrubbing families.
- **Rust-defined generators:** 4 remaining (see §3 below). Register via `inventory::submit!` in [crates/manifold-renderer/src/generators/](../crates/manifold-renderer/src/generators/).
- **Primitive vocabulary:** ~135 shipped — see NODE_CATALOG.md for the full inventory.
- **Infra:** all the foundation work has shipped — `system.generator_input` boundary node, variadic mux primitives, per-slot texture format declaration on the backend, the JSON loader (`JsonGraphGenerator`), `paramAliases` migration support, and StateStore plumbing for stateful primitives inside generators.

---

## 1. The audit precondition

**Before starting any generator decomposition, run the audit-by-analogy step from [DECOMPOSING_GENERATORS.md §2.5](DECOMPOSING_GENERATORS.md).** Identify the nearest shipped JSON preset, read it end-to-end, and reconcile your sketch against the existing primitive vocabulary. If you can't identify which shipped preset most closely resembles the shape you're decomposing into, you're not ready to start.

---

## 2. Shipped JSON-defined generators

See [NODE_CATALOG.md §6.1](NODE_CATALOG.md) for the topology shape of each. Each one is a reference graph for at least one decomposition pattern; future generators that fit the same pattern should follow the corresponding preset.

| Generator | Decomposition pattern it demonstrates |
|---|---|
| Plasma | Curated family primitive packs N variants behind one enum |
| StarField | Single-purpose procedural texture primitive |
| BasicShapes | Curated SDF shape primitive with trigger-cycled fills |
| Lissajous | Parametric curve + LFO control plumbing + render_lines |
| WireframeZoo | 3D wireframe pipeline (shape → rotate_3d → project_3d → render_lines) |
| Tesseract / Duocylinder | 4D wireframe pipeline (rotate_4d / project_4d) |
| ConcentricTunnel | Mux'd variants driven by trigger_count + ring stacker |
| NestedCubes | Cycled poses via cycle_table_row + mux_array + instanced mesh |
| DigitalPlants | Procedural instance layout + per-instance noise + cylinder/torus wrap + custom render |
| ComputeStrangeAttractor | Particle sim with attractor ODE + scatter + resolve + tone map |
| FluidSim2D | Particle fluid sim with ping-pong feedback + force-field gradient + downsample/blur |
| FluidSim3D | Volumetric fluid sim — 3D seed/simulate + per-axis separable blurs + curl/slope force field + camera-projected scatter for display |
| OilyFluid | Screen-space surface with atomized PBR shading (Lambert + matcap + Fresnel + Blinn summed) |
| MetallicGlass | 3D-mesh PBR-IBL on a displaced grid: feedback-displacement liquid surface + Cook-Torrance render + IBL env-map. First consumer of the new PBR-IBL atom family (cook_torrance_specular, equirect_envmap_sample, bake_equirect_envmap, render_3d_mesh_pbr_ibl, mirror_axis, pack_channels, clamp_texture). |
| MriVolume | Image-folder scrubbing with mux'd slices |
| TrivialPassthrough | Smoke-test fixture for the boundary nodes |

---

## 3. Remaining Rust-defined generators

The migration targets. Each lives at `crates/manifold-renderer/src/generators/<name>.rs`.

| Generator | Migration notes |
|---|---|
| **BlackHole** | ~14-pass relativistic geodesic trace + particle/density blur chain. Genuinely §5-shaped: per-pass coupling with native-precision boundaries. Lift shaders into `wgsl_compute_*` nodes; close frame-to-frame loops through `feedback` / `array_feedback`. Reference: FluidSim2D / FluidSim3D for the wgsl_compute + feedback pattern. |
| **OscilloscopeXY** | Two superposed Lissajous curves with axis-asymmetric phase scaling and a custom 10-row beat-driven ratio table with linear interpolation between adjacent beats. Not in the canonical fixture (zero layers); decomposition requires speculative-reuse primitives. **Status: deferred — revisit the visual goal before paying the migration tax.** |
| **ParticleText** | CPU text rasterizer + fluid-sim seeded from text bitmap. Needs new `text_rasterize` primitive wrapping the existing CPU text path. Reference: FluidSim2D for the particle-sim downstream. |
| **Text** | Glyph render via the CPU text rasterizer. Single-primitive wrap: lift the rasterizer into `text_rasterize`; preset is `system.generator_input → text_rasterize → final_output`. Pairs with the ParticleText work. |

---

## 4. Workflow per migration

Follow [DECOMPOSING_GENERATORS.md §3](DECOMPOSING_GENERATORS.md) start-to-finish for every migration. Key reminders:

- **Audit first** (§2.5 of the guide) — no proposed primitives until you've surveyed what exists and read the nearest reference preset end-to-end.
- **New primitives ship in their own commit before the preset.** Each with parity test against legacy math (GPU `gpu_tests` module against constant tables or computed reference, not CPU mirror — see DECOMPOSING_GENERATORS §9).
- **Parity test the whole graph** (§3 step 6). Bit-exact for Tier 1/2-shaped generators; numerically bounded with documented justification for RNG-seeded particle sims.
- **Delete the legacy Rust file in the same commit as the preset.** Don't leave shadowed dead code.
- **`paramAliases` + `GeneratorAliasMetadata`** for renamed outer params so old projects load unchanged.

Validation gates:

- Iteration loop: `cargo run -p manifold-renderer --bin check-presets` (no GPU, sub-second) after every JSON edit; `cargo test -p manifold-renderer --test parity <generator>::` for parity runs.
- Migration commit: `cargo clippy --workspace -- -D warnings && cargo test --workspace` green.
- Visual parity against `Liveschool Live Show V6 LEDS.manifold` canonical fixture before declaring done.

---

## 5. Open questions

- **Anim progress return.** Generators historically return a `f32 anim_progress` value (drives the picker UI thumbnail animation). JSON-graph generators surface this through primitive `extra_fields`; a future `system.generator_output` boundary node could formalise this if more generators need it.
- **Whether to keep OscilloscopeXY at all.** Decomposing it would need two speculative-reuse primitives plus an extension to `generate_lissajous`. Worth the cost only if the visual is a Peter-keeper; otherwise replace with a different curve generator that does compose from the existing vocabulary.
- **Generator categories in the picker.** `GeneratorMetadata` has no `category` field today; the picker is flat alphabetical. Grouping is a UX win when there are 20+ JSON generators visible in one list.
