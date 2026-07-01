# Graph Editor ↔ Main Inspector Unification

**Status:** Change 2 (selection-follows) SHIPPED 2026-07-01. Change 1 (inspector reuse) planned.
**Related:** `docs/GRAPH_EDITOR_REDESIGN.md`, `project_graph_editor_redesign`, `project_binding_unification_design`, `feedback_graph_editor_unified_surface`

> **Shipped — selection-follows.** Clicking an effect/generator card in the main
> inspector retargets an already-open graph editor to that card's graph. Landed as
> shared helpers on `Application` (`app_render.rs`): `resolve_effect_card_id`,
> `watch_effect_graph`, `watch_generator_graph` — now the single source for both
> the card cog (which also opens the window) and the new `EffectCardClicked` /
> `GenCardClicked` retarget arms. The retarget arms are gated to the main-window
> action segment (`action_idx < editor_card_seg_start`) so the editor's own card
> lane can't misfire a retarget, and to `graph_editor_window_id.is_some()` so a
> closed editor stays closed (opening remains a deliberate cog action).

## Goal

Keep the graph editor as a **separate window** (Peter finds it useful). But stop
maintaining two inspector implementations, and make the editor **follow the main
window's card selection** — click an effect/generator card in the main inspector
and the open editor instantly retargets to that effect's graph.

Two changes, independent, shippable in order:

1. **Inspector reuse** — the editor's node param panel becomes the main
   `ParamCard`/`param_slider_shared` widget, not its own `GraphEditorPanel` param stack.
2. **Selection-follows** — selecting a card retargets the editor (`WatchEffectGraph`),
   no cog click needed.

## What is already shared (do not touch)

- **The model.** Content thread owns the one `Project`. Both windows read
  `Arc<Project>` snapshots.
- **The mutation gateway.** Graph edits already route
  `GraphEditCommand` → `ContentCommand::Execute(Box<dyn Command>)` / `MutateProject`
  → `EditingService` → `Project`
  (`app_render.rs` graph-edit loop, ~L1823–L2320). Same path as the main window.
  Cross-window sync therefore already exists: edit a node, next snapshot, both
  windows reflect it. **No pointer / shared-widget scheme is needed and none should
  be added** (would be new `Arc<Mutex>` shared state — forbidden).

## What is forked (the target)

- **Main inspector:** `ParamCard` + `param_slider_shared`
  (`crates/manifold-ui/src/panels/param_card.rs`). Rich: sliders, scrub,
  T/~/A modulation drawers, audio card state, badges, mapping chevron.
  One implementation already serves BOTH effect and generator kinds
  (`sync_values_effect` / `sync_values_generator`), and has a `CardContext`
  (`Perform` / `Author`) that already gates authoring affordances.
- **Graph editor:** `GraphEditorPanel` + `GraphEditorNodeView` + `GraphEditorParam`
  + `GraphEditorParamKind` (`crates/manifold-ui/src/panels/graph_editor.rs`).
  A **thinner parallel**: sliders + scrub + its own drag/format code, **no** T/~/A,
  **no** modulation. Duplicate work.

## Key nuance: the two inspectors edit different data

- Main inspector edits **exposed card params** — `param_values` / `user_param_bindings`
  (the curated performance surface). Emits effect/generator param commands.
- Graph editor edits **raw node params** — `GraphEditCommand::SetGraphNodeParam { node_id, .. }`
  (graph internals).

Same Project, same gateway, **different param sets**. So reusing the widget means
reusing the *widget code and layout*, bound to node-param data and emitting
`SetGraphNodeParam`. It is not "show the same rows." If the two windows ever look
out of sync, it's this-by-design, not a bug.

---

## Change 1 — Inspector reuse

### The binding adapter

`ParamCard` is driven by two calls:
- `configure(&ParamCardConfig)` — static shape (which params, kinds, labels, context).
- `sync_values(&[UiParamSlot])` — per-frame live values.

Today the editor builds `GraphEditorNodeView` (via `build_graph_editor_view` in
`app_render.rs` ~L4852) and renders it through `GraphEditorPanel`. The adapter:

1. **Map the selected node's params → `ParamCardConfig` + `[UiParamSlot]`.**
   `GraphEditorParam{name, kind, current_value, enum_labels, vec_value, …}` already
   carries everything `ParamCardConfig` needs. `GraphEditorParamKind` → the card's
   param kind is a 1:1 match (Float/Angle/Frequency/Int/Bool/Enum/Trigger/Color/Vec2-4/String).
2. **Card in `CardContext::Author`.** Author context already exists and is the right
   one for the editor (cog/mapping affordances, no perform-only chrome).
3. **Route the card's edits to `SetGraphNodeParam`.** The card emits its param-edit
   intent; the editor window translates that into
   `GraphEditCommand::SetGraphNodeParam { node_id, param_name, new_value }` keyed by
   the selected node's `node_id`. This is the one real seam — the card's edit output
   must be re-homed onto the graph-edit vocabulary instead of the effect-param one.

### Open decision — T/~/A on raw node params

`ParamCard` renders the T/~/A modulation column. On the main inspector that drives
modulation of **exposed** params (drivers/envelopes/audio on `param_values`).
Whether a **raw internal node param** can be modulated is **not confirmed** — the
existing modulation backend is built around exposed/user params. Options:

- **(A, recommended to start)** Reuse the card in a **reduced mode**: no T/~/A on raw
  node params. Matches today's editor behavior exactly, zero backend risk. Modulation
  still lives where it does now — on exposed card params in the main inspector.
- **(B, later)** Wire modulation for raw node params too, so T/~/A works in the editor.
  Separate, larger backend work; do not bundle into this change.

The graph already has a two-tier model — raw node params vs **exposed** params
(`GraphEditCommand::ToggleNodeParamExpose`). Option A preserves that tiering cleanly:
raw params get the shared *widget* minus modulation; exposed params get the full card
in the main inspector.

### What deletes

Once the card renders node params:
- `GraphEditorPanel`'s param-row rendering, drag (`handle_drag_begin`/`handle_drag`),
  and value formatting (`format_inner_param_value`).
- `GraphEditorParam` / `GraphEditorParamKind` collapse into a thin mapping to the
  card's param types (or delete if the card's types are fed directly).
- The editor-specific scrub/typein duplicated from `param_slider_shared`.

Keep: `GraphEditorNodeView` as the per-node data the adapter consumes (or fold into
the `ParamCardConfig` builder).

---

## Change 2 — Selection-follows

### Existing machinery

- `Application.watched_graph_target: Option<GraphTarget>` is the editor's target
  (effect id or generator layer). Editor renders whatever this points at.
- `ContentCommand::WatchEffectGraph(Some(eid))` / `WatchGeneratorGraph(Some(layer))`
  set it on the content thread.
- Today only the **card cog** (`PanelAction::OpenGraphEditor(ei)` ~L1180) sets
  `watched_graph_target` + sends `WatchEffectGraph` + `pending_open_graph_editor = true`.

### The change

On **card selection** in the main inspector (not just the cog):
- If the editor window is **open**, set `watched_graph_target` to that card's
  effect/generator and send `WatchEffectGraph` / `WatchGeneratorGraph`.
- The cog keeps its extra job: **open** the window (`pending_open_graph_editor`).
  Selection only **retargets** an already-open window.

Net: select a card → editor follows instantly. Click the cog → open the editor on
that card. Same underlying retarget call, two entry points.

### Edge cases to handle

- **No editor open:** selection does nothing extra (don't force-open — that would
  fight the authoring-vs-perform boundary; opening stays a deliberate cog action).
- **Card with no graph** (degenerate target): editor already handles a `None`/absent
  target by showing the panel's empty state (`app_render.rs` ~L3153). Selecting such
  a card should clear or leave the target; pick clear for predictability.
- **Rapid selection:** `WatchEffectGraph` is UI-state pushed to the content thread;
  last-writer-wins is fine, no queueing concern.

---

## Sequencing

1. **Change 2 first (selection-follows).** Small, isolated, no widget risk. Immediately
   delivers the "click effect → editor is on it" feel. Purely additive wiring on
   existing `watched_graph_target` machinery.
2. **Change 1 next (inspector reuse), Option A.** Build the `GraphEditorParam →
   ParamCardConfig/UiParamSlot` adapter, route card edits → `SetGraphNodeParam`,
   swap the editor's inspector render to `ParamCard` in `Author` context, delete the
   duplicate param stack.
3. **Option B (modulation on node params)** only if wanted, as its own change.

## Testing

- Per-crate UI: `cargo test -p manifold-ui --lib panels::param_card::` and
  `panels::graph_editor::` — the graph_editor.rs tests assert the click→`GraphEditCommand`
  resolution; they must still pass (or be ported) after the swap.
- Manual (headless PNG per `reference_ui_headless_png_verification`): render the editor
  with a node selected, confirm the card matches the main inspector's look.
- Manual: open editor, click different cards in the main window, confirm the editor
  retargets each time; edit a node param in the editor, confirm the main window's card
  reflects it next snapshot (and vice-versa where params overlap).

## Risks

- **Card edit → SetGraphNodeParam re-homing** is the one non-mechanical seam. The card
  was built to emit effect/generator param intents; its edit output must be adaptable
  to the graph vocabulary without forking the widget. If the card's edit path is too
  welded to effect params, that coupling is the thing to fix (at the card, not by
  re-forking).
- **Do not** let the reused card drag perform-only behavior into the editor — use
  `CardContext::Author` and verify no perform chrome leaks in.
- **Overclaim check:** "node params get modulation for free" is FALSE until Option B.
  Option A ships the shared widget without modulation on raw node params.
