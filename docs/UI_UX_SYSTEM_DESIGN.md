# MANIFOLD — Graph & Parameter UI/UX System Design

Status: **rationale doc — partly built** (2026-05-31).
This is the original vision for the UI/UX pass on the effect/generator/graph/node
surfaces. It is the human-facing half of the node "UX backend" initiative
([project_node_descriptor_ux_backend], `docs/NODE_CATALOG.md`): the descriptor
backend already exists and feeds the agent catalog; this design surfaces that
same backend to humans, and adds the structural UI it implies.

**Much of this has since shipped** (on-node controls, collapse, in-place edit,
single-node preview, groups navigation, popup palette, window behaviour). For the
*current state + the remaining work*, see `docs/GRAPH_EDITOR_UX_BUILD_BRIEF.md`
(2026-06-13), which is code-verified and authoritative. Read this doc for the
*why*; read the brief for the *what's-left*.

---

## 1. The one idea: everything is a boundary

The unit of the UI is not "the atom." It is **the boundary**: a thing with a
simple *face* on the outside and detail hidden *inside*. You see the face; you
open it when you want the guts. The same primitive appears at three scales:

| Scale | Face | Interior |
|---|---|---|
| **Graph** | a node's name + a few ports + macro params | a subgraph (one dispatch, a hand-made group, a saved recipe, or a whole effect) |
| **Value** | one knob: its name, range, invert, unit | the inner param it actually drives |
| **Layout** | a panel | the surface it contains; you toggle / dock / snap it |

One sentence holds the whole system:

> **Boundaries nest (graph), boundaries map (params), boundaries dock (layout).**

Learn it once and it pays off everywhere — for a performer reaching for a knob
mid-show and for an AI agent composing a graph from the catalog. They touch the
same primitive.

**We already own this primitive — it is the effect card.** "A face of exposed
macros over an inner graph" describes the card word-for-word. It is the Ableton
rack: eight macros on the front of a device chain, nestable inside itself. The
work is to make *the node* and *the effect/card* the **same object** and let it
nest — not to invent a new concept. (This is the unify-the-model rule, not
fork-per-kind — see [feedback_graph_editor_unified_surface].)

Three guardrails fall straight out of the model and hold across every section:

1. **Don't fork behaviour on kind.** Atom vs group vs effect, effect vs
   generator, compiled vs uncompiled — these are *data*, not separate code
   paths. When behaviour forks, unify the model underneath.
2. **The compiler stays below the authoring view.** The graph you see and edit
   (boundaries visible) is not the graph that runs (fused). The UI always shows
   the authored view.
3. **One source of truth, two faces.** The node descriptor is the single
   backend. `node_catalog.json` is the agent's face; the palette/canvas is the
   human's face. Build the human face off descriptors and the two vocabularies
   cannot drift.

---

## 2. The backend we are building on

This design consumes infrastructure that already ships. "Full use of the
backend" means every asset below lands on a surface.

| Backend asset | Where it lives | What it powers in this design |
|---|---|---|
| Friendly **label** | `primitive!` / `PrimitiveFactory.picker` | node title, palette row |
| 19-cat **`Category`** | `node_graph/descriptor.rs` | palette grouping, node header colour |
| **`Role`** (Source/Map/Filter/Sink/Control) | `descriptor.rs` | node glyph/affordance, agent validator food |
| **`aliases`** (old name + synonyms + TD operator) | `descriptor.rs` | palette **search** |
| **`summary`** (hand-written, VJ-facing) | `descriptor.rs` | palette hover, node info-on-select |
| **`purpose`** (precise technical) | `descriptor.rs` | deep info popover |
| per-param **`tooltip`** | `node_graph/param_doc.rs` (`tooltip_for`) | on-node knob tooltip |
| **`ParamType::Angle` / `Frequency`** | `node_graph/parameters.rs` | on-node value units (°, Hz) — already wired to card + sidebar |
| binding **`min` / `max` / `convert` / `is_angle` / `label`** | `manifold-core` `UserParamBinding` | the param-mapping popover (§5) |
| Ableton **invert (`ned`) + range** | `manifold-core` `ableton_mapping`, INV button in `param_slider_shared.rs` | the param-mapping popover (§5) |
| driver/envelope **`range_min` / `range_max`** | `manifold-core` modulation, `ResolvedParam` | the param-mapping popover (§5) |

The descriptor backend is finished. The work is the UI that reads it.

---

## 3. The three surfaces, three verdicts

### 3a. The card (performance face) — already right, don't redesign
The card is the Ableton rack and the correct abstraction for a live instrument.
It is the part you touch on stage. Its remaining headroom is *affordance detail*
(the param-mapping editor, §5), not structure. Leave the model alone.

### 3b. The node palette (authoring discovery) — the weak link
**Current:** [`node_graph/palette.rs`](../crates/manifold-renderer/src/node_graph/palette.rs)
groups by `PaletteCategory` — two buckets, Atom / Driver. The panel
([`graph_palette.rs`](../crates/manifold-ui/src/panels/graph_palette.rs)) is a
200px flat list of `+ Label` rows. No search, no descriptions, no categories.
The whole 19-category / alias / summary effort is invisible here.

**Target:** the discovery surface MANIFOLD already ships for *whole effects* —
[`browser_popup.rs`](../crates/manifold-ui/src/panels/browser_popup.rs), a
"grid-based browser with search bar, category chips, and a scrollable grid,"
already wired to the `text_input` system — brought down one level to *atoms*,
powered by the descriptor:
- **Search box** matching `aliases` (so "blur", "clouds", "Noise TOP" all land).
- **Category sections/chips** from the 19-cat `Category`, not the 2-bucket split.
- **`summary` on hover / select.**
- Reuse `browser_popup`'s machinery (text_input + chips + grid) rather than
  inventing a second one.

**The one seam to get right up front (§7):** the browser must consume a single
merged list of entries that mixes a **static** source (built-in atoms — the
`&'static` descriptor inventory) and a **dynamic** source (user-saved recipes /
groups — runtime data). Then adding recipes later is "populate a kind," not
"rebuild the palette."

### 3c. The canvas (authoring comprehension) — middling, big upside
**Current:** node tiles ([`graph_canvas.rs`](../crates/manifold-app/src/graph_canvas.rs))
show header + one summary line + ports. Ports are **already type-coloured**
(texture / scalar / array / camera / light / material). Params are edited in a
separate 320px right sidebar ([`graph_editor.rs`](../crates/manifold-ui/src/panels/graph_editor.rs)).

**Target:** see §4 (on-node controls), §6 (previews + animation), plus
category-coloured node headers (instant Blender/TD-style visual grouping).

---

## 4. On-node controls — no side menu

Each node wears its everyday knobs **on its face**, Blender-style. You read what
a node does and tune it in the same spot, no eyes darting to a side panel. The
canvas becomes the whole instrument. There is **no permanent side menu** for
params.

Rules:
- **Depth opens in place, not at the edge.** A knob's deep settings — its
  min/max/invert/unit mapping (§5) — open as a **popover off the knob**, not a
  side panel. Detail when you ask, gone when you don't. (Keeps the box rule: the
  knob is a face; opening it reveals its interior.)
- **Progressive disclosure — clean by default (decided 2026-05-31, shipped).**
  Dumping every param on every node turned a real 20-node graph into an
  unreadable wall (tall towers force a 25% zoom where text is sub-pixel mush).
  So the dose is: **default collapsed** = header + category tint + one summary
  line (the key param, e.g. "Mode: FoldX") + ports; **expand** (header chevron)
  = every param row, draggable; **zoom LOD** = below ~0.5 zoom, drop all body
  text so the node reads as a clean colour-coded box. Ports stay visible in
  every state (you still wire collapsed nodes). Body height tracks the
  collapsed/expanded state only, not zoom, so ports never jump. This is what
  delivers *focus*: quiet the nodes you're not on, expand the one in your hands.
- **Expose-to-card moves on-node.** A per-param promote control (a dot/button)
  replaces the sidebar checkbox — you expose a param right where you see it,
  exactly like mapping a macro on an Ableton rack.

The data is already there: `draw_node` receives every node's full `ParamSnapshot`
list today (it collapses it to one summary line in `build_summary`). Putting
controls on the face is mostly "stop throwing the params away."

**Build in passes** (so the look can be tuned before interaction is wired):
1. Params rendered on the face, **read-only**, with a fill bar for ranged
   values. (Tune density, sizing, node width.) Note: `set_snapshot` currently
   early-returns on unchanged topology — once values are on the face it must
   refresh them in place on param-only changes.
2. Per-node **collapse** toggle.
3. **In-place editing** (drag/click on the knob), porting the sidebar's edit
   logic onto the tile; emits the existing `SetGraphNodeParam`.
4. **Previews** (§6).

The right sidebar is demoted as the on-node controls land, then retired. Do not
delete it on day one — build alongside, then remove.

---

## 5. The parameter-mapping editor — unify the fragmentation

The affordances you asked about (min, max, invert, range, card output) are
**already in the backend but fragmented across three contexts that don't know
about each other**:

- **Card-expose binding** — `UserParamBinding` carries `label`, `min`, `max`,
  `default_value`, `convert`, `is_angle`. No UI edits any of it.
- **Ableton mapping** — `ned` invert flag + a range, with an actual **INV**
  button wired in `param_slider_shared.rs`.
- **Driver / envelope modulation** — `range_min` / `range_max`, `ResolvedParam`
  min/max.

Three range mechanisms, three-or-zero editors, for what is **one idea**: a
source drives a target through **[min, max, invert, curve, unit]**. That
fragmentation is why "degrees" cost threading `is_angle` through five crates —
there is no single param-mapping layer to drop a unit into.

**Target:** one param-mapping model and **one editor surface** — the knob's
in-place popover (§4) — reused across all binding contexts. Degrees, Hz, invert,
range, custom name become *fields in that one editor*, not bespoke threading.
This is the **value-scale boundary**: a face (the knob) over an interior (the
mapping). It is the same editor pattern as a group's face, one scale down.

This is the live performance instrument
([feedback_param_values_is_performance_surface]) — min/max/invert/range on a
macro is how a knob feels right under the fingers mid-show. It is
instrument-building, not authoring polish.

**Design + build plan (from the `param-mapping-editor-design` workflow,
2026-05-31).** The binding already carries the linear remap; we expose + extend
it:
- **Reuse, don't invent.** MANIFOLD already ships `MacroCurve` (Linear /
  Exponential / Logarithmic / SCurve, `macro_bank.rs`) with a pure
  `apply(t) -> [0,1]`. The binding reuses it verbatim — one curve type app-wide.
- **Model:** add `#[serde(default)] invert: bool` + `curve: MacroCurve` to
  `UserParamBinding` (`effects.rs`). serde(default) ⇒ invert=false /
  curve=Linear for every saved show, exact 1:1, zero migration (same pattern as
  `convert` / `is_angle`).
- **Apply at the one write boundary.** `ResolvedBinding::apply`
  (`param_binding.rs`) is the single per-frame point where the card slot crosses
  into the inner param. Reshape there: normalize to [0,1] within [min,max] →
  invert (1−n) → `curve.apply(n)` → scale to min+(max−min)·n → existing convert.
  **Identity early-skip** when invert=false && curve=Linear, so every existing
  binding is byte-identical to today and the per-frame write path drivers /
  Ableton / envelopes share is untouched.
- **Edit + undo.** `EditUserParamBindingCommand` mirrors
  `ToggleEffectParamExposeCommand` (locator + reverse-state); never touches the
  binding `id` (it is forever). Drag-driven min/max uses the
  snapshot/changed/commit triad (one undo entry per drag). Bumps
  `user_param_bindings_version` so the renderer re-resolves.
- **Surface:** an in-place popover off the knob (§4), built **surface-agnostic**
  so it later serves the card knob, Ableton, and driver/envelope. Reuses
  `build_trim_handles` (min/max), the Ableton INV button style, `config_btn_style`
  (curve dropdown).
- **Order:** (1) model fields, (2) renderer carry+apply behind the identity
  skip, (3) command+undo, (4) PanelAction+app_render route, (5) popover, (6)
  live verify. Steps 1–4 are byte-identical until a user opts in.

**THE STRUCTURAL PAYOFF (Peter, 2026-05-31): this editor REPLACES the
`affine_scalar` mapping nodes scattered through every graph.** Those nodes exist
*only* to remap a card slider onto a param range — the card param wires to an
`affine_scalar.a` (scale) input, not the target directly — so they are plumbing,
not signal, and they dominate the noise in complex graphs (the 3D fluid-sim
generator is mostly a tower of them). A binding's min/max **is** an affine remap
(scale = max−min, offset = min), invert is the negative scale, curve is the
non-linearity an affine can't do — so the editor's write-boundary reshape does
everything a mapping affine does and more. deg→rad affines are already redundant
via `ParamType::Angle`. **Migration (after the editor ships):** per preset, fold
each *mapping-only* affine (criterion: a card-exposed param feeds it, it does
pure scale+offset, its output drives exactly one target with no other consumers)
into its binding's min/max/curve, rewire the card param straight to the target,
delete the node. Genuine in-graph-math affines stay. Across the 46 presets this
is a fan-out **migration workflow**, one agent per graph; the payoff is every
effect/generator graph shedding its mapping tower.

**Forks (Peter's call before the visible build):** unify all three range systems
now vs card-binding-first (rec: card first — the popover is surface-agnostic so
Ableton/driver adopt it later); curve set (rec: the four `MacroCurve` shapes; a
custom/editable curve is the deferred Table widget); invert-then-curve order
(rec: yes, matches the Ableton source-invert order); unit scope (rec: defer a
full Unit enum, surface only the existing `is_angle` degrees toggle); trigger
location (wire onto the temporary sidebar rows now vs wait for on-node knobs
§4-pass-3).

---

## 6. Previews + data-flow — understand a graph at a glance

The thing that turns a graph from a wiring diagram into something you *read*.

- **Every node shows a tiny live view of its output.** Image nodes → the image.
  Control nodes (LFO, envelope, math) → a small moving trace/sparkline of their
  value, so even the "invisible" math nodes become legible.
- **Wires drift, subtly,** in the signal direction — low, slow, only on live
  wires. Never a light show; the moment it distracts it is wrong.
- **Micro-feel.** Wires snap and softly glow on connect; nodes ease open on
  expand; values glide instead of jumping. Satisfying, subtle, informative —
  the difference between a form and an instrument.

**Architecture — the authoring tap.** Per-node previews fight the fusion
compiler: once it fuses N atoms into one pass, the middle outputs don't exist as
separate images. The fix: **the editor runs an authoring version of the graph**
— unfused, tapped at every node, refreshed a few times a second — while the
performance path stays fused and fast. The editor is never the hot path (you
never patch mid-show), so it can afford to be the slow, fully-honest one. The
editor being "a bit slower because it previews" is by design and acceptable.

**The compiled/uncompiled seam (a trait, not a UI mode).** The UI must not
branch on "compiled vs uncompiled." Instead the **graph offers previews as a
capability**: the UI asks each node *"what's at your output?"* and gets
`Some(preview)` or `None`. The authoring runtime implements the tap (sees every
node); the performance compiler does not (only the final image exists). So
compiled-vs-uncompiled is "the tap is there or it isn't," never an `if` in the
UI. A new preview kind later (scalar sparkline, histogram) is one more thing the
tap can return; the fast path simply doesn't provide it. One UI, two backends
behind the seam — extends without rotting. (A base trait the two runtimes
implement is the right shape.)

---

## 7. Future boundaries this UI must already fit

The model is chosen *now* so these drop in as data, not rewrites:

- **Node groups** — collapse a selection into one boundary (its face = exposed
  ports + macros).
- **Sub-networks** — a named, saved boundary; the canvas gains **navigation
  across levels**: dive into a group, a **breadcrumb** to find the way back,
  collapse-into-group and expand-back. (This is the load-bearing future canvas
  feature — more than thumbnails.)
- **Recipes** — a boundary you drop from the palette (a saved subgraph). Lives
  in the **dynamic** half of the palette's merged source (§3b).
- **Effects / generators** — the outermost boundary; the card is its face.
- **Graph compiler / fusion** — execution detail *below* the boundary. A group
  is a UX boundary the compiler is free to fuse straight through, so grouping is
  **free** — it never costs a render target.

**Floated, not committed:** the node palette and the effect/generator browser
are two discovery UIs for the *same act* — dropping a thing into a graph (an
effect *is* a node graph; an atom *is* a node). They could become one browser
with a kind/scope toggle (atom / recipe / group / effect). A real architectural
bet; flagged here, not decided.

---

## 8. Layout — one window, panels that toggle / dock / snap

**Current:** the graph editor is a **separate OS window** (winit
`create_window`, [`app_lifecycle.rs:762`](../crates/manifold-app/src/app_lifecycle.rs#L762),
opened on Cmd+Shift+G, its own workspace + present path). It loses focus and is
hard to recover — two top-level windows competing. Other panels sit at fixed
coordinates (inspector 320px right, palette 200px left, canvas centre): arranged
*for* the user, not *by* them.

**Decided (2026-05-31) — keep the editor a first-class window, make it
well-behaved. SHIPPED.** The original pain was not "it is a separate window," it
was "it is a *badly-behaved* one that falls behind the main window, and the
hotkey then does nothing." The fundamental fix keeps the editor a normal,
first-class window (so **AltTab and Cmd-` surface it as its own window**) and
guarantees it can never get stuck:
- **Always summon to front.** `open_graph_editor` now refocuses the existing
  window instead of no-opping when already open, so Cmd+Shift+G (and the card
  buttons) always bring it forward (`Window::focus_window` + `set_minimized(false)`).
- **Remember its place.** Outer position + inner size are captured on close and
  restored on reopen (`Application::graph_editor_geometry`), so it lands where
  you left it.
- **Why not a child / owned window:** a macOS child window rides the parent and
  cannot fall behind, but it is *excluded* from AltTab and Cmd-` — it stops being
  its own window. That trades away exactly the native window-switching we want.
  First-class + guaranteed-summon gives "never stuck" without that cost, and is
  simpler (no parent-window API, touches no render or input path).

**Deferred to the layout phase — in-window docking.** The richer end-state is
still to bring the editor *inside* the main window as a dockable region, composed
with a mini timeline + the live preview, **reusing the perform-mode render-path
swap**: a master `active` flag short-circuits `tick_and_render` to a different
layout; enter/exit deferred through `about_to_wait` (window mutations need
`ActiveEventLoop` in scope); input rerouted; UI quiesced on entry and rebuilt on
exit; **content thread + output window untouched**. The `editor-in-window-audit`
workflow mapped the full additive 7-step migration (state struct → lifecycle
handler → `tick_editor_mode` into the main offscreen → input re-keyed on
`editor.active` → toggle → compose timeline+preview → delete the window), with
the GPU-surface-sharing, ProMotion-cadence, and mid-drag-leak risks called out.
That plan stays valid for when we build the dock/snap workspace — it is **not**
blocked by the window fix above. Until then the well-behaved separate window is
the right tool: it keeps the full timeline + inspector visible *alongside* the
graph, which suits authoring (two real panes, not a strip).

---

## 9. Recommended build order

1. **Window behaviour fix** (§8) — first-class window + always-summon-to-front +
   remembered geometry. **Shipped.** (In-window docking is deferred to the layout
   phase; the `editor-in-window-audit` 7-step plan is kept for it.)
2. **On-node controls** (§4), in the four passes. Read-only first to tune the
   look.
3. **Search-first, category-grouped palette** (§3b) off the descriptor backend,
   reusing the `browser_popup` pattern, built on the merged static+dynamic
   source (§7).
4. **Param-mapping popover editor** (§5) — unify the three range systems behind
   one surface.
5. **Previews + animation** (§6) — the authoring tap + the preview-capability
   trait.
6. Later: node groups / sub-networks + canvas level-navigation (§7); dockable
   layout; the unified atom/effect browser.

---

## 10. Conventions

- Voice for all copy: [feedback_product_copy_voice] — natural, readable,
  professional; no em-dashes, no semicolons, no AI-speak.
- The graph editor is **authoring, never performance**
  ([feedback_graph_editor_is_authoring_not_perform]) — these surfaces speed up
  building looks; the card is the stage face.
- Don't fork behaviour on Effect vs Generator
  ([feedback_graph_editor_unified_surface]); don't propose reverting graph
  effects to Rust to dodge complexity ([feedback_no_rust_revert_for_graph_effects]).
- `param_values` + `user_param_bindings` are the live instrument
  ([feedback_param_values_is_performance_surface]) — never "migrate them into
  the graph" as cleanup.
