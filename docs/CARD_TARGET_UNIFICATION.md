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
position-against-the-active-context.** That identity is the effect's
[`EffectId`] (resolved via `Project::find_effect_by_id[_mut]`, which already
searches master + every layer + every clip) or the layer's generator
(`GraphTarget::Generator(LayerId)`). The editor's *watched* graph is named by
[`manifold_core::GraphTarget`](../crates/manifold-core/src/graph_target.rs)
(`Effect(EffectId)` / `Generator(LayerId)`), and that same identity drives the
dispatch.

Param indices stay positional WITHIN an effect (`param_values: Vec<ParamSlot>`
is correctly index-addressed). Only *which effect* changes from positional to
identity.

### Correction (2026-06-01): two targeting concepts, not one

The original draft of this doc said "delete `EffectTarget` entirely." Grounding
the command layer proved that wrong. `EffectTarget` plays **two** roles and only
one of them is the bug:

1. **Single-effect edits** (toggle, param value, expose, binding mapping,
   driver edits) address ONE instance. These are the ambient-targeting bug
   path. They become `EffectId` and resolve via `find_effect_by_id_mut` — which
   reaches master / layer / clip, so **clip effects work for free** and the
   `editor_card_config` clip guard is deleted.
2. **List / structural ops** (add, remove, reorder, group-reorder, rack ops)
   address an effect *list* + a position. An insert has no instance to name, so
   an `EffectId` cannot express the destination. These legitimately keep a
   list-target enum. `EffectTarget { Master | Layer }` is exactly that enum and
   **stays** (its narrowed, correct role). The done-criteria below are amended:
   `EffectTarget` survives for list ops; what disappears is its use for
   *instance* addressing.

So the end-game is a clean split: **`EffectId` for instances, `EffectTarget`
for lists.** `DriverTarget::Effect` carries `EffectId` (driver edits are
single-effect). Envelope cleanup on an unbind needs the owning layer, which the
instance doesn't hold — `Project::layer_id_for_effect(&EffectId)` resolves it
(master → `None`, no envelopes).

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

This is one connected, compiler-guided refactor of the live mutation gateway.
The editing-layer signature change forces every app-side call site to convert in
the same landing, so it must compile + pass the adversarial perform-byte-identical
check **atomically** before commit (see "Verification" below). Stage it as:

### Stage A — editing layer (DONE; preserved in `git stash@{0}`)

The foundation is written and compiles as a lib. It is stashed (not committed,
because it can't build the workspace until Stage B lands). `git stash show -p
stash@{0}` to restore. It does:

- `crates/manifold-core/src/project.rs` — adds `layer_id_for_effect(&EffectId)
  -> Option<LayerId>` (the owning layer for layer/clip effects; `None` for
  master). Powers the envelope-cleanup reach.
- `crates/manifold-editing/src/commands/effects.rs` — `ToggleEffectCommand`,
  `ChangeEffectParamCommand`, `ToggleEffectParamExposeCommand`,
  `ToggleStaticParamExposeCommand`, `EditUserParamBindingCommand` now take
  `EffectId` and resolve via `find_effect_by_id_mut`. List commands
  (`AddEffect`, `RemoveEffect`, `ReorderEffect`, `ReorderEffectGroup`) keep
  `EffectTarget` + `with_effects_mut`. Tests updated to pass the effect id.
- `crates/manifold-editing/src/commands/effect_target.rs` — `DriverTarget::Effect`
  carries `effect_id: EffectId`; doc on `EffectTarget`'s narrowed list role.
- `crates/manifold-editing/src/commands/drivers.rs` — `with_drivers_mut` Effect
  arm resolves via `find_effect_by_id_mut`.

### Stage B — app dispatch (the bulk; NOT yet done)

1. **Dispatch by identity, not ambient shadow.** Replace the A.2
   `editor_override: Option<&EditorDispatchTarget{tab, active_layer}>` (which
   *shadows* the ambient `(tab, active_layer)`) with the editor's identity:
   `editor_target: Option<&GraphTarget>`. In `dispatch_inspector`
   ([inspector.rs](../crates/manifold-app/src/ui_bridge/inspector.rs)), each
   `Effect*` arm resolves the `EffectId` as:
   `editor_target.and_then(Effect-id) .or_else(|| resolve_effect_id(tab,
   active_layer, ei))` — the editor segment uses its identity (ignoring the
   positional `ei`, since the editor card IS one effect); the inspector resolves
   its OWN active context (correct, not a bug). Build the id-based Stage-A
   commands. `Gen*` arms resolve the generator layer the same way
   (`editor_target` Generator vs `active_layer`). Keep `resolve_effect_target`
   for the list/group arms only.
2. **Retype `current_editor_target` away.** `app.rs` /
   `app_render.rs` — drop `current_editor_target: Option<(EffectTarget, usize)>`;
   use `watched_graph_target: Option<GraphTarget>` everywhere (it already holds
   the edited effect/generator id). `editor_card_config`
   ([state_sync.rs](../crates/manifold-app/src/ui_bridge/state_sync.rs)) finds
   the effect by id and **drops the clip guard**. The `app_render` `EffectMapping*`
   arms resolve the watched effect id and build `EditUserParamBindingCommand` by id.
3. **Fix the remaining call sites** the compiler flags: `input_host.rs` (5
   sites), and the two test files — `manifold-editing/tests/command_roundtrips.rs`
   and `manifold-app/tests/user_param_bindings_e2e.rs`.
4. **Remove the `CARD-TARGET-UNIFICATION` tags** as each site converts.

### Stage C — the `Effect*`/`Gen*` enum collapse (optional final purity)

After Stage B the editor reads zero ambient context, but the *inspector* still
resolves its own `(tab, active_layer, ei)` (legitimately — that IS its context).
The purest end-game removes even that: every card (inspector cards included)
carries its own `EffectId`/`LayerId` on its actions, so dispatch never reads
ambient at all, and the mirrored `Effect*`/`Gen*` variant pairs collapse into one
target-carrying family (see "Bonus payoff" below). This is the spec-sanctioned
deferrable: Stage B is "targeting-correct" without it.

## Verification (the atomic-landing gate)

Because Stage B rewrites the live mutation dispatch, the landing is gated, in
order:

1. `cargo build -p manifold-app` + `cargo clippy -p manifold-app -p
   manifold-editing --all-targets -- -D warnings` — clean.
2. `cargo test -p manifold-editing` (command_roundtrips) +
   `cargo test -p manifold-app` (user_param_bindings_e2e) — green.
3. **Adversarial workflow** — N independent skeptics, refute-by-default, each
   checking that the *perform* path (inspector card edits: param drag, driver
   config, envelope, expose, Ableton trim, reorder) produces byte-identical
   commands to pre-migration, and that the editor path now targets by identity.
   This is the real gate for the show rig — do not commit Stage B on a green
   build alone.
4. Load the canonical fixture (`Liveschool Live Show V6 LEDS.manifold`); it must
   render + edit byte-identical.

## Temporary guards to remove when step 3 lands

- **Clip-effect guard** (`editor_card_config` in `state_sync.rs`): the editor
  card bails to an empty lane when the resolved instance id doesn't match the
  editor's watched effect id. This exists because `EffectTarget` has no Clip
  variant, so the A.2 `(tab, active_layer)` override can't address a clip-scoped
  effect. When step 3 makes the editor resolve effects by `EffectId` via
  `find_effect_by_id`, clip effects become addressable and this guard should be
  removed (the card then shows + drives clip effects correctly).

## Done criteria (grep-able — amended 2026-06-01)

```
# 1. No (EffectTarget, usize) instance addressing, and no single-effect command
#    takes EffectTarget. EffectTarget SURVIVES for list ops (add/remove/reorder/
#    group) — that is correct, not a leftover:
rg -t rust "EffectTarget, *usize" crates             # → gone (no instance addressing)
rg -t rust "with_effects_mut|with_effects\b" crates  # → only list/group commands

# 2. current_editor_target is gone; the editor dispatches by watched identity:
rg -n "current_editor_target" crates                 # → gone (use watched_graph_target)

# 3. dispatch resolves single effects by id, not against the ambient active
#    layer. resolve_effect_target survives ONLY for list/group arms:
rg -n "resolve_effect_target" crates                 # → list/group arms only
rg -n "find_effect_by_id_mut" crates/manifold-app    # → present in the Effect* arms

# 4. The A.2 scaffolding is gone:
rg -n "EditorDispatchTarget|editor_override|editor_dispatch_target" crates   # → gone
rg -n "CLIP-SAFETY" crates/manifold-app/src/ui_bridge/state_sync.rs          # → gone

# 5. No tags left:
rg -rn "CARD-TARGET-UNIFICATION" crates              # → empty
```

When all hold (Stage B complete), this doc is "targeting-complete." Stage C (the
`Effect*`/`Gen*` collapse) is the only remaining purity; once it lands too, delete
this doc and drop the memory pointer `project_card_target_unification`.

## Bonus payoff: the `Effect*` / `Gen*` action fork collapses

The card emits ~67 `PanelAction` variants, but that count is inflated by two
multipliers, one of which this migration dissolves:

1. **Undo triad (legitimate, KEEP).** ~36 of the 67 are the Snapshot / Changed
   / Commit triad applied to six draggable surfaces (param slider, envelope
   ADSR params, envelope range, param trim, modulation target, Ableton trim).
   That triad is why a drag is one undo step, not a hundred — earned
   complexity, not bloat. Not a target of this migration.

2. **`Effect*` / `Gen*` mirror (avoidable — THIS migration's side effect).**
   Nearly every action is doubled because the card serves both effects and
   generators and bakes that distinction into the action *name*:
   `EffectParam{Snapshot,Changed,Commit}` ↔ `GenParam{…}`,
   `EffectEnvParam*` ↔ `GenEnvParam*`, `EffectEnvRange*` ↔ `GenEnvRange*`,
   `EffectTrim*` ↔ `GenTrim*`, `EffectTarget*` ↔ `GenTarget*`,
   `AbletonTrim*` ↔ `AbletonGenTrim*`, the toggles, etc.

Once an action carries (or is dispatched with) a `GraphTarget` — which already
encodes `Effect(id)` vs `Generator(id)` — the name no longer needs to. The
mirrored pairs collapse to a single target-carrying variant
(`ParamChanged{target, slot, phase}` replaces `EffectParam*` + `GenParam*`,
and so on). So identity-based targeting isn't only the bug fix; it roughly
**halves the card action enum** as a consequence.

Caveat: a handful of `Gen*` actions are genuinely generator-only and have no
effect mirror — `GenParamFire`, `GenParamToggle`, `GenStringParamClicked`,
`GenStringParamDropdownClicked`, `GenTypeClicked`, `GenCardRightClicked`. Those
stay. The collapse applies to the *mirrored pairs*, not to generator-specific
surfaces.

**Scope:** fold the enum collapse into step 3 (or a tightly-scoped follow-on),
AFTER the target plumbing is identity-based — it's a downstream simplification
the unification unlocks, not a prerequisite. Track it here so it isn't lost:
the migration is "targeting-complete" per the done-criteria above even if the
`Effect*/Gen*` enum collapse is deferred, but leaving the doubled enum in place
forever would re-accrue the smell the migration was meant to remove, so it is
an expected part of finishing the job, not optional polish.

## What this is NOT

- Not a change to param-within-effect indexing (`param_values` stays
  positional — correct).
- Not a change to the live performance instrument
  (`param_values` / `user_param_bindings` stay the per-frame surface;
  [[feedback_param_values_is_performance_surface]]).
- Not a card-component change: `ParamCardPanel` stays target-agnostic (host
  owns the target in and out). Editor vs perform chrome is a SEPARATE
  `CardContext` axis, orthogonal to this.
