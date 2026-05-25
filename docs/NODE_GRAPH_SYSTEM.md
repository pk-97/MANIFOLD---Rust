# Node-Based Effect & Generator System

**Status:** Shipped. The legacy linear-effect-chain dispatcher was deleted in May 2026; every shipping effect now runs through the node graph. This doc captures the current architecture and what remains parked. Design rationale for individual sub-systems lives in sibling docs (linked inline).

**Last updated:** 2026-05-19

---

## 1. Overview

MANIFOLD's effects and generators run as **node graphs**. Each preset is a graph of small typed nodes (primitives) wired together; the graph editor lets users (and AI agents) crack open any preset, see the internal structure, modify it, and save the result.

The unit of authoring is the **primitive** — a single GPU dispatch with a fixed port shape (e.g. `Threshold`, `Gain`, `Feedback`, `ColorSample`). The unit of distribution is the **preset** — a JSON file under `assets/effect-presets/` describing the graph topology + parameter bindings + card-UI metadata. The runtime is a single dispatcher (`ChainGraph`) that walks the graph and emits Metal dispatches.

The system is intuitive by default — drop a preset, it works — and exposes power-user depth (rewiring, exposing internal params, scalar-driven modulation) only when reached for.

---

## 2. Design Principles

1. **Decomposition is opt-in, not required.** A user shouldn't need to understand how a fluid sim works to use one. Single-node atomic primitives are first-class.
2. **Clip-agnostic.** The graph runtime doesn't know about clips, layers, or any host concept. State lives in graph instances.
3. **Stable IDs are forever.** Once shipped to a real user, type IDs and parameter names are public API. Additive evolution only.
4. **Bundle wins.** When a project's bundled composite differs from a future user-library version, the bundle is canonical for that project.
5. **Generator/effect distinction collapses.** A graph is a graph. Whether it acts as a generator or an effect is determined by its boundary port shape (`Source` → it's an effect; no `Source` → it's a generator).
6. **JSON is the source of truth for presets.** Rust composite builders (`build_bloom`, `build_strobe_opacity`, …) survive only as dev fixtures for parity tests against legacy fused shaders. New presets ship as JSON only.

---

## 3. Core Concepts

### 3.1 Primitives

Every node in the graph is an instance of a [`Primitive`](../crates/manifold-renderer/src/node_graph/primitive.rs). A primitive declares (via the `primitive!` macro):

- A stable **`type_id`** — e.g. `"node.gain"`, `"node.chromatic_aberration"`. Treated as public API once shipped.
- **`inputs`** — named typed ports (Texture2D, Texture3D, Scalar(F32/V2/V3)). Each is required or optional.
- **`outputs`** — named typed ports.
- **`params`** — typed scalar parameters with default + range + optional enum labels.
- A **`run`** method — executes one frame given an `EffectNodeContext` with bound inputs, outputs, params, GPU encoder, and optional `StateStore` for stateful primitives.

Primitives are registered via `inventory::submit!` so adding a new file under `crates/manifold-renderer/src/node_graph/primitives/` is the only step required — no central registry edit. See [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md).

### 3.2 Graphs

A `Graph` is a topologically-sorted DAG of `NodeInstance`s connected by `NodeWire`s. Connection legality is enforced at `connect` time (port types must match). Boundary nodes (`Source`, `FinalOutput`) mark the graph's external interface.

At runtime, an `ExecutionPlan` (compiled by `execution_plan.rs`) carries the topo-sorted step list, the texture-lifetime plan, and the resource bindings table. The `ChainGraph` runs this plan once per frame.

### 3.3 Presets

A **preset** is a `LoadedPresetView` over a JSON file. `assets/effect-presets/<TypeId>.json` is scanned at build time by `build.rs`, which emits `BUNDLED_PRESETS_GENERATED` into the crate. Each preset carries:

- `nodes` + `wires` (the graph topology)
- `presetMetadata.params` (the effect-card slider list)
- `presetMetadata.bindings` (how card sliders route to inner-node params or wires)
- `presetMetadata.skipMode` (the optimisation hint — see §6.2 of [EFFECT_RUNTIME_UNIFICATION.md](EFFECT_RUNTIME_UNIFICATION.md))

Composite Rust builders under `composites/` still exist (Bloom, Halation, Infrared, Mirror, SoftFocus, StrobeOpacity) but are used only for parity tests against legacy fused shaders. Shipping artifacts are the JSON files.

---

## 4. Port Types

Three kinds of wire live in the graph today:

- **Texture2D** — the bread-and-butter colour buffer. `Rgba16Float` for intermediates, `Rgba32Float` on demand.
- **Texture3D** — used by FluidSim3D and any future volume primitive.
- **Scalar** — `F32`, `Vec2`, `Vec3`. Allows parameter-as-wire: an LFO node's `out` (Scalar(F32)) wires into a Bloom node's `threshold` (Scalar(F32) input port).

**Buffer** ports (particles, mesh data, audio waveforms, blob lists) remain V2 — generators with internal Buffer state (particle systems, mesh generators) still keep that state opaque inside the primitive.

### 4.1 Port-shadows-param convention

When a primitive declares a scalar input port with the same name as one of its `ParamDef`s, the wire wins when present and the param is the fallback. This is the standard pattern for control-rate modulation — used by `node.gain`, `node.wet_dry`, `node.affine_transform.rotation/translate_x/translate_y`, `node.smoothing.time_constant`, `node.feedback.amount`, `node.chromatic_aberration.amount`.

In the editor: rows whose param is currently driven by a wire show `← wired` and disable the expose checkbox + value cell, so users can't double-bind the same parameter.

### 4.2 Texture→Scalar bridges

A small family of primitives reduce a texture to a scalar via shared-mode `MTLBuffer` readback (one-frame latency):

- **`node.luminance`** — average luma.
- **`node.peak`** — max luma.
- **`node.color_sample`** — region-averaged RGB at a configurable UV, plus a `luma` aux output.

These close the loop between image content and scalar modulation. ColorCompass uses four `color_sample`s arranged at cardinal UVs to drive `affine_transform.translate_x / translate_y`; SmearMosh uses `edge_detect → luminance → smoothing` to drive `chromatic_aberration.amount` based on per-frame edge density.

---

## 5. Catalog

Live registries beat hand-maintained tables — the inventory channels populate the catalog at compile time, and the JSON preset directory is browsable directly.

- **Primitives** — 30+ shipping in `crates/manifold-renderer/src/node_graph/primitives/`. See [NODE_CATALOG.md](NODE_CATALOG.md) for the curated naming + categorisation spec, [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) for the design rationale and decomposition recipes.
- **Atomic complex primitives** — `crates/manifold-renderer/src/node_graph/atomic/` holds the three irreducible kernels (Plasma, FluidSim2D, Glitch); FluidSim3D lives alongside the primitives. These don't decompose to atoms without losing what they are.
- **Composite Rust builders** — `crates/manifold-renderer/src/node_graph/composites/` (Bloom, Halation, Infrared, Mirror, SoftFocus, StrobeOpacity). Dev fixtures for parity tests; new composites ship as JSON.
- **Shipping presets** — `crates/manifold-renderer/assets/effect-presets/` (29 as of 2026-05-19). Each is one JSON file; the build script codegens the bundled table.

---

## 6. State and Lifecycle

- **Graphs are clip-agnostic.** The runtime knows nothing about clips, layers, or hosts.
- **State lives in a `StateStore`** keyed by `(owner_key, node_id)`. One Bloom-preset used on three clips = three independent state entries.
- **Lifecycle is RAII.** State entries are seeded when the chain is built, cleared when the chain is rebuilt, evicted when the chain is freed.

State-impacting operations:

- **Seek / project load** → `clear_all_effect_state` walks every `ChainGraph` and every primitive instance.
- **Layer goes idle** → that layer's chain `clear_state` fires.
- **Clip removed** → chain freed, state evicted.

Stateful primitives today: `Feedback` (`prev: RenderTarget`), `Smoothing` (`previous: f32`), `BlobTracking` (background worker handle), `DepthOfField` depth mode (MiDaS worker), `Watercolor` (pigment ping-pong). Lifecycle hooks live in [EFFECT_CHAIN_LIFECYCLE.md](EFFECT_CHAIN_LIFECYCLE.md).

---

## 7. Parameter System

### 7.1 Card UI — unchanged

Effect cards present a flat list of named, typed parameters. MIDI/OSC mapping, modulation envelopes, and Ableton mapping route to card slots by `ParamId`. The outside world doesn't know whether an effect is a JSON-defined graph or a Rust-defined composite.

### 7.2 Bindings — unified (May 2026)

A card-slider write resolves through a single `Vec<ResolvedBinding>` on each effect slot. The binding can target an inner-node param, a control-wire scalar source, a composite-Rust handle, or a custom slot. See [BINDINGS_UNIFICATION_PLAN.md](BINDINGS_UNIFICATION_PLAN.md) (the closed historical record) and §7.11 of [EFFECT_RUNTIME_UNIFICATION.md](EFFECT_RUNTIME_UNIFICATION.md).

The bug class that motivated unification — passing `&[]` for the user-bindings slice — is now structurally unrepresentable. Wire format carries `ParamId` directly; no `pi: usize → ParamId` translation layer remains.

### 7.3 Expose flow

Inside the graph editor, every inner-node param row has an expose checkbox. Checking it adds a `bindings` entry to the effect's `EffectInstance.user_param_bindings`, which routes through the same `ResolvedBinding` pipeline as static spec bindings. Wire-driven rows are checkbox-disabled (see §4.1).

---

## 8. Shader Fusion (Graph Compiler)

**Status:** Architecturally committed, implementation deferred.

[PRIMITIVE_LIBRARY_DESIGN.md §12.5](PRIMITIVE_LIBRARY_DESIGN.md) commits the stance: *decompose at authoring time, fuse at compile time.* The editor sees small primitives; the GPU runs fused dispatches for per-pixel chains.

The fusion classification (pixel-local / UV-rewriting / neighborhood / reduction / multi-pass / stateful), partition algorithm, and `naga_oil`-based toolchain are designed in detail at §8.2–§8.4 of this doc's predecessor and still apply.

**Why deferred:** §12.5 / §12.9 of PRIMITIVE_LIBRARY_DESIGN explicitly defers until measured frame-budget pressure on a real show file demands it (Profile a fully-decomposed FluidSim or Black Hole on the Liveschool fixture before committing to the fusion infrastructure). §12.6 also softened the urgency — scalar wires removed the fp16-quantisation motivation, so the remaining pressure is purely dispatch overhead, which is fine at current scale.

---

## 9. Live Editing

**V1 (current):** Parameter tweaks during playback are free. Topology edits (adding nodes, rewiring) trigger a synchronous chain rebuild on the content thread; rebuilds are rare enough post-chain-pool-refactor that the cost is invisible in practice.

**V2 (parked):** Atomic `Arc<ExecutionPlan>` swap with state-continuity rules. The compile thread is designed but not built — see §10. The trigger for building it is editing during a live show becoming a routine performer surface (see §12.9 "Mid-show preset editing safety" in PRIMITIVE_LIBRARY_DESIGN.md).

---

## 10. Background Compilation

**Status:** Not started. Today's flow is synchronous on the content thread.

When topology changes (chain rebuild), `ChainGraph::new` walks the new graph, allocates render targets via the pool, and builds Metal pipelines via `naga`. Rebuilds are rare in steady state — typically only on clip activation, layer reorder, or preset swap — so the synchronous path doesn't hurt frame pacing today.

If live topology editing during playback ships, this becomes load-bearing: compilation moves off the content thread, the executor swaps the ExecutionPlan Arc atomically on the next frame boundary.

---

## 11. Migration (Done)

The May 2026 migration ran in two coordinated arcs:

- **§11 Preset migration** (blocks 4–9): every shipping effect moved from `inventory::submit! { EffectMetadata, EffectFactory }` to `assets/effect-presets/<TypeId>.json` + `build.rs` codegen. `EffectRegistry`, `EffectFactory`, `metadata_by_id`, `effect_category_registry`, and 21 orphan `.rs` files were deleted. The graph runtime is the only dispatcher.
- **Bindings unification** (Phases 1–5): static + user binding paths collapsed onto one `ResolvedBinding` walk, one cache, one `ParamConvert` enum, `ParamId` on the wire.

Projects from before the migration load unchanged. The legacy `PostProcessEffect` and `Generator` traits are gone — there is no coexistence period any longer; the trait-wrapped path was always a migration scaffold.

---

## 12. Library and Sharing (V2)

User-saved composites are not yet shipped. The shipping presets in `assets/effect-presets/` are the only graph artifacts a user encounters today.

Designed but parked:

- **UUID + content hash** for composite identity.
- **App-scoped library** at `~/Library/Application Support/Manifold/library/`.
- **Project bundling** of custom composites into the V2 ZIP under `graphs/custom_composites/`.
- **Bundle wins** on identity-collision with the user's library.

The mid-show editing safety question (§12.9 of PRIMITIVE_LIBRARY_DESIGN) is upstream of shipping user composites — it dictates whether mid-show edits apply live, undo cleanly, and handle node-removal without breaking a render.

---

## 13. Save Format

Preset JSON schema lives in [`crates/manifold-core/src/effect_definition_registry.rs`](../crates/manifold-core/src/effect_definition_registry.rs) and the structures it points at. Key invariants:

- `version: 2` is the current schema.
- `nodes` carry stable `typeId` (`"node.gain"`, `"node.feedback"`, …) — treated as public API.
- `params` are a typed map by name; additive evolution only (new optional fields with defaults are fine; renames and semantic changes require a new type ID).
- `presetMetadata.bindings` carry `ParamId` (not positional index).
- `presetMetadata.skipMode` is the optimisation hint for the zero-cost skip-passthrough path.

User-saved composites (V2) will land under `graphs/custom_composites/<id>.json` inside the project ZIP.

---

## 14. Open Questions

Real ones, parked. Not the "(none yet)" placeholder from V0.

- **Mid-show preset editing safety.** If presets become M4L-style devices and the graph editor is the depth, editing during a live performance is a real possibility. What gets undo? What happens if a user removes a node mid-render? See §12.9 of PRIMITIVE_LIBRARY_DESIGN.md.
- **Rebake-on-change scheduler caching.** Heavy generators (Black Hole's deflection map, ParametricSurface mesh build) need per-node dirty bits so the executor can skip re-evaluation when inputs haven't changed. Similar pattern to skip-passthrough but content-based. See §12.9 of PRIMITIVE_LIBRARY_DESIGN.md.
- **When to build fusion-on-compile.** Pending a measured profile on a fully-decomposed Black Hole or FluidSim on the Liveschool fixture. See §8 above and §12.5 / §12.9 of PRIMITIVE_LIBRARY_DESIGN.md.
- **Chain pool rekey by semantic ID.** [CHAIN_POOL_REFACTOR_PLAN.md](CHAIN_POOL_REFACTOR_PLAN.md) is audited and designed but not started. Layer reorder still hits the positional-indexing bug class.
- **Array port / Buffer port.** §12.3 of PRIMITIVE_LIBRARY_DESIGN.md promotes Array (particle pipelines) to V1. Buffer (audio waveforms) and 3D-volume primitives remain V2.

---

## 15. References

- [PRIMITIVE_LIBRARY_DESIGN.md](PRIMITIVE_LIBRARY_DESIGN.md) — primitive catalog, decomposition recipes, parity tests, design rationale
- [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) — authoring a new primitive
- [ADDING_EFFECTS_AND_GENERATORS.md](ADDING_EFFECTS_AND_GENERATORS.md) — authoring a new preset (JSON workflow)
- [NODE_CATALOG.md](NODE_CATALOG.md) — naming + categorisation spec
- [EFFECT_RUNTIME_UNIFICATION.md](EFFECT_RUNTIME_UNIFICATION.md) — runtime design, bindings, skip-passthrough, IR
- [BINDINGS_UNIFICATION_PLAN.md](BINDINGS_UNIFICATION_PLAN.md) — historical record of Phases 1–5
- [EFFECT_CHAIN_LIFECYCLE.md](EFFECT_CHAIN_LIFECYCLE.md) — chain pool, state-cache eviction, feedback bleed-through
- [MANIFOLD_GPU_ARCHITECTURE.md](MANIFOLD_GPU_ARCHITECTURE.md) — Metal backend, texture formats, uniform layout
- `crates/manifold-renderer/src/node_graph/` — module structure for the runtime
- `crates/manifold-renderer/assets/effect-presets/` — shipping presets
