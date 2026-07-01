# Graph Editor — Layout & Node Redesign (2026-07)

<!-- index: The bold graph-editor redesign: compact content-sized nodes, previews-as-anchor with a filmstrip LOD, resizable panels via a dock_* splitter widget, typed-color wires, and a bottom mini-timeline for time-based authoring. Supersedes the "polish only" guardrail of GRAPH_EDITOR_UX_BUILD_BRIEF for the editor's layout/node anatomy. Working HTML mock in docs/mockups/graph-editor-redesign.html. -->

Status: **direction agreed 2026-07-01, not yet built.** This is a deliberate step past
the earlier polish pass. `GRAPH_EDITOR_UX_BUILD_BRIEF.md` and
`GRAPH_AND_UI_POLISH_PLAN.md` said "polish only, don't propose a new visual language."
Peter reopened that for the editor's **layout and node anatomy** — those two docs stay
authoritative for the effect-card / modulation visuals, which do not change here.

**Reference mock:** [`docs/mockups/graph-editor-redesign.html`](mockups/graph-editor-redesign.html)
— open in a browser. Interactive: drag dividers, drag the zoom slider (watch the filmstrip
LOD), scrub the bottom strip, drag node + card sliders. It is a *concept* artifact; final
look is tuned in the bitmap renderer, not HTML (per `feedback_graph_editor_is_authoring_not_perform`).

---

## The problem (root cause)

The editor has the features but they fight each other. The screenshot that opened this
work showed a 12-node graph as an unreadable knot at 25% zoom. That is not missing
features — it is oversized nodes:

- `NODE_WIDTH = 300`, and always-on preview thumbnails make each node ~200–300px tall
  (header 22 + preview ~160 + body + `18 × port_rows`). [mod.rs:88](../crates/manifold-ui/src/graph_canvas/mod.rs#L88), [model.rs:182](../crates/manifold-ui/src/graph_canvas/model.rs#L182)
- A 6-column graph is ~2000px wide. In the middle-column canvas it can only `zoom_to_fit`
  down to its **0.25 floor**. [camera.rs:104](../crates/manifold-ui/src/graph_canvas/camera.rs#L104)
- At 0.25 the LOD hides body text (`PARAM_LOD_ZOOM = 0.5`) but **port labels are not
  LOD-gated** and are pinned to a 7px floor — so they overlap into mush. [render.rs:628](../crates/manifold-ui/src/graph_canvas/render.rs#L628)

Big nodes → forced to the zoom floor → the one thing that should vanish (labels) is the
one thing still drawn.

## ⚠ Feedback 2026-07-01 (after first build) — direction correction, 1–3 FIXED

Peter reviewed the three shipped increments (columns, label-LOD, 210px nodes) in
the running app and raised four issues. **These override the LOD/filmstrip
principle below** — do not re-argue it. Issues 1–3 are now fixed (2026-07-01);
issue 4's mini-timeline landed the same pass. Issue 4's "hide-unused sockets" and
the remaining steps (typed wires, inline slider restyle) are still open.

1. **NO hiding text on zoom. — FIXED.** The label-LOD gate (`51a32b8a`) was reverted
   and the pre-existing `show_text` / `PARAM_LOD_ZOOM` gating deleted. All node text
   (title, params, port labels) now scales linearly with zoom — the `.max(7.0)` font
   floors are gone (they caused the mush by stopping scale-down) — and is elided to
   its box via the shared `graph_canvas::model::{elide_to_width, text_width}` helpers
   (one source for the summary/param/port/title truncation the face repeats). Text is
   never dropped, only scaled + clipped.
2. **Doubled resize divider. — FIXED.** `Dock::draw` now draws **one** strip per edge
   whose width+colour change with state: a 1px `DIVIDER_COLOR` line at idle, the full
   `DOCK_HANDLE_W` band in `RESIZE_HANDLE_HOVER`/`_DRAG` centred on the same seam. No
   more band-plus-seam = two bars.
3. **Port labels must stay. — FIXED** (covered by #1; labels always draw, elided to
   half the node width so a long name clips instead of crossing columns).
4. **Bottom mini-timeline. — DONE (chrome + scrub + minimap).** New
   `manifold_ui::MiniTimeline` widget (stateless drawer + geometry, mirrors `Dock`):
   readout + play/pause button, bar ruler, clip minimap (every layer a thin row,
   clips coloured via the shared `get_clip_color`), draggable playhead. The `Dock`
   bottom edge is enabled by default (`show_bottom = true`, `bottom_h = 150`); the
   canvas gives up its height for it. Data via `app_render::mini_timeline_data`
   (shared by the live present pass + headless snapshot). Input: press in the strip
   body scrubs (`SeekToBeat`), press on the play button toggles `Play`/`Pause`;
   scrub-drag state on `Workspace.timeline_scrubbing`.
   - **Full-width refinement (2026-07-01):** the strip now spans the *entire*
     bottom edge (`DockRects.bottom` = `area.x, area.width`); the left/right
     columns are shortened to `canvas_h` and sit *above* it (`DockRects.left/right`
     height = `canvas_h`, vertical seams stop at the strip). Sidebar/card layout
     reads `canvas_height` (not `logical_h`) so the columns lift off the strip.
   - **Spacebar toggles play/pause** in the editor window (`editor_keyboard_input`
     nav block, reached only when no text field/popover is active) — same
     `Play`/`Pause` as the strip's button.

Still open from this feedback: **hide-unused sockets + "+N" chip** (issue 4's other
half — filter unused ports at NodeView construction; ports are name-keyed so
index-safe), then steps 4–5 (typed wires, inline slider restyle).

## The direction (Blender-informed)

> **SUPERSEDED on the LOD point** by the feedback block above: principle #3 no
> longer holds — text scales with zoom and clips, it is never hidden.


The lesson from Blender: a graph is legible at one zoom when nodes are **sized to content**.
Applied here, with our video-tool needs kept:

1. **Nodes are compact and content-sized** (~180px wide, height = header + preview +
   *shown* sockets). Not a fixed 300px block.
2. **Previews stay always-on — they are the node's anchor.** For a video tool, seeing each
   stage is the point (Peter's locked call, correct). Zoom hides *text*, never the image.
3. **Filmstrip LOD:** zoom out → labels and value widgets drop away, previews stay and
   enlarge slightly. A big graph reads as a chain of live thumbnails. Zoom in → controls
   return. (This is the earlier "semantic zoom" idea, corrected so it never hides previews.)
4. **Hide unused sockets** + a "+N hidden" chip (Blender `Ctrl+H`). Kills the label mush at
   the source (Generator Input shows 2 wired outputs, not 7).
5. **The node face is also its inline inspector.** On-node numeric scrub already exists
   (`DragMode::ParamScrub`); restyle it as a real slider track and distinguish connected
   sockets (solid dot) from param sockets (hollow dot + widget). Rich editors
   (color/vec/table/string) stay in the right sidebar.
6. **Typed-color wires** — wire takes a gradient from its source socket type to its target
   type. Trace signal by color. (Socket *dots* are already typed; wires are not.)
7. **Resizable panels** so the user grows the area they are working in.
8. **A bottom mini-timeline** — scrub time + a clip minimap — because generators/effects
   animate over time and you author to a frame.

## The `Dock` widget — SHIPPED (build step 1, 2026-07-01)

A real GUI-crate widget ([`manifold-ui/src/dock.rs`](../crates/manifold-ui/src/dock.rs)),
not a copied pattern. Each panel owns **one px number**; the canvas absorbs the rest.
Window-resize just works (no ratios to re-clamp).

**Shape refinement vs the original sketch.** The sketch was three chained
`dock_left/right/bottom(area, &mut w) -> (panel, rest)` calls. What shipped is a
single **`rects(area) -> DockRects`** that returns every panel + canvas + handle
rect together. The chaining form's whole value was consistency; a bundled `rects()`
delivers that more directly — it can't be called in the wrong order, and all five
consumer sites (render, `editor_canvas_viewport`, the headless PNG path, and the two
pointer handlers) get identical geometry from one method. In a two-pass
(render/input) architecture that's strictly harder to misuse than three chained calls
repeated across three files. Same spirit ("one API you can't fuck up"), better fit.

```rust
// Geometry — one call, every rect:
dock.rects(area) -> DockRects { left, right, bottom, canvas,
                                left_handle, right_handle, bottom_handle }
dock.canvas(area) -> Rect      // convenience for viewport-only consumers

// Interaction — mirrors the main split's triad:
dock.hit_test(area, pos) -> Option<DockEdge>
dock.set_hover_from(area, pos)          // hover highlight + resize cursor
dock.begin(edge) / dock.drag(area, pos) / dock.end()
dock.cursor() -> Option<TimelineCursor>

// Draw — one call from the render pass:
dock.draw(area, &mut dyn Painter)       // thin seam always, band on hover/drag
```

Editor use: state is one `manifold_ui::Dock` on the editor `Workspace`
([workspace.rs](../crates/manifold-app/src/workspace.rs)), seeded by `Dock::editor()`.
Render + both pointer handlers read `ws.dock.rects(area)`; the old fixed
`SIDEBAR_WIDTH` / `EDITOR_CARD_LANE_WIDTH` constants are now just the default seed
(single number in `dock.rs`). Left + right columns are live; the **bottom edge is
built but disabled** (`show_bottom = false`) until the mini-timeline (step 6) fills it.

Reused from the main UI, no new theme: `RESIZE_HANDLE_IDLE/HOVER/DRAG`,
`DIVIDER_COLOR`, `INSPECTOR_RESIZE_HANDLE_WIDTH`, `TimelineCursor::ResizeHorizontal/
Vertical`. The main UI's `timeline_split_ratio` / `inspector_width` splits
([layout.rs](../crates/manifold-ui/src/layout.rs)) can migrate onto the same widget
later.

## New: the mini-timeline

Not the full track editor — a compact strip: a ruler with a draggable playhead, and a
clip minimap (every layer squashed to a thin row, clips colored by layer). Mostly assembly
from existing parts:

- beat↔pixel via the shared `Axis` (the graph canvas already uses it — [camera.rs](../crates/manifold-ui/src/graph_canvas/camera.rs)).
- `scrub_to_time` on the timeline host for the scrub.
- per-layer clip colors from `bitmap_painter::get_clip_color`.
- playhead render like the main timeline.

## Full diff: mock vs current Rust

### 1. Window layout & panels
| Aspect | Current Rust | Mock | Action |
|---|---|---|---|
| Columns | Fixed left sidebar + canvas + right card | Same 3 columns | — |
| Resize columns | No — hardcoded widths ([graph_editor.rs:225](../crates/manifold-ui/src/panels/graph_editor.rs#L225)) | Drag dividers, each panel owns 1 px value | **New: `dock_*` widget** |
| Bottom timeline | None in editor | Scrub strip + clip minimap + play | **New** |
| Left pane | 2 stacked 16:9 previews (Node Output + Master Out) | Same | — |
| Hide panels | No | Toggle | New (minor) |

### 2. Node anatomy & sizing
| Aspect | Current Rust | Mock | Action |
|---|---|---|---|
| Node width | `NODE_WIDTH = 300`, fixed | ~184, content-sized | **Shrink + size to content** |
| Previews | Always-on thumbnail atlas | Always-on (kept) | — |
| Unused sockets | All shown → label mush | Hidden + "+N hidden" chip | **New: hide-unused** |
| Collapse | Chevron folds params | ▾ to title bar | Minor tweak |
| Node height | header + preview + all ports + body | header + preview + shown ports | Falls out of above |

### 3. Params & editing
| Aspect | Current Rust | Mock | Action |
|---|---|---|---|
| On-node scrub | Yes (`ParamScrub`, [interaction.rs:487](../crates/manifold-ui/src/graph_canvas/interaction.rs#L487)) | Yes | — |
| Widget look | Value row + 2px bar | Slider track + fill + centered value | **Restyle** |
| Connected vs param socket | Not distinguished on face | Solid dot vs hollow dot + widget | **New** |
| Rich editors (color/vec/table/string) | Right inspector | Not in mock | Keep in sidebar |
| Right effect card | Full card, draggable | Full card, draggable | — |

### 4. Sockets & wires
| Aspect | Current Rust | Mock | Action |
|---|---|---|---|
| Socket dot color | Typed (`port.color`) | Typed | — |
| Wire color | Single base + feedback color ([render.rs:698](../crates/manifold-ui/src/graph_canvas/render.rs#L698)) | Gradient source→target type | **Type-color wires** |
| Feedback wire | `RETURN_WIRE_COLOR`, arcs around bodies | Dashed | Merge (keep the arc) |

### 5. Zoom & level-of-detail
| Aspect | Current Rust | Mock | Action |
|---|---|---|---|
| LOD | Hides body text < 0.5 | Filmstrip: drop all text, previews stay + enlarge | **Extend LOD** |
| Port labels at low zoom | Still drawn (7px floor) → overlap | Dropped at far tier | **Fix (gate by LOD)** |
| Fit on open | Yes, floor 0.25 | Yes | Keep (nodes now fit) |
| Zoom range | 0.25–4.0 | 0.35–1.4 | Keep Rust range |

### 6. Time & playback
| Aspect | Current Rust | Mock | Action |
|---|---|---|---|
| Scrub in editor | No | Drag strip → playhead | **New** (reuse `Axis` + `scrub_to_time`) |
| Clip minimap | No | Layers squashed, clip colors | **New** (reuse `get_clip_color`) |
| Play/transport | No | ▶ auto-advance | New (optional) |

### 7. In current Rust, NOT in the mock — must preserve
| Feature | Note |
|---|---|
| Wire hover-highlight + dim | Keep — mock lacks it |
| Long-wire arc-around bodies (`skip_bump`) | Keep |
| Connection-time green/red ghost | Keep |
| Sugiyama auto-layout (Cmd+L) | Keep |
| Groups: enter, color (Cmd+T), rename (F2) | Keep |
| Find-a-node (Cmd+F), copy/paste (Cmd+C/V/D) | Keep |
| Control-node sparklines | Keep — fold into the node face |

## Build order (proposed)

1. ✅ **`Dock` splitter widget** in the GUI crate + editor consuming it (resizable
   columns). **DONE 2026-07-01** — `dock.rs` + `Workspace.dock`; render/input/headless
   all read `dock.rects(area)`; left+right live, bottom stubbed for step 6.
2. **Compact content-sized nodes** + **hide-unused sockets** + **gate port labels by LOD**
   (this is the mush fix). Biggest legibility win. **PARTIAL 2026-07-01:**
   - ✅ gate port labels by LOD (51a32b8a) — dots stay, labels drop below 0.5 zoom.
   - ✅ compact nodes: `NODE_WIDTH` 300→210, `COL_SPACING` 360→270 (fb22348c) —
     FluidSimulation now fits at 51% (was 38%), so labels are legible at default fit.
   - ⬜ **hide-unused sockets** + "+N" reveal chip — still TODO. The "Inputs" node
     shows 9 outputs when ~2 are wired; filter unused at `NodeView` construction
     (`set_snapshot` has `level_wires` in scope; ports are name-keyed so indices stay
     safe), with a per-node reveal toggle so you can still wire a currently-unwired
     input. This is the last height lever + has real chip interaction surface.
   - ⬜ true per-node content-sizing (width from longest label) — refinement past
     the uniform 210.
3. **Filmstrip LOD** (previews stay + enlarge, text drops).
4. **Typed-color wires** (keep hover-dim + arc routing).
5. **Inline param slider restyle** + connected/param socket distinction.
6. **Mini-timeline** (scrub + clip minimap), reusing `Axis` / `scrub_to_time` / `get_clip_color`.

## Verify bar

- UI is authoring, not the performance path: visual inspection is the gate
  (`feedback_graph_editor_is_authoring_not_perform`, `feedback_visual_effects_skip_gpu_parity`).
- The mini-timeline touches the content-thread scrub path — run the Liveschool fixture,
  confirm no perform-path regression.
- `cargo clippy -p <crate> --all-targets -- -D warnings` before each commit.
- Unified editor: everything works identically on effect and generator graphs — no fork
  (`feedback_graph_editor_unified_surface`).
