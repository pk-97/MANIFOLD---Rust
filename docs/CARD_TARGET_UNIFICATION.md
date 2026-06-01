# Card Target Unification — deferred migration (do NOT leave half-done)

**Status: OPEN.** The fork is created when the editor's identity-based
dispatch lands (card-rebuild step A.2); at that point a
`// CARD-TARGET-UNIFICATION:` tag is placed at each fork site (the editor's
new id-dispatch and the inspector's legacy ambient resolution). This migration
is not done until `rg -rn "CARD-TARGET-UNIFICATION" crates` returns empty AND
the done-criteria below all hold.

This is the load-bearing follow-up to the graph-editor card rebuild. The
editor card was deliberately moved to identity-based targeting FIRST (it's
zero-risk to the perform path), which leaves the inspector / perform path on
the legacy positional scheme. That fork is intentional and temporary. This
doc defines exactly how to close it so we never ship the half-state as the
permanent state.

## The invariant we are establishing

**A card is bound to an effect/generator by stable identity, never by
position-against-the-active-context.** That identity is
[`manifold_core::GraphTarget`](../crates/manifold-core/src/graph_target.rs):

- `GraphTarget::Effect(EffectId)` — addresses ANY effect (master, layer, clip;
  `Project::find_effect_by_id` already searches all of them).
- `GraphTarget::Generator(LayerId)` — addresses a layer generator.

Param indices stay positional WITHIN an effect (`param_values: Vec<ParamSlot>`
is correctly index-addressed). Only *which effect* changes from positional to
identity.

## Why (the bug class this kills)

`ui_bridge::dispatch` currently resolves "which effect" for every `Effect*`
action against the **ambient** `active_layer` (+ inspector tab) — see
`dispatch`'s `active_layer: &mut Option<LayerId>` parameter and
`resolve_effect_target`. Any host whose context differs from "the active
layer" mis-targets silently. The graph editor is exactly such a host (it edits
`current_editor_target`, which can differ from the active layer). This is the
same shape as the 2026-06-01 generator-scale bug: a hidden context one caller
reads differently than another. Identity-based targeting removes the ambient
read entirely, so the ambiguity cannot exist.

## Current state after step 1 (the intentional fork)

- **Editor card (DONE / in progress):** dispatches its card actions against its
  own `GraphTarget` (`watched_graph_target` / `current_editor_target`), by id.
  Correct regardless of the active layer.
- **Inspector / perform path (LEGACY, still positional):** still emits
  `Effect*` actions resolved by `dispatch` against the ambient `active_layer`.

The fork sites carry a `// CARD-TARGET-UNIFICATION:` comment pointing here.

## The migration (step 3) — exact sites and changes

1. **Make `dispatch` target-explicit.**
   `crates/manifold-app/src/ui_bridge/mod.rs` `dispatch()` — add an explicit
   `effect_target: &GraphTarget` (or per-action target) parameter and have
   every `Effect*` arm resolve the instance via `find_effect_by_id` /
   `find_layer_by_id` instead of the ambient `active_layer`. The inspector
   passes the `GraphTarget` it already computes from the active tab/layer, so
   the change is **behavior-preserving for perform** — the resolution just
   stops being implicit.

2. **Retype `current_editor_target`.**
   `crates/manifold-app/src/app.rs` — `Option<(EffectTarget, usize)>` becomes
   `Option<GraphTarget>` (the editor already has the `GraphTarget` in
   `watched_graph_target`; collapse the two). Update the editor dispatch arms
   (`EffectMapping*` etc. in `app_render.rs`) to carry `GraphTarget`.

3. **Delete the ambient resolver and `EffectTarget`.**
   - `crate::ui_bridge::resolve_effect_target` — delete.
   - `manifold_editing::commands::effect_target::EffectTarget` +
     `with_effects` / `with_effects_mut` — replace callers with
     id-based `find_effect_by_id(_mut)` / `find_layer_by_id(_mut)`, then delete
     `EffectTarget` (or reduce it to nothing). Master effects resolve via
     `GraphTarget::Effect(EffectId)` like any other.

4. **Remove the `CARD-TARGET-UNIFICATION` tags** as each site converts.

## Done criteria (grep-able — all must hold)

```
# 1. EffectTarget is gone (or only its grave remains):
rg -t rust "EffectTarget"            # → no enum, no (EffectTarget, usize), no with_effects

# 2. current_editor_target is identity-typed:
rg -n "current_editor_target" crates/manifold-app/src/app.rs   # → Option<GraphTarget>

# 3. dispatch no longer resolves effects against the ambient active layer:
rg -n "resolve_effect_target" crates                 # → gone

# 4. No tags left:
rg -rn "CARD-TARGET-UNIFICATION" crates              # → empty
```

When all four hold, delete this doc (or move it to a "closed migrations"
appendix) and drop the memory pointer `project_card_target_unification`.

## What this is NOT

- Not a change to param-within-effect indexing (`param_values` stays
  positional — correct).
- Not a change to the live performance instrument
  (`param_values` / `user_param_bindings` stay the per-frame surface;
  [[feedback_param_values_is_performance_surface]]).
- Not a card-component change: `ParamCardPanel` stays target-agnostic (host
  owns the target in and out). Editor vs perform chrome is a SEPARATE
  `CardContext` axis, orthogonal to this.
