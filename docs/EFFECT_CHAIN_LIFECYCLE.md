# Effect Chain State Lifecycle

How per-layer and per-group effect chains are created, preserved, cleared, and
dropped, and how that affects stateful effects such as feedback, bloom, and
halation.

Read this when investigating feedback bleed-through across project loads,
unexpected continuity after a topology edit, ghost trails from an earlier
scene, or a look that persists after a layer is muted.

## Where effect state lives

The compositor owns pools of optional [`PresetRuntime`](../crates/manifold-renderer/src/preset_runtime/core.rs)
instances, keyed by `LayerId` for layer and group chains. A runtime owns its
graph and its per-instance [`StateStore`](../crates/manifold-renderer/src/node_graph/state_store.rs).
State may also live in a primitive instance: `EffectNode::clear_state` is the
reset hook for both forms.

Disabled effects and disabled groups are omitted from a newly built graph. The
runtime is therefore the authoritative owner of state for the enabled graph;
there is no separate legacy effect state cache to synchronize.

## Chain pool policy

`LayerCompositor::trim_excess_buffers` applies two eviction rules:

1. It drops a pool entry as soon as its `LayerId` is absent from the current
   frame's layer list.
2. It drops an entry after `CHAIN_GRACE_FRAMES` unused render calls (currently
   18,000, approximately five minutes at 60 fps).

The idle reset is separate from eviction. Every layer, group, or LED group
chain that did not dispatch in the current frame is passed through
`clear_idle_chain_state`, which calls `PresetRuntime::clear_state` while
leaving the chain instance resident. A later activation therefore rebuilds no
chain resources, but its retained effect state starts clean.

## When state is cleared or retained

| Trigger | State behavior | Reason |
|---|---|---|
| Active clip dispatches effects | Retained and advanced normally | The graph runs for this frame. |
| No active clip, muted, or outside the solo set | Cleared immediately; runtime stays pooled | `clear_idle_chain_state` resets every stateful node and the runtime store. |
| Layer or group removed from the project | Runtime and buffers dropped on the next pool trim | The `LayerId` is no longer alive. |
| Project load or seek | All pooled effect runtimes are cleared | `clear_all_effect_state` walks layer, group, LED group, and master chains. |
| Compositor resize | Cached chains are dropped and rebuilt at the new dimensions | Resolution-dependent state is not carried across resize. |
| Topology rebuild at unchanged dimensions | Compatible card state may be harvested | `PresetRuntime::try_build` matches unchanged cards and node identity; membership changes, edited content, or changed upstream order reset the affected state. |

The topology rule is deliberately narrower than “every rebuild resets.” A
rebuild caused by fusion or an authoring change can preserve a compatible
card's persistent state, while adding, removing, disabling, or reordering the
relevant upstream cards prevents that harvest.

Feedback does not decay during an idle frame: the effect is not evaluated, and
the idle clear removes or resets its retained state instead. A later clip
starts from the primitive's fresh-state behavior. Continuous frames within an
active clip retain and evolve feedback according to that effect's parameters.

## Common symptoms and likely causes

| Symptom | Likely cause | Where to look |
|---|---|---|
| Old content appears after loading another project | A chain was not included in the clear path | [`LayerCompositor::clear_all_effect_state`](../crates/manifold-renderer/src/layer_compositor.rs) |
| A muted layer resumes with a stale trail | The chain was marked used or bypassed the idle clear | [`clear_idle_chain_state`](../crates/manifold-renderer/src/layer_compositor.rs) and the node's `clear_state` implementation |
| A topology edit unexpectedly changes a trail | The card failed the state-harvest match | [`PresetRuntime::harvest_state_from`](../crates/manifold-renderer/src/preset_runtime/core.rs) |
| A long-unused layer rebuilds on re-entry | Pool grace eviction reclaimed the runtime | `CHAIN_GRACE_FRAMES` and `trim_excess_buffers` |
| Memory grows during a long show | Pool trimming is not seeing the live layer list | The `trim_excess_buffers(frame.layers)` call in the compositor render path |

For rebuild diagnostics, `MANIFOLD_LOG_REBUILD_REASON=1` logs topology and pool
events, while `MANIFOLD_LOG_HARVEST=1` logs compatible node state carried into a
new runtime. These logs identify why state was rebuilt, harvested, or evicted;
they do not replace a visual runtime reproduction.

## Contract for stateful primitives

When adding a stateful primitive, keep persistent textures, buffers, and
accumulators in the primitive instance or the runtime's `StateStore`, and
override `clear_state` for every resource it owns. That hook is used by:

- idle layer and group clearing;
- project-load and seek clearing; and
- runtime reset paths that clear the graph and its `StateStore` together.

If a primitive needs a first-frame clear or seed after reset, keep that marker
with the primitive and make the first subsequent evaluation perform the clear
or seed. Do not add another compositor-level state cache.

Motion Mosh and Data Mosh extend this contract for retained image and mask
history. Their retained-state decisions, recovery behavior, fixed flow lag,
and reset expectations are defined in
[`MOSH_EFFECTS_DESIGN.md`](MOSH_EFFECTS_DESIGN.md).

## Related docs

- [`MOSH_EFFECTS_DESIGN.md`](MOSH_EFFECTS_DESIGN.md) — retained-state contract for Motion Mosh and Data Mosh.
- [`CHAIN_POOL_REFACTOR_PLAN.md`](archive/CHAIN_POOL_REFACTOR_PLAN.md) — LayerId-keyed pool design.
- [`EFFECT_RUNTIME_UNIFICATION.md`](EFFECT_RUNTIME_UNIFICATION.md) — `PresetRuntime`, graph, and `StateStore` design.
- [`ADDING_PRIMITIVES.md`](ADDING_PRIMITIVES.md) — primitive authoring and lifecycle hooks.
- [`PRIMITIVE_LIBRARY_DESIGN.md`](PRIMITIVE_LIBRARY_DESIGN.md) — primitive catalog and composition patterns.
