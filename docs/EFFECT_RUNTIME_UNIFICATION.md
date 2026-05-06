# Effect Runtime Unification

**Status:** Phase 0 (research & design). Not yet implemented. This document captures the audit results, architecture, and phased plan agreed during design discussion.

**Last updated:** 2026-05-06

**Companion docs:** [`NODE_GRAPH_SYSTEM.md`](NODE_GRAPH_SYSTEM.md) — overall node-graph architecture. [`MANIFOLD_GPU_ARCHITECTURE.md`](MANIFOLD_GPU_ARCHITECTURE.md) — manifold-gpu crate.

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

- One runtime path for all effect work. Delete `EffectChain`.
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

## 7. Stateful effects inventory

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

## 8. Migration plan with gates

### Phase 0 — Audit + design doc *(this document)*

Complete. Findings synthesized here. Gates:

- ✅ manifold-gpu API audit
- ✅ Stateful effect inventory  
- ✅ Graph runtime internals review
- ✅ Performance baseline analysis
- ⏳ User approval of this doc

### Phase 1 — Foundation *(5-7 commits)*

**Goal:** State extraction + plan caching + pipeline barrier API.

1. **manifold-gpu: pipeline barrier API.** Add `GpuEncoder::pipeline_barrier(reads, writes)`. Metal impl: no-op stub. Document the Vulkan-future contract.
2. **manifold-gpu: feature-gate Metal-only modules.** `mps`, `metalfx`, `fft` behind `metal` feature. Audit caller sites.
3. **node_graph: StateStore API + tests.** Type-erased state buckets keyed by `(NodeInstanceId, owner_key)`. Tests against a fake `NodeState`.
4. **node_graph: edit-driven plan caching.** `Graph::topology_version` + `CachedPlan` pattern. Tests confirming param changes don't invalidate, structural changes do.
5. **Migrate one stateful effect end-to-end as proof-of-concept.** Recommend **Auto Gain** as the first target — smallest state (4 floats + 16-byte buffer), no resolution dependence, no workers. If Auto Gain ports clean, Phase 1 mechanics are validated.
6. **Migrate remaining stateful effects.** Bloom, Halation, Watercolor, Stylized Feedback, DOF, BlobTracking, WireframeDepth. Each in its own commit for easy rollback. Workers stay with their effects.

**Gate to Phase 2:** all stateful effects pass their existing tests under the new pattern. No regression in unit tests or visual output (manual A/B).

### Phase 2 — Optimization passes *(2-3 commits)*

**Goal:** Compiler/runtime mechanisms for skip + groups.

7. **Static elision pass.** `EffectNode::static_passthrough` opt-in. Compiler reroutes wires, drops nodes from plan. Tests against `UVTransform[mode=Identity]`.
8. **Dynamic bypass instruction.** `Backend::alias_resource`, `bypass_predicate` field on `ExecutionStep`. `LegacyPostProcessNode` exposes inner `should_skip` as predicate. Tests on the executor with a dummy bypass predicate.
9. **Wet/dry sub-graph translation.** Compiler emits `Mix` + dry-fan-out for groups. Static elision collapses `wet_dry == 0/1`. Tests confirming param routing works, snapshot output equals current chain output.

**Gate to Phase 3:** wet/dry sub-graph output matches `EffectChain::apply_wet_dry_lerp` output bit-exact (or within float epsilon) for a representative test scene.

### Phase 3 — Cutover *(1-2 commits)*

**Goal:** Replace `EffectChain` with plan-driven executor.

10. **`LayerCompositor` builds and caches per-chain graphs.** One `Graph` per effect chain (master / layer / clip), stored on the compositor. `apply_chain` becomes `executor.execute_frame_with_gpu(graph, plan, ...)`.
11. **Delete `EffectChain`.** Remove all references. Update tests.

**Gate to ship:** 
- Existing 53-layer / 2928-clip benchmark project loads, plays, scrubs. **No visual regression** (frame-by-frame compare against pre-cutover).
- Frame time at production scale ≤ current `EffectChain` baseline + 5%. (Some headroom because the elision/bypass path is strictly cheaper, but plan rebuilds on edits could spike. Both should average out.)
- All unit tests pass.
- 24-hour soak in the dev environment with no panics.

### Phase 4 (future, post-arc)

Not part of this migration. Documented for completeness:

- **Dispatch fusion compiler.** Adjacent pixel-local primitives compile to a single shader.
- **Multi-queue / async compute.** Independent branches dispatch on parallel queues.
- **Vulkan backend.** Once manifold-gpu has a Vulkan implementation, `VulkanBackend: Backend` plugs into the existing executor. No graph-runtime changes.
- **User-built composite saving / sharing.** Per `NODE_GRAPH_SYSTEM.md` §13.

---

## 9. Risks and open questions

### Risks

1. **Performance regression at scale.** Mitigated by: analytical baseline (§4.2), edge-driven plan caching, executor scratch reuse. Measured by: 53-layer benchmark Phase 3 gate.

2. **Stateful migration complexity.** WireframeDepth has 8 RTTs + 4 CPU buffers + 3 workers — a lot of state. Mitigated by: Auto Gain proof-of-concept first; one-effect-per-commit migration.

3. **Wet/dry float-equality at sub-graph cutover.** `Mix` shader vs imperative `WetDryLerpPipeline` should produce identical output, but float ordering can drift. Mitigated by: explicit Phase 2 gate comparing output bit-exact (or within epsilon) before cutover.

4. **Background workers + StateStore.** Workers are shared singletons across owners. Their state isn't per-owner. Solution: workers stay with their effects (not in StateStore), but the per-owner *result* state (depth_texture, depth_buffer) lives in StateStore. The split is clean.

5. **Plan recompile during user edits.** If the user holds shift+drag a slider that's actually a topology-affecting param, plan rebuilds every drag-frame. Mitigated by: edit events go through the editing service which already debounces, and plan compile is ~10μs (imperceptible).

### Open questions

1. **Should `EffectInstance.enabled == false` be static elision, dynamic bypass, or both?** Recommend: static. Toggle is a user action (UI click), so it's a topology-version-bump event. Plan rebuilds; the disabled effect doesn't appear in the plan at all.

2. **What's the canonical owner_key namespace under graph-as-chain?** Today: master=0, layer=layer_index+1, clip=hash(clip_id). With per-chain graphs, can we keep this? Yes — owner_key is host-level identity; graphs are unaware. StateStore is per-host-process, keyed by `(NodeInstanceId, owner_key)`.

3. **Composite expansion vs in-place sub-graph?** Composites today expand inline (`build_bloom` adds 4 nodes to the parent graph). Should they stay inline or become opaque sub-graphs? Recommend: stay inline. Inline composites participate in static elision and lifetime analysis. Opaque sub-graphs would require a recursive executor.

4. **Vulkan barrier granularity.** `vkCmdPipelineBarrier2` supports per-stage and per-access masks for finer-grained barriers. Current proposed `pipeline_barrier(reads, writes)` is coarse (full memory + execution barrier). For Phase 1 the coarse barrier is fine; finer granularity is a future Vulkan-side optimization.

5. **Should `Graph` be `Send`?** Plans, yes (immutable, owned). Graphs, currently no (`Box<dyn EffectNode>` not Send). Once `EffectNode: Send` is enforced trait-wide (already is), graphs are Send too. Confirms that content thread can own the graph store.

---

## 10. Implementation order summary

```
Pre-Phase: manifold-gpu changes
  • Add pipeline_barrier API (Metal: no-op, Vulkan-ready)
  • Feature-gate Metal-only modules

Phase 1: Foundation
  • StateStore API
  • Plan caching (edit-driven)
  • Migrate Auto Gain (POC)
  • Migrate Bloom, Halation, Watercolor, Stylized Feedback
  • Migrate DOF, BlobTracking, WireframeDepth

Phase 2: Optimization passes
  • Static elision (EffectNode::static_passthrough)
  • Dynamic bypass (Backend::alias_resource + bypass_predicate)
  • Wet/dry sub-graphs (Mix + dry-fan-out)

Phase 3: Cutover
  • LayerCompositor builds + caches graphs
  • Delete EffectChain
```

Total: ~10-12 commits across the arc. Each commit independently shippable. Each phase has a hard gate. Rolling back from any phase leaves the system functional.

---

## 11. References

- [`docs/NODE_GRAPH_SYSTEM.md`](NODE_GRAPH_SYSTEM.md) — overall node-graph architecture.
- [`docs/MANIFOLD_GPU_ARCHITECTURE.md`](MANIFOLD_GPU_ARCHITECTURE.md) — manifold-gpu design.
- [Frostbite frame graph paper](https://www.gdcvault.com/play/1024612/FrameGraph-Extensible-Rendering-Architecture-in) — the resource-and-state-as-runtime-resource pattern this borrows from.
- [Unreal RDG documentation](https://docs.unrealengine.com/5.0/en-US/render-dependency-graph-in-unreal-engine/) — same pattern, different vocabulary.
