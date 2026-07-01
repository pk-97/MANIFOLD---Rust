# Graph Editor ↔ Main Inspector Unification

**Status:** Change 2 (selection-follows) SHIPPED 2026-07-01. Change 3 (full
inspector column in the editor) IN PROGRESS 2026-07-01 — supersedes the narrower
Change 1 below.
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

---

## Change 3 — Full inspector column in the editor (the current direction)

**Decision (2026-07-01).** The editor's right lane stops being a single watched
card (`editor_card: ParamCardPanel`) and becomes the **whole inspector column** —
the same `InspectorCompositePanel` the main window shows: master / layer / clip
tabs, every effect card, generator params, macros, chrome. The editor keeps its
own **preview monitors** (left) and **mini-timeline** (bottom) as they are; only
the right lane changes. (Scope narrowed from "full main-UI shell" to
"inspector column only, existing mini-timeline is fine for now" — Peter, same day.)

### "Literally the same object" — what that resolves to

Not one shared instance. A panel builds its nodes into **one** `UITree`, and each
window owns its own tree + offscreen — a single `InspectorCompositePanel` can hold
only one window's node ids at a time (this holds even though both `UIRoot`s live
on the same UI thread). So it is the **same panel type, driven by the same
`Arc<Project>` snapshot**: identical look, identical data, edits reflect both ways
on the next snapshot. Behaviourally one instrument in two windows; structurally
two instances over one source of truth. Consistent with the Change-1 ruling — no
pointer / shared-widget scheme, and none should be added (it would be forbidden
cross-tree shared state).

### Why this is mostly wiring, not new infrastructure

- **The editor window already owns a full `UIRoot`** (`Workspace.ui_root`) with an
  unused `inspector: InspectorCompositePanel` field — the second instance already
  exists.
- **Sync is window-agnostic.** `sync_inspector_data(ui: &mut UIRoot, project, …)`
  configures any `UIRoot`'s inspector from the snapshot. Call it against the
  editor's `ws.ui_root` each editor frame.
- **Dispatch is already editor-aware.** `dispatch_inspector(…, editor_target)` +
  `editor_dispatch_context` route a card action to the editor's watched target
  today (that's how the single `editor_card` works). The full inspector emits the
  same action vocabulary — more of it (chrome, macros, tabs, add-effect, card
  drag-reorder), each of which already has a dispatch arm.

### The seams (where real code changes)

1. **Build-to-rect.** `InspectorCompositePanel::build` keys every rect off
   `layout.inspector()` (`inspector.rs:1762`, plus type-in/drag helpers at ~2605,
   ~2794). Decouple: add `build_in_rect(tree, rect)`; `Panel::build` becomes
   `build_in_rect(tree, layout.inspector())`. The editor passes `dock.right`. The
   two helper sites take the same rect (thread it, don't re-read `layout`).
2. **Present pass.** `present_graph_editor_window` (`app_render.rs`) swaps the
   `editor_card` build/sync block for: `sync_inspector_data(&mut ws.ui_root, …)`
   then `ws.ui_root.inspector.build_in_rect(&mut ws.ui_root.tree, dock.right)`.
   The single-card resolver (`editor_card_config`), `editor_card_config_hash`,
   and the `editor_card` field are deleted.
3. **Input.** The editor's pointer/scroll/drag path (`window_input.rs`
   `editor_mouse_input` / `editor_cursor_moved` / `editor_mouse_wheel`) currently
   only feeds `process_pointer` for the one card. Extend it to drive the full
   inspector interaction set the way `UIRoot::process_events` does for the main
   window: click → drain → dispatch, `inspector.handle_drag[_end]` for slider/param
   drags, `inspector.handle_scroll` / `try_scroll_in_place` for the wheel,
   `try_begin_card_drag` / `update_card_drag` / `end_card_drag` for reorder.
4. **Selection-follows from the editor.** Clicking a card in the *editor's*
   inspector must retarget the canvas (set `watched_graph_target`, send
   `WatchEffectGraph` / `WatchGeneratorGraph`) — the same retarget helper the main
   window uses, now reachable from the editor's action drain.
5. **Editor dispatch context.** Every inspector action dispatched from the editor
   passes `editor_target = watched_graph_target` so modulation/param arms resolve
   against the edited graph, not the main window's selection.

### What deletes

`editor_card` (the single `ParamCardPanel`), `editor_card_config_hash`,
`editor_card_config` resolver, and the jump-to-node-by-card-label special-case that
only existed because the right lane held one card. The full inspector's own card →
canvas retarget replaces it.

## Unified node UI — the on-node controls must match the inspector

**Mandate (2026-07-01).** The node faces in the canvas and the inspector cards
must read as **one system**. The on-node param rows (name, value cell, slider,
checkbox, T/~/A modulation glyphs) use the **same UI elements, theme tokens,
sliders, fonts, and text sizes** as `param_slider_shared` / `ParamCardPanel` —
not a parallel look. Today the node rows are a thinner, differently-styled
parallel (see the screenshot: node rows vs. the polished right-lane card). Bring
them onto the shared widget vocabulary so a slider on a node and the same slider
on the card are pixel-identical. Concretely: shared `color::` tokens (no bespoke
node greys), `param_slider_shared` for the slider + value cell, `color::FONT_*`
sizes, the same T/~/A glyph set and hit-targets. Any new node-face control starts
from the card widgets, never a fork.

### Shipped (2026-07-01) — full widget parity for ranged params

Ranged numeric node-face params (`Float`/`Angle`/`Frequency`/`Int` with a
range — anywhere `p.fill.is_some()`) now draw the **exact same** track/fill/
thumb/value-cell widget the inspector card uses, not a token-matched
lookalike:

- `crates/manifold-ui/src/slider.rs` — `BitmapSlider::draw`, an immediate-mode
  twin of the card's tree-building `BitmapSlider::build`. Both share the same
  parameterized geometry math (`compute_fill_width`/`compute_thumb_rect` now
  take `fill_inset`/`thumb_width`/`thumb_inset` instead of hardcoding the
  consts), so `draw` — the canvas's zoom-scaled renderer — and `build` — the
  card's fixed-size renderer — can't drift apart. `draw` reads `SliderColors`
  (from `chrome::Theme::slider_colors()`) exactly like `build` does; cards
  never call `draw`, so their pixels are untouched.
- `crates/manifold-ui/src/graph_canvas/render.rs` — the `NodeRow::Param` block
  now branches on `p.fill`: ranged params call `BitmapSlider::draw`
  (`Theme::INSPECTOR.slider_colors()`, text swapped to `TEXT_DIMMED_C32` when
  wire-driven so the whole row dims as one unit); everything else (enum /
  bool / colour / string / table — never had a fill bar) keeps the plain
  label+value text row, just re-tokened onto `color::TEXT_PRIMARY_C32` /
  `TEXT_DIMMED_C32` and `color::FONT_BODY`-based sizing. Building dropdown-chip
  / toggle-switch canvas widgets for those kinds is explicitly out of scope —
  a separate, bigger project than "sliders, fonts, text sizes".
- `crates/manifold-ui/src/graph_canvas/mod.rs` — `PARAM_ROW_H` 18→24 (matches
  the card's `ROW_HEIGHT`); `PARAM_FILL_BG`/`FG` now only back the unrelated
  Color/Vec channel-editor popover bar, not the node row.
- `crates/manifold-ui/src/draw.rs` — `text_width`/`elide_to_width` moved here
  from `graph_canvas::model` (were canvas-only; now the shared text-metric
  home for any immediate-mode `Painter` consumer, `slider::draw` included).
- Interaction is untouched by design: `DragMode::ParamScrub` is a
  relative-delta scrub from the press origin, independent of what's drawn in
  the row — this was a render-only change. Verified against `ui-snap graph`
  and `ui-snap editor` headless PNGs; 468 `manifold-ui` + 73 `manifold-app`
  tests and workspace clippy all pass.

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

## Change 1 — Inspector reuse (SUPERSEDED by Change 3)

> Change 1 planned reusing just the `ParamCard` *widget*, bound to raw node
> params, inside the editor's own thin param stack. Change 3 goes further: the
> editor hosts the entire real inspector column, so the node-param adapter below
> is no longer the plan. Kept for the T/~/A-on-node-params analysis, which still
> informs the Unified-node-UI work.

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
