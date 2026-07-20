# UI / GUI / Interaction — Architecture Overhaul (North-Star Reference)

Status: **authoritative plan** (2026-06-18). This is the reference point for
upgrading the entire GUI, UI, and interaction surface of MANIFOLD. It captures
the full audit of what exists, an unbiased critique of what's weak, the target
architecture, and the agent-authoring bar the new surface must meet.

Companion: [`UI_ARCHITECTURE_AUDIT.md`](archive/UI_ARCHITECTURE_AUDIT.md) holds the detailed
as-built findings (every subsystem, file-level). This document is the *plan* built
on top of that audit. Read the audit for "what is"; read this for "what should be
and how we get there."

---

## 0. CURRENT POSITION (read first, update last)

> **Status: ALL PHASES 0–8 COMPLETE (2026-06-23).** The overhaul checklist is
> done end-to-end; only the §10.2 deferred next-gen items remain, and those are
> intentionally unscheduled. **Phase 6** (the last follow-on) generalised
> `IntentRegistry<A>` / `NodeIntent<A>` off `PanelAction` (default `A =
> PanelAction`, so every chrome panel + `ui_root` compile unchanged) and rewired
> the graph-editor sidebar's click dispatch onto its own
> `IntentRegistry<GraphEditCommand>`: `GraphEditorPanel::handle_click_event`
> became `register_intents`, `handle_event` now carries only the stateful drag,
> and the app resolves sidebar clicks through a new `editor_sidebar_intents`
> registry (mirroring `editor_card_intents`). Behaviour-preserving; manifold-ui
> 370 + manifold-app 55 tests green, clippy `-D warnings` clean. The historical
> per-phase status below is kept as the running record.
>
> **Status: Phases 0–5 + 7 + 8 COMPLETE (2026-06-23); only Phase 6 remains (its
> own chat, scoped in §13).** Phase 2 (Chrome API), Phase 3 (Timeline API),
> Phase 4 (Canvas API), Phase 5 (layering inversion), **Phase 7 (one shared input
> owner)**, and **Phase 8 (relocate the graph canvas into `manifold-ui`)** all
> landed behaviour-preserving, tests + clippy green — see each phase's sub-design
> doc (`CHROME_API_DESIGN`, `TIMELINE_API_DESIGN`, `CANVAS_API_DESIGN`,
> `UI_LAYERING_INVERSION`) and the §13 checklist. **Phase 5** moved `manifold-ui`
> to UI-local events + view-data (the app maps them to engine commands via
> `ui_translate.rs`) so the crate compiles with no `manifold-core` dependency — it
> depends only on the new zero-dep `manifold-foundation`. That *unblocked* three
> deeper moves earlier phases deferred to it (Phases 6–8, each independent, its
> own chat, scoped in §13): generalise `IntentRegistry` off `PanelAction` (6), one
> shared input owner for the editor window (7), relocate the graph canvas out of
> `manifold-app` (8). **Phase 7 is now done** — `editor_input.rs` became
> `window_input.rs`, the single owner both windows route input through:
> `window_event`'s pointer/wheel/keyboard arms are one delegation each into
> `input_*` dispatchers, the ~614 inlined primary-window input lines moved beside
> the `editor_*` bodies, and the line-delta→pixel scroll rule + physical→logical
> cursor projection are now shared helpers (the canvas stays immediate-mode and
> the three text-input keyboard policies stay window-specific by design — see §13
> Phase 7). **Phase 8 is also done** — the 4,334-line `graph_canvas/` + its
> mapping popover moved into `manifold-ui` reading a UI-local `graph_view`
> snapshot (the app translates the renderer's via `ui_translate.rs`) and painting
> through a new `Painter` trait the renderer impls for `UIRenderer`; the canvas no
> longer depends on `manifold-renderer`. **Phase 6 is the one remaining
> follow-on.** The Phase-2b-era status below is kept as history.
>
> **Status: Phase 2b IN PROGRESS (2026-06-22)** — 7 panels fully migrated; **3 of
> the 4 heavyweights chrome-staged** (`param_card` frame + generator header,
> `layer_header` top chrome, `audio_setup` modal chrome); the `inspector`'s
> sub-panels are all migrated (it's the orchestrator). All verified + pushed (~20
> commits), plus the `slider_row` + `dropdown_trigger` building blocks. **Key
> result, proven on three beasts: the heavyweights stage into committable, tested
> steps (frame/chrome → header → body) — not all-or-nothing.** What remains is each
> beast's dynamic *body* — param_card's dragged rows + the effect-header badge fork,
> layer_header's scroll rows, audio_setup's meters/spectrogram, the inspector's
> interleaved section bgs + add buttons. All dragged/real-time surfaces that want
> the app running to verify. Branch `ui-chrome-phase2b`.
>
> **Typed building blocks (the direction Peter steered to 2026-06-22):** the
> repeated interactive widgets become *typed Chrome components the host
> materialises*, so panels compose them declaratively instead of hand-rolling
> imperative slots. **Shipped:** (1) `View::slider_row(SliderSpec).key(K)` — the
> `ChromeHost` builds the `BitmapSlider` into the laid slot (byte-identical) and
> exposes its ids via `ChromeHost::slider_ids(K)`; the panel's `SliderDragState`
> drives value+drag (host owns structure, panel owns value); master/layer/macros/
> clip compose it. (2) `dropdown_trigger_view(current, font)` (in
> `param_slider_shared`) — a typed `View` button, the declarative twin of the
> now-deleted `build_dropdown_trigger`; clip composes it (`.key().inert()`,
> resolved on click). **Next blocks (same shape):** progress-bar, Ableton /
> driver / envelope / audio drawers, trim handles — each still built imperatively
> into a keyed slot; promoting them is what makes param/audio-setup clean
> compositions.
>
> **Done + verified + pushed (6 panels + the slider block):**
> - **2b.8 footer**, **2b.7 header**, **2b.6 transport** — the static bars, each
>   rewritten on the Chrome API: `Panel::build` → `host.build(view, rect)`,
>   `Panel::update` → `host.update` (in-place reconcile, free when unchanged since
>   the tree setters dirty-check), `register_intents` → `host.register_intents`.
>   Value setters drop their `&mut UITree` arg and just store the field. No more
>   `self.*_id` hoarding, no `build()`/`sync_*()` dual write. Each carries a
>   `#[cfg(test)]` golden that reproduces the original pixel math and asserts every
>   interactive cell lands at the same rect — provably non-regressing at build.
> - **2b.2 master_chrome**, **2b.3 layer_chrome** — the first slider-bearing
>   inspector cards, on the **hybrid** pattern (below): host owns the card's
>   declarative chrome + `Fill` slider slots, the `BitmapSlider` drops into the
>   recovered slot byte-identical. Slot-golden asserts the slider lands at the old
>   rect. Public interface unchanged → the inspector composite is untouched.
> - **Chrome API extensions landed for the migration:** `View::key` →
>   `LaidNode.key` → `ChromeHost::node_id_for_key` (stable semantic addressing —
>   a panel resolves a specific element's tree id for overlay anchoring instead of
>   storing it); `View::disabled(bool)` (host applies/toggles `UIFlags::DISABLED`
>   in place, excluded from the structural signature).
>
> **Remaining 2b (the slider/drawer cards) — verification boundary:**
> `param_card`, `macros_panel`, `master_chrome`, `layer_chrome`, `clip_chrome`,
> `layer_header`, `inspector` (composite), `audio_setup_panel`. These are **not
> "largely mechanical"** the way the static bars were. Two reasons:
> 1. **Sliders aren't `View` nodes.** `BitmapSlider` is a 5-node widget whose
>    fill/thumb rects are computed *after* layout (from the resolved track width ×
>    value) and mutated *imperatively* during drag by `SliderDragState`. A `View`
>    is described *before* layout, so a slider can't be a pure View node. The
>    faithful path is a **hybrid**: the host builds the card's declarative chrome
>    (header, dividers, toggles, labels) and lays out a `Fill` "slider slot"; the
>    panel recovers the slot rect by key and builds the `BitmapSlider` into it
>    (byte-identical, zero slider risk). Reconcile-vs-drag must be gated so the
>    per-frame `host.update` never overwrites a live drag. (The alternative —
>    teaching the renderer to draw a single `Slider` node as a composite — is the
>    cleaner end-state but a live-render-path change.)
> 2. **What headless can't prove.** A build-time tree-equivalence golden proves the
>    *built* tree matches the old one. It cannot prove the *dynamic* paths these
>    cards live or die on — drag tracking while the card reconciles, drawer-open
>    timing, collapse rebuild ordering. Those need a **running build**, on the
>    central perform-mode inspector. This is the same "runtime visual pass" the
>    old 2b.0 note flagged for `param_card`, now understood to apply to the whole
>    slider/drawer family.
>
> **UPDATE 2026-06-22: all of the above is DONE** — every card + the inspector
> composite + `layer_header` + `audio_setup_panel` chrome + 2b.11 typed dropdowns
> are migrated, verified (golden byte-equivalence + focused tests), and pushed (35
> commits). The dragged/real-time widget bodies stay imperative by design (the
> chrome-declarative / widget-imperative split). The note below is kept as the
> historical rationale for why the heavyweights were staged rather than done blind;
> the running-build check still applies to anyone re-touching the live
> drag/drawer/meter paths. See the Phase-2b completion summary at the end of §13.
>
> **Phase 2a (Chrome API):** a declarative `chrome` module in `manifold-ui` — a panel
> describes its UI once as a `View` tree; a `ChromeHost` reconciler decides build-vs-update
> and emits minimal `UITree` mutations, removing the `build()`/`sync_*()` dual write.
> Three pure layers: `view` (builders + per-axis `Sizing` + intent-at-build + `validate`
> loud-fail), `layout` (pure mini-flexbox `solve`, headless-tested), `diff` (`ChromeHost`:
> in-place update when the structural signature matches → ids/intents survive; `NeedsRebuild`
> otherwise → app re-runs `build()`, mirroring the existing `truncate_from` model). 22 unit
> tests + a 6-case golden `param_card`-shape proof (`tests/chrome_param_card_proof.rs`):
> value change in-place, badge toggle in-place, drawer-open → NeedsRebuild → rebuild grows,
> intents fold up, validation catches an unwired control. Design + 2b contract:
> [`CHROME_API_DESIGN.md`](CHROME_API_DESIGN.md). **The live `param_card` rewrite-and-delete
> is deliberately 2b.1, not 2a.5** — it is the most interaction-dense panel and needs a
> runtime visual pass; 2a proved the API on its shape first.
>
> **Status: Phase 1 COMPLETE (2026-06-22)** — all of 1.1–1.6 landed.
>
> **Phase 0 decision:** production stays `panic = "abort"` ([`Cargo.toml`](../Cargo.toml)
> `[profile.release]`). In-process recovery (catch_unwind / respawn / watchdog) is
> therefore **off the table** — under abort any thread panic aborts the whole process
> and there is nothing to catch. Resilience is handled by **prevention**: the content
> tick must be tested to not panic. The unwind + catch_unwind recovery path (§7, old
> 0.2) is kept below as the deferred alternative if "keep abort for now" is revisited.
>
> **Workflow:** one chat per phase (Phase 2 splits into 2a + 2b). The chat is the
> worker; **the checklist in §13 is the memory.** Work design → build → test →
> commit, ticking §13 after each committable piece. Compaction is safe because
> progress lives in commits + ticked boxes, never only in chat context. At the end
> of every chat, update this CURRENT POSITION block: what's done, what's next.
>
> **Last committable step done:** 1.2 — generic `DragController<T>` ([`drag.rs`](../crates/manifold-ui/src/drag.rs)):
> one grab→track→release lifecycle with a typed payload + start/current positions, owning the
> lifecycle only (the delta→meaning mapping stays with the caller, because it differs per surface).
> `SliderDragState`'s `dragging: bool` was replaced by a `DragController<()>` as the proof — the
> slider is the degenerate consumer (no payload, absolute-position tracking), so it exercises the
> skeleton; the timeline/canvas wrappers will exercise the typed payload + delta. `SliderDragState`'s
> public API is unchanged, so the ~8 panel consumers are untouched. 298 `manifold-ui` lib tests green
> (+5 controller tests), clippy clean. The other four drag machines (per-panel bools, `UIState`
> timeline, `InteractionOverlay::DragMode`, canvas `DragMode`) still stand — folding them in is later
> work, not gated on this.
>
> **Prior:** 1.1 — `NodeId(u32)` + `Option<NodeId>` replaced every `i32`/`-1`, `u32::MAX`, and
> `usize::MAX` node-id sentinel across `manifold-ui` (foundation + all ~22 panels), `manifold-app`
> (`ui_root`, `app_render`), and `manifold-renderer` (`ui_renderer`). Scope ~20× the doc's
> "tree/input/intent" line because the tree API is the universal panel boundary, and it reached into
> `manifold-app`. Method: foundation by hand → 22-agent edit-only fan-out for the panels →
> hand-reconciled the cross-file/cross-crate seams.

---

## 1. Thesis (read this first)

The UI has **solid bones and a dated surface.** The foundations — the node tree,
the two-thread optimistic-echo loop, dirty-gating, the slider drag machine, the
overlay driver — are well-built and fast. But the **API you actually type
against** was *mechanically ported from Unity, not designed for Rust*. Nearly
every file says "Mechanical translation of…". It inherited Unity's imperative,
id-hoarding, manual-pixel-layout style and never got re-architected.

So this is **not a rewrite.** It's a surface redesign over good foundations. The
target:

- **Three purpose-built APIs** — chrome widgets, timeline, graph canvas — over
  **one shared substrate**. Not one framework swallowing everything; not one good
  API beside two bespoke piles.
- **Declarative and reactive**, so a panel is described once (build+update
  collapse) instead of hand-wired twice.
- **State-of-the-art for agent authoring**, because agents (this one and others)
  are first-class authors of this surface — it must be machine-legible,
  machine-verifiable, machine-addressable, and fail loud.

**Non-negotiable principle: full adoption is the destination.** Incremental
migration is the *path*, not an excuse to stop halfway. A half-converted codebase
(new API beside old) is worse than either. Every phase ends with the old code it
replaces **deleted**. "Leave the timeline on the old way forever" is not allowed.

Performance is **not** the weak axis and is not a goal of this work — it's already
handled (dirty-gating, MIP chains, Arc-skip-clone, per-frame scratch buffers).
Do not chase it.

---

## 2. Current architecture (condensed)

### Build layers
- **Tree** (`tree.rs`/`node.rs`) — flat SoA, `id == index`, parent/child/sibling
  arrays, per-node dirty flags, partial rebuild via `truncate_from`,
  `structure_version` counter. The spine. Clean.
- **Top-level layout** (`ScreenLayout`) — declarative computed rects for screen
  regions. Good.
- **Intra-panel layout** — *none.* Constant tables only; panels do manual
  `x/y/w/h` math. The rushed seam.
- **Input** (`UIInputSystem`) — hover/press/focus/drag/double-click off `hit_test`.
- **Dispatch** (`IntentRegistry`) — node→action with parent-chain fold-up.
  Right-click migrated; left-click still scattered.
- **Panels** — 9 impl `Panel`; inspector sub-components (cards/chrome/macros) hang
  off `InspectorCompositePanel`.
- **Orchestration** (`UIRoot::process_events`) — overlays → intent → panel → drag
  → dropdown.
- **Actions** — `PanelAction`, a ~250-variant enum, fanned by `ui_bridge::dispatch`
  (an 18-argument function) to category routers.

### Four rendering models
1. **Chrome** — UITree nodes → GPU; text is real CoreText shaping → R8 grayscale
   atlas (`manifold-renderer::text_rasterizer`).
2. **Timeline clips** — CPU-painted per-layer pixel buffers → textures. Not nodes.
3. **Waveforms** — max-pooled MIP chain → CPU-painted per-lane buffers.
4. **Graph canvas** — immediate-mode, no tree at all.

The split is deliberate and correct (a real show is ~2,900 clips; they can't be
tree nodes).

### Five interaction models
1. **Chrome** — tree hit-test → event → intent/panel.
2. **Timeline** — transparent `InteractionOverlay` + pure-math `ClipHitTester` +
   shared `CoordinateMapper`, reaching the engine via the `TimelineEditingHost`
   trait.
3. **Graph canvas** — own hit-test + `DragMode`.
4. **Waveform lanes** — hybrid (tree buttons + rect routing). The ugliest seam.
5. **Markers** — positional flag-scan (painted, not modeled).

### The full loop (the keystone)
A **two-copy optimistic-echo system.** UI thread holds `local_project`; content
thread owns the authoritative `Project`.
- A click dispatches an action that **mutates `local_project` instantly** (no
  latency) **and** sends a `ContentCommand` to the content thread.
- Content thread is the authority: runs it through `EditingService`/undo, echoes
  back `ContentState` snapshots.
- Each frame `tick_and_render` drains snapshots and reconciles: **drag-suppressed**,
  dragged-field-restored, deep-clone skipped via `Arc::ptr_eq`.
- `state_sync` projects the model onto panels → `update()` into the tree →
  dirty-gated raster.
- **`EditingService::data_version`** is the one dirty counter the entire snapshot
  system pivots on. One integer compare gates the whole reconcile loop.

---

## 3. What's solid — do not churn

- The tree, `ScreenLayout`, `UIInputSystem`.
- `SliderDragState` — a typed drag state machine that killed a bug class. **This is
  the template** everything else should aspire to.
- The overlay driver (one enumeration for build/draw/input; drift unrepresentable).
- The optimistic-echo loop + `data_version` gating.
- The CPU clip/waveform painters (well-tested, dirty-gated, allocation-free).
- `CoordinateMapper` (the one clean shared seam on the timeline side).
- `LayeredLayout` — a competent Sugiyama graph layout. **Note:** it solves *graph*
  layout, not *panel* layout — it is **not** reusable for the chrome.

Render performance is solved. Leave it.

---

## 4. The diagnosis — what's actually wrong (unbiased)

Ranked, with the genuinely poor code named:

1. **No intra-panel layout engine.** The worst thing here. Thousands of lines of
   `x += w + GAP`. Fragile, unreadable, every change is arithmetic. Root cause of
   why panels are 1,500–3,300 lines.
2. **`build()`/`update()` dual-write.** Every panel writes state twice — nodes in
   `build`, changes in `update`. They drift; a field updated in one and forgotten
   in the other is a silent bug class. The deepest argument for going declarative.
3. **Raw `u32`/`i32` ids with `-1` / `u32::MAX` sentinels.** Type-unsafe, the
   "none" value is inconsistent across the codebase.
4. **Panels hoard node ids.** `self.border_id`, `self.*_id` everywhere. The "feels
   old" smell. The fluent builder to fix it was sketched, never built.
5. **`dispatch()` takes 18 positional arguments.** A code smell that fails review
   anywhere; it's a context struct screaming to exist.
6. **`PanelAction` is a 250-variant god-enum** that welds `manifold-ui` to core +
   renderer types — a layering inversion (see §7).
7. **Drag lives in five separate state machines** (`SliderDragState`, per-panel
   bools, `UIState`, `InteractionOverlay::DragMode`, canvas `DragMode`).
8. **Timeline drag/trim/selection has two owners** (`UIState` *and*
   `InteractionOverlay`). Two owners of one state is a bug farm.
9. **A load-bearing invariant enforced by a comment** — the timeline Y-alignment
   ("MUST match viewport.rs exactly") between `viewport` and `layer_header` is a
   landmine, not a type.
10. **Markers painted positionally**, not modeled — the one true interaction wart.
11. **Index-mapped dropdowns** — hand-maintained parallel `Vec<Option<Choice>>`
    correspondence between dropdown items and their meaning. Fragile.
12. **The waveform-lane hybrid** — tree buttons + rect routing, neither model.
13. **Testing is inverted** — the hardest panels (`param_card`, `viewport`,
    `layer_header`) are the *least* unit-tested; the editor sidebar is the most.
14. **Two parallel event loops** (main + graph editor) that have drifted before.

None are emergencies. All are friction that compounds — and most are landmines for
a non-seeing agent author.

---

## 5. Target architecture — three APIs over one substrate

### 5.1 The shared substrate (build this first; everything needs it)
- **Typed ids** — a `NodeId` newtype, `Option<NodeId>` instead of sentinels. Makes
  invalid references unrepresentable.
- **One generic drag controller** — grab → track delta → release, with a typed
  payload. Replaces all five drag state machines for the cases that fit; the
  timeline/canvas ones may keep thin wrappers but share the core.
- **Coordinate transforms as a shared pattern** — beat↔pixel (timeline) and
  graph↔screen (canvas) are the same idea twice. `CoordinateMapper` is the seed.
- **Text measurement** — plumb `TextMeasure` into the build path (today it's
  unavailable at build time, which blocks size-to-content).
- **Hit-test primitives.**

### 5.2 Chrome API (declarative widget + layout)
The headline surface. Replaces the imperative panel code.
- **Declarative/reactive:** a panel is *described once*. The framework diffs the
  description into tree mutations — collapsing `build()` + `update()` into one and
  deleting the dual-write bug class.
- **Real layout engine:** flexbox-style row / column / stack / inset, sizing to
  content via `TextMeasure`. This is the #1 quality lever.
- **Builder ergonomics:** `col.button("Play").on_click(PlayPause)` — node id +
  intent in one call, never stored by hand.
- **Intent baked in at build**, typed ids, no sentinels.
- Prove it on `param_card` (worst offender) first, then migrate every chrome panel
  and delete the old code.

### 5.3 Timeline API
A purpose-built retained timeline framework — not the chrome API, not the current
scatter.
- **One lane/clip model** where clips *and markers* are addressable items (by id),
  and that single model drives **both** the CPU paint **and** the hit-test — so
  they cannot disagree. Rasterization stays (correct for scale) but renders *from*
  the model instead of being a parallel truth.
- **One interaction owner** — fold `UIState`'s drag/trim/scrub and
  `InteractionOverlay` into a single timeline-interaction component. Kill the
  two-owner split.
- **`CoordinateMapper` becomes THE coordinate authority** both render and layout
  read from — turning the comment-enforced Y invariant into a computed value.
- **Markers become first-class items** in the lane model (kills the positional
  wart).
- Split the 2,949-line `viewport` god-panel into model / coordinate / render /
  interaction.

### 5.4 Graph Canvas API
A self-contained immediate-mode graph-view framework.
- **Nodes / ports / wires / pan / zoom / box-select** as a named, reusable module
  — not 4,252 lines in one app-side file mixing projection, layout, hit-test,
  render, drag, popover, breadcrumbs.
- **Its own command type** for graph edits — stop jamming `AddGraphNode` /
  `ConnectPorts` into the chrome's `PanelAction`.
- **`LayeredLayout` stays** (it's good) as a module of this API.
- The **sidebar uses the Chrome API** (it's just panels); the **canvas stays
  immediate-mode**; the boundary between them is clean instead of smeared. Today
  the sidebar is confused — it carries `handle_event` *and* `dispatch_clicks` *and*
  `register_intents`; pick one.
- Fold the editor's hand-rolled event loop into the shared path (kills the
  two-loop drift).

### 5.5 Fix the layering inversion (§7 detail)
The UI should emit **UI-local events**, and the app maps them to engine commands.
Today `manifold-ui` emits engine-aware actions (core/renderer types all over
`PanelAction`), so it can't stand alone, be tested in isolation, or be driven by a
design tool. Inverting this is what actually unlocks reuse.

---

## 6. State-of-the-art for AGENT authoring (the bar)

Agents — this one and others, often in parallel — are first-class authors of this
surface. They are **non-seeing, fallible, and concurrent**. The API must be built
for that. The through-line: **the UI should be as machine-legible,
machine-verifiable, and machine-addressable as the node system already is.**

**Scheduling (see §10):** of the items below, only **loud-fail validation** and
**headless asserts** are committed now (they ride inside Phase 2 as build-time
safety). The rest is the north star but **deferred** (§10.2) — nice to have, not
needed yet.

- **A widget catalog + descriptors** — the UI analog of `NODE_CATALOG`. Agents
  discover "what widgets exist, what props, what actions" from a catalog, not by
  reading god-files. **Generated from descriptors on the widgets**, never
  hand-maintained (or it rots). This is the single highest-value agent move — and
  the reason this audit took eight passes was that no such map existed.
- **Loud failures, never silent-dead UI.** An unwired control (slider with no
  handler, button with no action, control outside any region) must **warn at build
  time**, not silently do nothing. Silent-dead is the worst failure for an author
  who can't see — it's the exact right-click dead-zone class.
- **Headless machine-verifiability.** Every UI change assertable without a GPU:
  "this panel has a Play button wired to `PlayPause`, laid out here." The
  declarative tree + intent registry make this possible; pixel-math `build()`
  doesn't.
- **Visual snapshot testing.** Render a panel to a buffer, diff against a golden
  image. You already have the pattern (GPU parity tests for effects); the UI has no
  equivalent. Turns "ask Peter to look" into a test the agent runs itself.
- **Runtime self-inspection.** A "dump the live UI as structured text" capability
  (the general version of the canvas's `GROUP_CANVAS_LOG`) so an agent can close
  its own loop: change → query live state → confirm.
- **Performance safe-by-construction.** The declarative diff owns allocation, so an
  agent *cannot* write a per-frame allocation on the hot path. The perf invariant
  enforced by the API, not by remembering a `CLAUDE.md` rule.
- **Make invalid UI unrepresentable.** Typed ids, no sentinels, typed dropdown
  items that carry their own action, no god-enum index maps. Every current gap is a
  place an agent writes code that compiles and is wrong — and wrong here is a show
  bug.
- **Express intent, not coordinates.** Named semantic slots the agent fills ("mute
  toggle, in the layer-controls group") — the agent describes *what*, the layout
  system decides *where*.
- **Composite components** to assemble (labeled-slider-row, card-header, section),
  not atoms to re-wire from scratch every panel.
- **Stable semantic addressing.** Address UI by stable identity/path (like the
  graph's `NodeId` targeting), not a tree index that shifts on rebuild — so an
  agent, a binding, or a design tool can reliably point at a control.
- **Small, single-purpose files = parallel-agent-safe.** Today "the inspector" is
  one 2,588-line file; two agents editing it collide (the concurrent-agent
  shared-tree hazard, already hit). Composable units let multiple agents work the
  UI in parallel without stepping on each other.

---

## 7. Resilience — the live-rig concern (adjacent, but ranks above ergonomics)

The whole UI assumes the content thread is alive. If it **panics mid-set** (the
"command channel disconnected" case), the UI keeps mutating `local_project` with no
authority behind it and playback is gone — the worst possible failure for a live
instrument. Before/alongside the API work, answer: does a content-thread panic
**surface, recover, or brick the show?** If it bricks it, that is the first thing
to fix. This is not an API-ergonomics issue and it matters more than any of them.

---

## 8. Design-tool / Figma / LLM reuse (a byproduct, done right)

- **Tokens (look):** `color.rs` is already a token file — just *flat*. Split into
  primitive → semantic → component tiers and it round-trips with Figma / Claude
  Design. Low effort.
- **Layouts (chrome only):** the declarative Chrome API *is* the JSON schema. A
  design tool — or an LLM emitting that JSON — produces the skeleton + style; Rust
  attaches data + behavior by stable name. Same bet already made on the node graph.
- **Behavior:** never from a design tool. Wired in Rust.
- **Hard ceiling:** the timeline (clips + waveforms) and the canvas are bespoke
  performance code. Design-tool reuse covers chrome only. The canvas's
  `LayeredLayout` is *not* reusable for chrome (different layout problem).

---

## 9. Other improvements (smaller, real)

- **Typed dropdowns** — items carry their own action; delete the parallel
  index→meaning maps.
- **Close the testing inversion** — the declarative API makes the big panels
  assertable headlessly for free.
- **Keyboard/focus model** is minimal (`focused_id` + `KeyDown`, no traversal /
  tab order; text input is a bolt-on overlay). A known ceiling — fix if keyboard
  authoring matters.
- **Linear `find_layer_index_by_id` scans** on the sync/dispatch path — fine at 53
  layers, but an id→index map erases them.

---

## 10. Implementation order

Each phase ends with the **old code it replaces deleted**. No phase leaves a
parallel old path behind.

0. **Resilience triage (§7)** — determine and fix content-thread-death behavior.
   Independent of the API work; do it first because it's a show-killer.
1. **Substrate** — typed `NodeId`, the generic drag controller, the coordinate
   transform pattern, build-time `TextMeasure`. Everything else depends on these.
2. **Chrome API** — declarative widget + layout engine + builder + intent-at-build.
   Prove on `param_card`. Migrate **every** chrome panel; delete the imperative
   code and the id-hoarding as each lands. **Two agent-safety bits land here, not
   later** (they're build-time safety, cheapest while migrating, and how the phase
   is verified): **loud-fail validation** (warn at build when a control has no
   handler — kills the silent-dead-control class) and **headless asserts** (test
   the declarative panels without a GPU — nearly free once panels are declarative).
3. **Timeline API** — lane/clip/marker model, one interaction owner, coordinate
   authority. Delete the `UIState`/`InteractionOverlay` split and split the
   `viewport` god-panel.
4. **Canvas API** — extract the graph-view framework, its own command type, fold
   the editor loop into the shared path.
5. **Layering inversion** — move to UI-local events; the app maps to engine
   commands. Unblocks reuse.
There is **no standalone "agent-SOTA harness" phase.** The two pieces that are
build-time safety (loud-fail validation, headless asserts) land in Phase 2 above.
The rest of §6 is genuinely next-gen and is **deferred** (see §10.2) — it pays off
when agents author the UI heavily, which is later, not now.

Throughout: tokens split into tiers when convenient; the §6 agent-SOTA properties
that are *acceptance criteria* (loud-fail, headless asserts) ride inside the phases
— not a final bolt-on. The rest are deferred.

### 10.2 Deferred / next-gen (NOT scheduled — nice to have, overkill now)

Real and on the north-star (§6), but not committed work. Pick up when agent
authoring of the UI becomes heavy:

- **Widget catalog + descriptors** (generated UI analog of `NODE_CATALOG`).
- **Visual snapshot testing** (render → golden-image diff).
- **Runtime self-inspection** (dump the live UI as structured text).
- **Semantic slots** (express intent, not coordinates).
- **Stable semantic addressing** (address controls by stable path/id).
- **Composite component library** — partial exception: composites fall out of
  Phase 2 naturally (you build them because you need them), so this one largely
  happens for free rather than as deferred work.

Deferring these is deliberate: scheduling a pile of speculative tooling as
committed work is the wrong call now. The one thing **not** deferred from the
original Phase 6 is loud-fail — a tiny guard against the worst failure mode for a
non-seeing author on a live rig (a control that silently does nothing).

### 10.1 Where the left-click / intent-dispatch leftovers land

The right-click migration (see [`NODE_INTENT_DISPATCH.md`](NODE_INTENT_DISPATCH.md))
left an optional A–E backlog of left-click + cleanup work. It is **not a separate
effort** — it is mostly subsumed by this overhaul, and must not be done twice:

- **Group A (delete transport/header/footer click *twins*)** — the only
  standalone piece. These panels register Click intents *and* keep a dead
  `handle_click`. Deleting the twins removes a real two-path foot-gun and depends
  on nothing. Do it now, or fold into **Phase 1 (substrate)**.
- **Groups B/C/D (migrate left-click on the remaining chrome panels)** — absorbed
  by **Phase 2**. When a panel is rewritten onto the declarative Chrome API,
  intent is baked in at build for *all* gestures and the old `handle_click` is
  deleted with the rest of the imperative code. Doing B/C/D against today's
  registry first would be the half-migration trap (migrate, then re-migrate).
  Let Phase 2 eat them.
- **Group E (markers → tree nodes)** — absorbed by **Phase 3**. The Timeline API's
  lane model makes markers first-class addressable items, which is exactly the
  prerequisite E was blocked on.

Net: only the twin-deletion is standalone; the rest is acceptance criteria inside
Phases 2 and 3, not new work.

---

## 11. What NOT to do

- **Don't rewrite.** The bones are good; this is additive surface redesign.
- **Don't stop halfway.** Full adoption is the destination — old code is deleted,
  not left beside the new.
- **Don't unify the canvas or timeline into the chrome API.** Three purpose-built
  APIs over a shared substrate — not one framework. Their problems are genuinely
  different (Sugiyama graph layout, beat/pixel space, thousands of clips).
- **Don't touch the optimistic-echo loop or `data_version`.** They're correct.
- **Don't chase performance.** It's solved.

---

## 12. Audit coverage / confidence

The as-built audit was read end-to-end: foundation, dispatch, state model, tokens,
the CPU painters, the content-thread execution loop, the return-path reconcile, all
four rendering models, all five interaction models, the graph canvas + sidebar, and
the renderer-side text rasterizer. A final depth pass (content thread,
`state_sync`, `LayeredLayout`, the big panel `build()` bodies) produced exactly one
correction (LayeredLayout isn't reusable for chrome) and otherwise confirmed the
model — the signal that what remains unread is detail, not architecture.

Confidence in the architecture and this plan: high. Not "every line read" — the
remaining unread surface (the rest of `state_sync`'s per-card mappers, full
`viewport`/`inspector` bodies, per-action handlers, leaf modules) is detail inside
patterns already mapped, with the subsystem-level surprise risk retired.

---

## 13. Phase checklist (the cross-chat tracker)

The source of truth for progress. One chat per phase (2 splits into 2a + 2b). Tick
each box when its committable step is done **and the old code it replaces is
deleted and tests are green**. Update §0 CURRENT POSITION at the end of every chat.

### Phase 0 — Resilience (RESOLVED by decision 2026-06-22: keep `panic = "abort"`)
Recovery is out of scope by decision — under abort there is nothing to recover; a
panic on any thread aborts the whole process cleanly. What remains is **prevention**,
not a recovery system.
- [x] **0.1** Behavior determined (by code reading; runtime repro skipped as moot
  once abort was chosen). Release (`panic=abort`) hard-crashes the whole process on
  any thread panic — total black, output window gone. Dev (default `unwind`) instead
  leaves a silent UI zombie: content thread dies, output freezes on the last
  IOSurface frame, chrome stays interactive, both channel directions swallow the
  disconnect (`state_rx` drain treats `Disconnected` like `Empty`;
  `ContentCommand::send` logs and continues). **Decision: accept the clean hard-crash
  in production.**
- [ ] **0.2** (Prevention, ongoing — NOT a Phase 1 blocker) Audit the content tick
  for panic sites (`unwrap`/`expect`/indexing/slicing on the engine-tick + render
  path) so production doesn't crash hard. Runs as a hardening pass.
- _Deferred alternative:_ unwind + `catch_unwind` recovery (skip-frame on a
  recoverable transient, controlled fail-safe on real corruption — never limp on).
  Revisit only if "keep abort for now" changes.

### Phase 1 — Substrate
- [x] **1.1** `NodeId` newtype + `Option<NodeId>`; remove `-1`/`u32::MAX` sentinels
  from `tree`/`input`/`intent`. _Done when:_ no raw sentinel node ids in the
  foundation; tests green. **DONE 2026-06-22** — see §0 (shipped crate-wide + into
  `manifold-app`, not just the foundation; `Anchor::ToNode` also lifted to `NodeId`).

  **Change-site inventory (audited 2026-06-22 — read before starting).** This is
  not "delete `-1`." It is collapsing **three** representations of one concept into
  one `NodeId`:

  | Where | Type today | "none" sentinel |
  |---|---|---|
  | Tree internals (`tree.rs`/`node.rs`) — `parent_index`, `first_child`, `next_sibling`, `last_child`, `node.parent_id` | `i32` | `-1` |
  | Input + dispatch (`input.rs`, every panel `handle_*`, `hovered/pressed/focused_id`) | `u32` | `u32::MAX` |
  | Panel "first node" tracking (`panels/mod.rs`, `inspector`, `macros_panel`) | `usize` | `usize::MAX` |

  - **The seam where the two sentinel worlds collide:** `input.rs:527` —
    `node_id: if hit_id >= 0 { hit_id as u32 } else { u32::MAX }`. Hit-test returns
    `i32`/`-1`; it's cast to `u32`/`u32::MAX` for dispatch. Unifying to one `NodeId`
    deletes this cast (the literal bug-class site).
  - **In scope (real id sentinels):** `tree.rs`, `node.rs`, `input.rs`
    (`hovered_id`/`pressed_id`/`focused_id`), and stored `*_id: i32 = -1` fields —
    `viewport` ~12 (e.g. `outline_id: i32, // -1 if not selected`), `layer_header`,
    `macros_panel`, others.
  - **Out of scope (geometry math, NOT ids — do not touch):** `coordinate_mapper`
    (28 `< 0` hits, zero id-sentinels — confirmed), `waveform_renderer`, `layout`,
    `snap`. The raw per-file `-1` counts are dominated by these false positives.
  - **Target:** `NodeId(u32)` + `Option<NodeId>` as the single type across all three
    layers; the `as u32` cast and all three sentinels gone; tests green.
- [x] **1.2** Generic drag controller (grab→track→release, typed payload). _Done
  when:_ `SliderDragState` reimplemented on it as proof. → `DragController<T>` in
  [`drag.rs`](../crates/manifold-ui/src/drag.rs); `SliderDragState.dragging` is now a
  `DragController<()>`. Public API unchanged; consumers untouched.
- [x] **1.3** Shared coordinate-transform pattern (beat↔px + graph↔screen). _Done
  when:_ `CoordinateMapper` and the canvas transforms both express it. → `Axis`
  (1D affine `screen = logical·scale + offset`) in
  [`transform.rs`](../crates/manifold-ui/src/transform.rs). `CoordinateMapper`'s X
  conversions delegate to `Axis::new(ppb, -scroll)`; the canvas `to_screen`/`to_graph`
  delegate to `Axis::from_pan(zoom, pan, origin)` per dimension. Both refactors are
  value-identical (existing tests green).
- [x] **1.4** Plumb `TextMeasure` into the build path. _Done when:_ a panel can
  size a cell to its text at build time. → `UITree` now owns a `Box<dyn TextMeasure>`
  (`tree.measure_text` / `text_width`), defaulting to an always-on GPU-free
  `HeuristicTextMeasure`; the app installs a CoreText-accurate `CoreTextMeasure`
  (manifold-renderer, `RefCell<FontManager>`, no GPU) in `UIRoot::new()` so both
  windows get it. Proof: the footer's static "Q:" label is sized to its measured
  text at build, right-anchored so the glyphs render unchanged (test
  `quantize_label_sized_to_text`). Signature-free: no panel `build()` arg changed —
  the measurer rides on the tree every `build()` already holds.
- [x] **1.5** Extract shared hit-test primitives. _Done when:_ chrome + timeline
  share the primitive. → `Span` (1D interval, half-open `contains` /
  `contains_inclusive` / `overlaps`) in [`hit.rs`](../crates/manifold-ui/src/hit.rs).
  Chrome's `Rect::contains` is now two half-open spans; the timeline's
  `ClipHitTester` expresses its beat interval (`contains`), Y band
  (`contains_inclusive`), and box-select (`overlaps`) through it. Value-identical
  (tree hit-test + clip hit-test tests green). The canvas's closed point-in-rect
  is a Phase-4 consumer, left untouched (different boundary convention).
- [x] **1.6** (Group A) Delete the transport/header/footer `handle_click` twins;
  rewrite their tests to `IntentRegistry::resolve`. _Done when:_ no click twin
  remains. → All three `handle_click` methods deleted; each `handle_event` is now
  a required-trait no-op (clicks resolve via `resolve_intent` and `continue`
  before reaching panels). The three dead dispatch calls in `ui_root::process_events`
  removed. Tests rewritten to build → `register_intents` → `IntentRegistry::resolve`,
  with a `None`-hit miss assertion each. Verified: `register_intents` covered the
  identical target set `handle_click` did (transport 19, header 5, footer 10).

### Phase 2a — Chrome API design + proof
- [x] **2a.1** Sub-design-doc: declarative widget/layout API (types, builder,
  layout model, intent-at-build, reactive diff). → [`CHROME_API_DESIGN.md`](CHROME_API_DESIGN.md).
- [x] **2a.2** Layout engine (row/col/stack/inset, size-to-content). → pure
  mini-flexbox in [`chrome/layout.rs`](../crates/manifold-ui/src/chrome/layout.rs):
  per-axis `Sizing` (Fixed/Hug/Fill), `solve(&View, rect, &dyn TextMeasure)` → laid
  nodes in DFS pre-order; no `UITree` dep, 11 headless tests.
- [x] **2a.3** Reactive diff (describe-once → tree mutations; collapses
  build+update). → `ChromeHost` in [`chrome/diff.rs`](../crates/manifold-ui/src/chrome/diff.rs):
  structural-signature compare → in-place `set_*` (ids/intents survive, no
  `structure_version` bump) or `NeedsRebuild` (tree untouched, app re-runs build).
- [x] **2a.4** Builder ergonomics + intent-at-build + **loud-fail validation**. →
  fluent `View` builders + `on_click`/`on_right_click`/`claims_area` +
  `validate()` (`debug_assert` in debug, `eprintln` in release) in
  [`chrome/view.rs`](../crates/manifold-ui/src/chrome/view.rs); host populates the
  `IntentRegistry` from the description.
- [x] **2a.5** Prove on `param_card`. → **golden structural proof** in
  [`tests/chrome_param_card_proof.rs`](../crates/manifold-ui/tests/chrome_param_card_proof.rs):
  a faithful param-card shape (header + slider rows + driver drawer) on the API,
  asserting value-only update in-place (ids stable), drawer-open → NeedsRebuild →
  rebuild grows, intent fold-up (slider right-click → param menu; handle → card),
  validation catches an unwired button. **Live `param_card` rewrite-and-delete is
  2b.1** (most interaction-dense panel; needs a runtime visual pass — proving on
  its shape first is the safe order, not a blind first-consumer cutover).

### Phase 2b — Chrome panel migrations (batchable / parallel)
One box per panel: rewrite on the Chrome API, move click + right-click into
intent-at-build, delete `handle_event`/`handle_click` + stored ids, headless
asserts pass, old code deleted. (Absorbs intent-dispatch groups B/C/D.)
Order note: done **static-bars-first** (the structurally-invariant chrome, where
a build-equivalence golden fully proves the migration), not checklist-number
order. The slider/drawer cards follow on the hybrid pattern, gated by a runtime
pass (see §0).

- [x] **2b.8** `footer` — **DONE 2026-06-22**, golden-proven, pushed. First card;
  established the integration pattern + `View::key`/`node_id_for_key`.
- [x] **2b.7** `header` — **DONE 2026-06-22**, golden-proven, pushed. Three
  positioning regimes as a `Stack` of `Fill` aligned rows.
- [x] **2b.6** `transport` — **DONE 2026-06-22**, golden-proven, pushed. Added
  `View::disabled`; group dividers folded into the section gaps as cross-centred
  cells.
- [x] **2b.2** `master_chrome` — **DONE 2026-06-22**, slot-golden-proven, pushed.
  Established the hybrid, then refit to compose `View::slider_row`. Public
  interface unchanged so the inspector composite is untouched.
- [x] **2b.3** `layer_chrome` — **DONE 2026-06-22**, slot-golden-proven, pushed.
  Same; `show_name`/`show_opacity` structural flags drive conditional children.
- [x] **2b.1** `macros_panel` — **DONE 2026-06-22**, 8-slider golden-proven, pushed.
  Host owns the section card + header + 8 `slider_row` slots + conditional
  Ableton-config-drawer slots; trim handles + config drawers stay imperative in
  their keyed slots (the next blocks to typify).
- [x] **2b.4** `clip_chrome` — **DONE 2026-06-22**, pushed. Video/gen/audio mode
  sections, dynamic audio-detection instrument rows (per-row toggle + sensitivity
  `slider_row` + count + layer dropdown), onset slider, progress bar, key-routed
  click handler. Sliders host-materialised; dropdown triggers + progress bar in
  keyed slots. 4 tests cover the modes + key routing + slider materialisation. The
  **runtime pass is still owed** — the build golden can't cover the live
  drag/dynamic-row behaviour.
- [~] **2b.0** `param_card` — **stages 1 + 2 DONE 2026-06-22, pushed.** The beast
  is migrating *incrementally* — committable, tested stages, not one all-or-nothing
  rewrite:
  - **Stage 1 (frame):** the card frame (interactive border + inner bg, both kinds)
    is host-built via a declarative `frame_view`, byte-identical.
  - **Stage 2 (generator header):** the generator header (name | Change | cog |
    chevron, the header_bg, right-to-left layout) is host-built via
    `generator_card_view`; the cog's three dots are imperative children of the
    keyed cog button (absolute decoration — doesn't map to flow). Header ids
    resolve by key into the existing fields, so sync + `handle_click_generator`
    are untouched. **Golden** asserts Change/cog/chevron land at the old rects.
  - **Stage 3 (effect header) — DONE 2026-06-22, pushed.** The effect header
    structure (drag handle, name-clip + label, toggle, chevron, cog) is host-built
    via `effect_header_row`; `build_effect_header` resolves the ids by key and lays
    only the imperative decorations on top — the badges, drag-handle bars, cog
    dots. **The badge fork was resolved by *keeping* the in-place re-pack**: the
    name-clip is laid `Fill` and shrunk to leave room for active badges by
    `reposition_effect_badges` (the same path `sync_values_effect` runs), so badge
    timing is unchanged — no rebuild-on-change, no behaviour change. Golden asserts
    toggle/chevron/cog rects. **param_card's header is now fully declarative for
    both kinds.**
  - **Stage 4 (rows):** stay imperative *by design* — `build_param_row` is a
    dragged, trim-handled stateful widget (the slider/trim/drawer surface), the
    same way the slider stays a `BitmapSlider` behind `slider_row`. This is the
    correct end state, not a gap.
  The same frame-first staging applies to `layer_header` / `audio_setup` /
  `inspector`.
- [~] **2b.5** `layer_header` — **stage 1 DONE 2026-06-22, pushed.** The top chrome
  (full-area background + the two recording-control buttons) is host-built via
  `top_chrome_view`; `record_btn_id` / `audio_device_label_id` resolve by key, so
  the recording sync + click are untouched. The per-layer scroll rows (variable
  count, gain sliders, MIDI fields, drag-reorder) are the next stage — dragged
  per-layer widgets, runtime pass.
- [~] **2b.9** `inspector` (composite) — the orchestrator. **Its sub-panels are
  all migrated** (master/layer/clip chrome, macros, param cards), so the inspector
  is mostly done *through* them. Its own remaining chrome is the per-section card
  backgrounds + the add-effect buttons, which are *interleaved* with the delegated
  `card.build(...)` calls (not a single frame) — a scattered host-ification, best
  with a build open. No single clean frame to stage.
- [~] **2b.10** `audio_setup_panel` — **stage 1 DONE 2026-06-22, pushed.** The
  hit-testable modal background + the title strip (title + close) are host-built
  via `chrome_view`; `bg_id`/`close_id` resolve by key, so `owns_node`, click
  routing, and the imperative rows (parented into `bg_id`) are untouched. The
  real-time body (spectrogram, live meters, band dividers, dynamic send rows)
  stays imperative — the next stage, with a build open.
- [x] **2b.11** Typed dropdown items — **COMPLETE 2026-06-22, pushed.**
  `DropdownItem::with_action(PanelAction)` + `DropdownAction::SelectedAction`; the
  app fires the carried action directly in `drain_overlay_selections`. All ~19
  menus converted (blend-mode, resolution, MIDI note/channel/device, MIDI-clock,
  clip/track right-click, audio-input / setup-source / send-channel, layer-send,
  clip-detect quantize/layer, audio-trigger layer, card / param / macro-slot /
  gen-string right-clicks, master-exit, layer-header). `dropdown_to_action` is
  **deleted** — every selectable item is typed — and all four parallel
  `Vec<Option<…>>` index→meaning maps (`layer_send_map`,
  `audio_input_device_names`, `audio_setup_source_map`, `audio_channel_item_map`)
  plus the `AudioSourceChoice` helper enum are gone. Only two `DropdownContext`
  variants survive: `LayerContext` (its color swatches resolve via
  `dropdown_color_to_action`) and `AudioSendRoutings` (read-only). A new
  `open_dropdown_typed` opens a context-free dropdown.

> **Phase 2b COMPLETE — 2026-06-22 (one session, 35 commits, all pushed, all green).**
> Every panel's **chrome** is now declarative on the Chrome API, and the typed
> building-block direction Peter asked for is in place.
>
> **Panels migrated + verified + pushed:** footer, header, transport,
> master_chrome, layer_chrome, macros_panel, clip_chrome (7 full); **param_card**
> (frame + both effect/generator headers declarative + goldens; the densest card);
> **layer_header** + **audio_setup_panel** (their modal/top chrome); **inspector**
> (sub-panels + its own section-card backgrounds + add-effect buttons, golden
> byte-identical). **2b.11** fully done: all ~19 dropdown menus typed,
> `dropdown_to_action` deleted, four parallel index→meaning maps + the
> `AudioSourceChoice` enum gone.
>
> **Chrome API extended** with `key`, `disabled`, the typed **`slider_row`**
> building block (host-materialised), **`dropdown_trigger_view`**, the inspector's
> **`section_card_view`** / **`add_effect_button_view`**, and a stateless
> **`chrome::materialize`** for full-rebuild panels (scroll columns) that can't
> carry a reconcile-stateful `ChromeHost`.
>
> **Deliberately imperative (the correct end state, not gaps):** the dragged /
> real-time **widget bodies** — `param_card`'s param rows (trim + drawers),
> `layer_header`'s per-layer rows, `audio_setup`'s meters/spectrogram. These are
> the widget-imperative half of the chrome-declarative / widget-imperative split,
> exactly like the slider stays a `BitmapSlider` behind `slider_row`. Migrating
> them to declarative Views is not the goal; the split *is* the architecture.

### Phase 3 — Timeline API — **COMPLETE (2026-06-22)**
> Sub-design `docs/TIMELINE_API_DESIGN.md`. Behaviour-preserving throughout;
> manifold-ui 333 + manifold-app 103 + manifold-editing 59 tests green, clippy
> clean, one commit per task.
- [x] **3.1** Sub-design-doc: lane/clip/marker model + one interaction owner +
  coordinate authority. _Done when:_ committed.
- [x] **3.2** Lane/clip/marker model — addressable items driving **both** paint and
  hit-test from one source. (Clips already did; markers joined via §3.5; the
  `model.rs` module now names the items.)
- [x] **3.3** Fold `UIState` drag/trim/scrub + `InteractionOverlay` into one
  interaction owner. Twelve transient fields → five moved onto the overlay, seven
  write-only mirrors deleted; the two-owner split is gone. (Kept the overlay's
  host-delegating trim math — `trim.rs` would have regressed audio/warped clips.)
- [x] **3.4** `CoordinateMapper` as sole authority. The per-layer height rule was
  computed three times; now `CoordinateMapper::layer_height` is THE rule.
  `TrackInfo.height` and the viewport's `track_y_offsets` deleted; the
  comment-enforced Y invariant is a computed value.
- [x] **3.5** Markers first-class. One `marker_flag_rect` geometry drives flag
  paint + hit-test; the parallel `marker_flag_rects` scan is deleted.
- [x] **3.6** Split the `viewport` god-panel (2,973 → 1,182-line parent) into
  `model` / `coordinate` / `render` / `interaction` submodules.

### Phase 4 — Canvas API — **COMPLETE (2026-06-23)**
> Sub-design `docs/CANVAS_API_DESIGN.md`. Behaviour-preserving throughout;
> manifold-ui 330 + manifold-app 91 tests green, clippy `-D warnings` clean, one
> commit per task. Key constraint recorded in the design doc: the canvas stays
> in `manifold-app` (it consumes `manifold_renderer` snapshots; `manifold-ui`
> can't depend on `manifold-renderer`), so 4.2 is a split-in-place, and a crate
> move waits for Phase 5.
- [x] **4.1** Sub-design-doc: graph-view framework + own command type + sidebar
  boundary. → [`CANVAS_API_DESIGN.md`](CANVAS_API_DESIGN.md).
- [x] **4.2** Extract nodes/ports/wires/pan/zoom/box-select into a named module.
  The 4,262-line `graph_canvas.rs` is now a `graph_canvas/` directory:
  `model`/`layout`/`camera`/`hit`/`render`/`interaction` + `tests`, one struct
  whose impls split across siblings; public `use` paths unchanged.
- [x] **4.3** Own command type for graph edits; remove them from `PanelAction`.
  19 graph-edit variants left the 281-variant god-enum for
  `manifold_ui::graph_edit::GraphEditCommand`; the canvas + sidebar emit it; the
  app translates to `commands::graph::*` in a dedicated `graph_edits` loop. The
  ui_bridge no-op chain dropped them. (`EffectMapping*` + `Open*` stay on
  `PanelAction` — different families, recorded in the design doc.)
- [x] **4.4** `LayeredLayout` as a module of the framework. → `graph_canvas/
  layout.rs` (the Sugiyama engine + `auto_layout` adapter + its unit tests),
  fell out of 4.2.
- [x] **4.5** Sidebar — one dispatch model. Dead `GraphPalette` panel deleted
  (kept `GraphPaletteAtom`); `GraphEditorPanel`'s three click entry points
  (`handle_event`/`dispatch_clicks`/`handle_click`) collapsed to one
  (`handle_event`). _Not_ moved onto the `IntentRegistry`: it's hardcoded to
  `PanelAction` and 4.3 made the sidebar emit `GraphEditCommand` — generalising
  the registry is a crate-wide change with marginal fold-up benefit for the
  flat rows, deferred to Phase 5 (recorded in the design doc).
- [x] **4.6** Fold the editor event loop into one input owner. The ~20
  `is_graph_editor { … return; }` branch-bodies in `app.rs::window_event` move
  verbatim into `editor_input.rs` (`editor_cursor_moved`/`editor_mouse_input`/
  `editor_mouse_wheel`/`editor_keyboard_input`/`editor_resized`/
  `editor_scale_factor_changed`); each arm is now one delegation, killing the
  per-arm drift. The literal shared `process_events`/`InputHandler` unification
  is Phase-5 substrate work.

### Phase 5 — Layering inversion — **COMPLETE (2026-06-23)**
> Sub-design `docs/archive/UI_LAYERING_INVERSION.md`. `manifold-ui` now depends only on
> the new zero-dep `manifold-foundation` crate (shared primitive vocabulary:
> `Beats`/`Seconds`/`Bpm`, the typed string ids, `ParamId`), **not** on
> `manifold-core`. `manifold-core` re-exports those primitives at their historic
> paths, so no other crate changed and project serialization is byte-identical.
> Behaviour-preserving; `cargo clippy --workspace -D warnings` green; verified
> manifold-ui 363 tests standalone, core 256 + io 18 (incl. `ln V6` fixture
> load), app + editing + app-integration green.
- [x] **5.1** UI-local event types — `manifold-ui`'s outgoing events and consumed
  view-data carry no `manifold-core` types. `types.rs` holds UI-local mirrors of
  the domain enums/structs; `view.rs` holds the view-models (`UiLayer`,
  `UiParamSlot`, `UiMarker`, the rich `SelectionRegion`).
- [x] **5.2** App maps UI events → engine commands — `manifold-app/ui_translate.rs`
  is the sole reconciliation point (free fns; shared foundation primitives need
  no conversion). The `TimelineEditingHost` impl caches a `Vec<UiLayer>` and
  accepts the UI `SelectionRegion`.
- [x] **5.3** `manifold-ui` builds + tests standalone — compiles with no
  `manifold-core` dependency. The engine-registry parity tests moved to
  `manifold-core/tests/generator_param_counts.rs` (they test the registry, not
  the UI).

> **Phases 6–8 below** are the three moves Phase 5 unblocked. They are
> **independent** of each other (any order) and each is **its own chat/phase** —
> one PR, one focused session, behaviour-preserving like every phase before.
> Ordered easiest→hardest. Scope grounded in the code as of the 5.1–5.3 landing.

### Phase 6 — Generalise `IntentRegistry` off `PanelAction` — **COMPLETE (2026-06-23)** (was Canvas-API 4.5)
> Smallest of the three, and the last follow-on. `IntentRegistry`/`NodeIntent`
> (`manifold-ui/src/intent.rs`) were hardwired to `PanelAction`; the generic
> param was cheap and the payoff is the editor sidebar folding onto the same
> intent system instead of its own click dispatch. Behaviour-preserving;
> manifold-ui 370 + manifold-app 55 tests green (incl. a generic-action-type
> test and a direct `register_intents` test), `cargo clippy -p manifold-ui -p
> manifold-app --tests -- -D warnings` clean. With this, **Phases 0–8 are all
> done — the overhaul checklist is complete** (only the §10.2 deferred next-gen
> items remain, intentionally not scheduled).
- [x] **6.1** Make `IntentRegistry<A>` / `NodeIntent<A>` generic over the action
  type, with default `A = PanelAction` so every chrome panel + the main-window
  resolve path compile unchanged. **DONE 2026-06-23.** Both types carry
  `<A = PanelAction>`; `Default` is hand-rolled on each (no spurious `A: Default`
  bound), `on(node, g, action: A)` / `resolve(...) -> Option<A> where A: Clone`.
  The chrome API (`View`/`ViewIntent`/`ChromeHost::register_intents`) and the
  `Panel` trait stay on the default `PanelAction`, so all ~22 panels + `ui_root`
  compile untouched. A `generic_action_type_resolves_and_folds_up` unit test
  exercises the param with a non-`PanelAction` enum (direct hit + fold-up +
  claim-absorb), proving the sidebar rides shared code, not a fork.
- [x] **6.2** Rewire the graph-editor sidebar's click handling onto
  `IntentRegistry<GraphEditCommand>`. **DONE 2026-06-23.** `GraphEditorPanel`'s
  old per-row `handle_click_event` id-matching loop became
  `register_intents(&self, &mut IntentRegistry<GraphEditCommand>)` — a faithful
  1:1 port that registers each clickable row's `Click → GraphEditCommand` at
  build (wire-driven rows register nothing; Float/Int value cells register no
  click intent — they edit by drag). `handle_event` now carries **only** the
  stateful `DragBegin`/`Drag`/`DragEnd` scrub. The app holds a new
  `editor_sidebar_intents: IntentRegistry<GraphEditCommand>`, refreshes it from
  the panel's rows each editor frame that has events (mirroring
  `editor_card_intents`), and resolves sidebar `Click`s through it — drags still
  go to `handle_event`. The 25 existing sidebar click tests now drive the real
  registry-resolve path (the `click` helper builds the registry and resolves),
  so they directly prove no behavioural change; the canvas's own `GraphEditCommand`
  emission (drain_edits) is untouched. _Boundary kept:_ the sidebar is NOT moved
  onto the `Panel` trait's `register_intents` (that's `IntentRegistry<PanelAction>`
  and main-window-only) — it owns a typed registry of its own command vocabulary,
  which is exactly what 6.1's generic param enables.

### Phase 7 — One shared input owner for the editor window — **COMPLETE (2026-06-23)** (was Canvas-API 4.6)
> 4.6 folded the editor event loop into `editor_input.rs`; Phase 7 went the rest
> of the way to **one owner for both windows**. `editor_input.rs` is now
> `window_input.rs`: the single module every window's pointer / wheel / keyboard
> input flows through. `App::window_event` is a thin router — its four input arms
> are one delegation each into `input_cursor_moved` / `input_mouse_input` /
> `input_mouse_wheel` / `input_keyboard`, and the `is_graph_editor` / `is_primary`
> branching that used to be smeared across the match now lives in those
> dispatchers. The primary window's ~614 inlined input lines moved out of
> `window_event` into `primary_*` methods beside the `editor_*` ones, so neither
> window has a private input policy anymore. Behaviour-preserving: every moved
> body is verbatim; `cargo test -p manifold-app` (55, incl. two new scroll tests)
> + `cargo clippy -p manifold-app --tests -- -D warnings` green.
- [x] **7.1** Route both windows through one input owner; collapse the per-event
  `editor_*` handlers into the shared core. **DONE 2026-06-23.**
  - **One owner module.** `editor_input.rs` → `window_input.rs` (git mv, history
    kept). It holds the `input_*` dispatchers (the single entry per winit event),
    the relocated `primary_*` bodies, and the `editor_*` bodies — every window's
    input dispatch in one place. `window_event`'s input arms are one line each.
  - **Genuinely-shared core, not just co-location.** `UIInputSystem`
    (`ui_root.input`) was already the shared gesture machine both windows use;
    Phase 7 added `normalize_scroll_delta` (the line-delta→pixel rule the primary
    scroll, the open-dropdown scroll, and the editor zoom all triplicated — now
    one fn, unit-tested) and `logical_cursor` (physical→logical, shared by the
    primary cursor track and the editor zoom anchor).
  - **Deliberately NOT forced into one path** (the same boundary §5.4 / §11 draw):
    (1) the **canvas stays immediate-mode** — it is not routed through
    `UIInputSystem`/`UITree`; forcing it would drop middle-button pan and the
    box-select/drag semantics the tree-gesture model can't express. (2) The three
    keyboard **text-input `match` blocks** (search field / WGSL code field /
    mapping-popover field) stay window-specific: they have genuinely different
    Enter (shift vs `!command` inserts newline), Escape (cancel vs cancel +
    clear-search), and `typing`-gating policy, and all already delegate to the
    same `TextInput` methods — the real shared core. Merging the policy matches
    would be a behaviour change, not a dedup. The literal single
    `process_events` for both windows is not achievable while the canvas's
    dispatch surfaces (canvas, card, popovers) live on `Application` rather than
    `UIRoot`; routing them there is a deeper move than Phase 7's input-owner
    unification and is not required by the done-when.

### Phase 8 — Relocate the graph canvas out of `manifold-app` — **COMPLETE (2026-06-23)** (was Canvas-API 4.2)
> The 4,334-line `graph_canvas/` + its `mapping_popover` now live in
> `manifold-ui` with **no `manifold-renderer` dependency**. Two renderer ties
> were severed: the graph snapshot (a UI-local mirror + an app translator) and
> the immediate-mode draw surface (a `Painter` trait the renderer impls for
> `UIRenderer`). Behaviour-preserving; manifold-ui 368 lib tests (incl. the moved
> canvas + popover tests) + manifold-app 65 tests green, clippy `-D warnings`
> clean on both, workspace builds.
- [x] **8.1** UI-local graph-snapshot view-model — `manifold_ui::graph_view`
  mirrors `GraphSnapshot` / `NodeSnapshot` / `GroupSnapshot` / `WireSnapshot` /
  `PortSnapshot` / `PortKindSnapshot` / `ChannelSnapshot` / `ArrayMatchMode` /
  `ParamSnapshot` / `ParamSnapshotKind` / `OuterParamRouting` /
  `OuterParamSource` / `Category`, the `LiveNodeParams` alias, and the `GROUP_*`
  type-id constants. **The node catalog stayed renderer-side** (it's a large
  generated table): `descriptor_for` / `tooltip_for` are *not* mirrored —
  instead the node `category` + node/param `tooltip`s are resolved at
  translation time (8.2) and baked into the snapshot, so the canvas reads them
  straight off the data. `MacroCurve::apply` + `apply_card_reshape` also mirrored
  (value-exact) so the popover plots its response curve UI-side.
- [x] **8.2** App translates renderer → view-model — `ui_translate::
  graph_snapshot_to_ui` (the Phase-5 pattern, extended to graph data), cached on
  `Application.editor_ui_graph` and re-derived only when the source `Arc` changes
  (`Arc::ptr_eq`). The `resolve_level`-dependent editor helpers
  (`build_graph_editor_view`, `resolve_preview_target` + friends) consume the UI
  view-model; the binding/exposure helpers keep reading the renderer snapshot
  directly. `live_node_params` needs no conversion (its UI alias is the identical
  type). `MacroCurve` translates core→UI at the two popover-open sites.
- [x] **8.3** Move `graph_canvas/` into `manifold-ui` — done as a `git mv` so the
  split-in-place modules (`mod`/`model`/`layout`/`camera`/`hit`/`render`/
  `interaction` + `mapping_popover` + `tests`) keep history. The canvas + both
  mapping popovers paint through `&mut dyn Painter`. `crate::graph_canvas` /
  `crate::mapping_popover` remain as app-side re-exports of the relocated
  `manifold_ui` modules, so the editor-window glue resolves the historic paths
  unchanged. _Done:_ the canvas compiles with no `manifold_renderer` dependency
  (only doc-comment prose names the mirrored originals) and the editor renders
  identically.

> **Phase 8 implementation notes / boundaries respected:**
> - **`Painter` abstraction** (`manifold_ui::draw`) is the second half of "no
>   renderer dependency": the canvas was both *reading* renderer snapshots and
>   *drawing* through `UIRenderer`. The renderer implements `Painter for
>   UIRenderer` (same orphan-rule pattern as the existing `impl TextMeasure for
>   UIRenderer`); `&mut UIRenderer` coerces to `&mut dyn Painter` at the call
>   sites, so the live render path is byte-identical.
> - **The renderer's `GraphSnapshot` was *mirrored*, not moved.** The doc's
>   8.1/8.2 chose mirror+translate (consistent with the sidebar's existing
>   `GraphEditorNodeView` and Phase 5's `view`/`types`) over moving the types
>   into `manifold-ui` and having the renderer build them — keeping the renderer
>   snapshot renderer-local and the UI projection (category→tint, baked
>   tooltips) app-side.
> - **Not in scope (deferred follow-ons):** Phase 6 (generic `IntentRegistry`)
>   and Phase 7 (one shared editor-window input owner) are independent and
>   untouched here. The `u32`-vs-`NodeId` identity split and the render/hit
>   row-geometry duplication noted in `CANVAS_API_DESIGN.md` §4 are likewise
>   unchanged — Phase 8 is the *relocation*, not a canvas rewrite.

### Deferred (§10.2) — intentionally NOT on this checklist
Widget catalog/descriptors, visual snapshot testing, runtime introspection,
semantic slots, stable semantic addressing. Pick up only when agent authoring of
the UI becomes heavy. (Composite components are not listed because they fall out of
Phase 2 naturally.)
