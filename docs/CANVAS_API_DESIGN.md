# Canvas API — Phase 4 Design

**Status:** SHIPPED 2026-06-23 — as-built record.

> **SHIPPED 2026-06-23.** All six tasks landed behaviour-preserving, one commit
> each; manifold-ui 330 + manifold-app 91 tests green, clippy `-D warnings`
> clean. This doc is the as-built record — the §3 scoping notes (canvas stays in
> `manifold-app`; `GraphEditCommand` not on the `IntentRegistry`; the editor
> keyboard owner over a literal shared `process_events`) match what shipped.

Sub-design-doc for **Phase 4** of the UI Architecture Overhaul
(`docs/UI_ARCHITECTURE_OVERHAUL.md` §5.4, §13). Scopes the six Phase-4 tasks
against the codebase as it actually stands today. Written after a full read of
the graph-editor surface (the 4,262-line `graph_canvas.rs`, the sidebar panels,
the `PanelAction` graph block, and the editor window's input fork); every claim
below is backed by a `file:line`.

The graph editor is the **authoring** surface, not a perform-time surface — but
it is the surface agents (this one and others) build effect/generator graphs
against, so making it machine-legible matters. Phase 4 is **behaviour-preserving
structural cleanup**, not a rewrite. Each task lands as its own commit, gated by
the existing tests plus new ones, so any regression is bisectable.

The two precedents this mirrors: **`CHROME_API_DESIGN.md`** (the greenfield-API
framing — the `ChromeHost` describe-once/signature/reconcile discipline) and
**`TIMELINE_API_DESIGN.md`** (the monolith-extraction framing — split a god-file
into one-concern modules, one owner per idea). The Canvas situation is closer to
the Timeline one: an existing monolith to decompose, not a new framework to
invent.

---

## 0. The decision that frames everything: the canvas stays in `manifold-app`

`graph_canvas.rs` imports `manifold_renderer::node_graph::{GraphSnapshot, …}`
and `manifold_renderer::ui_renderer::UIRenderer` (`graph_canvas.rs:20-23`). The
dependency graph is `renderer → ui` (CLAUDE.md crate table; `manifold-ui`'s
`Cargo.toml` depends only on `manifold-core`). So the canvas **cannot** move into
`manifold-ui` without a cycle (`ui → renderer → ui`).

**Therefore Phase 4.2 is a split *in place* into a `graph_canvas/` directory
inside `manifold-app`** — exactly what Phase 3.6 did to `viewport.rs`. The
"named, reusable module" the overhaul (§5.4) asks for is a directory of
one-concern submodules under one `GraphCanvas` struct, not a crate move. Moving
the canvas to its own crate (or into `manifold-ui`) is gated on **Phase 5's
layering inversion** (UI-local events, the app maps to engine commands) and is
explicitly *out of scope here*. Phase 4 leaves the canvas behaviourally
identical; Phase 5 is the prerequisite for relocating it.

This is the one place the Canvas API genuinely differs from Chrome: Chrome lives
in `manifold-ui/src/chrome/`; the Canvas API lives app-side because it consumes
renderer snapshots. The overhaul's §11 ("three purpose-built APIs, don't unify
them") and §5.4 ("the canvas stays immediate-mode") both hold — we mirror the
*discipline* of the Chrome host (describe-once, one dispatch model), not its
UITree plumbing.

---

## 1. What the graph editor is made of today

| Concern | Type / fn | File | Notes |
|---|---|---|---|
| The monolith | `GraphCanvas` | `graph_canvas.rs` (4,262 lines) | One struct: nodes/wires, pan/zoom, drag, selection, scope, popover, pending actions — every concern in one `impl`. |
| View model | `NodeView` / `PortView` / `ParamView` / `WireView` / `PortHit` | `graph_canvas.rs:204-1008` | Owned view of the snapshot. `NodeView`'s geometry methods (`height`, `*_port_pos_graph`, `*_port_offset`) are the **shared geometry spine** read by layout, hit-test, and render alike. |
| Layout | `LayeredLayout` + `auto_layout` | `graph_canvas.rs:714-932, 1839-1974` | Sugiyama layered layout. `LayeredLayout` is **pure** (plain `Vec`s, no app coupling, 3 isolated tests); `auto_layout` is the adapter that reads `NodeView` geometry and writes `pos_graph`. |
| Projection / camera | `x_axis`/`y_axis`/`to_screen`/`to_graph`/`on_scroll`/`zoom_to_fit` | `graph_canvas.rs:1976-2003, 2680-2688, 1693-1723` | Already delegates to the shared `manifold_ui::transform::Axis` (Phase 1.3). Camera state is just `pan:(f32,f32)` + `zoom:f32`. |
| Hit-test | `node_under`/`header_under`/`param_row_under`/`port_under`/`chevron_under`/`wire_into`/`breadcrumb_hit`/`marquee_hits` | `graph_canvas.rs:2005-2236, 2316-2329, 3679-3691` | Pure-math picks in graph/screen space; no internal dispatch. |
| Render | `render` + `draw_node`/`draw_wire`/`draw_grid`/`draw_ghost_wire`/`draw_sparkline`/`draw_hover_tooltip`/`draw_debug_overlay` | `graph_canvas.rs:2692-3509` | Immediate-mode through `UIRenderer` rect+text. No UITree. |
| Drag SM | `enum DragMode` (None/Pan/WireFrom/NodeMove/ParamScrub/Marquee) | `graph_canvas.rs:942-999` | Advanced by 3 handlers (down/move/up); **read** by render (ghost wire, marquee) and by `apply_live_values` (scrub-skip). |
| Interaction / event entry | `on_left_button_down`/`_up`/`on_pointer_move`/`on_right_button_down`/`on_pan_button_*`/`on_scroll` + `request_*` | `graph_canvas.rs:2240-2688, 1180-1353` | The canvas exposes per-event methods the host calls directly; there is **no** internal `handle_event`. |
| Command emission | `pending_actions: Vec<PanelAction>` → `drain_actions` | `graph_canvas.rs:1034, 1170` | Mutations pushed onto a `Vec<PanelAction>` from the input + snapshot paths, drained each frame. |
| Right sidebar | `GraphEditorPanel` | `panels/graph_editor.rs` (3,110 lines) | UITree node-param inspector. Imperative `build()` + `RowState` id-tracking; **three** click entry points. |
| Left "Atoms" sidebar | `GraphPalette` | `panels/graph_palette.rs` | **Dead.** Zero call sites; only its data struct `GraphPaletteAtom` is still used (it feeds the node-spawn popup). |
| Editor input | `is_graph_editor` branches | `app.rs:2098 + 20 sites` | A second dispatch *policy* inlined across `window_event`'s arms — raw winit → `GraphCanvas` imperative calls, ~700 lines, early-`return` per arm. |

### The six problems Phase 4 removes (named precisely)

1. **One 4,262-line app-side file** mixes projection, layout, hit-test, render,
   drag, popover, breadcrumbs, and command emission. Render and hit-test
   independently reproduce row/port geometry — a drift class the file avoids
   only because both halves live in one struct (`graph_canvas.rs` passim).
2. **`LayeredLayout` is buried** in the monolith (`:764`) though it is a pure,
   self-contained, already-unit-tested algorithm that belongs in its own module.
3. **Graph edits ride the `PanelAction` god-enum** — ~18 graph-mutation variants
   (`mod.rs:442-691`) carrying `manifold_core` payloads (`SerializedParamValue`,
   `ParamConvert`, `NodeId`) sit in a 281-variant union, no-op'd in
   `ui_bridge::dispatch` (`ui_bridge/mod.rs:390-427`) and actually handled by an
   Application-level intercept in `app_render.rs` (~1108-1850). The graph surface
   has no command vocabulary of its own.
4. **The sidebar carries three dispatch paths** — `GraphEditorPanel` exposes
   `handle_event` (live, `graph_editor.rs:1376`), `dispatch_clicks` (dead,
   `:1739`), and `handle_click` (tests-only, `:1398`), all bottoming out in one
   `handle_click_event` row-loop id-match. The left-lane `editor_card` in the
   *same window* is already on the `IntentRegistry`; the sidebar is the lone
   id-matching holdout. And `GraphPalette` is entirely dead surface.
5. **A second event loop** — the `is_graph_editor` fork (`app.rs:2098`) routes
   the editor window's input through ~700 lines of inlined branches that bypass
   `InputHandler`/`UIRoot::process_events`. Keyboard is the worst drift:
   text-editing logic is triplicated and same chords (Cmd+G, Cmd+Z) have separate
   editor handlers kept apart only by the fork's early-return.
6. **No build-time legibility** — nothing flags a canvas gesture wired to
   nothing; the sidebar's dead controls are found on stage, not at build.

---

## 2. Target shape

One `GraphCanvas` struct, its `impl` split across one-concern sibling modules
(Rust allows `impl` for one type across sibling modules of a parent). No new
shared state, no `Arc<Mutex>`; the canvas is still fed `GraphSnapshot`s and still
emits commands drained each frame.

```
crates/manifold-app/src/graph_canvas/        (4.2 — the monolith, split)
├── mod.rs          GraphCanvas: fields, new(), public API, Rect, re-exports
├── model.rs        NodeView/PortView/ParamView/WireView/PortHit + geometry +
│                   snapshot ingestion (set_snapshot/apply_live_values) +
│                   the format/summary/scope free fns
├── layout.rs       LayeredLayout + layout_median/layout_resolve_overlaps +
│                   intrinsic constants + auto_layout adapter   (4.4)
├── camera.rs       x_axis/y_axis/to_screen/to_graph + on_scroll zoom +
│                   zoom_to_fit/focus/pan — projection over `Axis`
├── hit.rs          node_under/header_under/param_row_under/port_under/
│                   chevron_under/wire_into/breadcrumb_hit/marquee_hits
├── render.rs       render + every draw_*
└── interaction.rs  DragMode + on_*_button_*/on_pointer_move + selection +
                    scope nav (enter/exit/breadcrumb) + request_* emitters
```

`GraphEditCommand` (4.3) is the canvas surface's own command vocabulary; it lives
in `manifold-ui` (so both the app-side canvas and the `manifold-ui` sidebar can
emit it) — see §3.3.

The three ideas, each with exactly one owner:
- **4.2/4.4 — one geometry source.** `NodeView` (in `model.rs`) is the single
  geometry authority; layout, hit, and render all import it. `LayeredLayout` is
  its own pure module.
- **4.3 — one command vocabulary.** Graph edits leave the `PanelAction` god-enum
  for a focused `GraphEditCommand`; the app resolves target+scope and maps to
  `commands::graph::*` at the boundary, exactly as today.
- **4.5/4.6 — one dispatch model.** The sidebar's discrete clicks resolve through
  the `IntentRegistry` (like the editor_card already does); the editor window's
  input flows through one `GraphEditorInput` chain instead of the inlined fork.

---

## 3. Task-by-task

### 4.2 — Split the monolith into `graph_canvas/` *(do first)*

Mechanical extraction, behaviour-preserving. `GraphCanvas` stays one struct; its
`impl` blocks split across the files above. Free types (`NodeView` etc.) and free
functions move with their concern; `mod.rs` re-exports the public surface
(`Rect`, `resolve_level`, `resolve_card_param_node_id`, `node_preview_target`,
`GraphCanvas`) so every external `use crate::graph_canvas::…` path is unchanged.

The risk the map flagged: render and hit-test reproduce the same row/port
geometry by convention. The split does **not** make this worse (both still call
the same `NodeView` methods), but to keep it from drifting across files, the
shared geometry stays as `NodeView` methods in `model.rs` — no geometry math is
copied into `render.rs` or `hit.rs` that isn't already a `NodeView` method today.
Where `draw_node` and `param_row_under` each open-code `header_h + preview_h +
i*row_h`, that is preserved verbatim (a behaviour-preserving move, not a
unification — unifying it is a follow-up, noted in §4).

The test module (`graph_canvas.rs:3713-4262`) reaches into private fields
(`canvas.nodes`, `canvas.drag_mode`, `select_single`, `is_double_click`, the
`LayeredLayout` field-literals). Tests move alongside their target module, or the
fields gain `pub(crate)`/`pub(super)` visibility so the test module can reach
them. `git` shows moves, not rewrites.

*Done when:* `graph_canvas.rs` is a directory; each file owns one concern;
`cargo test -p manifold-app` green; `cargo build -p manifold-app` green; the
public `use` paths are unchanged.

### 4.4 — `LayeredLayout` as a module of the framework

Falls out of 4.2: `LayeredLayout` + `layout_median` + `layout_resolve_overlaps` +
the intrinsic constants (`LAYOUT_VGAP`/`LAYOUT_DUMMY_H`/`LAYOUT_ORDER_ITERS`/
`LAYOUT_COORD_ITERS`) + its 3 unit tests move into `graph_canvas/layout.rs`. The
`auto_layout` adapter (which reads `NodeView` geometry and writes `pos_graph`)
moves there too; `COL_SPACING`/`LAYOUT_ORIGIN` (caller constants) travel with the
adapter. The algorithm stays app-free — `NodeView` geometry is read by the
adapter, never absorbed into `LayeredLayout`.

*Done when:* `LayeredLayout` and its tests live in `layout.rs`; the layout tests
are green; nothing outside `layout.rs` references `LayeredLayout`.

### 4.3 — `GraphEditCommand`: the graph surface's own command type

Today every graph mutation is a `PanelAction` variant (`mod.rs:442-691`),
no-op'd in `ui_bridge::dispatch` and handled by an app-side intercept that
resolves `watched_graph_target` + `watched_catalog_default` (+ scope) and builds
a `manifold_editing::commands::graph::*` command. The variant is intentionally
**context-free** — the app injects identity at the boundary.

Introduce `enum GraphEditCommand` in a new `manifold-ui/src/graph_edit.rs`. It
owns the graph-mutation set (the variants whose handlers build a
`commands::graph::*`):

- `AddGraphNode`, `OpenNodePicker`, `AddGraphNodeAt`
- `ConnectPorts`, `DisconnectPorts`, `RemoveGraphNode`
- `RevertEffectGraph`, `MoveGraphNode`, `RelayoutGraph`
- `SetGraphNodeParam`, `BrowseGraphNodePath`, `EditGraphNodeStringParam`,
  `EditGraphNodeWgsl`, `EditGraphNodeTableCell`
- `GroupSelection`, `Ungroup`, `SetGroupTint`
- `ToggleNodeParamExpose`, `SetNodePreviewNormalize`

Payloads are unchanged (all already `manifold_core` types — `SerializedParamValue`,
`ParamConvert`, `NodeId` — which `manifold-ui` already depends on). The
"layering smell" the overhaul names is fixed not by changing the payloads but by
moving them off the 281-variant god-enum onto a focused 19-variant command type
that *is the graph surface's vocabulary*. This is also a down-payment on Phase 5:
`GraphEditCommand` is a UI-local command the app maps to engine commands.

**Deliberately scoped OUT** (recorded so the next reader doesn't redo it):
- **`EffectMapping*` (12 variants, `mod.rs:483-546`)** — constructed in
  `mapping_popover.rs`, they edit a `UserParamBinding` via
  `EditUserParamBindingCommand`, a *different* command family (binding mapping,
  not graph topology). They live on the graph-editor surface but are not graph
  edits; folding them in would conflate two families. They stay in `PanelAction`.
- **`OpenGraphEditor` / `OpenGeneratorGraphEditor` / `OpenCardMapping`** — window-
  open *intents* constructed from `param_card` in the **main** window, not the
  canvas. Not mutations. They stay in `PanelAction`.

Mechanics:
1. `graph_edit.rs` defines `GraphEditCommand` (move the variant bodies verbatim).
2. The canvas's `pending_actions: Vec<PanelAction>` → `Vec<GraphEditCommand>`;
   `drain_actions` → `drain_edits() -> Vec<GraphEditCommand>`. The `request_*`
   emitters and the inline `on_*` emit sites switch to the new type.
   (`SetGroupTint`'s same-frame coalesce in the pending vec is preserved.)
3. The sidebar's emit sites (`graph_editor.rs:1468/1486/…`) switch to
   `GraphEditCommand`.
4. The ~18 variants and the `GraphParamTarget`-free graph block leave
   `PanelAction`; the no-op `|`-arm in `ui_bridge/mod.rs:390-427` drops them.
5. `app_render.rs`: a `for cmd in graph_edits { … }` translation that mirrors the
   existing per-variant arms 1:1 (same `watched_graph_target`/scope resolution,
   same `commands::graph::*`). The `MappingPopover::drain_actions` path is
   unaffected (it still emits `PanelAction::EffectMapping*`).

The `u32` (canvas runtime id) vs `core::NodeId` (expose addressing) split is
**preserved as-is** — reconciling the two identity spaces is a deeper change than
Phase 4 and is noted as a wart in §4, not fixed here.

*Done when:* no graph-mutation variant remains in `PanelAction`; the canvas and
sidebar emit `GraphEditCommand`; `app_render` translates it to the same
`commands::graph::*` as before; `cargo test -p manifold-ui -p manifold-app` green
(in-crate `matches!(PanelAction::…)` tests updated to the new type).

### 4.5 — Sidebar on one dispatch model

Two moves:

**(a) Delete the dead `GraphPalette` panel.** Zero call sites
(`graph_palette.rs` `build`/`handle_click`/`dispatch_clicks` are unreferenced);
keep the `GraphPaletteAtom` data struct (used by the spawn popup). Its tests go
with it.

**(b) Collapse `GraphEditorPanel` to one dispatch model.** The runtime drives
`handle_event`; `dispatch_clicks` is dead and `handle_click` is tests-only — the
"three competing dispatch paths" the audit named. Collapse to **one**:
`handle_event` is the sole discrete-input path (`Click` → row → `GraphEditCommand`;
`DragBegin`/`Drag`/`DragEnd` → value-cell scrub). Delete `dispatch_clicks` and
`handle_click`; the click tests drive `handle_event` via a `click()` test helper
that wraps a `UIEvent::Click` (the real runtime path), the drag tests already do.

**Why `handle_event` and not `register_intents` (a scoping decision made during
implementation):** the design above aimed to put the sidebar's discrete clicks on
the `IntentRegistry` like the editor_card. But `IntentRegistry`/`NodeIntent` are
hardcoded to carry **`PanelAction`** (`intent.rs:30,112`), and Phase 4.3 made the
sidebar emit **`GraphEditCommand`**. Registering its clicks as intents would
require generalising the registry to carry both action types (or a wrapper enum)
— a crate-wide change touching every panel, larger than 4.5's "one dispatch
model" warrants, and the registry's headline benefit (parent-chain fold-up) is
marginal for the sidebar's *flat* rows (no nested inert children to fold through).
So 4.5 delivers the stated goal — the three id-matching entry points collapse to
one — without the registry generalisation. Folding the sidebar onto a
`GraphEditCommand`-aware registry is left to whenever the registry is generalised
(naturally part of Phase 5's UI-local-event work). This mirrors
`TIMELINE_API_DESIGN` §3.3's "not reusing `trim.rs`" call: take the correct,
scoped step, record why the larger one waited.

> **RESOLVED by Phase 6 (2026-06-23).** The registry is now generic —
> `IntentRegistry<A>` / `NodeIntent<A>`, default `A = PanelAction` — so the
> sidebar folds onto its own `IntentRegistry<GraphEditCommand>` after all.
> `handle_click_event` became `GraphEditorPanel::register_intents`; `handle_event`
> keeps only the stateful drag; the app resolves sidebar clicks through a new
> `editor_sidebar_intents` registry (mirroring `editor_card_intents`). The
> "crate-wide change touching every panel" never materialised — the default type
> param means the chrome panels + `ui_root` compile untouched. See
> `UI_ARCHITECTURE_OVERHAUL.md` §13 Phase 6.

**Why not a full declarative `view()` rewrite of the 3,110-line `build()`:** the
Chrome API's headline win is killing the `build()`/`update()` dual-write — but
`GraphEditorPanel` has no `update()`; it full-rebuilds each frame, so there is no
dual-write to kill. Its value cells carry stateful drag (scrub, per-channel
Color/Vec sliders) and inline String/Table/WGSL editors — the exact
"widget-imperative" surface Phase 2b kept imperative *by design*, with a runtime
visual pass as the verification boundary (which isn't available headlessly). So
4.5's deliverable for this panel is the **one-dispatch-model** half — headlessly
verifiable, and it removes the actual foot-gun (three id-matching paths).
Converting the imperative `build()` body to a `View` tree (and onto a generalised
intent registry) is tracked as the runtime-visual-pass + Phase-5 follow-up.

*Done when:* `GraphPalette` is deleted; `GraphEditorPanel` has one discrete-input
path (`handle_event`); `dispatch_clicks`/`handle_click` are gone;
`cargo test -p manifold-ui -p manifold-app` green.

### 4.6 — Fold the editor event loop into the shared path

The "second loop" is the `is_graph_editor` fork (`app.rs:2098`, 20 sites): every
`window_event` arm is `if is_graph_editor { <hand-rolled> return; }`. The output
side is already shared (`drain_edits` feeds the same dispatch). The editor is
**half-forked** — its palette/sidebar/node-picker chrome already routes through
`ed.ui_root.input.process_pointer`; only the centre canvas column bypasses it.

The canvas is a stateful immediate-mode widget and the overhaul keeps it that way
(§5.4) — so we do **not** fold it into the UITree (the "deeper one-substrate fix"
is explicitly not Phase 4). Instead we mirror the timeline's structure: an
`InputHandler` + host-trait pair already serves the primary window
(`input_handler.rs:59` + `TimelineInputHost`/`AppInputHost`). Introduce the graph
analog so the editor window runs **one** routing chain, not inlined branches:

- A `GraphEditorInput` owner (app-side, in `graph_canvas/` or a sibling
  `editor_input.rs`) with `pointer(...)`, `scroll(...)`, `key(...)` entry points
  that consume the same `winit`-derived events the primary path does. The
  per-arm `is_graph_editor { … }` bodies collapse to a single delegation
  (`if is_graph_editor { return self.editor_input_event(...); }`) at the top of
  the pointer/scroll/keyboard arms — the inlined ~700 lines move into the owner,
  named and single-entry.
- Keyboard is the priority (the worst drift): the editor's shortcuts +
  text-field editing route through one handler instead of the triplicated inline
  branches. Context-scoped chords (Cmd+G = group-nodes here vs group-layers in
  the timeline; Cmd+Z) stay correct because dispatch is by focused window — the
  same guarantee the `is_graph_editor` early-return gave, now expressed as one
  branch into one handler rather than 20 scattered ones.
- The viewport-slice math (`palette_width`/`sidebar_x`) and the explicit
  `offscreen_dirty = true` marking are preserved inside the owner (the editor has
  no idle repaint loop — losing the dirty mark freezes it).

This is the achievable, behaviour-preserving "fold into the shared path": one
named input owner per window, the fork reduced to a single dispatch branch, the
duplicated keyboard/text logic consolidated. Routing the editor through a
literal shared `process_events` (one function for both windows) is the deeper
move that belongs with Phase 5's substrate unification.

*Done when:* the `is_graph_editor` per-arm hand-rolled bodies are gone from
`window_event` (collapsed to one delegation each into `GraphEditorInput`); the
editor's keyboard/pointer/scroll run through that one owner; no shortcut works in
only one window by accident; `cargo build -p manifold-app` + `cargo test -p
manifold-app` green.

---

## 4. Invariants this must not break

- **No behaviour change.** Phase 4 is structural. The canvas looks and acts
  identically; the existing tests are the safety net, new tests pin the specific
  duplications removed.
- **The shared geometry spine.** `NodeView` geometry stays the single source for
  layout + hit + render; no geometry math is copied across the split.
- **`Axis` is shared, not forked.** The canvas keeps delegating projection to
  `manifold_ui::transform::Axis` (Phase 1.3) — no second affine type.
- **The optimistic-echo loop + `data_version`** are untouched; Phase 4 is
  input/UI-side only. The canvas still emits context-free commands; the app
  injects `watched_graph_target` + scope at the boundary.
- **No new `Arc<Mutex>` / shared state.** The content thread still owns the
  `Project`; the canvas still consumes `GraphSnapshot` and emits commands.
- **Context-scoped shortcuts.** Same-chord-different-meaning (Cmd+G, Cmd+Z) must
  remain dispatched by focused window after 4.6.

### Known warts left in place (deliberately, scoped out)

- **`u32` vs `core::NodeId` identity split** across the graph-edit commands —
  reconciling the two id spaces is deeper than Phase 4.
- **`render`/`hit` row-geometry duplication** — preserved verbatim by the split;
  unifying the two open-codings into one `NodeView` method is a follow-up.
- **`GraphEditorPanel`'s imperative `build()`** — stays imperative (drag-bearing,
  needs a runtime visual pass); the declarative `view()` rewrite is the Phase-2b-
  style follow-up.

## 5. Order & verification

`4.2 → 4.4 → 4.3 → 4.5 → 4.6`, one commit each, each pushed.

- **4.2/4.4** first — isolated to `graph_canvas.rs`, lowest blast radius,
  de-risks everything by giving each concern a home.
- **4.3** next — the command-type extraction touches `PanelAction`, the canvas,
  the sidebar, `app_render`, and `ui_bridge`; doing it after the split means the
  canvas emit sites are already organised in `interaction.rs`.
- **4.5** then **4.6** — the dispatch unifications, sidebar before the loop so the
  sidebar already speaks `GraphEditCommand` + intents when the loop folds.

Per step: `cargo test -p manifold-app` and/or `cargo test -p manifold-ui` (the
moved/rewritten tests), `cargo clippy -p manifold-ui -p manifold-app -- -D
warnings`, `cargo build -p manifold-app`. The full picture is
behaviour-preserving, so the existing tests gate the moves and new tests pin each
removed duplication.
