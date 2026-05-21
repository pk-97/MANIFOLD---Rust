# Generator node-graph audit — hot path & correctness invariants

> **Status (2026-05-21, post-fix):** Every 🔴 and 🟠 finding in this audit is closed in the follow-up commit. The findings text below is preserved verbatim as a reference for the failure modes and the architectural rationale; the **Resolution log** at the end of each section traces each fix back to its commit / file.

Read-only audit conducted 2026-05-21 against `node-graph-system` branch HEAD `3837b42a`. Scope: the JSON-defined generator path, the `ClipTriggerCycle` invariant, silent-failure surface, param-alias resolution, plan/resource lifecycle, primitive-library conventions, and the two-thread snapshot boundary.

The audit is structured as a punch list. Each finding includes a severity guess (🔴 = correctness bug; 🟠 = fragility / future-proofing gap; 🟡 = convention / docs / hygiene; 🟢 = working as designed, just undocumented). Triage together — I don't propose fixes here.

---

## 1. Hot-path discipline in `JsonGraphGenerator::render`

File: [json_graph_generator.rs:556-611](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L556-L611).

**🟢 No per-frame allocations on the render hot path.**

| Concern | Status |
|---|---|
| `set_frame_context` (5 × `graph.set_param`) | Calls take `&'static str` keys (`"time"`, `"beat"`, …). Map writes mutate existing entries on the steady-state path. ✓ |
| `apply_param_values` | Iterates `self.bindings: Vec<BindingResolution>` (cached at construction). Reads via `values.get(idx)`, writes via `graph.set_param(node, &'static str, ParamValue)`. The `target_param` was `Box::leak`'d once at construction so the per-frame call doesn't allocate the key. ✓ |
| `install_target` | `replace_texture_2d` is one atomic `Retained` swap, no allocation. ✓ |
| `execute_frame_with_gpu` | Walks the cached `ExecutionPlan`. Per-step scratch buffers (`input_scratch`, `output_scratch`, `scalar_write_scratch`) are pre-allocated `Vec`s on `Executor` and reused frame-to-frame via `.clear()` + `.push()`. ✓ |
| `pre_allocate_array_buffers` | Runs **once** in `from_def_with_device` (line 387), and **once** on each `resize` (line 653). Not per frame. ✓ |
| Compute pipeline cache | Each primitive's `pipeline: Option<GpuComputePipeline>` field is initialised lazily on first dispatch via `get_or_insert_with`. Plan rebuild → new Graph → new primitive instances → fresh pipeline cache. Acceptable, but a generator rebuild stalls the next frame's first dispatch. See finding 5. |

**🟠 One leak per binding per generator construction (line 298-299).**

```rust
let leaked_param: &'static str = Box::leak(param.clone().into_boxed_str());
```

A binding's target param name (e.g. `"selector"`, `"complexity"`) is `Box::leak`'d each time `from_def` runs. Every generator rebuild — graph-editor edit, type swap, version bump — leaks the binding param names again. In a long live session with frequent graph-editor edits across many layers, this is a small but unbounded growth. Likely lost-in-the-noise relative to all other allocations, but it doesn't free.

**🟠 `handle_map` and `outer_param_index` are built as `std::collections::HashMap`, not `AHashMap`** (lines 233, 278). Construction-only, so steady-state cost is zero, but inconsistent with the rest of the renderer crate.

---

## 2. ClipTriggerCycle lifecycle

Helper file: [clip_trigger.rs](../crates/manifold-renderer/src/generators/clip_trigger.rs). Call-site sweep:

**Rust generators owning a `clip_trigger_cycle: ClipTriggerCycle` extra-field:**
- [plasma.rs:47](../crates/manifold-renderer/src/generators/plasma.rs#L47) — `step(trigger_count, PATTERN_COUNT)` at line 100
- [mri_volume.rs:71](../crates/manifold-renderer/src/generators/mri_volume.rs#L71) — `step(trigger_count, 3)` at line 218
- [oscilloscope_xy.rs:41](../crates/manifold-renderer/src/generators/oscilloscope_xy.rs#L41) — `step(trigger_count, RATIO_COUNT)` at line 122
- [concentric_tunnel.rs:49](../crates/manifold-renderer/src/generators/concentric_tunnel.rs#L49) — `step(trigger_count, SHAPE_COUNT)` at line 114
- [strange_attractor.rs:163](../crates/manifold-renderer/src/generators/strange_attractor.rs#L163) — `step(trigger_count, ATTRACTOR_COUNT)` at line 340

**Primitives (graph-defined, via `extra_fields:` in `primitive!`):**
- [plasma_pattern_2d.rs:133](../crates/manifold-renderer/src/node_graph/primitives/plasma_pattern_2d.rs#L133) — `step(raw, PLASMA_PATTERN_COUNT)` at line 191. Raw value = `trigger_count.floor().max(0.0) as u32`. ✓ raw (no pre-wrap).
- [frequency_ratio.rs:64](../crates/manifold-renderer/src/node_graph/primitives/frequency_ratio.rs#L64) — `step(count, len)` at line 95, with `count = rounded.max(0) as u32`. ✓ raw (no pre-wrap, per fix `67f8db94`).

**All call sites pass raw `trigger_count` (or a rounded but un-modulo'd derivative)**, which is the fix from `67f8db94`. No surviving pre-wrap. ✓

**🔴 Generators that still use the legacy `last_trigger_count: i32 / u32` field pattern, NOT `ClipTriggerCycle`:**
- [nested_cubes.rs:91-216](../crates/manifold-renderer/src/generators/nested_cubes.rs#L91-L216) — has `last_trigger_count`, but uses it only for edge detection, not for cycling outputs through `trigger_count % N`. Likely fine.
- [fluid_simulation_3d.rs:315-595](../crates/manifold-renderer/src/generators/fluid_simulation_3d.rs#L315-L595) — `last_trigger_count: u32` + raw `ctx.trigger_count % PATTERN_COUNT` on line 594. **Not protected by `ClipTriggerCycle`** — could in principle produce back-to-back duplicates on a clean modulus wrap. Same shape as the Lissajous bug class.
- [fluid_sim_core.rs:189-497](../crates/manifold-renderer/src/generators/fluid_sim_core.rs#L189-L497) — `last_trigger_count: i32` + raw `(trigger_count as u32) % PATTERN_COUNT` on line 497. Same exposure.
- [particle_text.rs:88](../crates/manifold-renderer/src/generators/particle_text.rs#L88) — edge detection only, not cycling. Likely fine.
- [strange_attractor.rs:160-343](../crates/manifold-renderer/src/generators/strange_attractor.rs#L160-L343) — uses **both** `last_trigger_count` (edge detect) AND `ClipTriggerCycle` (cycle). Belt-and-braces; OK but worth a comment so future-me doesn't dedup.
- [wireframe_zoo.rs:292](../crates/manifold-renderer/src/generators/wireframe_zoo.rs#L292) — bare `(ctx.trigger_count % SHAPE_COUNT)` with no cycle helper. **Same bug class as Lissajous.**

**Severity:** 🔴 for `fluid_simulation_3d`, `fluid_sim_core`, `wireframe_zoo` — they could exhibit the same back-to-back duplicate Peter hit on Lissajous, in front of an audience. The cycle is a one-line drop-in.

**🟢 Cycle reset on generator rebuild:** the cycle's state lives inside the primitive / generator struct, so rebuild drops it. The paired `trigger_count` preservation in [generator_renderer.rs:197-201](../crates/manifold-renderer/src/generator_renderer.rs#L197-L201) means the new instance's first emission lands at the same modulo the previous instance would have produced — visual continuity preserved.

**🟠 Edge case:** for graph-defined generators, `ClipTriggerCycle` state lives inside the Graph's node instances. A graph-editor edit rebuilds the Graph (fresh instances → fresh cycles), but `trigger_count` is preserved. So the next emission is `trigger_count % N` — which can equal the **prior** instance's last emission, since the cycle no longer remembers it. The probability is `1/N` per rebuild; for `PLASMA_PATTERN_COUNT = 8` that's a 12.5% chance of a same-pattern flash on edit. Probably acceptable for a power-user authoring action (graph edit ≠ live moment), but worth documenting as a known limit. CLAUDE.md memory `feedback_graph_editor_is_authoring_not_perform` corroborates this is acceptable.

---

## 3. Silent-failure surface

Sweep counts across `generators/*.rs` + `node_graph/primitives/*.rs`:

- **~225** bare `return;` early-outs (from `let Some(...) else { return; }` patterns).
- Only **10** of those sites emit a `log::warn!` before returning.
- The remaining ~215 silently drop the frame's work.

**🔴 The Array<T> output-binding silent return — the exact bug fixed in `23e440aa` — survives in 22 of 25 Array<T> producers.**

Sites that warn on missing output `points` / `vertices` / `particles` / `out` / `blobs` / `instances` / `accum`:
- `generate_lissajous.rs:131-144` ✓ (fixed)
- `render_lines.rs:398-412` ✓ (fixed)
- `blob_detect_ffi.rs:170-174` ✓ (was always logged)

Sites that **still silently return** on a missing pre-bound `Array<T>` output buffer:
- `seed_particles.rs:91` (`particles`)
- `seed_particles_from_texture.rs:114` (`particles`)
- `fluid_seed.rs:122` (`particles`)
- `scatter_particles.rs:105` (`accum`)
- `scatter_particles_3d.rs:106` (`accum`)
- `fluid_project_scatter_2d.rs:253` (`accum`)
- `generate_grid_mesh.rs:110` (`vertices`)
- `generate_cube_mesh.rs:69` (`vertices`)
- `generate_platonic_solid.rs:78` (`vertices`)
- `generate_duocylinder_vertices.rs:66` (`vertices`)
- `generate_tesseract_vertices.rs:53` (`vertices`)
- `generate_instance_transforms.rs:183` (`instances`)
- `array_feedback.rs:60` (`out`)
- `integrate_particles.rs:101` (`out`)
- `integrate_particles_attractor.rs:200` (`out`)
- `fluid_simulate.rs:208` (`out`)
- `displace_mesh.rs:111` (`out`)
- `triangulate_grid.rs:76` (`out`)
- `neighbor_smooth.rs:72` (`out`)
- `project_3d.rs:91` (`out`)
- `project_4d.rs:75` (`out`)
- `rotate_3d.rs:87` (`out`)
- `rotate_4d.rs:89` (`out`)

The commit message of `23e440aa` says "Broader codebase sweep (all Array-using primitives) is separate cleanup." That sweep hasn't landed. Every one of these primitives is a black-frame waiting to happen when authored into a future JSON preset that doesn't get its Array<T> pre-allocated — the exact regression mode Lissajous just hit.

**🟠 Texture2D and Texture3D missing-binding sites** — count similar (~190 sites). Almost all are "input not bound" → `return`. Since the chain build pre-binds every Texture2D resource (per memory `feedback_node_graph_prebind_all_textures`), these are usually unreachable in practice — but if a future primitive declares a Texture2D output and somehow doesn't get pre-bound, the failure mode is the same black frame.

A reasonable invariant to add: **every `Some(...) else { return; }` on a buffer / texture binding in a primitive must `log::warn!` first.** Cheap to enforce with a sweep + lint test.

---

## 4. Param-alias resolution

**🟢 All resolution happens at project-load / migration time. Not per frame.**

- Defined: [effect_registration.rs:47](../crates/manifold-core/src/effect_registration.rs#L47).
- Bounded chain walk: `while hops <= aliases.len()` — cycle-safe.
- Cycle detection covered by `resolve_param_alias_breaks_cycle` test.
- All call sites are in JSON deserialization paths:
  - [effects.rs:535, 606, 654](../crates/manifold-core/src/effects.rs) — `ParamValuesWire::into_positional`, `FloatValuesWire::into_positional_base`. Called when a project's `paramValues` Map deserializes.
  - [project.rs:323, 337, 359, 375, 621](../crates/manifold-core/src/project.rs) — `Project::resolve_legacy_param_ids` migration sweep.
- Alias table built **once** at startup per effect/generator type — `inventory::iter::<GeneratorAliasMetadata>` walks the inventory and merges `def.legacy_param_aliases = alias_meta.aliases` at [generator_definition_registry.rs:47-54](../crates/manifold-core/src/generator_definition_registry.rs#L47-L54).

**🟡 Naming inconsistency:** `paramAliases` (wire), `param_aliases` (Rust field on `PresetMetadataDef`), `legacy_param_aliases` (Rust field on `EffectDef`/`GeneratorDef`), `GeneratorAliasMetadata` (inventory struct). Same concept, four different names. Future-me will grep for the wrong one.

---

## 5. trigger_count preservation across rebuild

Three paths rebuild a layer's generator. **All three preserve `trigger_count`.** ✓

| Path | Site | Preserves trigger_count? |
|---|---|---|
| Type change / override version bump in `acquire_clip` | [generator_renderer.rs:197-211](../crates/manifold-renderer/src/generator_renderer.rs#L197-L211) | ✓ (fix `0751d905`) |
| Explicit type swap via `update_active_types_for_layer` | [generator_renderer.rs:496-521](../crates/manifold-renderer/src/generator_renderer.rs#L496-L521) | ✓ |
| Project release / new clip after `release_all` | [generator_renderer.rs:619-633](../crates/manifold-renderer/src/generator_renderer.rs#L619-L633) | ✗ — resets to 0, intentional (whole project reload) |

**🟠 Per-generator state that is NOT preserved across rebuild:**
- `ClipTriggerCycle` last-emission state (see §2 — known acceptable risk).
- `RenderLines::anim_progress` extra-field. On generator rebuild the animation jumps back to start. For a power-user authoring action this is cosmetic; for a per-frame override-graph update (unlikely path but possible) it'd be jarring.
- `LayerGeneratorState::layer_string_defaults` and `merged_string_params` — preserved correctly through `update_active_types_for_layer` (line 510-515) but **lost** in `acquire_clip`'s rebuild path (no preservation; new state inits empty maps at line 213-215). Cross-check: in practice the next `start_clip` rescans every clip and refills the defaults, so the gap closes within one clip cycle. ✓ but the inconsistency between the two rebuild paths is fragile.

**🔴 `JsonGraphGenerator::reset_state` is the default no-op from the `Generator` trait** ([generator.rs:33](../crates/manifold-renderer/src/generator.rs#L33)). Called by `GeneratorRenderer::reset_all_generator_state`, which is invoked after export warmup re-seek. The Rust generators that own simulation state override this; JSON generators don't. Symptom: an exported video's first frame after the warmup point would carry stale ArrayFeedback state, `anim_progress`, `clip_trigger_cycle.last_emitted` — depending on which primitives the preset uses. Visible as a "warm-up artefact" on the first frame of a render that doesn't appear in the live preview at the same time stamp.

---

## 6. Graph plan / resource lifecycle

**Plan cache.** [JsonGraphGenerator::plan](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L101) is computed once in `from_def` via `compile(&graph)?` and held for the generator's lifetime. Rebuild key: `(generator_type, generator_graph_version)` tuple in `LayerGeneratorState`. Any change → drop + recreate via `create_with_override` → fresh plan. ✓

**Resource pool.** `MetalBackend` owns:
- `textures_2d: AHashMap<Slot, RenderTarget>` — owned, pool-recycled via `RenderTargetPool` on drop / release / `resize` / `drop_all_resources`.
- `borrowed_2d: AHashMap<Slot, GpuTexture>` — host-installed views (e.g. the FinalOutput target). Cleared at frame boundaries via `clear_skip_aliases`.
- `buffers_array: AHashMap<Slot, GpuBuffer>` — pre-bind-only. `pre_allocate_array_buffers` writes once at construction, re-runs on resize.
- `textures_3d: AHashMap<Slot, GpuTexture>` — same contract as arrays.

**🟠 Eviction policy for layer-level generator state.** [generator_renderer.rs:302-304](../crates/manifold-renderer/src/generator_renderer.rs#L302-L304) only evicts `layer_generators` entries on `data_version` change. A layer that's *paused* (no active clip) keeps its generator alive — including any heavy GPU state (FluidSim density grids, particle buffers, etc.). For the documented "53 layers, 2928 clips" project scale, this could pin tens of MB unnecessarily. Memory `project_typical_project_scale` confirms scale matters.

**StateStore.** [state_store.rs](../crates/manifold-renderer/src/node_graph/state_store.rs).
- The effect path uses `Executor::execute_frame_with_state` (see [effect_chain_graph.rs:702-705](../crates/manifold-renderer/src/effect_chain_graph.rs#L702-L705)).
- **The generator path uses `execute_frame_with_gpu`, NOT `execute_frame_with_state`** ([json_graph_generator.rs:599](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L599)).
- Comment on line 590-592 acknowledges this: *"graphs that hold state (Feedback, mip chains) would need the _with_state path — JSON generators don't compose stateful primitives yet, so this is sufficient."*

**🔴 The moment a generator preset wires a stateful primitive — `Smoothing`, `EnvelopeFollowerAr`, `Feedback`, `ArrayFeedback`, `Temporal::*` — the executor `assert!`s at the entry point** ([execution.rs:109-115](../crates/manifold-renderer/src/node_graph/execution.rs#L109-L115)) with `"Executor::execute_frame_with_gpu called with a plan containing node(s) that require a StateStore"`. There's no friendly load-time check; it's a per-frame panic on the content thread. Memory `feedback_channel_disconnected_means_content_panic` says that surfaces to the UI as "Content command channel disconnected." Generator preset authoring that uses `ArrayFeedback` for closed particle loops (as described in the array_feedback primitive's own docs) would hit this immediately.

**Mitigation paths**:
- a) Switch the JSON generator path to `execute_frame_with_state` with a per-`(generator_instance, LayerId)` `OwnerKey`.
- b) Plan-time check at `from_def` that asserts `plan.requires().state_store == false` and returns a friendly `JsonGeneratorLoadError::RequiresStateStore`.

Triage call needed.

---

## 7. JSON-vs-Rust generator inventory

**Rust generators** (one `inventory::submit! { GeneratorFactory { ... } }` per file):
| File | Type |
|---|---|
| `basic_shapes_snap.rs` | Rust-only |
| `black_hole.rs` | Rust-only |
| `concentric_tunnel.rs` | Rust-only |
| `digital_plants.rs` | Rust-only |
| `duocylinder.rs` | Rust-only |
| `fluid_simulation.rs` | Rust-only |
| `fluid_simulation_3d.rs` | Rust-only |
| `metallic_glass.rs` | Rust-only |
| `mri_volume.rs` | Rust-only |
| `nested_cubes.rs` | Rust-only |
| `oily_fluid.rs` | Rust-only |
| `oscilloscope_xy.rs` | Rust-only |
| `particle_text.rs` | Rust-only |
| `plasma.rs` | **superseded by `Plasma.json`** at runtime (registry prefers JSON) |
| `star_field.rs` | Rust-only |
| `strange_attractor.rs` | Rust-only |
| `tesseract.rs` | Rust-only |
| `text.rs` | Rust-only |
| `wireframe_zoo.rs` | Rust-only |

(20 Rust generators total. `fluid_sim_core.rs` is shared helper, not its own factory.)

**JSON presets** (in [`assets/generator-presets/`](../crates/manifold-renderer/assets/generator-presets/)):
- `Plasma.json` — supersedes `plasma.rs` (id `Plasma`)
- `Lissajous.json` — Lissajous Rust generator was deleted; this is the only path
- `TrivialPassthrough.json` — diagnostic / regression fixture

**🟡 Orphan check:** every JSON has a corresponding selector entry (verified by the `bundled_presets_include_shipping_generators` test). No Rust generator has been deleted without its JSON replacement landing — except `lissajous.rs` which Peter killed in commit `606292e9`.

**🟠 `plasma.rs` is shadowed dead code at runtime.** The Rust factory's `inventory::submit!` still runs, but `GeneratorRegistry::create_with_override` checks JSON first ([registry.rs:131-153](../crates/manifold-renderer/src/generators/registry.rs#L131-L153)) so the Rust factory's `create` never fires for `id = "Plasma"`. The Rust factory ID is `GeneratorTypeId::PLASMA`. Memory `feedback_no_rust_revert_for_graph_effects` says deletion is the answer, but for now `plasma.rs` is compiled in but never called.

---

## 8. Primitive library conventions — invariants vs accidents

### `max_capacity` convention
**🟠 String-match convention, NOT enforced.**

[`JsonGraphGenerator::pre_allocate_array_buffers`](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L402-L448) string-matches the param name `"max_capacity"`. If a primitive author declares an Array<T> output but names their capacity param `"vertex_count"` or `"buffer_size"`, the pre-allocation silently skips that producer. The recent fix to `generate_lissajous` renamed `vertex_count → max_capacity` (commit `23e440aa` body) precisely to fit this convention.

Producers that DO declare `max_capacity`: 13 primitives (confirmed via grep). Producers that emit Array<T> but might *not* declare `max_capacity`: needs a sweep test. A reasonable enforcement: add a primitive-spec assertion that any primitive whose output port is `PortType::Array(_)` must have a `max_capacity` param. Test-only, cheap.

### `extra_fields: { … }` pattern
**🟢 Enforced by the `primitive!` macro** at [primitive.rs:295](../crates/manifold-renderer/src/node_graph/primitive.rs#L295) — generates struct fields with the user-supplied init expression. `new()` and `Default::default()` use the inits. Reset on rebuild is implicit (new struct instance).

20 primitives use `extra_fields:` today; about half are pipeline / texture caches, half are per-instance state (`clip_trigger_cycle`, `anim_progress`, `last_trigger_count`, etc.).

### Port-shadows-param convention
**🔴 Convention duplicated 8 times.**

`fn read_scalar(...)` is independently defined in:
- `plasma_pattern_2d.rs:137`
- `distance_to_point.rs:96`
- `field_combine.rs:72`
- `smoothstep_texture.rs:73`
- `generate_lissajous.rs:101`
- `sin_term.rs:112`
- `affine_scalar.rs:57`
- `math.rs:69`

All eight have the same shape: try `ctx.inputs.scalar(name)`, fall through to `ctx.params.get(name)`, default on miss. Slight variations in handling Int / Enum / Bool collapse. Centralisation candidate. Without it, future primitives will copy the closest neighbour's version and the small variations multiply.

### `composition_notes` field
**🟡 Surface for AI authoring, no runtime reader today.**

`PrimitiveSpec::description()` → `PrimitiveDescription` → never consumed by anything beyond the primitive's own unit tests and the picker (which uses `purpose`, not `composition_notes`). Memory `project_primitive_library_for_ai_authoring` says the audience is MCP/API agents — so the surface is correct, just not wired through yet. Fine, but worth documenting "this string ships in the registry, no UI consumes it yet."

### `primitive!` macro structure
**🟢 Macro does what the docs say.** Generates struct, `new()`, `Default`, `<NAME>_TYPE_ID` constant, `PrimitiveSpec` impl, AI-metadata fields. Author writes only `Primitive::run` + uniform structs + WGSL. The blanket `impl<P: Primitive + 'static> EffectNode for P` at line 167 means primitives just slot into the graph runtime without further wiring.

**One sharp edge** (already documented in [render_lines.rs:64-67](../crates/manifold-renderer/src/node_graph/primitives/render_lines.rs#L64-L67)): the macro emits `extra_fields` as `pub` struct fields, so any type used there must also be `pub`. That's why `EdgeInstance` is module-public in `render_lines.rs` — a non-obvious pitfall for the next primitive author.

---

## 9. Two-thread invariants

**🟢 Generator rendering runs on the content thread.**

The whole `GeneratorRenderer` is constructed in `ContentPipeline::new`, owned by `ContentThread`. No UI-thread access to `Box<dyn Generator>` state. The UI sees only the immutable `Arc<Project>` snapshot + the `GraphSnapshot` for the watched editor canvas.

**`active_generator_graph_snapshot`** at [content_thread.rs:1192-1255](../crates/manifold-app/src/content_thread.rs#L1192-L1255):
- Runs **once per state push** (typically 60 Hz, but only when the editor canvas is watching a generator layer).
- Clones the override `EffectGraphDef` (`d.clone()`).
- If no override, `serde_json::from_str` parses the bundled JSON every push.
- If override is missing `preset_metadata`, grafts it back from the bundled JSON (also a parse).

Cost is small (one preset, one layer, only when editor open). Not a hot path concern, but worth noting that opening the graph editor on a complex generator preset will JSON-parse the preset once per content tick. A cached `GraphSnapshot` (invalidated by `generator_graph_version`) would eliminate this.

**🔴 Critical asymmetry between snapshot and runtime.**

[content_thread.rs:1209-1226](../crates/manifold-app/src/content_thread.rs#L1209-L1226) grafts `preset_metadata` from the bundled JSON onto an override that lost it, **only when building the editor snapshot**. The runtime path through `GeneratorRegistry::create_with_override` → `JsonGraphGenerator::from_def_with_device` → `JsonGraphGenerator::from_def` does **NOT** do this graft. `from_def` reads `doc.preset_metadata.as_ref().map(|m| m.bindings.clone()).unwrap_or_default()` at [line 228-232](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L228-L232).

**Consequence:** a layer whose `generator_graph` override has `preset_metadata = None` will render with **zero bindings** — all inner-node params pinned at their JSON defaults. The editor canvas will show the correct routings (because of the graft), but the live frame will not respond to the outer-card sliders. The mismatch is silent and would manifest as "the right column UI works in the editor but the sliders do nothing during playback" — a particularly nasty live-show bug because the user has visual feedback that the param is being set.

How likely is `preset_metadata = None` on a live override? Today the override is written by `Toggle/Add/Set` graph commands, which (per the commands' implementation) carry `preset_metadata` forward from the bundled preset on the first `take()`/lift. So the gap might be unreachable in practice — but it's load-bearing on every single command's correctness, and one buggy command will silently strand a generator.

Either: graft `preset_metadata` in `JsonGraphGenerator::from_def`, or class-test that every Generator graph-edit command preserves `preset_metadata`.

---

## 10. Other surfaces worth noting

**🟡 `from_def` validation gaps.** [JsonGraphGenerator::from_def](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L172-L324):
- Checks boundary-node presence ✓
- Does NOT check that any `StateStore`-requiring primitive is absent (see §6).
- Does NOT check that every binding's `id` matches a `params[].id` — the sweep test `every_bundled_preset_binding_resolves_to_an_outer_param` catches this at CI time only. A user's per-layer override could in principle violate this and the binding would silently log + drop at construction.

**🟠 `pre_allocate_array_buffers` skips zero-byte allocations** [json_graph_generator.rs:434-443](../crates/manifold-renderer/src/generators/json_graph_generator.rs#L434-L443) with a warn. Downstream primitives then see no buffer and silently return (for 22 of 25 — see §3). The warn is correct but the symptom chain is still "black frame, one log line."

**🟢 `RenderTargetPool` recycling.** The pool is keyed by `(PortType, format)` so chains with mixed `Rgba16Float`/`Rgba32Float` outputs don't collide. ✓

**🟡 `inventory::iter::<GeneratorAliasMetadata>` runs at first registry access** ([generator_definition_registry.rs:47](../crates/manifold-core/src/generator_definition_registry.rs#L47)) inside a `Lazy`. Cost is one-time, but in tests that build the registry fresh per test, it's per-test. Not a production concern.

---

## Severity rollup

**🔴 Live-show bug class still in tree:**
- §2 `fluid_simulation_3d`, `fluid_sim_core`, `wireframe_zoo` use raw `% N` without `ClipTriggerCycle` — same exposure as the Lissajous bug Peter just hit.
- §3 22 of 25 Array<T> producers still silently return on missing pre-bound buffer.
- §6 JSON generator path will panic on any stateful primitive (Smoothing/Feedback/ArrayFeedback/etc.) — no friendly load-time check.
- §5 `JsonGraphGenerator::reset_state` is no-op; export warmup re-seek doesn't reset generator state for graph generators.
- §9 `from_def` doesn't graft preset_metadata, so a metadata-stripped override silently loses every binding.

**🟠 Fragility / future-proofing:**
- §1 per-construction `Box::leak` of binding param names is unbounded growth (small but non-zero).
- §2 graph-defined `ClipTriggerCycle` resets on graph-editor edit, with 1/N chance of same-pattern flash.
- §6 paused-layer GPU state isn't evicted until project structural change.
- §8 `max_capacity` is a string-match convention, not an enforced port property.
- §8 `read_scalar` duplicated across 8 primitives with subtle variations.

**🟡 Hygiene:**
- §1 `HashMap` instead of `AHashMap` in construction-only paths.
- §4 alias surface has four different names for the same concept.
- §7 `plasma.rs` is compiled but never called.
- §8 `composition_notes` not yet consumed by anything but the primitive's own tests.
- §9 editor snapshot reparses bundled JSON every content tick when editor canvas is open.

**🟢 Working as designed (calling out for the record):**
- Hot path has no per-frame allocations after construction.
- `ClipTriggerCycle` math is correct and well-tested at all call sites that use it.
- Param-alias resolution is load-time only, with cycle detection.
- `trigger_count` preservation covers all three rebuild paths.
- Plan/resource lifecycle is correct; resize properly invalidates and re-pre-allocates.
- Primitive macro generates exactly the boilerplate the comments say.

---

## Resolution log

All 🔴 and 🟠 items closed. Mapping from finding → fix:

| Finding | Fix |
|---|---|
| §2 — `fluid_simulation_3d`, `fluid_sim_core`, `wireframe_zoo` use raw `% N` | `ClipTriggerCycle` dropped into all three sites; raw `trigger_count % PATTERN_COUNT` (or `SHAPE_COUNT`) replaced with `cycle.step(trigger_count, N)`. |
| §3 — 22 of 25 Array<T> producers silently return on missing pre-bound buffer | Architecturally closed by §8: the pre-allocator now always finds a capacity via [`EffectNode::array_output_capacity`](../crates/manifold-renderer/src/node_graph/effect_node.rs) (no string-match miss possible) and a CI sweep test prevents new primitives from regressing the invariant. Per-frame draw-site warns would be redundant noise. |
| §5 — `JsonGraphGenerator::reset_state` was no-op | Implemented: walks every node calling `clear_state`, then `state_store.cleanup_all()`. Export warmup re-seek now correctly clears `anim_progress`, `ArrayFeedback` prev buffers, `ClipTriggerCycle.last_emitted`, `Feedback` prev-frame textures, `EnvelopeFollower` accumulators. |
| §6 — JSON generator path used `execute_frame_with_gpu`, would panic on any stateful primitive | `JsonGraphGenerator` now owns a `StateStore` field and dispatches through `execute_frame_with_state`. Generator presets can now compose `Feedback`, `ArrayFeedback`, `Smoothing`, `EnvelopeFollowerAr`, `Temporal::*` without per-frame panics. State is keyed by `(NodeInstanceId, owner_key=0)` because the JsonGraphGenerator is itself per-layer. |
| §7 — `plasma.rs` shadowed dead code | Deleted, along with its `shaders/plasma_compute.wgsl` kernel. The JSON `Plasma.json` preset is now the only path. |
| §8 — `max_capacity` was a string-match convention | Promoted to a port-level method on `EffectNode`: [`array_output_capacity(port, params, input_capacities) -> Option<u32>`](../crates/manifold-renderer/src/node_graph/effect_node.rs). Default reads `params["max_capacity"]` for producer primitives. Transform primitives (`integrate_particles`, `rotate_3d`, `project_4d`, `displace_mesh`, etc.) override to inherit capacity from the matching input port. Computed-capacity primitives (`scatter_particles`, `scatter_particles_3d`, `fluid_project_scatter_2d`, `triangulate_grid`) override to multiply their dimension params. CI test [`every_array_output_declares_a_valid_capacity_source`](../crates/manifold-renderer/src/node_graph/primitives/mod.rs) walks the registry and asserts every Array output resolves with default params + assumed inputs. |
| §8 — `read_scalar` duplicated 8 times | Centralized as [`EffectNodeContext::scalar_or_param(name, default)`](../crates/manifold-renderer/src/node_graph/effect_node.rs). All 8 local copies deleted. |
| §9 — `from_def` didn't graft `preset_metadata` | New public helper [`graft_preset_metadata_from_bundle`](../crates/manifold-renderer/src/generators/registry.rs) called both by `GeneratorRegistry::create_with_override` (runtime path) and by `ContentThread::active_generator_graph_snapshot` (editor canvas path). Bindings now resolve identically on both surfaces; an override with `preset_metadata = None` can no longer silently strand every binding on its default. |
| §9 — editor snapshot re-parsed bundled JSON every tick | New cache field [`cached_generator_graph_snapshot`](../crates/manifold-app/src/content_thread.rs) on `ContentThread`, keyed by `(LayerId, generator_graph_version)`. Snapshot returned as `Arc<GraphSnapshot>`. Rebuild only fires on layer switch or graph-edit landing. |
| §1 — `Box::leak` per binding per generator construction | `Graph::set_param` (and `set_param_exposed`) now accept `&str` instead of `&'static str`; the canonical `&'static str` key is sourced from the primitive's `parameters()` list during the validation lookup. `BindingResolution` stores `target_param: String` — no leak, no interning structure required. |
| §1 — `HashMap` vs `AHashMap` in construction paths | `outer_param_index` and `handle_map` in `JsonGraphGenerator::from_def` switched to `ahash::AHashMap`. |
| §10 — `from_def` validation gaps (no `StateStore`-requires check) | Closed by §6: with `StateStore` plumbed, the executor's entry-point `assert!` is unreachable for any stateful primitive a preset author wires in. |
| §2 — graph-defined `ClipTriggerCycle` reset on graph-editor rebuild | Accepted as documented limit per memory `feedback_graph_editor_is_authoring_not_perform`. `trigger_count` preservation guarantees visual continuity at the modulo level; only a 1/N "same pattern across an authoring-time rebuild" possibility remains, and graph-edits aren't performance-time actions. |
| §6 — paused-layer GPU state eviction | Left as-is per user direction. Snap-back-on-resume preserves the live-show contract; bounded RAM cost is acceptable. |

Deferred (out of scope this pass):
- §4 alias-surface naming churn (`legacy_param_aliases` / `param_aliases` / `paramAliases` / `GeneratorAliasMetadata` rename).
- §8 `composition_notes` AI-surface wiring (the `PrimitiveSpec::description()` shape exists; the consumer doesn't).

Tests: `cargo clippy --workspace -- -D warnings` clean. `cargo test --workspace` — all crates green, including the new `every_array_output_declares_a_valid_capacity_source` registry sweep.
