# Node-Based Effect & Generator System

**Status:** Design phase. Not yet implemented. This document captures the architecture, V1 scope, and phased roadmap agreed during design discussion.

**Last updated:** 2026-04-30

---

## 1. Overview

Replace MANIFOLD's linear effect-chain model with a **node graph** at the per-effect level. Each effect/generator can be cracked open via a cog icon (UX deferred) to reveal an internal graph of high-level nodes the user can rewire, modify, and recombine.

The motivating use case: a user clicks a cog on Bloom and sees its internal multi-pass structure (Threshold → MipChain → Blur → Add) as wired nodes. They can tweak each pass, reorder, drop in additional nodes, and save the result as a new custom effect (`MyBloom`). For complex atomic kernels like FluidSim, the user can't crack open the simulation itself, but they can wire its internal data (density, velocity) into other nodes, enabling visuals that aren't possible today.

The system is **intuitive by default** — drag a node, it works — and exposes power-user depth (rich port surfaces, custom composites, sub-graphs) only when reached for.

---

## 2. Design Principles

1. **Decomposition is opt-in, not required.** A user shouldn't need to understand how a fluid sim works to use one. Single-node atomic effects are first-class.
2. **Clip-agnostic.** The graph runtime doesn't know about clips, layers, or any host concept. State lives in graph instances.
3. **Stable IDs are forever.** Once shipped to a real user, type IDs and parameter names are public API. Additive evolution only.
4. **Bundle wins.** When a project's bundled composite differs from the user's library version, the bundle is canonical for that project. Predictability over convenience.
5. **Phase aggressively.** Every phase ships independently and is independently useful. No "we built half a thing."
6. **Generator/effect distinction collapses.** A graph is a graph. Whether it acts as a generator or an effect is determined by its boundary port shape.

---

## 3. Core Concepts

### 3.1 The `EffectNode` abstraction

Every effect, generator, primitive, boundary node, and user-saved composite is an `EffectNode`. Each `EffectNode` has:

- A **type identity** (`EffectNodeType`) — stable string ID (`primitive.blur`, `effect.bloom`, `composite.user.<uuid>`)
- A set of **`NodeInput`s** — each with a name, type, and required/optional flag
- A set of **`NodeOutput`s** — each with a name and type, including one designated **default output**
- A set of **parameters** — typed, named, rangeable, with per-parameter expose flag
- A per-frame **evaluate** step — runs once per frame given inputs, parameters, and a place to write outputs
- Optional **per-instance state** — held by the node's instance, not by the graph or host

`NodeInput` and `NodeOutput` are aliases for the underlying `NodePort` struct, distinguished by `PortKind`. Connections between nodes are `NodeWire`s, each binding one node's output port to another node's input port.

The **default output** convention keeps simple connections one-click: dragging a `NodeWire` from A to B connects `A.default → B.default` unless the user explicitly picks a different port.

### 3.2 Two flavors, same trait

- **Atomic `EffectNode`s** — implemented in Rust + Metal, opaque internals. FluidSim, Plasma, Glitch, Voronoi, Kaleidoscope, mesh generators, etc.
- **Composite `EffectNode`s** — defined as a sub-graph of other `EffectNode`s. Bloom-rebuilt-from-primitives, user-built customs, alias presets like Mirror.

Both implement the same `EffectNode` trait. The graph engine doesn't distinguish them.

### 3.3 Mix and match is free

Once everything is a node, "generator" and "effect" stop being categories of node. They become properties of a graph's port shape:

- A graph with no Source input = generator-shaped
- A graph with a Source input = effect-shaped

Inside any graph, any node can be used regardless of how it's typically labelled. A generator graph can use Blur. An effect graph can use Plasma. A FluidSim's `spawn_mask` input can be driven by another clip's pixels, turning the fluid sim from a generator into an effect.

### 3.4 Boundary nodes — Source and FinalOutput

Every composite graph has explicit boundary `EffectNode`s:

- **`Source`** nodes have no `NodeInput`s, only `NodeOutput`s. They represent data coming in from outside the graph (the clip's pixels, depth buffer, audio buffer, etc.).
- **`FinalOutput`** nodes have only `NodeInput`s. They represent data leaving the graph for the host — the finished result.

`Source` and `FinalOutput` are always present in composites and cannot be deleted. They are inserted automatically when a new graph is created.

**V1 constraint:** composites have at most one `Source` (Texture2D) and exactly one `FinalOutput` (Texture2D). Multi-Source / multi-FinalOutput composites — which would let users build their own rich-ports nodes — defer to a later phase.

---

## 4. Port Types

V1 supports three port types:

- **Texture2D** — the bread and butter. RGBA color buffer.
- **Texture3D** — for volume rendering (MRI, 3D fluid sim density/velocity).
- **Scalar** — Float, Vec2, Vec3, Vec4, Color. Allows parameter-as-wire (e.g. audio level → bloom intensity).

**Buffer** ports (particle positions, mesh data, audio waveforms, blob lists) defer to V2. This means several existing generators (oscilloscope, particle systems, mesh generators) keep their Buffer-shaped data fully internal in V1.

Adding a port type later is a real cost — every existing node has to potentially understand the new type. Keep the V1 set tight.

---

## 5. Node Catalog

### 5.1 Categorization

| Category | Description | V1 count |
|---|---|---|
| **Primitives** | Small reusable building blocks. The vocabulary of the graph editor. | 10 |
| **Boundary nodes** | Source, FinalOutput. Graph boundaries. | 2 |
| **Atomic complex** | Irreducibly one thing — sims, simulations, complex kernels. | 3 |
| **Composite presets** | Built-in graphs that compose primitives. Cog opens them. | 5 |
| **Wrapped legacy** | Existing effects/generators implementing the new trait, no decomposition. | ~35 |

### 5.2 Effect classification (full catalog)

| Name | Kind (target) | Optional Inputs | Aux Outputs | Notes |
|---|---|---|---|---|
| auto_gain | Atomic+Ports | — | `current_gain` | Histogram-driven |
| blob_tracking | Atomic+Ports | — | `blobs` (Buffer, V2) | Plugin-backed |
| **bloom** | **Composite** | — | `bright_pass`, `blur_chain` | Threshold → MipChain → Blur×N → Add |
| chromatic_aberration | Atomic | — | — | ChannelOffset is conceptual primitive |
| **color_grade** | **Composite (V2)** | — | — | Needs Curves+LiftGammaGain primitives |
| depth_of_field | Atomic+Ports | `depth` | `coc_mask` | |
| dither | Atomic | `pattern` | — | |
| edge_detect | Atomic | — | `edge_mask` | Also a primitive in palette |
| edge_stretch | Atomic+Ports | — | `edge_mask` | |
| glitch | Atomic | — | — | V1 atomic node example |
| **halation** | **Composite** | — | — | Reuses Bloom's chain with tint |
| hdr_boost | Atomic | — | — | |
| **infrared** | **Composite** | — | — | Luminance → GradientMap |
| invert_colors | Atomic | — | — | |
| kaleidoscope | Atomic | — | — | |
| mirror | **Alias preset** | — | — | UVTransform[mirror mode] |
| quad_mirror | Alias preset | — | — | UVTransform with different mode |
| strobe | Atomic | — | — | |
| **stylized_feedback** | **Composite (V2)** | `modifier` | `history` | Feedback → Transform → Blend |
| transform | Alias preset | — | — | UVTransform itself |
| voronoi_prism | Atomic | — | — | |
| watercolor | Atomic+Ports | — | `pigment_density` | |
| wireframe_depth | Atomic+Ports | `depth` | — | |

### 5.3 Generator classification (full catalog)

| Name | Kind (target) | Optional Inputs | Aux Outputs | Notes |
|---|---|---|---|---|
| basic_shapes_snap | Atomic | — | — | |
| black_hole | Atomic+Ports | — | `lens_field` | |
| concentric_tunnel | Atomic | — | — | |
| digital_plants | Atomic+Ports | — | `branches` (Buffer, V2) | |
| duocylinder | Atomic+Ports | — | `mesh` (Buffer, V2) | |
| **fluid_simulation** | **Atomic+Ports** | `force_field`, `spawn_mask`, `dye_color` | `density`, `velocity`, `pressure` | V1 hero example |
| fluid_simulation_3d | Atomic+Ports | `force_field` (3D), `spawn_mask` (3D) | `density` (3D), `velocity` (3D) | |
| galactic_rock | Atomic+Ports | — | `mesh` (Buffer, V2) | |
| lissajous | Atomic+Ports | — | `points` (Buffer, V2) | |
| metallic_glass | Atomic | — | — | |
| mri_volume | Atomic+Ports | — | `volume` (3D) | |
| mycelium | Atomic+Ports | — | `branches` (Buffer, V2) | |
| nested_cubes | Atomic+Ports | — | `mesh` (Buffer, V2) | |
| oily_fluid | Atomic | — | — | |
| oscilloscope_xy | Atomic+Ports | `audio` (Buffer, V2) | — | |
| parametric_surface | Atomic+Ports | — | `mesh` (Buffer, V2) | |
| particle_text | Atomic+Ports | — | `particles` (Buffer, V2) | |
| plasma | Atomic | — | — | V1 atomic example |
| star_field | Atomic+Ports | — | `positions` (Buffer, V2) | |
| strange_attractor | Atomic+Ports | — | `points` (Buffer, V2) | |
| tesseract | Atomic+Ports | — | `mesh` (Buffer, V2) | |
| text | Atomic+Ports | — | `text_mask` | |
| wireframe_zoo | Atomic+Ports | — | `mesh` (Buffer, V2) | |

### 5.4 Primitive palette (target ~30, V1 = 10)

Bolded primitives are required by V1 composite presets. Italics are V1.

| Category | Primitives |
|---|---|
| Sources | SolidColor, Gradient, Noise (FBM/Perlin/Worley) |
| UV | *UVTransform* (translate/scale/rotate/mirror modes), Polar |
| Filter | *Blur* (modes), Sharpen, ***Threshold***, EdgeDetect, Pixelate, Posterize |
| Color | ***Luminance***, *ColorMatrix*, Curves, LiftGammaGain, HueSat, ***GradientMap***, ColorMath (invert/multiply) |
| Channel | ChannelOffset, ChannelSplit, ChannelCombine |
| Spatial | Downsample, Upsample, ***MipChain*** |
| Temporal | Feedback, FrameDelay, Trail |
| Compose | ***Blend*** (modes), *Mix*, Mask |
| Shape | Circle, Box, Vignette |
| Sampling | *Sample* (with explicit UV input) |

V1 list (10): UVTransform, Threshold, Blur, MipChain, Mix, Blend, Luminance, GradientMap, Sample, ColorMatrix.

---

## 6. State and Lifecycle

- **Graphs are clip-agnostic.** Runtime knows nothing about clips, layers, or hosts.
- **State lives in graph instances**, keyed by `node_instance_id` within the graph.
- One Bloom-graph used on three clips = three independent graph instances = three independent state maps. No cross-contamination.
- **Lifecycle is RAII.** State is born when the graph instance is constructed, dies when dropped. The host (clip today, anything else tomorrow) owns the instance lifetime; the graph code doesn't model that.

State-impacting operations:

- **Seek** — calls `clear_state` on every stateful node in the graph.
- **Clip removed** — graph instance dropped, all state freed.
- **Clip duplicated** — new graph instance with fresh state. State is not copied across instances.
- **Graph topology edited (V2 live editing)** — see Section 9.

---

## 7. Parameter System

### 7.1 External interface — unchanged

A graph instance presents itself externally exactly like a current effect — flat list of named, typed parameters with ranges. The card UI, MIDI/OSC mapping, and modulation envelope system work unchanged. The outside world doesn't know whether a given effect is hand-written Rust or a graph under the hood.

### 7.2 Internal — per-parameter expose flag

Inside the graph editor (V2+), each node parameter has an **expose** checkbox. When checked, the parameter appears on the effect card. The graph maintains a routing table from `exposed_param_slot` to `(node_id, param_name)` and forwards writes through it.

The expose mechanism is purely a graph-internal concern. It introduces zero changes to the parameter/modulation infrastructure outside the graph.

### 7.3 Address scheme

Existing modulation bindings target effect-card parameter slots by name/index. This continues unchanged for graph-backed effects. No new address scheme is introduced.

---

## 8. Shader Fusion (Graph Compiler)

### 8.1 Why fusion exists

Without fusion, a 5-node ColorGrade composite runs as 5 dispatches where today it runs as 1. At 4K60 this is the difference between smooth and skipping frames. Fusion compiles chains of fuseable nodes into single shaders so decomposition is roughly free for pixel-local chains.

### 8.2 Fusion categories

The graph compiler classifies each node by what it reads:

| Category | Behavior | Examples |
|---|---|---|
| **Pixel-local** | Output pixel = f(same pixel input, params) | ColorMatrix, Curves, Saturation, Threshold, Mix, Blend, Luminance, GradientMap, Invert |
| **UV-rewriting** | Transforms where to sample, not pixel value | UVTransform, Mirror, Polar |
| **Neighborhood** | Reads multiple input pixels per output pixel | Blur, EdgeDetect, Sharpen, DOF |
| **Reduction** | Reads whole image to produce small result | AutoGain (luminance), Histogram |
| **Multi-pass** | Internally several passes | Bloom mip chain, fluid sim pressure projection |
| **Stateful / temporal** | Reads from previous frame state | Feedback, Trail, FrameDelay |

**Fusion rules:**

- Pixel-local nodes fuse with adjacent pixel-local and UV-rewriting nodes into one shader.
- Neighborhood nodes break fusion with their input but can fuse with subsequent pixel-local nodes (tail fusion).
- Reduction nodes break fusion entirely; they need their own pass.
- Stateful nodes break fusion with their input; can fuse with subsequent pixel-local.
- Multi-pass nodes treat each internal pass as its own pass.

### 8.3 Toolchain

- **Custom graph compiler** — partitions the graph into fusion groups, generates WGSL source per group.
- **naga_oil** — handles WGSL composition (imports, namespacing, identifier collision avoidance, binding remapping).
- **naga** (already in stack) — compiles generated WGSL → SPIR-V → MSL.
- **Pipeline cache** — keyed by topology hash; only recompile when graph topology changes.

Custom code estimate: 700-1000 lines of Rust + WGSL templating. Bounded, well-understood work.

### 8.4 Phased rollout

- **Phase 0 (V1):** No fusion. Every node = one dispatch. ColorGrade-as-composite stays atomic in code (don't decompose yet) until fusion exists. Bloom-as-composite ships because it's already multi-pass anyway.
- **Phase 1:** Pixel-local fusion. Most performance-sensitive composites become decomposable.
- **Phase 2:** UV-rewriting fusion. Chains of UV transforms collapse into one combined transform.
- **Phase 3:** Neighborhood→pixel-local tail fusion. Diminishing returns; do if profiling demands.

### 8.5 Reference implementations to study

- **Godot Engine VisualShader** (open, MIT, C++) — closest analogue. Smaller and more readable than Blender. Recommended starting reference.
- **Blender shader nodes** (open, GPL, C++) — battle-tested, larger.
- **Bevy shader system** (open, MIT, Rust) — closest tech stack match. Heavy WGSL composition with naga_oil.
- **Unity ShaderGraph** (partial source on GitHub) — best UX of the bunch.

### 8.6 Risks and discipline

- **Category misclassification** — primitive marked pixel-local that actually reads neighborhood = silent wrong output. Tests must diff fused-vs-unfused output for every primitive.
- **Identifier collisions in concatenated WGSL** — handled by naga_oil module system, but primitive authors must follow a naming convention.
- **Pipeline cache memory** — every unique fusion group compiles a unique pipeline. Cache must survive editing without bloat.

---

## 9. Live Editing

### 9.1 V1 — parameters only

- Parameter tweaks during playback: free, no recompile.
- Topology edits during playback: disallowed. User must pause/stop to rewire.

This sidesteps every state-continuity question and ships sooner.

### 9.2 V2 — atomic ExecutionPlan swap

`Arc<ExecutionPlan>` on the content thread. Editor edits stage onto a new plan; background thread compiles new pipelines; once ready, content thread swaps the Arc on the next frame boundary.

State-continuity rules (V2 commitments):

- **Node persists across edit (same `node_instance_id`):** state carries over.
- **Node added to running graph:** state initialized to default, applied at next frame boundary.
- **Node removed from running graph:** state freed immediately on swap.
- **Whole graph replaced:** no state carries over (treated as new instance).

---

## 10. Background Compilation

- Graph compilation (WGSL emission + naga + Metal pipeline build) is **never** on the content thread or any vsync callback.
- Compile thread: dedicated background thread. Picks up a new ExecutionPlan when topology changes, builds pipelines, hands compiled plan back via channel.
- Content thread: continues running with the previous (old) ExecutionPlan until the new one is delivered. Atomic swap on next iteration.
- Parameter tweaks bypass the compile thread entirely.

---

## 11. Migration

The current ~58 effects/generators map onto the new system as follows:

- **Thin effects** (Mirror, QuadMirror, Transform, Invert) → rebuilt as one-node alias presets of primitives.
- **Decomposable composites** (Bloom, Halation, Infrared, ColorGrade, StylizedFeedback) → rebuilt as composite graphs from primitives. Each requires that the necessary primitives exist; some defer to V2 (ColorGrade needs Curves and LiftGammaGain).
- **Irreducibly atomic** (FluidSim, Plasma, Glitch, Voronoi, Kaleidoscope, Watercolor, EdgeStretch, mesh generators, particle systems, etc.) → reimplemented as atomic Nodes. Same Rust + Metal kernel as today, new trait wrapper.

Existing projects open and play **unchanged**. No migration UI required. Users can opt into rebuilding a clip's effect chain as a graph when they want to; until then, the linear chain runs as a degenerate graph (a straight line of wrapped atomic nodes).

The current `PostProcessEffect` and `Generator` traits eventually retire once the new `EffectNode` trait covers all cases, but both can coexist for the migration period.

---

## 12. Library and Sharing

### 12.1 Composite identity

- **UUID** — stable across edits. Identifies the composite as a concept.
- **Content hash** — bumped on every edit. Identifies the snapshot.
- **Library copies are immutable by default.** To "edit someone else's" composite, fork it (new UUID, new identity). Your own composites edit freely.

### 12.2 Storage tiers

- **Built-in primitives, atomic nodes, composite presets** — compiled into the binary. Stable type IDs (`primitive.blur`, `effect.bloom`). Not serialized.
- **App-scoped library** (V2) — `~/Library/Application Support/Manifold/library/`. One file per composite. Drag-and-drop installable.
- **Project bundle** (V2) — every composite a project uses is snapshot into the project ZIP under `graphs/custom_composites/`. Self-contained.

### 12.3 Collision resolution

When the project bundle and user's library have the same UUID but different content hashes:

- **Bundle wins.** Project always loads its bundled version.
- UI shows a one-line notice: "This project includes custom effects that differ from your library version." Options: install bundled to library, fork bundled as new variant, dismiss.

### 12.4 V1 scope

V1 ships **project-scoped composites only**. App-scoped library and project bundling are V2 features.

---

## 13. Save Format

- New `graphs/` directory in V2 ZIP — additive, no V2→V3 bump needed.
- **Schema version field** at the top of each graph file.
- **Stable type IDs** as `domain.name` strings. Treated as public API once shipped.
- **Parameters stored as typed map by name** (additive evolution).
- **Per-clip graph instances** inlined into `project.json` for V1; can be split out later if file size demands.
- **Forward compatibility rules:** additive only. New optional fields/ports/parameters with defaults, fine. Renames and semantic changes, never — make a new type ID.
- **Custom composites in `graphs/custom_composites/<id>.json`** (V2 only).

Graph file structure (sketch):

```json
{
  "schema_version": 1,
  "id": "composite.user.<uuid>",
  "content_hash": "<sha256>",
  "name": "MyBloom",
  "nodes": [
    { "id": "n1", "type": "primitive.threshold", "params": { "level": 0.8 }, "exposed": ["level"], "editor_pos": [120, 200] }
  ],
  "wires": [
    { "from": ["n1", "out"], "to": ["n2", "in"] }
  ],
  "exposed_parameters": [
    { "label": "Threshold", "node": "n1", "param": "level" }
  ]
}
```

---

## 14. V1 Scope (Concrete)

### Port types (3)
Texture2D, Texture3D, Scalar. Buffer ports defer to V2.

### Boundary nodes (2)
Source, FinalOutput. Always present in composites, can't be deleted.

**Constraint:** V1 composites have at most one Source (Texture2D) and exactly one FinalOutput (Texture2D).

### Primitives (10)
UVTransform, Threshold, Blur, MipChain, Mix, Blend, Luminance, GradientMap, Sample, ColorMatrix.

### Atomic effects/generators (3)
Plasma, FluidSim 2D (with `density` and `velocity` Texture2D outputs), Glitch.

### Composite presets (5)
Bloom, Halation, Infrared, Mirror (alias), SoftFocus.

### Wrapped legacy nodes (~35)
Every other existing effect/generator wrapped as an atomic Node. Same Rust + Metal kernel, new trait.

### Runtime
Graph data model, topological sort, execution plan, texture lifetime planner, per-instance state per stateful node. **No fusion.** One dispatch per node.

### Compilation
WGSL emission for composites runs on background thread. naga + Metal compile. Atomic Arc swap. Parameter tweaks bypass compilation.

### Editing
Parameter changes only during playback. Topology edits require pause.

### Editor
**None in V1.** Validation through:
- Code-built graphs (Rust constructs a graph in tests, runs frames, snapshot diffs against expected output).
- One hardcoded debug graph for visual sanity check.

### Library
Project-scoped only. App-scoped library and bundling defer to V2.

### Estimated effort
4-6 weeks of focused work for runtime + new node implementations + adapter pass for the ~35 legacy nodes. Editor work begins separately after V1 lands.

---

## 15. Phased Roadmap

| Phase | Adds | Why |
|---|---|---|
| **V1** | Runtime, ~20 new nodes, ~35 wrapped legacy, no editor, no fusion | Validate the abstraction before committing to UI |
| **V2** | Editor UI, app-scoped library, project bundling, live topology edits, more composites | Ship the actual product |
| **V3** | Pixel-local fusion compiler, Buffer ports, particle/mesh integration, more primitives | Performance and expressiveness |
| **V4** | UV-rewriting fusion, sub-graphs of sub-graphs, plugin-style sharing | Polish the platform |

---

## 16. Decisions Log

Track key architectural decisions and the reasoning behind them. Append-only.

| Date | Decision | Rationale |
|---|---|---|
| 2026-04-30 | Use coarse-grained "TouchDesigner TOP-level" granularity, not fine-grained shader-math nodes | Single-node atomic effects are first-class. Decomposition opt-in. |
| 2026-04-30 | Mix and match between effect and generator nodes is unrestricted | Generator/effect distinction collapses to graph port shape. |
| 2026-04-30 | FluidSim and similar simulations stay atomic with rich port surface | Simulation kernels can't decompose to primitives without losing what they are. Rich ports expose internals. |
| 2026-04-30 | Old effects ship as composite presets where decomposable, atomic Nodes otherwise | Reference implementations for users; no rewrite of working code. |
| 2026-04-30 | Thin effects (Mirror, Transform) stay as themselves via alias presets | Discoverability beats theoretical elegance. |
| 2026-04-30 | Graph runtime is clip-agnostic; state lives per graph instance | Decouples graph from host. State key is `node_instance_id`. |
| 2026-04-30 | Parameter system requires zero external changes | Graph adapts to existing effect-card interface internally. |
| 2026-04-30 | Per-parameter expose checkbox is graph-internal only | Modulation/MIDI/OSC unchanged. |
| 2026-04-30 | Custom graph compiler + naga_oil + naga; no off-the-shelf solution | None exists. Custom layer is bounded (~1000 lines). |
| 2026-04-30 | Phase 0 ships with no fusion | Validate abstraction first. Fusion is Phase 1+. |
| 2026-04-30 | App-scoped library + project bundle, bundle wins on collision | Predictability + portability. Like font embedding in PDFs. |
| 2026-04-30 | UUID + content hash for composite identity | Stable concept ID + edit-tracking snapshot ID. |
| 2026-04-30 | Live topology editing during playback deferred to V2 | Sidesteps state-continuity questions for V1. |
| 2026-04-30 | V1 has no editor; validate through code | Editor is months of work; abstraction must be right first. |
| 2026-04-30 | Source and FinalOutput boundary nodes mark graph edges | Self-documenting, supports future multi-port composites. |
| 2026-04-30 | Naming: `EffectNode`, `NodeInput`/`NodeOutput`/`NodePort`, `NodeWire`, `Source`, `FinalOutput` | Domain-prefixed names disambiguate from UI tree nodes / network ports. Boundary nodes (Source, FinalOutput) stay un-prefixed since they're standalone graph concepts, not port mechanics. |
| 2026-04-30 | Step 1 lands as a module inside `manifold-renderer` (not a new crate) | Smallest commitment; split into `manifold-graph` later if it earns it. |

---

## 17. Open Questions

Things that need answering during implementation. Append as discovered, resolve inline with date and answer.

- *(none yet — open as work begins)*

---

## 18. Progress Tracking

### V1 Milestones

| Milestone | Status | Notes |
|---|---|---|
| `EffectNode` trait designed and reviewed | Done (2026-04-30) | Core abstraction in `crates/manifold-renderer/src/node_graph/`. |
| Port type system (Texture2D, Texture3D, Scalar) | Done (2026-04-30) | `node_graph/ports.rs`. |
| Graph data model (`Graph`, `NodeWire`, `NodeInstance`) | Done (2026-04-30) | `node_graph/graph.rs`. Connection legality enforced at `connect` time. |
| Topological sort + cycle detection | Done (2026-04-30) | `node_graph/validation.rs`. DAG-only for V1; explicit feedback edges deferred. |
| Execution plan compiler (no fusion) | Not started | |
| Texture lifetime planner | Not started | Last-use analysis for pool reuse. |
| Background compile thread + Arc swap | Not started | |
| Source/FinalOutput boundary nodes | Not started | |
| 10 V1 primitives | Not started | UVTransform, Threshold, Blur, MipChain, Mix, Blend, Luminance, GradientMap, Sample, ColorMatrix. |
| 3 V1 atomic nodes | Not started | Plasma, FluidSim 2D (with rich ports), Glitch. |
| 5 V1 composite presets | Not started | Bloom, Halation, Infrared, Mirror (alias), SoftFocus. |
| Wrapped legacy nodes (~35) | Not started | One Node trait wrapper per existing effect/generator. |
| Project save/load (graphs in V2 ZIP) | Not started | Schema v1, additive design. |
| Code-driven validation harness | Not started | Build graphs in tests, snapshot diff. |
| Hardcoded debug graph (visual sanity) | Not started | |

### V2+ (parking lot)

- Editor UI (canvas, palette, parameter inspector, port hit-testing)
- Live topology editing with state-continuity rules
- App-scoped library
- Project bundling + collision UI
- Phase 1 fusion compiler
- Buffer ports (particle systems, mesh, audio)
- More composite presets (ColorGrade, StylizedFeedback)
- Curves and LiftGammaGain primitives

---

## 19. References

- `crates/manifold-renderer/src/effect.rs` — current `PostProcessEffect` trait
- `crates/manifold-renderer/src/generator.rs` — current `Generator` trait
- `crates/manifold-renderer/src/effect_chain.rs` — current linear-chain runtime
- `crates/manifold-renderer/src/render_target_pool.rs` — current texture pool (will need extension for graph lifetime planner)
- `crates/manifold-gpu/src/metal/shader_compiler.rs` — naga toolchain entry point
- `docs/MANIFOLD_GPU_ARCHITECTURE.md` — overall GPU architecture
- `docs/ADDING_EFFECTS_AND_GENERATORS.md` — current effect/generator authoring guide (to be updated for nodes)
