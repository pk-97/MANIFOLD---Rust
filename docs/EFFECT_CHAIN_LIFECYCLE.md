# Effect Chain State Lifecycle

How per-layer / per-group effect chains are created, preserved, and dropped — and how that interacts with stateful effects (Watercolor, Stylized Feedback, Bloom, etc.).

**Read this first when chasing**: feedback bleed-through across project loads, feedback continuity issues after mutes / clip gaps, ghost trails from a previous scene, "the look resets when it shouldn't" or "the look persists when it shouldn't."

---

## Where effect state actually lives

Three separate caches, easy to confuse:

| Cache | Owned by | Keyed by | Holds |
|---|---|---|---|
| `LayerCompositor::effect_chains` (and the group / LED group variants) | The compositor | `LayerId` | `EffectChain` instances, which own a cached `chain_graph: Option<ChainGraph>` |
| `chain_graph.executor.backend()` textures | A specific `ChainGraph` inside a specific chain | Slot ids inside that graph | Compiled `Graph` + primitive instances (e.g. `primitives::Watercolor`) |
| Inside each primitive instance | The primitive (e.g. `Watercolor.feedback: Option<RenderTarget>`) | n/a — single field | The actual feedback texture for that effect |
| `EffectRegistry.processors[type].states[owner_key]` | The compositor | Effect type + `owner_key` (hash of `LayerId`, `ClipId`, etc.) | Legacy per-owner state for effects dispatched via the per-effect fallback path |

The hot path is **ChainGraph fast path** — Watercolor's feedback texture lives inside the primitive instance, which lives inside that chain's `chain_graph`. When the `chain_graph` is rebuilt, the primitive is recreated and the feedback texture is dropped + reallocated. Stale feedback is lost.

The legacy `EffectRegistry` path is the fallback (partial-wet-dry groups with non-contiguous positions, etc.). When a chain falls back, the `EffectRegistry`'s separately-keyed state is used.

These two state caches **do not synchronize automatically**. If you reset one without the other, you'll see inconsistent behavior. See [`clear_all_effect_state` in `layer_compositor.rs`](../crates/manifold-renderer/src/layer_compositor.rs) — it must walk both.

---

## Chain pool eviction policy (hybrid)

In [`LayerCompositor::trim_excess_buffers`](../crates/manifold-renderer/src/layer_compositor.rs):

1. **Event-based (immediate)**: drop a chain the moment its `LayerId` is no longer present in `frame.layers`. The user removed the layer from the project → chain (and its memory) goes immediately.
2. **Timer-based (safety net)**: drop a chain unused for more than `CHAIN_GRACE_FRAMES = 18000` (~5 min @ 60 fps). The layer technically still exists but the operator has clearly moved on from this section of the show.

Together: brief intra-song mutes / clip gaps preserve feedback state (visual continuity); long idle / explicit deletion reclaims memory.

**Tuning `CHAIN_GRACE_FRAMES`** is a trade-off:
- Lower → faster memory reclaim, more "state reset" surprises if a layer comes back unexpectedly.
- Higher → more visual continuity, more held memory.
- Counter advances per `render()` call, so the wall-time grace scales with project FPS (30-fps project gets 10 min, 120-fps project gets 2.5 min).

---

## Important: feedback **does not decay during mute**

Common misconception worth flagging: Watercolor's `decay` factor (default 0.99/frame) only multiplies when the effect actually runs. When a layer is muted or has no active clip, the effect doesn't run, so the feedback texture sits **frozen in GPU memory** at whatever the last-active frame looked like.

This means:
- After a 30-second mute, the feedback texture is unchanged from 30 seconds ago.
- When the layer resumes, watercolor sees that pre-mute frame as its feedback input — no natural fade has happened.
- The chain pool eviction policy is what *actually* resets feedback after a long idle.

If you want a layer's look to fade naturally during silence rather than freeze, the effect itself would need a "decay-while-idle" pass. None of the current MANIFOLD effects do this.

---

## When to expect a feedback reset

| Trigger | Resets feedback? | Why |
|---|---|---|
| Layer has an active clip dispatching effects this frame | **No** | Effect runs, state evolves normally per `amount`/`decay` parameters. |
| Layer has no active clip this frame (idle / muted / soloed-out) | **YES** | `clear_idle_chain_state` fires `clear_state` on the chain. Per-primitive state (Watercolor feedback, Bloom mips, Halation buffers) wiped. Chain instance stays in the pool — reactivation has no rebuild cost. |
| Layer is deleted from the project | **Yes** | Chain instance dropped immediately on next `trim_excess_buffers`. |
| Project is loaded (different `.manifold` file) | **Yes** | `clear_all_effect_state` clears legacy registry AND walks every chain to clear `chain_graph` state. |
| Compositor resizes (resolution change, render scale change) | **Yes** | `EffectChain::resize` sets `chain_graph = None` → next frame rebuilds with fresh primitives. |
| Seek (jumping playback head to a different time) | **Partial — currently only clears legacy** | `clear_all_effect_state` is also called on seek paths. After the 2026-05-13 fix, both caches clear consistently. |
| Topology change (effect added/removed, enabled toggled, group changed) | **Yes** | `is_compatible` returns false → chain_graph rebuilds. Stateful primitives lose their cached state. |

The "no active clip this frame" trigger is the key live-performance behavior: feedback effects start fresh on every clip retrigger after the layer goes idle, but feedback stays continuous *within* a clip and across rapid clip-to-clip transitions where the layer is never truly idle. Matches the operator intuition "if I muted this layer and unmuted it later, I want it to start fresh."

---

## Common symptoms → likely causes

| Symptom | Likely cause | Where to look |
|---|---|---|
| Ghost trails / old content visible after loading a different project | `clear_graph_runner_state` not wired into `clear_all_effect_state` | [`layer_compositor.rs::clear_all_effect_state`](../crates/manifold-renderer/src/layer_compositor.rs) — must call `clear_graph_runner_state()` on every chain |
| Layer's feedback "resets" after a long quiet stretch (>5 min) | Timer-based eviction triggered | `CHAIN_GRACE_FRAMES` — tune if needed, or rethink the policy |
| Same layer index produces different visuals frame-to-frame | Pre-Stage-1 bug, chains positionally indexed | Won't reoccur — chains are `AHashMap<LayerId, _>` |
| Master FX state contaminated by layer 0's effects | Pre-`5b77de38` bug, shared `effect_chains[0]` | Won't reoccur — master is a dedicated field |
| FPS tanks to ~30 fps at 4K with many effects | Pre-`07bfa5d4` bug, chain rebuild thrash | Won't reoccur — chains pinned to `LayerId` |
| Feedback look subtly different after refactor / migration | Eviction policy change shifted what gets retained between frames | See "When to expect a feedback reset" above |
| Memory grows unboundedly during a long show | Eviction policy not running, or `current_layers` slice empty | Check that `trim_excess_buffers(frame.layers)` is called every frame |

---

## Diagnosing rebuild thrash (if topology is suspected)

Set both env vars and run the scene where the symptom appears:

```bash
MANIFOLD_LOG_REBUILD_REASON=1 MANIFOLD_LOG_CHAIN_STATS=1 cargo run --release 2> debug.log
```

`MANIFOLD_LOG_REBUILD_REASON` dumps **both** the previous topology (`prev=...`) and the current frame's topology (`curr=...`) per rebuild, so you can diff in one line which field flapped. `MANIFOLD_LOG_CHAIN_STATS` prints per-second dispatch counters (dispatches / rebuilds / graph_runs / legacy_fallbacks).

Healthy: `rebuilds=0-2/sec` in steady state. Rebuilds every frame = topology flapping or chain identity issue.

---

## Don't add a third state cache

If you find yourself needing per-frame state for a new effect, **put it inside the primitive instance** (so it lives with the chain_graph) rather than introducing a fourth keyed cache. Three caches is already enough surface area to keep in sync.

When adding a new stateful primitive:
- **Override `clear_state()` to drop every persistent texture / accumulator the node owns.** See [`primitives/watercolor.rs::clear_state`](../crates/manifold-renderer/src/node_graph/primitives/watercolor.rs) as the reference impl: drop the `Option<RenderTarget>` fields and the `state_dims` so `ensure_state` knows to re-allocate. This single override hooks the primitive into:
  - `clear_idle_chain_state` (fires every frame the layer is idle — the live-performance reset).
  - `ChainGraph::clear_state` (fires on `clear_all_effect_state` — seek, project load).
  - `EffectChain::resize` (forces graph rebuild on resolution change).
- Set a `feedback_needs_clear: bool` flag on (re)allocation so the first `evaluate()` after a reset writes opaque/transparent black to the feedback. Reference: same `watercolor.rs`.
- Document the state in the primitive's docstring so this file's "Where state lives" stays accurate.

If you skip the `clear_state` override, your new primitive accumulates state indefinitely across mute/unmute cycles — the symptom is "feedback never clears, runs away to saturation." It's the silent-failure version of "I forgot to write a destructor."

---

## Related docs

- [`docs/CHAIN_POOL_REFACTOR_PLAN.md`](CHAIN_POOL_REFACTOR_PLAN.md) — the 2026-05 refactor that introduced LayerId-keyed pools (Stages 1–5).
- [`docs/EFFECT_RUNTIME_UNIFICATION.md`](EFFECT_RUNTIME_UNIFICATION.md) — design of `StateStore`, `ChainGraph`, and the legacy fallback contract.
- [`docs/ADDING_PRIMITIVES.md`](ADDING_PRIMITIVES.md) — primitive authoring guide.
- [`docs/PRIMITIVE_LIBRARY_DESIGN.md`](PRIMITIVE_LIBRARY_DESIGN.md) — primitive catalog + per-effect decomposition recipes.
