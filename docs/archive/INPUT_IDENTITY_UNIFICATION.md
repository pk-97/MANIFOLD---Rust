# Input Identity Unification — one input system, two UI architectures

SHIPPED 2026-06-25, branch `feat/input-widget-identity`. This is the design +
historical record for the `WidgetId` durable-identity layer that lets a single
`UIInputSystem` correctly serve both of MANIFOLD's UI architectures.

## The bug that motivated it

In the graph editor, **no click registered** — the mapping-drawer chevrons,
the card toggles, the sidebar, all dead. The timeline/inspector was fine.

Root cause, one broken assumption in the input system:

- On pointer **down** it stored the hit `NodeId` (`pressed_id`); on **up** it
  fired a `Click` only when `hit_id == pressed_id` — an exact `NodeId` compare.
- `NodeId` is generational (`docs` — see `node.rs`): every node minted in a build
  carries a fresh generation, and an id from one build does not validate against a
  later build. That is deliberate — it makes stale ids inert.
- The graph editor **rebuilds its entire `UITree` from scratch every frame**
  (`app_render.rs`, `present_graph_editor_window`: `tree.clear()` then rebuild,
  ungated). So between a press and a release — several frames apart — the tree was
  rebuilt many times and the pressed node carried a stale generation by release.
  `hit_id == pressed_id` could never hold → no click ever fired.

The timeline escaped it only because it is **dirty-tracked**: `ui_root.build()`
clears+rebuilds but is gated on `needs_rebuild`, so most frames it does *not*
rebuild and ids stay stable across a gesture.

## Why two UI architectures (and why we keep them)

The split is requirement-driven, not legacy:

| | Timeline / inspector | Graph editor |
|---|---|---|
| Model | Retained + **dirty-tracked** | **Immediate mode** (rebuild every frame) |
| Why | Thousands of clips at 60 fps on the show path; the bitmap sub-region cache serves unchanged regions for ~free | Tiny tree (one card + sidebar; the canvas is offscreen, not in the tree), content churns on every selection, off the show path |
| Rebuild | Only on change (`needs_rebuild`) | Unconditional, every frame |

Immediate mode for the editor is the right call (simpler, always matches state,
trivially cheap for a small tree). Retained+dirty for the timeline is the right
call (the static-heavy show surface must stay lightweight). The mistake was never
the two models — it was pointing a **stable-id-assuming** input system at the one
that churns ids.

The fix is to make the shared input layer **not depend on which model it serves.**

## The invariant

> **The input system never holds a generational `NodeId` across frames.** It
> tracks interaction targets (pressed / hovered / focused) by durable `WidgetId`,
> and resolves to a live `NodeId` only at the moment it emits an event or sets a
> flag.

Two id types, two clean roles:

- **`NodeId`** — transient per-frame *handle*. Generational. Correct for
  rendering and mutation *within* a build; inert against a later one.
- **`WidgetId`** — durable *identity*. The same logical widget gets the same
  `WidgetId` on every structurally-stable rebuild, so it survives the editor's
  per-frame clear+rebuild.

This is the same shape every mature toolkit uses: egui's hashed `Id`, React/
Flutter's key + reconciled element. Transient layout, durable identity.

## Design

### WidgetId derivation (`node.rs`, `tree.rs`)

`WidgetId = parent_widget.with(salt)`, where `with` is a splitmix64 mix and the
salt is:

- the node's **sibling index** by default (auto) — deterministic build order
  reproduces the same salts on rebuild; or
- an **explicit key** (`add_node_keyed` / `add_button_keyed`), namespaced by a
  high bit so it can never collide with an auto salt — for identity-bearing
  controls whose sibling position can shift between rebuilds.

`UITree` carries, parallel to its slots: `widget_ids` (every node — for hierarchy
and `widget_of`), `child_counts` / `root_count` (the auto salt source), and a
`widget_to_node` reverse map over **interactive nodes only** (the sole press /
hover / focus targets). `clear` and `truncate_from` maintain all of it. Two
accessors complete the contract:

> **Gap found and closed, 2026-07-08.** "`truncate_from` maintains all of it"
> was one line short. It recomputed `root_count` by counting the survivors whose
> parent is currently `None` — but `reparent_root_nodes` (the inspector wraps its
> sub-panels under a ClipRegion) moves a node that was *minted* as a root, and so
> already consumed a root salt, out of that count. A partial rebuild then
> re-issued an already-used root salt: the rebuilt overlay-chrome root got a
> *different* `WidgetId` than at press, and every control salted off it churned
> too. Only the Audio Setup panel consumes `PointerDown` (the BUG-059 stopgap),
> so it alone rebuilt the tree *between* a button's press and release — release
> resolved to a different widget than press, no `Click` fired, and the button
> needed a second click. Fix: a `root_minted` flag records mint-time parentage,
> and `truncate_from` salts `root_count` from it, so a partial rebuild reproduces
> the full build's root salts exactly. Guarded by
> `tree.rs::truncate_from_reproduces_root_salt_after_reparent` and the
> full-frame-loop `ui_root.rs::floor_stepper_fires_on_a_single_click_across_overlay_rebuild`.
> Shipped in the merge of `fix/audio-widgetid-churn`.

- `widget_of(NodeId) -> WidgetId` — durable id of a node; `NONE` if stale.
- `node_for_widget(WidgetId) -> Option<NodeId>` — resolve a held identity back to
  a **live** node in the current build.

A duplicate interactive `WidgetId` trips a `debug_assert` at insert — a duplicate
explicit key among siblings (or a sibling-salt collision) fails loudly in debug
instead of silently shadowing input.

> **Known gap, 2026-07-08 (open).** "Fails loudly" only covers keys that reach
> `mint`. The `ChromeHost` builder (`chrome/diff.rs`) records `View::key` for its
> own `node_id_for_key` lookup but mints every node with plain `add_node` — it
> **drops the key** before it reaches the tree, so keyed chrome falls back to the
> auto sibling salt and its identity rides on `root_count` (this is why the Audio
> Setup chrome root was position-dependent above). Two consequences: (a) any
> `ChromeHost` panel's chrome root is only as stable as its root salt, not truly
> keyed; (b) duplicate `View::key`s among siblings are **silently ignored today**
> rather than caught by the assert. Prototyping "make `ChromeHost` honor
> `View::key`" surfaced **5 such duplicate-key sites** in `inspector` /
> `param_card`. Closing this gap is a two-step follow-up, not yet done: (1) audit
> those panels and give each sibling a unique key; (2) switch `ChromeHost` build /
> materialize to `add_node_keyed` when `View::key` is set. Step 2 without step 1
> would turn those silent duplicates into live collisions. Not required for the
> 2026-07-08 fix above — the duplicate keys are inert while `ChromeHost` ignores
> them — but it would make overlay identity robust against structural reordering,
> not just against the partial-rebuild churn already closed.

### Input system (`input.rs`)

`pressed_widget` / `hovered_widget` / `focused_widget: Option<WidgetId>`.

- **Click** = press + release on the same *widget*. Stable across rebuilds, so it
  holds in the editor where the exact-`NodeId` compare never did. Emits the live
  release `NodeId`, which matches the freshly-built panel nodes downstream.
- **Drag / PointerUp** resolve the pressed widget to its live node at each emit.
- **Hover** compares by `WidgetId`, so a still cursor over the same widget is a
  no-op. (Pre-fix the stored hover `NodeId` went stale every rebuild → a
  HoverExit+HoverEnter pair fired *every frame* in the editor.)
- **KeyDown** resolves the focused widget (`process_key` takes `&tree`).
- `apply_interaction_flags(&mut tree)` re-applies HOVERED / PRESSED to the live
  nodes after a rebuild; the editor present calls it so interaction visuals
  survive its per-frame clear+rebuild instead of flickering.

### Editor card keying (`param_card.rs`, `param_slider_shared.rs`)

The editor card's per-row interactive controls (T/LFO/A arm buttons, mapping
chevron, generator toggle) take an explicit key `(param_index << 8) | role`, so
arming a modulator on an *earlier* row — which inserts drawer/overlay nodes and
shifts every later sibling — can't renumber another row's controls. Scoped to the
Author context; the perform inspector keeps auto identity (and never collides,
since each inspector card hangs off a different parent widget).

## Why this is the *fundamental* fix, not a patch

- The broken assumption lived in the input system; the fix is there.
- It is model-agnostic: the timeline (stable ids) and the editor (ids churn every
  frame) now satisfy one identity contract. Neither can produce this bug class
  again, by construction.
- It does **not** reach for the wrong roots: we did *not* dirty-track the editor
  (the generational system is *meant* to tolerate per-frame rebuilds), and we did
  *not* unify the two build models (the timeline's retained+cache perf is real).

A deliberately-narrow stopgap shipped first (commit `ef48371d`): match click
identity by node *index* instead of the stale `NodeId`. It worked only because the
editor rebuild is deterministic (same slots). The `WidgetId` layer removed that
assumption and deleted the stopgap.

## Test matrix

| Property | Test | File |
|---|---|---|
| WidgetId stable across clear+rebuild | `widget_id_is_stable_across_clear_and_rebuild` | `tree.rs` |
| Resolves to the live (rebuilt) id | `node_for_widget_resolves_to_the_live_id` | `tree.rs` |
| Stale id → `NONE` | `widget_of_stale_id_is_none` | `tree.rs` |
| Only interactive nodes resolvable | `only_interactive_nodes_are_resolvable` | `tree.rs` |
| Explicit key survives reordering | `explicit_key_survives_sibling_reordering` | `tree.rs` |
| **Click** survives rebuild mid-press | `click_survives_tree_rebuild_between_press_and_release` | `input.rs` |
| **Drag** survives rebuild mid-press | `drag_survives_tree_rebuild` | `input.rs` |
| **Hover** does not churn across rebuild | `hover_does_not_churn_across_rebuild` | `input.rs` |
| Keyed chevron stable when an earlier row arms a mod | `editor_chevron_identity_survives_earlier_row_arming_a_mod` | `param_card.rs` |
| Chevron is hit at its own center | `mapping_chevron_is_hit_at_its_own_center` | `param_card.rs` |

## Files

- `crates/manifold-ui/src/node.rs` — `WidgetId` type.
- `crates/manifold-ui/src/tree.rs` — widget index, `widget_of` / `node_for_widget`,
  `add_node_keyed` / `add_button_keyed`, clear/truncate maintenance.
- `crates/manifold-ui/src/input.rs` — the migration (the fix).
- `crates/manifold-ui/src/panels/param_slider_shared.rs` — row-control keying.
- `crates/manifold-ui/src/panels/param_card.rs` — editor card keying.
- `crates/manifold-app/src/ui_root.rs` — `process_key(&tree, …)`.
- `crates/manifold-app/src/app_render.rs` — editor present calls
  `apply_interaction_flags`.
