# Effect Runtime Unification

**Status:** Closed. Phases 1–4 shipped through the May 2026 migration sweep. The legacy `EffectChain` runtime was deleted; the graph runtime is the sole dispatcher. The bindings unification (separately tracked in [BINDINGS_UNIFICATION_PLAN.md](BINDINGS_UNIFICATION_PLAN.md)) and the JSON-authoritative preset migration both ride on top of the unified runtime documented here. The §0 "true goal" framing remains the canonical north star — the primitive library *is* the product.

> **Supersession note (2026-07-22, UI_FUNNEL P-Z):** references below to `dispatch_inspector` / `ActiveInspectorDrag` / `PanelAction` trio variants describe the PRE-decomposition architecture. Current state: 12 flat domain enums + exhaustive router (P-D), one Scrub gesture wire with `ScrubState.active` (P-I, `ActiveInspectorDrag` extinct), per-domain `dispatch/` handlers (P-B). Anchors here are historical.

**Last updated:** 2026-05-17 (Phase 1–3 completion) / 2026-05-19 (status note)

**Companion docs:** [`NODE_GRAPH_SYSTEM.md`](NODE_GRAPH_SYSTEM.md) — overall node-graph architecture. [`MANIFOLD_GPU_ARCHITECTURE.md`](MANIFOLD_GPU_ARCHITECTURE.md) — manifold-gpu crate. [`PRIMITIVE_LIBRARY_DESIGN.md`](PRIMITIVE_LIBRARY_DESIGN.md) — Phase 4a primitive catalog, decomposition recipes, parity test spec.

---

## 0. True goal of this work

The graph node system is a **TouchDesigner-style creative surface** for users *and* AI agents to compose visuals from atomic primitive nodes. MANIFOLD's product goal is intuitive live performance + creative composition without TouchDesigner's "mathy / know-how" barrier. Users wire small composable pieces. AI agents read primitive docs and generate graphs via scripts.

**The primitive library is the product**, not the runtime mechanics. A graph editor that wires monolithic blackboxes (`Bloom-blackbox → ColorGrade-blackbox → Glitch-blackbox`) is not meaningfully more expressive than today's effect chain. The creative leverage comes from the size and orthogonality of the primitive library.

This reframes every downstream decision in this document:

- **Runtime unification is in service of the creative surface, not an end in itself.** Deleting `EffectChain` matters because it removes a parallel runtime that can't host primitive composition. It is not the prize.
- **Phase 4 is decompose-first**, not wrap-first. Existing effects become *preset graphs* over a primitive library. "Add Bloom" loads a saved graph (`Threshold → MipChain → Gaussian → Mix`); the user can fork it, swap the Gaussian for a box blur, or route the threshold output elsewhere. AI agents emit the same preset shape.
- **Some effects don't decompose** — BlobTracking (FFI worker + One-Euro filter), WireframeDepth (3 DNN workers, 15 passes), AutoGain (resolution-independent envelope state), DepthEstimate (MiDaS), FlowEstimate. These stay as monolithic custom `EffectNode`s in the library, the way TouchDesigner ships monolithic DNN TOPs. Rough split: ~12–15 of 19 effects decompose; ~5 remain monolithic.
- **Every primitive needs**: clear semantic purpose, typed ports, named parameters, docstrings, example presets. Not just GPU correctness — the metadata is what makes AI composition possible.

Phase 4 as originally written (wrap each remaining effect in a `LegacyPostProcessNode`, then delete `EffectChain`) would ship faster but produces a graph runtime that runs the same effects. It does not unlock the creative surface. §9 now reflects the decompose-first rewrite.

---

## 1. Overview

MANIFOLD currently has **two parallel runtimes** for effect work:

1. **Linear chain runtime** ([`EffectChain::apply_chain`](../crates/manifold-renderer/src/effect_chain.rs)) — hand-rolled imperative loop with ping-pong buffers, group wet/dry blending, `should_skip` checks. Used by every layer / master / clip effect chain today.
2. **Graph runtime** ([`Executor`](../crates/manifold-renderer/src/node_graph/execution.rs) + [`MetalBackend`](../crates/manifold-renderer/src/node_graph/metal_backend.rs)) — topological execution of an `ExecutionPlan` over a `Backend` trait. Used by graph-backed effects (`Mirror`, `SoftFocus`) which run a sub-graph *inside* their `apply()` call, nested within the chain runtime.

The chain runtime cannot host:

- Cross-effect dispatch fusion (the `NODE_GRAPH_SYSTEM.md` §5.1 promise — adjacent pixel-local primitives compile to one compute shader).
- Static elision (disabled effects, identity transforms, zero-amount nodes drop out at compile time).
- Branching topology (the wet/dry "snapshot + lerp" dance is imperative).
- Independent branches on parallel queues.
- Any future cross-effect analysis (resource lifetime overlap, format coercion, render-target reuse).

This document specifies the unification: **collapse both runtimes into the graph runtime, with the chain becoming a degenerate linear graph compiled from the effect list.**

---

## 2. Goals & non-goals

### Goals

**Primary (creative surface, per §0):**
- An orthogonal primitive library users and AI agents compose into custom visuals. Primitives small, semantically named, typed ports, docstrings, example presets.
- Existing effects (Bloom/Halation/etc.) become preset graphs over primitives. "Add Bloom" loads a graph; users fork and remix.
- Preset graphs serialize as a stable, AI-readable format (the same project-file graph format).

**Secondary (runtime mechanics that enable the above):**
- One runtime path for all effect work. Delete `EffectChain` — its existence prevents primitive-level composition.
- Vulkan-portable IR. The runtime touches manifold-gpu only; no Metal types in graph code.
- Static elision and dynamic bypass as first-class compiler/runtime features (no per-frame memcpy hacks).
- Wet/dry effect groups expressed as sub-graphs with `Mix` tails (no imperative snapshot dance).
- Per-owner state (Bloom mip chain, Feedback prev-frame, etc.) lifted out of `PostProcessEffect` impls into a runtime-owned `StateStore`. Effects become pure behavior + port shape + parameter spec.
- Performance equal to or better than current chain at production scale (53 layers / 2928 clips / 128 effects).

### Non-goals (deferred)

- Dispatch fusion compiler. The IR must *enable* it (carry enough metadata) but we don't implement it as part of this arc.
- Multi-queue / async compute scheduling. Single queue stays.
- User-editable effect-level graphs. The chain remains a linear list at the editor level; graph-backed effects continue to be opened via the cog icon.
- Multi-Source / multi-FinalOutput composites. Stays single-Source/single-FinalOutput as in `NODE_GRAPH_SYSTEM.md` V1.

---

## 3. Cross-platform constraint

**Vulkan portability is a real future feature.** Every design decision in this document must keep the runtime compatible with a future Vulkan backend in manifold-gpu. The runtime IR itself is platform-agnostic by construction — it's pure data. The execution side touches GPU through manifold-gpu only. The constraint is:

1. **No Metal types in `node_graph/`**, ever. All GPU access through `GpuDevice`, `GpuEncoder`, `GpuBinding`, `GpuTexture`, etc.
2. **No Metal-specific semantics assumed.** Particularly: no implicit ordering between dispatches without an explicit barrier. Metal serializes within a queue automatically; Vulkan does not.
3. **No assumed access to argument buffers, heap aliasing, tile memory, or other Apple-specific features** as runtime promises. Backend may use them as optimizations transparently.

### 3.1 manifold-gpu audit findings

The crate is **~85% abstraction-ready**. Findings from the public-API audit:

| Category | Status | Risk | Action required |
|----------|--------|------|-----------------|
| Resource creation (texture / buffer / sampler / pipeline) | Clean | None | Direct Vulkan swap |
| Compute dispatch (`dispatch_compute`, `GpuBinding`) | Clean | None | Direct Vulkan swap |
| Specialization (`create_specialized_compute_pipeline`) | Clean (text-substitution) | None | Vulkan can use SPIR-V spec constants or same text approach |
| Shader compilation (WGSL → naga → SPIR-V → MSL) | Clean | None | Vulkan consumes SPIR-V directly |
| Texture pool | Clean | None | Direct Vulkan swap |
| **Synchronization (pipeline barriers)** | **Missing** | **High** | **Add explicit barrier API. Metal: no-op. Vulkan: `vkCmdPipelineBarrier2`.** |
| Queues (`raw_queue`, `clone_queue`) | Metal-exposed | Medium | Hide behind opaque handle or feature gate |
| Device escape hatch (`raw_device`) | Metal-exposed | Medium | Same |
| Command-buffer escape hatch (`raw_cmd_buf`) | Metal-exposed (intentional, for MPS/MetalFX interop) | Low | Document as backend-specific; gate Metal-only callers |
| `mps`, `metalfx`, `fft` modules | Metal-only (MPS / MetalFX / MPSGraph) | Low | `#[cfg]`-gate behind `metal` feature; provide Vulkan equivalents later |

**`GpuEncoder` is well-encapsulated.** All internals (`cmd_buf`, `state`, caches) are `pub(crate)` — earlier concern about a public `native_enc` field was wrong. The single intentional leak is `raw_cmd_buf()` for MPS/MetalFX interop, used outside `node_graph/`.

### 3.2 Required manifold-gpu changes before Phase 1

The graph runtime cannot compile to a Vulkan-compatible execution model without explicit synchronization. The single blocker:

```rust
// Add to GpuEncoder. Metal impl: no-op. Vulkan impl: vkCmdPipelineBarrier2.
pub fn pipeline_barrier(
    &mut self,
    reads: &[&GpuTexture],   // resources next dispatch reads
    writes: &[&GpuTexture],  // resources next dispatch writes
);
```

The `Executor` calls this between dependent steps once it knows the read/write set per step (it already does — `ExecutionStep::inputs` and `ExecutionStep::outputs`). On Metal the call is a no-op; on Vulkan it emits real barriers. Resource lifetime analysis already in `compile()` gives us all the information needed.

Other manifold-gpu changes are not blockers but should be done before Phase 1 lands so the boundary is clean:

- Gate `mps`, `metalfx`, `fft` modules behind a `metal` feature flag.
- Audit callers of `raw_device()` / `raw_queue()` / `clone_queue()` / `raw_cmd_buf()`. Each call site is either (a) Metal-only by nature (e.g., MPSGraph FFT) and should live behind the same feature flag, or (b) should migrate to abstract APIs.

These are precondition commits to manifold-gpu, not part of the unification arc per se.

---

## 4. Current state assessment

### 4.1 What we have today

The graph runtime infrastructure is **substantially built**:

- [`Graph`](../crates/manifold-renderer/src/node_graph/graph.rs) — node + wire container, ~300 LOC.
- [`compile`](../crates/manifold-renderer/src/node_graph/execution_plan.rs) — validate + topological sort + resource lifetime analysis. Pure data, no GPU. Already cacheable. ~400 LOC.
- [`Executor`](../crates/manifold-renderer/src/node_graph/execution.rs) — per-frame iteration over plan steps with slot acquire/release. Already uses pre-allocated scratch buffers (CLAUDE.md compliant). ~440 LOC.
- [`Backend` trait](../crates/manifold-renderer/src/node_graph/backend.rs) — platform-agnostic abstraction over slot allocation. `MetalBackend` is one impl; a `VulkanBackend` would slot in cleanly.
- [`MetalBackend`](../crates/manifold-renderer/src/node_graph/metal_backend.rs) — slot recycling identical to chain's ping-pong. Pre-binding via `pre_bind_texture_2d` solves first-effect-input optimization. ~285 LOC.
- [`LegacyPostProcessNode`](../crates/manifold-renderer/src/node_graph/legacy_adapter.rs) — wraps any `Box<dyn PostProcessEffect>` as an `EffectNode`. 1-input/1-output. Synthesizes `ParamDef` list from `EffectMetadata`. ~335 LOC.
- 9 primitives: `Source`, `FinalOutput`, `Mix`, `UVTransform`, `Blur`, `Threshold` (stub), `MipChain` (stub), `Sample` (stub), `Blend` (stub). ~600 LOC across `primitives/`.
- 5 composite presets: `Mirror`, `Bloom`, `Halation`, `Infrared`, `SoftFocus`. ~400 LOC across `composites/`.
- 2 production graph-backed effects: `MirrorFX`, `SoftFocusGraphFX`. ~500 LOC.
- `GraphSnapshot` for editor canvas; legacy adapter snapshot fallback. ~600 LOC.

**What's missing for unification:**

1. **State store** — per-owner state still lives inside `PostProcessEffect` impls (`AHashMap<owner_key, ...>` fields).
2. **Edit-driven plan caching** — currently plan is built once per graph instance. For chains, "the chain *is* the graph" so we'd need to rebuild the plan when the user edits the effect list.
3. **Static elision pass** — no compiler pass exists; disabled effects can't drop out at compile time.
4. **Dynamic bypass instruction** — no runtime mechanism for per-frame skip.
5. **Wet/dry as sub-graphs** — currently imperative in `EffectChain`.
6. **Pipeline barriers** — manifold-gpu lacks the API.

### 4.2 Performance baseline (analytical)

The `Executor`'s per-step cost is ~5-10 `AHashMap` lookups (~50-100ns each) plus the `evaluate()` body. For a 10-effect chain: ~2.5-10μs of executor overhead per frame. Negligible compared to GPU dispatch costs (typically tens to hundreds of μs per effect).

Compared to `EffectChain::apply_chain`'s per-effect overhead (1 `registry.get_mut` + 1 `should_skip` + `apply` + possible group lookup): same order of magnitude. **Executor overhead is not a perf concern.** The real per-frame allocation question is plan rebuild frequency — addressed in §6.2.

For typical project scale (53 layers, 128 effects across chains): plans compile on edit only, total plan storage ~tens of KB. Plan compilation itself is O(V+E) topo sort + O(V) resource analysis — at chain scale, ~10μs per compile. User-imperceptible.

---

## 5. Architecture

### 5.1 Five principled positions

These decisions are non-negotiable for the unified runtime:

1. **The graph IR is the single representation of effect work.** A chain is a degenerate linear graph. Layer effects, master effects, composites — all graphs. There is no `EffectChain`. There is an `ExecutionPlan` derived from a `Graph`.

2. **Plan compilation is edit-driven, not per-frame.** Mutating the effect list (add/remove/reorder/regroup/enable-toggle) invalidates the plan. Param value changes do not. Standard incremental-compiler design.

3. **Resources (textures, persistent state) belong to the runtime, not the nodes.** Nodes are pure *behavior + port shape + parameter spec*. State is identified by `(NodeInstanceId, owner_key)` in a runtime-owned `StateStore`. Nodes hold no `AHashMap<owner_key, ...>` fields. (Frame-graph pattern: Frostbite, Unreal RDG.)

4. **Skip is two distinct features:**
   - **Static elision** (compiler pass) — disabled effects, identity transforms, zero-amount effects drop from the plan entirely. Wires reroute. Runs on graph mutations, alongside topo sort and resource analysis.
   - **Dynamic bypass** (runtime instruction) — per-frame skip predicate aliases the node's output slot to its input slot. Zero work, zero copy. No memcpy hack.

5. **Wet/dry groups are sub-graphs with `Mix` tails.** Dry path forks before the group; wet path goes through; `Mix` reconciles at the end. The compiler collapses degenerate cases (`wet_dry == 1.0` elides dry branch + Mix; `wet_dry == 0.0` elides wet branch). Falls out of #4 for free. The imperative snapshot-and-lerp dance disappears.

### 5.2 IR structure

```
Graph (mutable; user-editable structure)
  ├── nodes: AHashMap<NodeInstanceId, NodeInstance>
  ├── wires: Vec<NodeWire>
  └── topology_version: u64       // bumped on any mutation

ExecutionPlan (immutable; derived from Graph)
  ├── steps: Vec<ExecutionStep>   // topologically ordered
  ├── resource_types: Vec<PortType>
  ├── built_for_topology: u64     // matches Graph.topology_version
  └── metadata: PlanMetadata
        ├── elided_nodes: Vec<NodeInstanceId>     // for editor display
        └── bypass_predicates: AHashMap<NodeInstanceId, BypassPredicate>

ExecutionStep
  ├── node: NodeInstanceId
  ├── inputs: Vec<(port_name, ResourceId)>
  ├── outputs: Vec<(port_name, ResourceId)>
  ├── free_after: Vec<ResourceId>
  ├── bypass_predicate: Option<BypassPredicateId>  // dynamic skip
  └── barrier_after: BarrierSet                    // Vulkan barrier metadata
```

### 5.3 StateStore

Owns all per-owner GPU state for stateful nodes. Keyed by `(NodeInstanceId, owner_key)`. Replaces every `AHashMap<owner_key, FooState>` field currently inside effect impls.

```rust
pub struct StateStore {
    /// Type-erased state buckets, one per (node_id, owner_key) pair.
    /// State is allocated on demand; freed on owner cleanup.
    states: AHashMap<(NodeInstanceId, OwnerKey), Box<dyn NodeState>>,
}

pub trait NodeState: Send {
    fn cleanup(&mut self, gpu: &GpuDevice);
}
```

Effects access state via a typed handle:

```rust
fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
    let state = ctx.state.get_or_init::<BloomState>(|gpu| {
        BloomState::new(gpu, ctx.width, ctx.height)
    });
    state.run(/* ... */);
}
```

`ctx.state` is a borrow scoped to the current node + current owner. Nodes don't see other nodes' state.

**Rationale** (see §7 stateful inventory): all 8 stateful effects key their state by `owner_key: i64` only. State lifetimes are uniform — lazy or eager allocation, cleanup on owner destruction, optional resize-triggered rebuild. A single `StateStore` API serves all of them.

**Background workers** (BlobTracking, DOF, WireframeDepth use FFI thread pools) stay with their effects. The `StateStore` owns GPU textures and CPU buffers. Workers are stateful singletons that manage their own thread lifetimes; moving them into `StateStore` would conflate two concerns.

### 5.4 Static elision pass

Runs as part of `compile()`. Sequence:

```
validate(graph)
  → topological_sort(graph)
  → static_elision_pass(order)         ← NEW
  → assign_resource_ids(order)
  → lifetime_analysis(order)
  → barrier_analysis(order)            ← NEW (for Vulkan)
  → ExecutionPlan
```

Elision drops nodes from the plan. For each candidate node:

- **Disabled effect** (host marks `Effect.enabled == false`) → drop, reroute consumer's input to producer's output.
- **Identity transform** (e.g., `UVTransform[mode=Identity, translate=0, scale=1, rot=0]` after constant folding) → drop, reroute.
- **Zero-amount Mix** (`amount == 0` → output equals input `a`) — only if amount is statically known, not bound to a modulator.

Each candidate is identified by an opt-in trait method:

```rust
pub trait EffectNode: Send {
    // ... existing methods ...

    /// Compile-time check: given current parameter values, can this node
    /// be elided to a passthrough? Returns the input port whose value
    /// should be wired through. Default: never.
    fn static_passthrough(&self, params: &ParamValues) -> Option<&'static str> {
        None
    }
}
```

Elision is conservative — only nodes that opt in are candidates. Side effects (Feedback, BlobTracking) opt out by leaving the default implementation.

**Wet/dry interaction:** when wet/dry==0 the wet branch's nodes can be elided wholesale. When wet/dry==1 the dry branch (a single `Source` fan-out wire) and the `Mix` tail elide.

### 5.5 Dynamic bypass instruction

For per-frame skip that depends on values that may change every frame (modulated `should_skip`, etc.):

```rust
pub struct ExecutionStep {
    // ...
    pub bypass_predicate: Option<BypassPredicateId>,
}

// In Executor::execute_frame_inner:
for step in plan.steps() {
    if let Some(pred_id) = step.bypass_predicate {
        let pred = &plan.bypass_predicates[pred_id];
        if pred.evaluate(&ctx.params, &ctx.frame_time) {
            // Bypass: alias output slot(s) to corresponding input slot(s).
            for (out_port, in_port) in pred.passthrough_mapping() {
                let in_slot = backend.slot_for(/* ... */).unwrap();
                backend.alias_resource(/* output */, in_slot);
            }
            continue;
        }
    }
    // Normal evaluate path.
}
```

`Backend::alias_resource(output_id, slot)` makes the output `ResourceId` resolve to the same physical `Slot` as the input. Zero GPU work. The downstream consumer reads from the input's storage transparently.

For 1-in-1-out adapters this is trivial. For multi-in nodes (rare on the chain side), we either declare which input is the bypass passthrough (`bypass_passthrough_input` static), or refuse dynamic bypass for that node type.

**Predicate evaluation** is pure CPU code — runs in microseconds. Predicates compose: a `LegacyPostProcessNode`'s predicate is the inner effect's `should_skip(fx)`.

### 5.6 Wet/dry as sub-graphs

Today: imperative `apply_wet_dry_lerp` in `EffectChain`.

Tomorrow: at plan compilation time, a chain with effect groups expands to:

```
input ─┬─────────────────────────────────────────────────────▶ Mix.a (dry)
       │
       └─▶ effect[i] ─▶ effect[i+1] ─▶ ... ─▶ effect[i+n] ─▶ Mix.b (wet)
                                                              ▼
                                                       Mix.amount = wet_dry
                                                              ▼
                                                            out
```

The compiler emits the `Mix` node + the dry-branch fan-out wire for each group. `wet_dry == 1.0` → static elision drops the dry branch and Mix entirely (single-input Mix is the wet branch). `wet_dry == 0.0` → drops the wet branch.

If `wet_dry` is modulated (driven by an LFO etc.), the elision can't apply at compile time, so the Mix runs every frame. That's still cheaper than a snapshot+lerp pass (one dispatch instead of a copy + a lerp dispatch).

### 5.7 Pipeline barriers (Vulkan precondition)

The plan compiler's `barrier_analysis` pass walks steps in order. For each step, it computes:

- The set of resources read.
- The set of resources written.
- The set of "barrier transitions" — resources that the previous step *wrote* and this step *reads* (read-after-write hazard).

These are stored on the step as a `BarrierSet`. The `Executor` calls `gpu.pipeline_barrier(reads, writes)` between steps with non-empty `BarrierSet`.

On Metal the call is a no-op. On Vulkan it emits the actual barrier. The IR is identical; the implementation differs.

**Important:** barriers are computed at compile time, not runtime. The cost is one method call per step transition with non-trivial barrier set, which the Metal backend implements as a tail-called no-op.

---

## 6. Runtime mechanics

### 6.1 The chain becomes a graph

For each effect chain (master / layer / clip), the host maintains a `Graph` containing:

- One `Source` node, pre-bound to the chain's input texture.
- One `LegacyPostProcessNode` per legacy effect in the chain (for migrated effects, the actual graph-backed node).
- One `Mix` node + a dry-fan-out wire per effect group (for wet/dry).
- One `FinalOutput` node, pre-bound to the chain's output texture.

When the user adds, removes, reorders, regroups, or enables/disables an effect, the graph is mutated and `topology_version` bumps. The plan is recompiled on the next frame's edge before execution.

Param value changes (slider drags, modulator updates) write directly to the graph's `ParamValues` map. They do not invalidate the plan.

### 6.2 Plan caching

```rust
pub struct CachedPlan {
    plan: ExecutionPlan,
    built_for_topology: u64,
}

impl Graph {
    pub fn ensure_plan(&self, cache: &mut Option<CachedPlan>) -> &ExecutionPlan {
        let needs_rebuild = cache
            .as_ref()
            .is_none_or(|c| c.built_for_topology != self.topology_version);
        if needs_rebuild {
            *cache = Some(CachedPlan {
                plan: compile(self).expect("graph valid"),
                built_for_topology: self.topology_version,
            });
        }
        &cache.as_ref().unwrap().plan
    }
}
```

Plans live alongside the graphs that produced them. The host owns both.

### 6.3 Pre-binding chain inputs/outputs

Identical to how `MirrorFX` / `SoftFocusGraphFX` work today. The host:

1. After plan compile, looks up the `ResourceId` of `Source.out` and `FinalOutput.in`.
2. Each frame, calls `backend.pre_bind_texture_2d(source_resource, chain_input_target)` and analogously for the output.
3. Calls `executor.execute_frame_with_gpu(graph, plan, frame_time, gpu)`.

This **already eliminates the first-effect-blit cost** — the executor's first node reads the chain's input texture directly.

### 6.4 Stateful node instantiation

Each stateful node's `EffectNode::new()` returns a stateless adapter. State is lazy-allocated via `StateStore::get_or_init` on first `evaluate()`. On owner cleanup, the host calls `state_store.cleanup_owner(owner_key)` which drops every `(*, owner_key)` entry.

### 6.5 Skipped-effect cost

- **Static elision** → zero cost. Node is not in the plan.
- **Dynamic bypass** → cost of one slot-alias call (~AHashMap insert, <100ns) + zero GPU work. **Strictly cheaper than today's `should_skip` short-circuit** (which still calls `should_skip` and then doesn't dispatch).

### 6.6 Per-frame allocation invariants

Same as today's CLAUDE.md rules: zero allocations on the hot path (`evaluate`, `execute_frame_inner`). The Executor's scratch vecs already follow this pattern. The `LegacyPostProcessNode::evaluate` currently allocates a `Vec<f32>` for `param_values` — this gets replaced by a reusable `SmallVec` or pre-allocated buffer keyed off `metadata.params.len()`.

---

## 7. Parameter binding & addressing

The runtime IR and StateStore handle the *flow of data* through the graph. This section addresses how *user-facing parameters* identify themselves, route through the graph, and survive every change short of removal.

### 7.1 The problem

Today, every parameter is identified by **position in `EffectInstance::param_values: Vec<f32>`**. That index is referenced from at least nine subsystems:

| Subsystem | Storage | Address shape |
|---|---|---|
| `EffectInstance.param_values` | Project file | `Vec<f32>` indexed by position |
| `ParameterDriver.param_index: i32` | Project file | Position |
| `ParamEnvelope.param_index: i32` | Project file | Position |
| `AbletonParamMapping.param_index: usize` | Project file | Position |
| `MacroMappingTarget::*::param_index: usize` | Project file | Position |
| `PanelAction::EffectParamChanged(fx_idx, param_idx, val)` | Live UI | Position |
| `ChangeEffectParamCommand.param_index: usize` | Live undo stack | Position |
| OSC routing | Derived at load | `osc_prefix` + `osc_suffix` (string, but indexed lookup to compute) |
| `align_to_definition()` | Post-load fixup | Hard-coded per-effect index remap (WireframeDepth 14→12) |

Position is *fragile*: reordering, inserting, or removing a parameter silently corrupts every persisted reference. The existing WireframeDepth-specific remap in `align_to_definition` exists *because of* this fragility. Scaling that pattern to ~20 effects × ongoing development is impractical.

We replace position with **stable string `ParamId`s** at every external addressing site. Internal storage stays positional (it's hot-path code); the conversion happens at the registry boundary.

### 7.2 ParamBinding framework

The user-facing surface of every effect becomes a slice of `ParamBinding`:

```rust
pub struct ParamBinding {
    /// Stable identity. Forever rule: never rename, never reuse.
    pub id: Cow<'static, str>,
    /// UI metadata — slider label, range, default, format, enum labels.
    /// `spec.name` is the editable display string; `id` is the stable key.
    pub spec: ParamSpec,
    /// Where this parameter's value flows in the graph.
    pub target: ParamTarget,
    /// Conversion from f32 (UI/storage) to the typed `ParamValue` the
    /// graph node receives.
    pub convert: ParamConvert,
}

pub enum ParamTarget {
    /// Routed through a `CompositeHandle`'s exposed-param map.
    Composite { outer_name: Cow<'static, str> },
    /// Direct to a node + parameter name. Used by single-primitive effects.
    Node { node: NodeInstanceId, param: Cow<'static, str> },
    /// Escape hatch — caller-supplied closure runs each frame.
    /// For legacy adapters whose routing isn't expressible above.
    Custom(fn(&mut Graph, f32)),
}

pub enum ParamConvert {
    Float,                              // identity
    IntRound,                           // f.round() as i32 → ParamValue::Int
    BoolThreshold,                      // f > 0.5 → ParamValue::Bool
    EnumRound,                          // f.round() as u32 → ParamValue::Enum
    EnumRemap(Cow<'static, [u32]>),     // legacy idx → graph enum (Mirror's case)
    Vec2 { x_idx: u8, y_idx: u8 },      // bundle two host params into one Vec2
    Color,                              // bundle 4 host params into one RGBA
    // ... grow as needed
}
```

`Cow<'static, str>` everywhere — `Borrowed` for compile-time IDs (V1 / developer-defined effects), `Owned` for runtime IDs (V2 / user-exposed params). Same trick as `EffectTypeId`. One type, both lifetimes.

The `&[ParamBinding]` slice attaches to `EffectMetadata` (for type-level developer params) and to `EffectInstance` (for instance-level user-exposed params). The `apply` shim iterates the *concatenation* of both:

```rust
fn apply_param_bindings(
    bindings: &[ParamBinding],
    user_bindings: &[ParamBinding],
    graph: &mut Graph,
    handle: &CompositeHandle,
    values: &[f32],
) {
    for (binding, &value) in bindings.iter().chain(user_bindings).zip(values.iter()) {
        let pv = binding.convert.f32_to_param_value(value);
        match &binding.target {
            ParamTarget::Composite { outer_name } => {
                handle.set_param(graph, outer_name, pv).ok();
            }
            ParamTarget::Node { node, param } => {
                graph.set_param(*node, param, pv).ok();
            }
            ParamTarget::Custom(f) => f(graph, value),
        }
    }
}
```

Every migrated effect's `apply()` becomes ~10 lines (param routing + executor invocation). No more hand-rolled `fx.param_values.first().copied().unwrap_or(...)` boilerplate, no more silent index-shift bugs.

### 7.3 ParamId migration across the addressing sites

Each subsystem migrates from `param_index` → `param_id: ParamId`:

```rust
// Before:
pub struct ParameterDriver {
    pub param_index: i32,
    // ...
}

// After:
pub struct ParameterDriver {
    pub param_id: ParamId,  // Cow<'static, str>
    // ...
}
```

Same pattern for `ParamEnvelope`, `AbletonParamMapping`, `MacroMappingTarget`, `ChangeEffectParamCommand`, `PanelAction::Effect*`. The runtime keeps internal storage as `Vec<f32>` indexed by position (hot path stays fast); the registry provides `ParamId → index` lookup at addressing boundaries.

```rust
impl EffectInstance {
    pub fn get_base_param_by_id(&self, id: &str) -> Option<f32> {
        let idx = registry::param_id_to_index(&self.effect_type, id)?;
        self.base_param_values.as_ref()?.get(idx).copied()
    }
}
```

`param_id_to_index` lookups happen on driver evaluation, Ableton mapping update, OSC dispatch — all per-frame but with one `&str → usize` map lookup each (AHashMap; ~50ns). Negligible at chain scale.

### 7.4 OSC routing locks to ParamId

OSC paths are currently derived as `/<scope>/<osc_prefix><osc_suffix>` (e.g., `/master/bloomAmount`). The `osc_suffix` is per-`ParamSpec` and stable. The `param_index` argument to `osc_param_router::PendingWrite` becomes `param_id`. Saved OSC mappings stay external (no project-file change for OSC); the router's internal lookup migrates from `Vec<f32>` indexing to `param_id_to_index` translation.

OSC is the one subsystem whose user-visible surface (the OSC paths themselves) doesn't change. External clients keep working unchanged.

### 7.5 Project file migration

The current project file format serializes:

```json
{
  "effectType": 29,
  "paramValues": [1.0, 144.0, 0.4, 0.0, 0.0, 0.82, 1.0, 1.0, 0.0],
  "drivers": [{ "paramIndex": 1, "beatDivision": 7, ... }],
  "abletonMappings": null
}
```

After migration, the canonical format is:

```json
{
  "effectType": "Bloom",
  "paramValues": { "amount": 1.0, "threshold": 144.0, "softness": 0.4, ... },
  "drivers": [{ "paramId": "threshold", "beatDivision": 7, ... }],
  "abletonMappings": null
}
```

Two forms persist at once. We implement **bidirectional custom `Deserialize` impls** that accept *either* shape:

```rust
impl<'de> Deserialize<'de> for EffectInstance {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        let param_values = match raw.get("paramValues") {
            Some(Value::Array(arr)) => positional_to_id_keyed(&effect_type, arr)?,
            Some(Value::Object(map)) => id_keyed_to_positional(&effect_type, map)?,
            _ => Vec::new(),
        };
        // ... assemble EffectInstance ...
    }
}
```

The pattern follows the existing `BeatDivision` custom deserializer (which accepts both `7` and `"FourWhole"`) — proven precedent in the codebase.

`ParameterDriver`, `ParamEnvelope`, `AbletonParamMapping`, `MacroMappingTarget` get analogous "accept old `paramIndex` or new `paramId`" deserializers. On load, all are normalized to `ParamId`. On save, everything writes the new form. Old projects keep loading; new projects write the canonical shape.

**`projectVersion` bump.** From `"1.0.0"` → `"1.2.0"`. The `migrate_if_needed` framework already exists (`migrate_v100_to_v110` in `manifold-io/src/migrate.rs`) — we add `migrate_v110_to_v120` running before deserialization so the bidirectional deserializers don't have to handle every legacy quirk.

**Hardcoded `align_to_definition` removed.** WireframeDepth 14→12 becomes a data-driven migration entry: `[("OldName1", "newName1"), ("OldName2", null /* dropped */), ...]`. Lives next to the effect definition. Other effects gain similar declarative migration tables as needed; the `align_to_definition` function disappears.

**Fixture round-trips.** Three test fixtures (`Burn V5.manifold`, `Burn V4.manifold`, `WAYPOINTS.manifold`) round-trip through load → serialize → load with assertions on driver counts, beat divisions, param values. Migration must keep these green at every commit.

### 7.6 V2 user-exposed params

The system supports user exposure from day one. When the user opens a graph editor and ticks "expose `UVTransform.translate`":

1. The editor generates a new `ParamId` (`"user_uvtransform_<n>"` or a name the user supplies).
2. A new `ParamBinding` is appended to `EffectInstance.user_param_bindings: Vec<ParamBinding>` (owned strings, runtime-allocated).
3. A new entry appears in `EffectInstance.param_values` at the corresponding ID slot.
4. The effect card UI rebuilds its slider list; the new param renders identically to developer-defined params.
5. OSC, Ableton, modulation, and macros can address the new param by its ID — the same way they address developer params.
6. The project file persists the user binding alongside the effect's other state.

V2 is *additive over V1 mechanics*. The framework is the same; the editor surface is what changes. No rework when V2 lights up — the data model and serialization already support it.

### 7.7 UI surface

The audit confirmed the existing UI infrastructure can host this with minimal scaffolding:

- **Graph canvas already has click-to-select.** `GraphCanvas::selected: Option<u32>` is set on left-button-down and rendered with a cyan border. We reuse this — no new selection plumbing.
- **Param panel slot.** When a node is selected, a right sidebar (built with `UITree` + `ScrollContainer` — both production-ready in the codebase) lists the node's parameters with expose checkboxes. Pattern: copy from `InspectorCompositePanel`'s two-column layout in `inspector.rs:97-107`.
- **Effect card EXP badge.** The card already has ABL / ENV / DRV badges (3 × 36px × 14px, right-aligned in the header — `effect_card.rs:611-700`). We add a 4th `EXP` badge with the same dirty-check sync pattern. Visibility tied to `EffectInstance.user_param_bindings.is_empty()`.
- **`PanelAction::EffectParamExpose(effect_index, node_id, inner_param_name, exposed: bool)`** — new action variant. Routed through the standard content-thread command path (same as `EffectParamChanged`).
- **Param widget rendering.** `BitmapSlider` already handles continuous, discrete, whole-number, and labeled-enum widgets. User-exposed params reuse the same widget set; they're indistinguishable from developer params at the slider level (intentional — IDs are the same, addressing is the same).

The graph editor canvas does NOT use UITree currently (it's raw `UIRenderer` primitives). The right-sidebar param panel is the first UITree component inside the editor window. Worth treating as scaffolding work in its own commit before the param-expose UI lands.

### 7.8 What's safe vs unsafe to change

Once this lands, the **stability rules** apply forever (just like `EffectTypeId`):

| Change | Safe? | Why |
|---|---|---|
| Rename `spec.name` (the slider label) | ✅ | Display string, mappings key off `id` |
| Rename `id` | ❌ | Forever rule. Add a new ID with deprecation alias if absolutely required. |
| Reorder `ParamBinding`s in the slice | ✅ | `param_values` is ID-keyed in storage, position is meaningless |
| Add a binding | ✅ | New ID gets default value; old projects gain a default-valued slot on load |
| Remove a binding | ❌ | Breaks any mapping referencing that ID. Add a deprecation flag instead — the binding stays but the UI hides it. |
| Change `target` (route to a different node) | ✅ | Internals are private |
| Change `convert` (e.g., add EnumRemap) | ⚠️ | Safe *if* the value the user sees vs stores doesn't change. Verify visually. |
| Decompose an effect into a sub-graph | ✅ | The `target` updates internally; the public IDs stay |
| Decompose and add new params | ✅ | New IDs append; old IDs route to the new sub-graph |

### 7.9 Generators

Generators have the same parameter shape as effects (`GeneratorParamState` mirrors `EffectInstance`). The same migration applies: `param_index` → `param_id`, same custom deserializer, same `ParamBinding` framework. We do them in lockstep with effects rather than in a separate phase — they're the same code shape and the alternative is two migrations.

Generator-specific concerns:
- Line-based generators (`is_line_based: bool` in `GeneratorMetadata`) use `Lines` output rather than `Texture2D`. The `ParamBinding` framework doesn't care about output type; routing is the same.
- String params (e.g., `GeneratorMetadata::string_params`) are a separate addressing system. They're already keyed by `key: &'static str` — already ID-style. No migration needed.

### 7.10 Macros

`MacroMappingTarget` includes `param_index` in 4 of its 5 variants (`MasterEffect`, `LayerEffect`, `GenParam`, etc.). Same migration: `param_index` → `param_id`. Saved projects with macros need the same custom deserializer treatment.

`MacroSlot::ableton_mapping: Option<AbletonParamMapping>` references its own `param_index` for the macro's own value (a macro is itself a parameter that Ableton can map). The pattern recurses cleanly: macros become identifiable by their ID just like effects.

### 7.11 Bindings unification (Phases 1–4, May 2026)

The earlier sections of §7 describe the *first* shape of the binding
system, where a renderer-side `ParamBinding` (static spec) and a
core-side `UserParamBinding` (per-instance) lived as parallel
structures. That parallel split survived for months before a user-
visible bug (an exposed slider that silently no-op'd) made it clear
the duality was load-bearing in only one direction — the runtime did
not need it. Phases 1–4 of `docs/BINDINGS_UNIFICATION_PLAN.md`
collapsed every parallel-tier path. The end-state matters for anyone
extending the binding system later, so it's captured here.

**Runtime (Phase 1, `1decd1a4`).** One `ResolvedBinding` type per
effect slot — a `Vec<ResolvedBinding>` of length `n_static + n_user`,
with `bindings[0..n_static]` hydrated from `ChainSpec.bindings` via
`ResolvedBinding::from_static` and `bindings[n_static..]` hydrated
from `EffectInstance.user_param_bindings` via
`ResolvedBinding::from_user`. The apply path walks one slice through
one loop, hitting one `LastAppliedCache.entries` parallel vec.
`apply_binding_defaults` walks the unified slice so the cache's
`Applied(default)` claim holds for both tiers. The runtime
resolved-target enum has only `Node | Composite | Custom` —
`HandleNode` is parse-time only and resolves at chain build.
Acceptance: the `&[]` second-slice bug class is unrepresentable
because there is no second slice.

**UI ↔ bridge wire format (Phase 2, `dfbeb1f1`).** Per-param
`PanelAction` variants carry `manifold_core::effects::ParamId`, not
positional `pi: usize`. `EffectParamInfo` / `GenParamInfo` carry
`param_id` populated at `state_sync.rs`. `AbletonPickerContext::*Param`
and `DropdownContext::*ParamContext` likewise. `ActiveInspectorDrag::
{EffectParam, GenParam}` records carry `param_id`. The bridge
handlers in `crates/manifold-app/src/ui_bridge/inspector.rs` consume
`ParamId` directly; no `pi → ParamId` translation, no
`effect_param_id`/`generator_param_id` helpers (deleted).
Acceptance: every modulation surface (drivers, envelopes, Ableton
macro mapping, "Map to Macro", expose toggles) works on user-
exposed sliders the same way it works on registry params, because
the wire format gives the bridge a `ParamId` directly. A future
handler can't reintroduce the bug — there's no positional index
step to misroute.

**Outer routing source tag (Phase 3, `6070031e`).** The snapshot's
`OuterParamRouting` carries `source: OuterParamSource`
(`Static | User`). `outer_routings_from_bindings` (runtime walk
over `Vec<ResolvedBinding>`) translates the binding's tier source
to the snapshot's tier marker. The registry walk in
`effect_registry::outer_routings_from_spec` always emits `Static`
(it operates on compile-time `ChainSpec.bindings` only). UI
consumers no longer re-derive the static-vs-user split from
external context.

**Convert enum merge (Phase 4, `9073daa9`).** The renderer-side
`ParamConvert` and core-side `UserParamConvert` were two enums with
overlapping shape. The Phase 4 collapse: deleted the renderer-
exclusive `EnumRemap(Cow<'static, [u32]>)` and `FloatTransform(fn)`
variants (both callerless — their curation moved into the
primitives), renamed core's `UserParamConvert` to `ParamConvert`,
deleted the renderer-side enum, exposed it via `pub use
manifold_core::effects::ParamConvert`. The `ParamValue` conversion
moved to a free function `convert_param_value(c, value)` in the
renderer because `ParamValue` can't cross into core. `ParamConvert`
is now `Copy`. The serde wire form is unchanged
(`{"type": "Float"|"IntRound"|"BoolThreshold"|"EnumRound"}` tagged
enum). The audit's `[CURATED]` row is structurally zero — a future
curation variant would fail the audit test as a reauthorisation
gate.

**Net architecture after Phase 5.** Three layers, one model:

1. **Source side.** Two tiers persist — registry-declared static
   bindings in `inventory::submit!` blocks and per-instance user
   bindings in `EffectInstance.user_param_bindings`. Different
   lifetimes, different persistence stories.
2. **Runtime side.** One `Vec<ResolvedBinding>` per slot. One apply
   loop. One cache. The runtime is tier-blind.
3. **External addressing.** One `ParamId` namespace shared by both
   tiers. OSC, MIDI, Ableton macros, drivers, envelopes,
   modulation evaluators, UI ↔ bridge wire — every external surface
   addresses by string id and walks both tiers transparently.

This is the end-state. The plan documents the rationale, the bugs
that motivated each phase, and the acceptance criteria in detail:
`docs/BINDINGS_UNIFICATION_PLAN.md`.

---

## 8. Stateful effects inventory

The 8 effects requiring `StateStore` migration:

| Effect | State shape | Resolution-dependent? | Quirks |
|---|---|---|---|
| **Bloom** | 2 `RenderTarget` mip pyramids (`mips_a`, `mips_b`), `count: usize` | Yes (full resize rebuilds) | Two pyramids, not ping-pong; `MAX_LEVELS=6`, `MIN_SIZE=16` |
| **Halation** | 2 quarter-res `RenderTarget`s (`buf_a`, `buf_b`) | Yes (always quarter-res) | Separable Gaussian + threshold extraction |
| **Watercolor** | 4 buffers (1 full-res feedback, 1 half-res flow, 2 full-res temps) | Yes (resize discards state) | Persistent feedback across frames; intentional discard on resize |
| **Stylized Feedback** | 1 full-res `RenderTarget` (prev frame) | Yes | Three blend specializations (Screen/Additive/Max) |
| **Blob Tracking** | Fixed 320×180 readback RT + 5 per-blob `Vec`s + `ReadbackRequest` + native FFI worker | No (fixed downsample) | Native FFI thread pool; readback every 3 frames; One-Euro filter |
| **Depth of Field** | 2 full-res blur ping-pong + analysis-res depth (`depth_buffer`, `depth_texture`, `ReadbackRequest`) + optional MiDaS worker | Yes (blur), partial (depth) | 3 modes: tilt-shift, radial, depth-DNN; depth update every 2 frames |
| **Wireframe Depth** | 8 RTs at analysis resolution + 4 CPU buffers + 3 GPU upload textures + parallel/monolithic worker config | Yes (analysis res, ~360 max dim) | 3 parallel DNN workers (depth/flow/subject); 15 compute passes |
| **Auto Gain** | 16-byte shared GPU buffer + 4 `f32` envelope state | No (resolution-independent) | Dual-EMA envelope; sparse 16×16 luminance sampling |

**14 stateless effects:** chromatic_aberration, color_grade, dither, edge_detect, edge_stretch, glitch, hdr_boost, infrared, invert_colors, kaleidoscope, mirror, quad_mirror, strobe, transform, voronoi_prism. (Most hold cached pipelines/samplers, which is fine to keep on the effect — those are cross-frame singletons, not per-owner state.)

**Common patterns:**
- All key by `owner_key: i64`.
- Lifecycle: lazy or eager alloc → cleanup on `cleanup_owner_state(owner_key)`.
- Resize: most rebuild (resolution-dep); Auto Gain ignores; Watercolor discards.
- 3 effects use background FFI workers (BlobTracking, DOF-depth, WireframeDepth). **Workers stay with the effect**, not in StateStore.

A single `StateStore` API serves all 8. The interface needs to express:

- `get_or_init<T: NodeState>(node_id, owner_key, init: impl FnOnce(&GpuDevice) -> T)`
- `cleanup_owner(owner_key)` — drops all `(*, owner_key)` entries
- `cleanup_all()` — full reset on shutdown
- `clear_state(owner_key)` — for `clear_state` semantics on seek (Feedback, etc.)

Resize-driven reallocation is handled by each `NodeState` impl's `resize` method, called by the host when render resolution changes.

---

## 9. Migration plan with gates

### Phase 0 — Audit + design doc *(this document)*

Complete. Findings synthesized here. Gates:

- ✅ manifold-gpu API audit
- ✅ Stateful effect inventory
- ✅ Graph runtime internals review
- ✅ Performance baseline analysis
- ✅ Parameter addressing audit (drivers, envelopes, Ableton, macros, OSC, UI, commands)
- ✅ Project file format audit (serialization, versioning, fixtures, custom deserializers)
- ✅ Editor UI audit (graph canvas, effect card, ScrollContainer, badges)

### Phase 1 — Foundation: barriers, state, plan caching *(4-5 commits)*

**Goal:** Runtime infrastructure for the parameter binding work. Some already shipped during Phase 0/StylizedFeedback POC.

1. ✅ **manifold-gpu: pipeline barrier API** *(shipped: c62432ca)*. No-op stub for Metal; signature ready for Vulkan.
2. **manifold-gpu: feature-gate Metal-only modules.** `mps`, `metalfx`, `fft` behind `metal` feature. Audit caller sites.
3. ✅ **node_graph: StateStore API + tests** *(shipped: 9d5b6489)*. Plumbed through `EffectNodeContext`.
4. **node_graph: edit-driven plan caching.** `Graph::topology_version` + `CachedPlan` pattern. Tests confirming param changes don't invalidate, structural changes do.

**Gate to Phase 2:** plan caching works against the StylizedFeedback graph and any other graph-backed effect.

### Phase 2 — Parameter binding system *(8-10 commits)*

**Goal:** ID-based parameter addressing across every subsystem. Ship V2 in one arc — no V1 staging.

This is the largest and most invasive phase. It touches data model, serialization, UI, every effect, every generator, every command. Ordering is critical so each commit individually compiles and tests pass.

5. **`ParamBinding` / `ParamTarget` / `ParamConvert` types** in `manifold-core`. Pure data types + tests. No callers yet.
6. **Add `id: Cow<'static, str>` to `ParamSpec`/`ParamDef`.** Mechanical: every `inventory::submit!` call across `manifold-renderer/src/effects/` and `manifold-renderer/src/generators/` gains an explicit `id` field per param. ~33 effect/generator entries, ~120 param entries.
7. **Registry: `param_id_to_index(effect_type, id) → Option<usize>`.** Used by every addressing site to translate IDs to internal storage indices. Cached `AHashMap` per effect type, built lazily.
8. **Migrate `ParameterDriver`: `param_index: i32` → `param_id: ParamId`.** Custom Deserialize accepts both old and new shapes. Tests against a synthesized old-format JSON.
9. **Migrate `ParamEnvelope`: same pattern.**
10. **Migrate `AbletonParamMapping`: same pattern.**
11. **Migrate `MacroMappingTarget`: same pattern (4 variants).**
12. **Migrate `EffectInstance.param_values` serialization.** Custom Deserialize accepts `Vec<f32>` (legacy) and `Map<String, f32>` (canonical). On save, write the map form. `projectVersion` bumps to `"1.2.0"`.
13. **Migrate `GeneratorParamState`** in lockstep with `EffectInstance`. Same shape, same migration.
14. **Bump `projectVersion` and add `migrate_v110_to_v120`** in `manifold-io/src/migrate.rs`. Pre-deserialization JSON normalization.
15. **Replace hardcoded `align_to_definition` per-effect remaps** with declarative `legacy_param_aliases: &[(old_id, new_id)]` per effect. WireframeDepth's 14→12 becomes data, not code.
16. **Migrate UI command structs**: `ChangeEffectParamCommand`, `PanelAction::Effect*`, `EffectCardConfig::params`. The UI carries `ParamId` from the click site to the content thread.
17. **Generic `apply_param_bindings` shim** in `manifold-renderer`. Migrated effects use it. Retrofit `MirrorFX`, `SoftFocusGraphFX`, `StylizedFeedbackFX` to use it instead of their hand-rolled routing.

**Gates to Phase 3:**
- All three project file fixtures (`Burn V5`, `Burn V4`, `WAYPOINTS`) round-trip cleanly under new format.
- Save → load → save produces byte-identical output (modulo new `projectVersion`).
- Driver / Ableton / macro / OSC mapping tests still pass.
- No regression in any visual A/B test.

### Phase 3 — V2 user-exposed parameters UI *(4-5 commits)*

**Goal:** The cog editor lets users tick checkboxes to expose inner-node params on the effect card. End-to-end: tick → save project → reload → mapping survives.

18. **Graph canvas: right-sidebar param panel.** Built with `UITree` + `ScrollContainer` (first UITree component inside the editor window). Lists the selected node's parameters with expose checkboxes.
19. **`PanelAction::EffectParamExpose(effect_index, node_id, inner_param_name, exposed: bool)`.** Routed through the standard content-thread command path.
20. **`EffectInstance.user_param_bindings: Vec<ParamBinding>`.** Owned-string bindings per instance. `apply_param_bindings` iterates `static_bindings.chain(user_bindings)`.
21. **Effect card EXP badge.** Visibility tied to `user_param_bindings.is_empty()`. Same dirty-check pattern as ABL/ENV/DRV.
22. **Project file: serialize `user_param_bindings`.** ID-keyed, owned strings. Full round-trip test: tick a checkbox, save, reload, confirm mapping intact.

**Gate to Phase 4:** A user can open the cog editor on Mirror, expose `UVTransform.translate`, save the project, reopen Manifold, and find the exposed param + any OSC/Ableton mappings made to it intact.

### Phase 4 — Primitive library + preset graphs + EffectChain cutover

**Rewritten 2026-05-11.** The original Phase 4 plan was wrap-first: take each remaining effect and shove it into a `LegacyPostProcessNode`. That ships fast but delivers a graph runtime that runs the same monolithic effects — it doesn't unlock the §0 creative surface. New plan is decompose-first.

#### 4a. Primitive library design + build

The central work. Get this wrong and the creative surface is bad forever. Rough scope: ~30–50 atomic primitives spanning UV/spatial, sampling, color, blur, compositing, multi-pass infra, distortion, edge/structure, noise. Existing primitives (`Source`, `FinalOutput`, `Mix`, `UVTransform`, `Blur`, stubs for `Threshold`/`MipChain`/`Sample`/`Blend`) are the seed.

Sketch of the library (not prescriptive — needs a real design pass that audits every existing effect for primitive candidates):

| Bucket | Primitives (candidate list) |
|---|---|
| Spatial / UV | `UVTransform` (translate/scale/rot/fold modes), `Polar`, `Kaleidoscope`, `LookupUV` |
| Sampling | `Sample` (bilinear/nearest/bicubic switch), `EnvironmentSample`, `Feedback` (prev-frame) |
| Color | `ColorGrade` (lift/gamma/gain), `HueShift`, `ChannelMix`, `ChannelSwizzle`, `Threshold`, `Tonemap`, `Invert` |
| Blur | `SeparableGaussian`, `BoxBlur`, `RadialBlur`, `DirectionalBlur` |
| Compositing | `Mix` (parametric blend modes), `Add`, `Multiply`, `Difference`, `Screen`, `Max` |
| Multi-pass infra | `MipChain` (downsample pyramid), `MipCombine` (upsample + blend) |
| Distortion | `ChromaticOffset`, `DisplacementMap`, `Voronoi`, `EdgeStretch` |
| Edge/structure | `SobelEdge`, `DepthEdge`, `Outline` |
| Noise | `BlueNoise`, `ValueNoise`, `Hash`, `Dither` |
| Generators (line) | `LineGenerator`, `LineStyle`, `LineModulate` |

Each primitive ships with: WGSL shader(s), parameter spec with semantic names + docstrings, typed ports, at least one example preset that uses it. Pixel-exact tests against any legacy implementation it replaces.

23a. **Audit every existing effect for primitive candidates.** Output: the actual ~30–50 primitive list (not the sketch above), with rationale per entry.
23b. **Pixel-exactness — SETTLED 2026-05-11:** Effects AND generators must be **pixel-perfect and mathematically exact** to current implementations. Primitives must be parameterizable enough to express each effect's exact float math, constants, texture formats, sampler state, and dispatch shapes. If a primitive can't reach bit-exactness for a given effect, that effect goes monolithic instead of being decomposed.
23c. **Build the missing primitives.** Each is small (~50–150 LOC + WGSL). Probably 20–30 new primitives to fill the library. Each primitive ships with a per-effect-reproduction test that compares output against the legacy shader bit-exactly.

#### 4b. Preset graphs replace decomposable effects

~12–15 of the 19 existing effects become preset graphs:

- Bloom = `Threshold → MipChain.down → Gaussian (per mip) → MipChain.up → Mix(in)`
- Halation = `ChannelMix(R-heavy) → Threshold → BoxBlur → Tint → Add(in)`
- Watercolor = `Feedback → Gaussian → UVTransform(drift) → Mix(in)`
- Infrared = `ColorGrade(IR LUT)`
- StylizedFeedback = `Feedback + ColorGrade + Mix`
- Mirror = `UVTransform(fold mode)` *(already is this)*
- Transform = `UVTransform` *(already is this)*
- Invert = `Invert` primitive
- Kaleidoscope = `Kaleidoscope` primitive (or `UVTransform + Polar`)
- ChromaticAberration = `ChromaticOffset`
- Glitch = `Hash → DisplacementMap`
- Dither = `Dither`
- HDRBoost = `ColorGrade` variant
- ColorGrade = `ColorGrade` *(direct)*
- EdgeDetect = `SobelEdge`
- EdgeStretch = `EdgeStretch` primitive
- Strobe = `Mix(time-modulated)`
- QuadMirror = `Kaleidoscope(n=4)`
- VoronoiPrism = `Voronoi`

24. **Preset graph format + loader.** Same serialization as project-file graphs. Shipped presets live in `assets/effect-presets/`.
25. **"Add Effect" UI loads preset.** One-click affordance unchanged; underneath, it loads the preset graph into the layer/master chain.
26. **Preset save/load UX for user-created graphs.** Critical for the creative surface — users save their forks. AI agents emit graphs into the same format.

#### 4c. Monolithic effects wrapped as custom nodes

The remaining ~5 effects don't decompose into pixel-local primitives — FFI workers, DNN models, custom algorithms:

27. **`BlobTrack`** custom node — wraps existing blob-tracking pipeline + One-Euro filter.
28. **`WireframeDepth`** custom node — wraps 3-worker DNN pipeline + 15 compute passes.
29. **`AutoGain`** custom node — resolution-independent envelope, sparse luminance sample.
30. **`DepthOfField`** custom node — multi-mode (tilt-shift / radial / depth-DNN); the depth-DNN variant uses the same depth state as wireframe.

These are "library primitives that happen to be monolithic," equivalent to TouchDesigner's DNN TOPs. They have typed ports, named parameters, docstrings — they're composable with the rest of the library, just not decomposable.

#### 4d. Runtime cutover

With every effect either a preset graph (4b) or a monolithic node (4c), `EffectChain` has no remaining clients. The original wrap-first runtime work happens here:

31. **Static elision pass.** `EffectNode::static_passthrough` opt-in. Compiler reroutes wires, drops nodes from plan.
32. **Dynamic bypass instruction.** `Backend::alias_resource`, `bypass_predicate` field on `ExecutionStep`.
33. **Wet/dry sub-graph translation.** Compiler emits `Mix` + dry-fan-out for groups.
34. **`LayerCompositor` builds and caches per-chain graphs.** Replace `EffectChain::apply_chain` with `executor.execute_frame_with_state`.
35. **Delete `EffectChain`.** Remove all references.

**Gate to ship:**
- Existing 53-layer / 2928-clip benchmark project loads, plays, scrubs. **No visual regression** (frame-by-frame compare; tolerance per 4a's pixel-exactness decision).
- Frame time ≤ pre-cutover baseline + 5% at 120 FPS / 4K target.
- All unit tests pass.
- AI agent smoke test: an agent can read primitive docs and emit a working preset graph (the deliverable surface, not just an internal milestone).
- 24-hour soak in the dev environment with no panics.

### Phase 5 (future, post-arc)

- **Dispatch fusion compiler.** Adjacent pixel-local primitives compile to a single shader.
- **Multi-queue / async compute.** Independent branches dispatch on parallel queues.
- **Vulkan backend.** `VulkanBackend: Backend` plugs into the existing executor. No graph-runtime changes.
- **User-built composite saving / sharing.** Per `NODE_GRAPH_SYSTEM.md` §13.

---

## 10. Risks and open questions

### Risks

1. **Performance regression at scale.** Mitigated by: analytical baseline (§4.2), edge-driven plan caching, executor scratch reuse. Measured by: 53-layer benchmark Phase 4 gate.

2. **Stateful migration complexity.** WireframeDepth has 8 RTTs + 4 CPU buffers + 3 workers — a lot of state. Mitigated by: ParamBinding framework lands first so each migration is mostly composition, not new mechanism. StylizedFeedback POC validates the smaller case.

3. **Wet/dry float-equality at sub-graph cutover.** `Mix` shader vs imperative `WetDryLerpPipeline` should produce identical output, but float ordering can drift. Mitigated by: explicit Phase 4 gate comparing output bit-exact (or within epsilon) before cutover.

4. **Background workers + StateStore.** Workers are shared singletons across owners. Their state isn't per-owner. Solution: workers stay with their effects (not in StateStore), but the per-owner *result* state (depth_texture, depth_buffer) lives in StateStore. The split is clean.

5. **Plan recompile during user edits.** If the user holds shift+drag a slider that's actually a topology-affecting param, plan rebuilds every drag-frame. Mitigated by: edit events go through the editing service which already debounces, and plan compile is ~10μs (imperceptible).

6. **Project file fixture round-trips.** Three fixtures must keep loading and serializing identically through Phase 2's serialization migration. Highest-risk piece because failure = corrupted user data. Mitigated by: bidirectional `Deserialize` impls accept both shapes; explicit fixture round-trip tests run on every commit; we keep `projectVersion = "1.0.0"` write-side until late in Phase 2 so old code can read new files until everyone's upgraded.

7. **OSC client compatibility during migration.** OSC paths derive from `osc_prefix` + `osc_suffix`. As long as those don't change per-param (and we won't change them), external OSC clients keep working. Confirmed by the audit — OSC is the one subsystem where the public surface is already string-based.

### Open questions

1. **Should `EffectInstance.enabled == false` be static elision, dynamic bypass, or both?** Recommend: static. Toggle is a user action (UI click), so it's a topology-version-bump event. Plan rebuilds; the disabled effect doesn't appear in the plan at all.

2. **What's the canonical owner_key namespace under graph-as-chain?** Today: master=0, layer=layer_index+1, clip=hash(clip_id). With per-chain graphs, we keep this — owner_key is host-level identity; graphs are unaware. StateStore is per-host-process, keyed by `(NodeInstanceId, owner_key)`.

3. **Composite expansion vs in-place sub-graph?** Composites today expand inline (`build_soft_focus` adds 2 nodes to the parent graph). Recommend: stay inline. Inline composites participate in static elision and lifetime analysis. Opaque sub-graphs would require a recursive executor.

4. **Vulkan barrier granularity.** `vkCmdPipelineBarrier2` supports per-stage and per-access masks for finer-grained barriers. Current proposed `pipeline_barrier(reads, writes)` is coarse (full memory + execution barrier). Coarse is fine for Phase 1; finer granularity is a future Vulkan-side optimization.

5. **`param_values` storage: positional or ID-keyed in memory?** Recommend: stay positional (`Vec<f32>`). Hot-path `apply_param_bindings` iterates the binding slice and indexes by position; switching to `AHashMap<ParamId, f32>` would cost a hash lookup per param per frame. The map form is the *serialization* shape; in-memory we use the registry's `ParamId → index` lookup once at addressing-site boundaries (Ableton update, OSC dispatch, modulation), then index by position from there.

6. **User-exposed param ID generation.** Recommend: `"user.<inner_node_type>.<inner_param>.<n>"` where `<n>` disambiguates collisions. E.g., the first user-exposed `UVTransform.translate` is `"user.uv_transform.translate.1"`. Stable, human-readable, collision-resistant. UUIDs are an alternative but they're opaque in project files.

7. **Migration of param removal.** If a developer removes a param (rare, generally forbidden), saved projects with mappings to that ID get orphaned. Recommend: mappings show in a "broken mappings" list in the UI and stay in the project file, so re-adding the ID re-binds them.

---

## 11. Implementation order summary

```
Phase 1: Foundation (4-5 commits, ~half done)
  ✅ pipeline_barrier API stub
  ✅ StateStore + EffectNodeContext plumbing
  □  Feature-gate manifold-gpu Metal-only modules
  □  Edit-driven plan caching

Phase 2: Parameter binding system (8-10 commits, the big one)
  □  ParamBinding / ParamTarget / ParamConvert types
  □  Add `id` to ParamSpec/ParamDef, fill in for all 33 effect+generator entries
  □  Registry: param_id_to_index lookup
  □  Migrate ParameterDriver to ParamId
  □  Migrate ParamEnvelope to ParamId
  □  Migrate AbletonParamMapping to ParamId
  □  Migrate MacroMappingTarget to ParamId
  □  Migrate EffectInstance.param_values serialization (custom Deserialize)
  □  Migrate GeneratorParamState in lockstep
  □  projectVersion bump + migrate_v110_to_v120
  □  Replace align_to_definition with declarative legacy_param_aliases
  □  Migrate UI commands (ChangeEffectParamCommand, PanelAction::Effect*)
  □  Generic apply_param_bindings shim; retrofit Mirror, SoftFocus, StylizedFeedback

Phase 3: V2 user-exposed parameters UI (4-5 commits)
  □  Graph canvas right-sidebar param panel (UITree + ScrollContainer)
  □  PanelAction::EffectParamExpose action + content thread routing
  □  EffectInstance.user_param_bindings field + serialization
  □  Effect card EXP badge
  □  End-to-end test: tick checkbox, save, reload, mappings intact

Phase 4: Primitive library + preset graphs + cutover (~20-25 commits)
  4a: Primitive library
    □  Audit existing effects for primitive candidates → real ~30-50 list
    □  Pixel-exactness decision (bit-identical vs close-enough)
    □  Build the 20-30 missing primitives, each with docstrings + example presets
  4b: Preset graphs replace decomposable effects
    □  Preset graph format + loader
    □  "Add Effect" UI loads preset
    □  User preset save/load UX
    □  Decompose 12-15 effects into preset graphs (Bloom, Halation, Watercolor, ...)
  4c: Monolithic effects as custom nodes
    □  BlobTrack, WireframeDepth, AutoGain, DepthOfField, FlowEstimate
  4d: Runtime cutover
    □  Static elision pass
    □  Dynamic bypass instruction
    □  Wet/dry sub-graph translation
    □  LayerCompositor builds + caches per-chain graphs
    □  Delete EffectChain
```

Total: ~40-50 commits across the arc under the reframed Phase 4 (was ~25-30 with the wrap-first plan). Each commit independently shippable. Each phase has a hard gate. Rolling back from any phase leaves the system functional.

Phases 2 and 4 are the two big design exercises. Phase 2 (parameter binding) was the precondition — done. Phase 4a (primitive library design) is the new center of gravity: it's where the §0 creative surface is actually built. Get the primitive library right and the rest of Phase 4 is composition + cutover; get it wrong and the system has a bad creative surface forever. Treat 4a as the most important design pass in the arc.

---

## 12. References

- [`docs/NODE_GRAPH_SYSTEM.md`](NODE_GRAPH_SYSTEM.md) — overall node-graph architecture.
- [`docs/MANIFOLD_GPU_ARCHITECTURE.md`](MANIFOLD_GPU_ARCHITECTURE.md) — manifold-gpu design.
- [Frostbite frame graph paper](https://www.gdcvault.com/play/1024612/FrameGraph-Extensible-Rendering-Architecture-in) — the resource-and-state-as-runtime-resource pattern this borrows from.
- [Unreal RDG documentation](https://docs.unrealengine.com/5.0/en-US/render-dependency-graph-in-unreal-engine/) — same pattern, different vocabulary.
