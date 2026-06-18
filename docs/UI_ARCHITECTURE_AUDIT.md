# UI Architecture Audit — manifold-ui + app-side wiring

Status: audit only (2026-06-18). Read of the whole `manifold-ui` crate (~33k
lines, 50 files) plus the app-side `ui_root` orchestration and `ui_bridge`
dispatch. No code changed. This is the grounding for any future UI API upgrade —
what we have, what's solid, where the rushed seams are, and a ranked plan.

## How the UI is built (the layers, bottom to top)

1. **Tree** — `tree.rs` / `node.rs`. Flat SoA store, `id == array index`,
   parent/child/sibling index arrays. Dirty flags per node, `has_dirty`, and a
   `structure_version` counter bumped only on structural ops. Partial rebuild via
   `truncate_from(panel_boundary)`. This is the spine and it's clean.
2. **Top-level layout** — `layout.rs` (`ScreenLayout`). Single source of truth
   for screen regions (transport / inspector / timeline / footer / split). Input
   fields in, computed `Rect`s out. Tested. Good.
3. **Intra-panel layout** — *none.* `widget_layout.rs` and `inspector_layout.rs`
   are flat **constant tables** (ported from Unity). Every panel positions its
   children with hand-written `x/y/w/h` arithmetic. This is the weak seam (see
   Gap 1).
4. **Input** — `input.rs` (`UIInputSystem`). Pointer state machine: hover, press,
   focus, 4px drag threshold, position-based double-click that survives tree
   rebuilds. Emits `UIEvent`s keyed by hit-tested `node_id`. Solid.
5. **Dispatch (new)** — `intent.rs` (`IntentRegistry`). Node→action map with
   parent-chain fold-up; the position-robust replacement for `event.node_id ==
   self.some_id` matching. Right-click fully migrated; click pending (see
   `NODE_INTENT_DISPATCH.md`).
6. **Panels** — `panels/*.rs`. 9 impl the `Panel` trait
   (`build`/`update`/`handle_event`/`register_intents`); the inspector
   sub-components (param cards, chrome, macros) are driven by
   `InspectorCompositePanel` rather than being panels themselves.
7. **Orchestration** — `app/ui_root.rs` (`UIRoot`, one per window). Owns the
   tree, input, layout, all panels, and the overlay driver. `process_events` is
   the central pump: drain input → overlays first → intent resolve → per-panel
   `handle_event` → drag routing → dropdown-open interception.
8. **Action dispatch** — `app/ui_bridge/`. `PanelAction` (a ~250-variant enum) is
   fanned by a top-level `match` in `mod.rs` to category routers
   (transport/editing/inspector/layer/project/marker).
9. **Render** — retained tree → dirty-gated incremental bitmap raster
   (`UICacheManager`, per-panel `sub_regions`). Not re-rasterized per frame.

## The full loop — click → action → model → pixel (the keystone)

This is the thing that ties the whole system together, and it's a **two-copy
optimistic-echo model**. The UI thread holds its own `local_project`; the content
thread owns the authoritative `Project`. One frame (`tick_and_render`):

1. **Drain content→UI.** `state_rx.try_recv()` pulls `ContentState` snapshots.
   Three kinds: a full `project_snapshot` (only deep-cloned when `data_version`
   changed — `Arc::ptr_eq` skips the clone otherwise), a lightweight
   `modulation_snapshot` (per-frame param values, no clone), and a
   `graph_snapshot` (→ canvas). Snapshots are **suppressed during drags** and the
   **actively-dragged field is restored** so an echo can't fight the user's hand.
2. **Collect input → actions.** `ui_root.process_events()` → `Vec<PanelAction>`
   (the forward path: hit-test → overlay/intent/panel).
3. **Dispatch each action.** `ui_bridge::dispatch(...)` does **two things at once**:
   mutates `local_project` immediately (instant on-screen feedback, no round-trip
   latency) **and** sends a `ContentCommand` to the content thread, which is the
   real authority. Returns `DispatchResult { structural_change, … }`.
4. **Project → panels.** On structural / selection / active-layer change,
   `sync_project_data` + `sync_inspector_data` read `local_project` and rebuild
   panel state structs (`ParamCardConfig`, viewport clips, …). `push_state` syncs
   live slider values every frame. This is `ui_bridge/state_sync.rs`.
5. **Rebuild + render.** `needs_rebuild` → `rebuild_scroll_panels`; `panel.update()`
   pushes changes into the tree; dirty-gated bitmap raster draws it.

Meanwhile the **content thread** runs independently: `ContentCommand` →
`EditingService` → `Command` → `UndoRedoManager` → mutates the authoritative
`Project` → emits `ContentState` snapshots back. So edits apply optimistically on
the UI side and are reconciled by the echo; playback/modulation flow the other way
as snapshots. The drag-suppression + dragged-field-restore logic is what keeps the
two copies from flickering against each other during live manipulation.

**Why this matters for the audit:** the local-copy + echo model is *the*
architecture. It's why dispatch both mutates and sends, why there's a whole
`state_sync` projection layer, and why snapshots are drag-aware. It's well-built
and not on the gap list — but nothing about the UI makes sense without it, and the
first pass of this audit completely omitted it.

## Rendering models — there are THREE, not one (audit correction)

The first pass treated rendering as one path. It isn't. The visible UI is
produced by three distinct renderers, and the split is deliberate and correct:

1. **Chrome → UITree nodes → GPU.** Panels, buttons, sliders, labels. Glyphs are
   rasterized by the renderer crate against a font atlas; `manifold-ui` stays
   backend-agnostic via the `TextMeasure` trait (`text.rs`). This is the path the
   intent registry, layout, and the whole API audit above are about.
2. **Timeline clips → CPU pixel buffers → per-layer textures.**
   `bitmap_renderer.rs` + `bitmap_painter.rs`. Each layer track owns one
   `Vec<Color32>` buffer; all its clips are painted as rectangles into it
   (`fill_rect`/`draw_clip`), then uploaded as one texture. Clips are **not**
   UITree nodes. A 6-condition dirty check gates repaints.
3. **Waveforms → CPU pixel buffers → per-lane textures.**
   `waveform_renderer.rs` (a max-pooled MIP chain with spectral coloring) +
   `waveform_painter.rs`. Zoom selects a MIP level; bars are painted directly.

**Why this is right, not rushed:** a real show is ~2,928 clips
(`typical-project-scale`). Making each clip a UITree node would explode the tree
and the O(n) hit-test. Per-layer CPU rasterization is the correct call for that
scale, and waveforms (per-sample bars) likewise. These paths are well-tested,
dirty-gated, and allocation-free — they are some of the **best** code in the
crate, not part of the "rushed" surface.

**What it means for the rest of the audit:**
- The "positional, not in the tree" surfaces are a deliberate *category*, not an
  oversight. Clips and waveforms are intentionally outside the UITree (and so
  outside the intent registry — forever). Markers are the only positional surface
  that *could* reasonably become nodes (there are few of them).
- Any declarative / Figma reuse (below) applies to **chrome only** — path 1.
  Paths 2 and 3 are bespoke performance code no design tool will ever generate.
  That's a meaningful fraction of the on-screen pixels (the whole timeline body)
  that stays hand-written by design.

## Interaction models — there are FIVE ways a gesture gets handled

Same story as rendering: there's no single input path. Five distinct models,
each correct for its surface, none sharing dispatch with the others:

1. **Chrome** — UITree `hit_test` → `UIEvent` → intent registry / panel
   `handle_event`. (The whole API audit above.)
2. **Timeline clips** — `interaction_overlay.rs`: one transparent overlay over
   the tracks area owns all clip click/hover/drag/trim/box-select. It hit-tests
   via the stateless pure-math `clip_hit_tester.rs` (beat,Y → clip + Body/Trim
   region) against the shared `coordinate_mapper.rs`, and reaches the engine
   through the `TimelineEditingHost` trait. No tree, no intent registry.
3. **Graph canvas** — its own `node_under` hit-test + `DragMode` (editor window).
4. **Waveform / stem lanes** — hybrid: UITree button nodes for the controls, but
   rect-containment routing in `ui_root` for scrub/drag.
5. **Markers** — positional flag-rect scan in the viewport (painted, not nodes).

This is the input-side mirror of the four rendering models, and it's the real
reason "unify the UI" can't mean one path: the timeline and canvas have genuinely
different needs (thousands of clips, beat/pixel coordinate space, infinite
pan/zoom) and correctly use bespoke interaction. `CoordinateMapper` is a good
shared seam — one source for beat↔pixel and layer-Y, used by viewport, headers,
and hit-tester alike.

## Design tokens already exist — they're just Rust, not JSON (`color.rs`)

`color.rs` is effectively a design-token file: a 6-level elevation palette,
semantic accents/status colors, a spacing scale (`SPACE_XS/S/M/L`), corner radii,
a semantic font-size scale (`FONT_CAPTION`…`FONT_TITLE`), zoom levels, and layout
dimensions. This is good news for the Figma idea — the tokens exist and are
mostly named semantically.

The gap for a token pipeline: it's **flat**. Primitive tokens (the raw palette),
semantic tokens (`STATUS_GOOD`), and component tokens (`HEADER_BUTTON_HOVER`,
`GEN_CARD_INNER_BG_C32`) are all mixed in one file, and a lot of per-panel colors
are one-offs. To round-trip with Figma/Claude Design you'd split it into
primitive → semantic → component tiers (the standard token hierarchy). That's a
reorganization, not new infrastructure — and it's the concrete first step of "use
design tokens from a design tool."

Also confirmed: chrome **text is real CoreText shaping** → R8 grayscale atlas →
GPU (`manifold-renderer::text_rasterizer`), exposed to panels via the
`TextMeasure` trait. Not a hand-rolled bitmap font — proper macOS font rendering.

## The graph editor — a second UI framework (audit extension)

The node-graph editor is not "another panel." It's a parallel UI stack that
shares almost nothing with the main one except the low-level draw primitives and
`PanelAction`. It runs in its own window with its own `UIRoot`/event loop, and
splits into three parts in **two different rendering styles**:

- **Inspector sidebar** (`graph_editor.rs`, manifold-ui) — UITree-based. Param
  rows, value cells, vec/string/table editors. Heavily unit-tested.
- **Palette** (`graph_palette.rs`) — UITree-based, small.
- **Canvas** (`graph_canvas.rs`, app-side, 4,252 lines) — **immediate-mode, no
  UITree at all** ("Rendering goes through UIRenderer rect+text primitives"). It
  carries its own coordinate system (graph↔screen pan/zoom), its own hit-testing
  (`node_under` / `header_under` / `param_row_under`), its own drag state machine
  (`DragMode`), its own popover/breadcrumbs/group navigation, and — notably — its
  own **layout engine** (`LayeredLayout`, with wire-crossing minimization).

**This brings the count to four rendering models:** UITree chrome, CPU clip
textures, CPU waveform textures, and the immediate-mode canvas.

**Two things this tells us:**
- **The canvas has a layout engine — but a different *kind*.** `LayeredLayout` is
  a Sugiyama layered-DAG layout (longest-path layering, virtual waypoints,
  crossing minimization, port-offset alignment). It's competent, real code — but
  it solves the *graph* layout problem, not the *panel* layout problem. The chrome
  needs flexbox-style row/column/stack. So Gap 1 can't be closed by lifting the
  canvas algorithm; the chrome needs its own engine. What the canvas proves is
  that the discipline and appetite for real layout infrastructure already exist
  here — not that the code is reusable. (Correcting an earlier note in this doc
  that implied it could be mined directly.)
- **The canvas should stay its own thing.** Infinite pan/zoom, wires, and
  thousands of potential nodes are genuinely different needs from a fixed panel.
  "Unify the UI API" has a hard, correct boundary here. The realistic goal is
  shared *primitives* (a drag controller, a layout core, typed ids), not one
  framework.

**The one genuinely clean seam:** the canvas is a *view* over the node runtime,
not an owner of it. It reads `GraphSnapshot` / `ParamSnapshot` / `LiveNodeParams`
from `manifold-renderer::node_graph` and emits mutations as `PanelAction`s
(`AddGraphNode`, `ConnectPorts`, `MoveGraphNode`, `SetGraphNodeParam`…). Model and
view are properly separated — the editor never mutates graph data directly. This
is the part to *not* touch.

> Scope note: this audits the editor *as UI*. The node **runtime** itself — the
> ~185 primitives, `EffectGraphDef`, the compositor — is a separate domain with
> its own docs (`NODE_GRAPH_SYSTEM.md`, `NODE_CATALOG.md`,
> `NODE_GROUPS_DESIGN.md`) and is out of scope here.

## What's genuinely solid — do not churn

- **The tree.** SoA, O(1) mutation, partial rebuild, structure-version gating.
  Well-documented invariants. No reason to touch it.
- **`ScreenLayout`.** The top-level layout is exactly right — declarative,
  single-source, tested.
- **`UIInputSystem`.** The gesture detection is correct and robust (the
  position-based double-click surviving rebuilds is a nice touch).
- **`SliderDragState`** (`slider.rs`). A real drag state machine that consolidated
  a whole bug class (cache-snapback, stuck-dragging, one-frame-lag). This is the
  *model* the rest of the drag code should follow.
- **The overlay driver** (`ui_root.rs`). One enumeration for build/draw/input with
  an exhaustive match — "built but never drawn" is unrepresentable. Recently
  shipped, good shape.
- **Render performance.** Incremental + dirty-gated. Performance is **not** the
  weak axis of this UI; don't invent work here.

## Gaps, ranked by payoff

### 1. No intra-panel layout engine — the root of "rushed"
Everything inside a panel is manual pixel math: `let x = ...; x += w + GAP;`,
repeated thousands of times. This is *why* `param_card.rs` is 3,300 lines,
`viewport.rs` 2,900, `layer_header.rs` 2,500. Every spacing change is hand-edited
arithmetic; alignment bugs are one fat-fingered offset away. A small layout
helper (row / column / stack / inset, computing child `Rect`s from a cursor)
would collapse enormous amounts of code and make panels readable. **Biggest
"easier + higher quality" lever, by a wide margin.**

### 1b. Text measurement exists but isn't plumbed into `build()`
`text.rs` defines a clean `TextMeasure` trait (and ellipsis truncation), but
`Panel::build(tree, layout)` gets no measurer. So panels can't size a cell to its
text at build time — they hard-code column widths and let text clip or truncate.
This is a direct blocker for Gap 1: a real layout engine needs measurement during
build to do "size to content" (which is exactly what Figma auto-layout assumes).
The fix is small and additive — thread `&dyn TextMeasure` into the build path —
but it has to happen *with* the layout engine, not after.

### 2. Raw `u32`/`i32` node ids with sentinels
Node ids are bare integers. "None" is `-1` in some places (`hit_test`, panel
fields) and `u32::MAX` in others (`process_right_click`). No type stops you from
passing a stale id, mixing an id with an index, or forgetting the sentinel check.
A `NodeId` newtype + `Option<NodeId>` makes "no node" unrepresentable and is
compiler-guided to adopt. **Biggest "safer" lever.**

### 3. Panels hoard node ids
Every panel stores dozens of `self.*_id` fields at build time and matches them in
`handle_event`. The intent registry removed this for *dispatch* (right-click), but
the builder ergonomics sketched in `NODE_INTENT_DISPATCH.md` (`IntentBuilder` —
create node + register intent in one call) was never built, so panels still stash
ids for everything else. Gaps 1–3 are the same smell from three angles: panels do
too much bookkeeping by hand.

### 4. The `dispatch()` function takes 18 positional arguments
`ui_bridge::dispatch(action, project, content_tx, content_state, ui, selection,
active_layer, drag_snapshot, trim_snapshot, target_snapshot, decay_snapshot,
audio_shape_snapshot, audio_crossover_snapshot, user_prefs,
active_inspector_drag, editor_target)` — threaded through every category router.
Should be a `DispatchCtx<'a>` struct. Pure mechanical refactor, large readability
win, zero behavior change.

### 5. Drag lives in FIVE separate state machines
`SliderDragState` is excellent but covers only sliders. Counting the whole
codebase, drag/trim state is tracked in at least five independent places:
`SliderDragState` (sliders), per-panel `dragging: bool` fields (card/layer
reorder, chrome, gain), `UIState` (`is_dragging`/`is_trimming`/`is_scrubbing` for
clip move + trim), `InteractionOverlay::DragMode` (the timeline overlay), and the
graph canvas `DragMode`. Each reimplements grab→track→release. A generalized drag
controller (typed payload, the `SliderDragState` pattern) could unify at least
the chrome ones; the timeline and canvas ones are coupled to their interaction
models and may stay separate. **Biggest "reliable" lever for the stateful half —
show-critical, so its own careful pass.**

### 6. Click dispatch still scattered
Right-click is unified on the intent registry; left-click is not. Fully
documented as groups A–E in `NODE_INTENT_DISPATCH.md`. Consistency, not
correctness (click never had the dead-zone bug).

### 7. Two parallel `ui_root` event loops
The main window runs `UIRoot::process_events`; the graph-editor window runs a
hand-rolled loop in `app_render.rs` with its own `editor_card_intents`. They have
diverged before (the editor was the last right-click holdout). Risk of ongoing
drift; a shared "resolve gestures → actions" core would close it.

### 8. `PanelAction` couples UI to core types
The 250-variant enum is fine for dispatch (the categorized router handles it),
but it pulls `ParamId`, `AudioSendId`, `NodeId`, `AbletonMacroAddress`, etc.
directly into `manifold-ui`. Works today; only worth revisiting if the UI ever
needs to stand alone. Lowest priority.

## Improvement plan — tiers by payoff ÷ risk

**Tier 1 — high payoff, low risk, mechanical. Do first.**
- **Layout helper** (Gap 1). Additive; adopt per-panel, no behavior change.
  Start with `param_card` (worst offender) as the proof.
- **`NodeId` newtype** (Gap 2). Compiler-guided migration; sentinels become
  `Option<NodeId>`.
- **`DispatchCtx` struct** (Gap 4). Kills the 18-arg signature in one refactor.

**Tier 2 — real payoff, medium risk. Deliberate passes.**
- **Drag controller** (Gap 5). Generalize `SliderDragState`. Show-critical —
  its own session with tests, not a bolt-on.
- **Finish click migration + builder ergonomics** (Gaps 3, 6). Completes "one
  dispatch path," removes the id-hoarding. Best done opportunistically while in
  each panel for the layout work.

**Tier 3 — lower priority.**
- **Unify the two event loops** (Gap 7).
- **Reconsider `PanelAction`/core coupling** (Gap 8) only if it ever matters.

## Audit coverage — what's actually been read

So this audit's limits are explicit, not implied:

- **Read in full:** `tree`, `node`, `layout`, `widget_layout`, `inspector_layout`,
  `input`, `intent`, `panels/mod` (Panel trait + `PanelAction`), `slider`,
  `scroll_container`, `text`, `ui_state`, `color`, `bitmap_painter`,
  `waveform_painter`, and the structure of `ui_root` + `ui_bridge` dispatch.
- **Read enough to characterize (headers/structure, not every line):**
  `bitmap_renderer`, `waveform_renderer`, `interaction_overlay`,
  `clip_hit_tester`, `coordinate_mapper`, `graph_editor` (inspector sidebar),
  `graph_canvas`, and the renderer-side `text_rasterizer` / `ui_renderer`.
- **Verified in a final depth pass (read the bodies, looked for surprises):** the
  content thread's command loop + snapshot production (`content_thread.rs` —
  confirms the `data_version`-gated optimistic-echo model, symmetric with the UI
  side), `state_sync::push_state` (per-frame projection, optimistic-local BPM),
  the `LayeredLayout` algorithm (Sugiyama — *corrected* the claim it was reusable
  for chrome), and the `build()` bodies of `param_card` + `layer_header` (confirm
  the manual-layout + id-hoarding pattern; no new architecture).
- **Still not read line-by-line (no remaining surprise risk):** the per-card
  mapping bodies in the rest of `state_sync`, the full `viewport`/`inspector`
  bodies, the per-action `ui_bridge` handlers, and leaf modules (`snap`, `trim`,
  `cursor_nav`, `dropdown`, `drawer`, `timeline_*_host` traits).

The earlier passes of this audit missed whole *subsystems* (CPU rasterization, the
graph canvas, the timeline interaction overlay, the token system, the return-path
loop). Those are now read. The final depth pass produced exactly one correction
(LayeredLayout isn't reusable for chrome) and otherwise confirmed the model — which
is the signal that what remains unread is genuinely detail, not architecture.

**The one global key worth stating:** `EditingService::data_version` is the dirty
counter the entire snapshot system pivots on. Editing commands bump it; the content
thread deep-clones a project snapshot only when it changes (else a lightweight
modulation snapshot); the UI accepts/structural-syncs only when it changes. One
integer compare gates the whole reconcile loop.

## One honest caveat
None of this is broken. The right-click dead-zone bug (the thing that actually
hurt on stage) is already fixed. Everything above is paying down "rushed," not
patching a wound — so it's worth doing in the natural order of whatever panel
work comes up, not as an emergency. The exception is the layout helper: it's the
one change big enough to be worth pursuing on its own, because it shrinks every
future panel edit.
