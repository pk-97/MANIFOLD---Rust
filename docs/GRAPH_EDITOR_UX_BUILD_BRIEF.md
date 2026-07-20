# Graph Editor — UX Build Brief (2026-06-13)

<!-- index: Scope for the next UI/UX pass on the graph editor — the surface has fallen behind the stable compiler/runtime and reads as a dev build. -->

Scope for the next UI/UX pass on the graph editor. The compiler/runtime is stable
and fast; the editor surface has fallen behind and reads as a dev build. This brief
is the durable, code-verified scope. Every claim below was checked against the
source on 2026-06-13 (file refs inline) — where an earlier design doc disagrees,
this brief is current and that doc is flagged stale in §7.

Supersedes the status/plan sections of `UI_UX_SYSTEM_DESIGN.md` (which still says
"nothing built yet" — false now) and `NODE_GROUPS_UI_DESIGN.md` ("planned, not
started" — Phases A–C are built). Those remain useful for *rationale*; this is the
*current state + work*.

## The diagnosis

The backend is sound. The editor feels half-built because three planned moves
stalled mid-stream, leaving seams visible:

1. **On-node controls landed but the sidebar never retired** (it's staying now —
   Peter's call — so this is a convergence problem, not a deletion).
2. **The preview/live-value "authoring tap" is single-node only**, and on-node
   values are frozen between edits.
3. **Groups stopped at navigate + collapse/ungroup** — no naming, color, or macros
   on the box.

Almost everything below reduces to **two foundations** plus a short list of
discrete features:

- **Foundation A — live values into the canvas.** The editor snapshot is built by
  `GraphSnapshot::from_def` and cached on `graph_version`
  (`content_thread.rs::graph_snapshot`, ~L1249). `graph_version` only bumps on
  edits. Per-frame modulation (LFO/envelope/driver/Ableton) writes to `param_values`
  and deliberately does **not** bump `data_version` — it builds a separate
  `modulation_snapshot` for the card (`content_thread.rs` ~L958). Net: **the left
  card animates under modulation, the canvas nodes are frozen to stored values.**
  The only live thing in the canvas is the single previewed node's scalar I/O
  (`preview_scalar_io`). Fix this and sparklines + per-node previews fall out.

- **Foundation B — one `ParamType`-keyed widget registry**, shared by the right
  sidebar and the on-node face. Today the inspector only handles
  Float/Angle/Frequency/Int/Bool/Enum/Trigger; everything else collapses to a
  disabled "Other" row (`graph_editor.rs` `GraphEditorParamKind`, L44–66 + L506–517).
  The full `ParamType` set is Float/Angle/Frequency/Int/Bool/Vec2/Vec3/Vec4/Color/
  Enum/Table/String/Trigger (`node_graph/parameters.rs` L61). So **Color, Vec, Table,
  and String params are uneditable in the graph editor today** — every color swatch,
  position, gradient, and text string forces a JSON edit.

## Decisions locked (Peter, this session)

- Right-sidebar param inspector **stays** (it's useful).
- Param exposure is **direct-to-card** by stable `NodeId`
  (`ToggleNodeParamExposeCommand`, `commands/graph.rs` L949) — invariant under
  grouping. **Groups are organisation-only; no group interface params.**
- **Per-node thumbnails everywhere = yes.** Editor perf loss is acceptable (it's
  authoring, never the performance path). It's also cheap — see §3.
- **Group naming + color = yes.**
- Card **mapping drawer already exists** for effects — extend it to generators, not
  rebuild.
- **Jump-to-node from a card slider = yes** (new feature Peter asked for).

---

## 1 — Foundation A: live values on the canvas

**Problem (verified):** `graph_snapshot` caches `from_def` on `graph_version`; nodes
show stored authoring values, not live modulation.

**Work:** feed the canvas the live per-frame values the card already gets. Options to
weigh at build time: extend `modulation_snapshot` to carry the watched graph's
resolved node values, or have the single-node tap generalize to a per-node value
map. Either way the canvas `NodeView.params` must refresh from live values, not the
`graph_version`-gated snapshot. `graph_canvas.rs::set_snapshot` already refreshes
param values in place on the unchanged-topology path (L996–L1022) — the missing
piece is a live *source*, not the refresh.

**Payoff:** the canvas stops lying. Prerequisite for sparklines and per-node previews.

## 2 — Foundation B: the `ParamType` widget set

One registry keyed on `ParamType`, used by both the sidebar inspector and (later) the
on-node face. Verified node mapping:

| Widget | `ParamType` | Verified nodes |
|---|---|---|
| **Color picker** | `Color`, color-semantic `Vec3/Vec4` | the Draw/HUD family (`draw_markers/gauge/ticks/scanlines/dots/connections`), `color` (L319/327 + Vec3/Vec4), `chroma_key` (Vec3 key, L44), `matcap_two_tone`, `blinn_specular`, `fresnel_rim`, `render_filled_rects`, `blob_overlay_render`, `render_value_overlay` (~15 nodes) |
| **Gradient / color-ramp editor** | `Table` of `[pos,r,g,b]` | `gradient_ramp` (L73, up to 16 stops) |
| **XY pad** | `Vec2` (position) | `color_sample` (UV point, L50) |
| **Numeric vector editor** | `Vec3/Vec4` (direction/scale) | non-color vectors |
| **Numeric table / sequence editor** | `Table` (numeric) | `cycle_table_row`, `scalar_array_accumulator` |
| **Text field** | `String` (content) | `render_text` |
| **File/folder picker** | `String` (path) | `image_folder` |

**Build the color picker first** — it unlocks the most nodes and kills the most JSON
edits.

**Watch out (verified nuance):** widget choice is not purely the raw type. `Vec3` is a
*color* in `color`/`chroma_key` but a *direction/scale* elsewhere; `Vec2` is a
*position* in `color_sample`. The registry needs a semantic hint (a descriptor role,
or the param name) to pick color-picker vs vector-editor vs XY-pad for the same
`ParamType`. The descriptor backend is the right place for that hint.

**Corrections from verification (do not re-add these):**
- Materials `unlit/phong/pbr/cel` are **all `Float` params** (already editable); their
  color arrives via a Material port, not a `Color` param. **No material panel needed.**
- `wgsl_compute` source is a dedicated `source: String` field edited via
  `wgsl_source()` / `set_wgsl_source()` (`wgsl_compute.rs` L95, L1745–1760), **not** a
  `ParamType::String` param — so its code editor is a node-family special (§5), not a
  registry widget.
- `lut1d` consumes a `lut: Texture2D` input (L29); `linear_gradient`/`colorize` are
  `Float`-only. **None need a gradient editor** — only `gradient_ramp` does.

## 3 — Per-node previews (image thumbnails + control sparklines)

**Why it's cheap (verified):** when the editor is open, the watched effect/generator
already renders **unfused** so the tap can read inner-node textures
(`generator_renderer.rs` L104–107; `preset_runtime.rs` `set_preview_target` L2100).
Every node's output texture already exists in memory this frame — thumbnails
downsample what's there, they don't re-render.

**Today:** one preview slot. `set_preview_node` / `set_preview_target` take a single
`Option<NodeId>`; one sidebar pane shows it (`graph_editor.rs` node-output pane +
`app_render.rs` ~L2517–2880).

**Work:**
- Generalize the single-node tap to capture every (visible) node's output into **one
  atlas texture** (not N IOSurfaces), throttled (~10 fps), visible-nodes-only. The
  capture hooks into the execution walk so each output is grabbed before the texture
  pool recycles it.
- **Control nodes → sparklines**, drawn CPU-side from the scalar I/O the tap already
  produces (`preview_scalar_io`). Nearly free, and it's the other half of fixing
  frozen values (§1).

## 4 — Groups: naming + color

**Today (verified):** scope path + breadcrumb + enter/exit + marquee + Ctrl+G /
Ctrl+Shift+G are built (`graph_canvas.rs`); `GroupNodesCommand` / `UngroupNodeCommand`
exist (`commands/graph.rs` L1549/1632). **Missing:** any interface-edit command, any
rename, any color — and `GroupDef` (`effect_graph_def.rs` L269) has **no tint field**.
All groups render one fixed `GROUP_HEADER_BG`.

**Work (no interface params, per the locked decision):**
- **Inline rename** on the group header and the breadcrumb leaf → a `RenameGroupCommand`
  (the handle is the namespace: validate unique + `/`-free).
- **Per-group color/tint:** add a `tint` field to `GroupDef`, a swatch affordance, and
  read it in `draw_node`'s group branch (`graph_canvas.rs` ~L2449–2480) instead of the
  constant. Cosmetic, but it's the legibility-under-stage-pressure win.

## 5 — Instrument + node-family specials

- **Extend the mapping drawer to generators.** `CardContext::Author` + the per-row
  chevron + sideways drawer are built (`param_card.rs` L46, L99, L348, L565); generator
  rows just set `mappable=false` (L99–100). Make `gen_params_to_config` set
  `mappable=true` for generator bindings and the chevron lights up with no new UI. (The
  generator binding model may need the same scale/offset reshape the effect path has —
  confirm at build.)
- **Code editor for `wgsl_compute`** — a text/code surface wired to
  `set_wgsl_source` (it re-parses via naga and re-derives ports). This is a real live
  surface; today it's JSON-only.

## 6 — Authoring ergonomics

- **Jump-to-node from a card slider.** Data path exists: card row → its
  `UserParamBinding` → `binding.node_id` (`NodeId`) → find the matching node in the
  (recursive) snapshot → its runtime `u32` id → `canvas.select_single` + pan; if the
  node is inside a group, also set the scope path to that group. Add a small affordance
  on the card row in `Author` context.
- **Node copy/paste/duplicate within a graph.** Effect/generator-level copy/paste
  exists (`panels/mod.rs` `CopyGenerator`/`PasteGenerator`/`PasteEffects`); there is
  **no node-level** copy/paste in `graph_canvas.rs`. Add Cmd+C/V/D over the selection.
- **Connection-time type feedback.** `wire_into` (`graph_canvas.rs` L2058) only finds
  an existing wire to replace — **no type validation**; `ConnectPorts` is emitted
  unconditionally at `on_left_button_up` (L2009). Add a valid/invalid port glow during
  a wire drag (the port `kind` is already known — it colors the ghost wire).
- **Find-a-node search** for large graphs (no search exists in `graph_canvas.rs`).

## 7 — Deferred (own passes, not this brief)

- **Clip-effect card lane.** Opening the editor on a clip-level effect bails to an
  empty left lane (`state_sync.rs` ~L1217). Full fix is card-target unification Stage 3
  (`CARD_TARGET_UNIFICATION.md`) — out of scope here.
- **Save authored graph as a reusable preset / recipe** — gated on the disk-load work
  (`project_bundled_presets_swap_deferred`).
- **Pinned previews** (TD-style) — natural once §3 lands.

## Stale docs/comments to reconcile (cleanup as we build)

- `UI_UX_SYSTEM_DESIGN.md` — status line "Nothing here is built yet" is false (on-node
  controls, collapse, in-place edit, single-node preview, groups nav, popup palette,
  window behaviour all shipped). Fix the status, keep the rationale.
- `NODE_GROUPS_UI_DESIGN.md` — "planned, not started" is false (Phases A–C shipped);
  Phase D is explicitly dropped (no interface params); Phase E (naming/color) is §4 here.
- `archive/EDITOR_REORG_BUILD_BRIEF.md` — historical progress doc; mark superseded by this one
  for the editor's current state.
- Comment sweep: as each area is touched, reconcile in-code comments that describe the
  pre-convergence state (e.g. graph_editor.rs's "demote then retire the sidebar"
  framing — the sidebar is staying).

## Suggested order

1. Foundation A (live values) — smallest change, biggest "it's alive" payoff, unblocks
   §3.
2. Color picker (establishes Foundation B's registry).
3. Per-node previews/sparklines (build on A).
4. Jump-to-node + group naming/color (cheap, high-value).
5. Generator mapping drawer.
6. Remaining registry widgets (gradient/XY/vector/table/text/file) + wgsl code editor.
7. Copy/paste, connection type feedback, find-a-node.

## Verify bar

- `cargo clippy -p <crate> --all-targets -- -D warnings` before each commit.
- UI is authoring, not performance: visual inspection is the gate
  (`feedback_visual_effects_skip_gpu_parity`, `feedback_graph_editor_is_authoring_not_perform`).
- Live-value and preview work touches the content thread + snapshot path — run the
  Liveschool fixture and confirm no perform-path regression.
- Voice for any user-facing copy: `feedback_product_copy_voice` (natural, professional;
  no em-dashes, no semicolons, no AI-speak).

## Build progress — autonomous run, 2026-06-13 (branch `graph-editor-ux`)

- **Phase 1 — Live values on the canvas. SHIPPED.** `PresetRuntime::live_node_params`
  taps each watched node's post-modulation params off the running graph (stable
  `NodeId`, `&'static` names), surfaced through `Compositor` / `GeneratorRenderer`,
  carried on `ContentState::live_node_params`, overlaid each frame by
  `GraphCanvas::apply_live_values`. Skips the actively-scrubbed param; zero cost when
  no editor watches. Tested (4 canvas tests).
- **Phase 2 — Inline colour + vector editor. SHIPPED.** `ParamSnapshot::vec_value`
  carries the real multi-component value; `ParamSnapshotKind` / `GraphEditorParamKind`
  split out Color/Vec2/Vec3/Vec4. The inspector renders a swatch + R/G/B/A (colour) or
  X/Y/Z/W (vector) channel sliders; each channel scrubs and emits the whole rebuilt
  value. Node face shows a hex / component readout. Still not single-slot card-exposable
  (correct — a macro slot is one `f32`). Tested (2 panel tests).
- **Phase 3 — Sparklines SHIPPED; per-node image atlas DEFERRED.** Control-node
  legibility (the design's §6 "even the invisible math nodes become legible") ships as a
  per-node sparkline: `GraphCanvas::spark_history` rings the primary param's normalized
  value from the live tap and `draw_sparkline` traces it on the collapsed node face (only
  when it actually moves). Tested (3 canvas tests). **Deferred:** the full multi-node
  *image* thumbnail atlas (executor multi-capture → atlas texture → IOSurface bridge → UI
  sampling). It is a large GPU feature on the content/render path whose correctness is
  visual, and the project gates authoring-canvas visuals on Peter's eyes
  (`feedback_graph_editor_is_authoring_not_perform`); building it blind risks the live
  path. The existing single-node sidebar image preview remains. This is the one piece of
  the original 7-phase plan intentionally left for a pixels-in-hand session.
- **Phase 4 — Jump-to-node + group colour SHIPPED; group-rename backend SHIPPED, its UI DEFERRED.**
  *Jump-to-node:* clicking a card param's name in the editor's left lane navigates the canvas to the
  node that defines it (snapshot `outer_routings` → stable `NodeId` → `focus_node`, descending into
  groups), read-only on the card. *Group colour:* `GroupDef::tint` carried through `GroupSnapshot` to
  the canvas group header; `Cmd+T` cycles a selected group through a muted preset palette via
  `SetGroupTintCommand` (cosmetic, undoable). *Group rename:* `RenameGroupCommand` ships tested
  (validates `/`-free + sibling-unique, structural, undoable) but has **no UI trigger yet** — inline
  canvas text-editing is the missing piece (the canvas has no text-input mode today; the editor only
  routes chars to the mapping popover). Deferred with the image atlas for the pixels-in-hand session;
  the backend is ready for whatever trigger we choose (breadcrumb-leaf edit or an inspector field).
- **Phase 5 — Generator mapping drawer: ALREADY SHIPPED (verified, no code needed).** The brief
  assumed a `mappable=true` flip blocked on generators lacking per-instance bindings. Reading the
  code, the card-target unification already landed: `preset_to_config` sets `mappable: true` for
  generator rows, and the whole drawer pipeline is target-generic — `open_editor_card_mapping`,
  `full_reshape_for`, `seed_def_for`, `watched_value`, and `preview`/`commit` all branch on
  `GraphTarget::Generator`, committing through `EditParamMappingCommand::new(target, …)` against the
  generator's `preset_metadata` reshape (range/scale/offset/invert/curve). So a generator card param
  shows the chevron and the sideways mapping drawer edits + undoes exactly like an effect. Cleaned up
  the stale `ParamInfo::mappable` doc comment that still claimed generators leave it `false`.
- **Phase 6 — registry widgets: the two headline widgets shipped in Phase 2; path picker added; the
  rest DEFERRED.** The brief's highest-value widgets — the **colour picker and the vector editor** —
  already landed in Phase 2 (`ParamSnapshotKind::Color/Vec2/3/4` + the swatch / channel sliders).
  This phase adds the one further widget that is both tractable and non-visual: a **native folder
  picker** for path-like String params (`image_folder.folder`). `ParamSnapshotKind::String` is split
  out, the value shows in the inspector + node face (`string_summary`), and a path-like name
  (folder/path/file/dir) gets a Browse button → `BrowseGraphNodePath` → `rfd::FileDialog::pick_folder`
  → `SetGraphNodeParam(String)`. Tested (2 panel tests). **Deferred (pixels-in-hand / text-input
  pass):** the gradient-ramp stop editor, the response-curve editor, the numeric table/sequence
  editor, a free-text field (`render_text.text`/`fontFamily`), and the `wgsl_compute` code editor.
  These all need either a canvas text-input mode (which doesn't exist — the editor only routes chars
  to the mapping popover) or a complex draggable-stops / curve widget whose correctness is visual, and
  the project gates authoring-canvas visuals on Peter's eyes. They share the deferred bucket with the
  image-thumbnail atlas and the group-rename UI.
- **Phase 7 — connection type feedback + node copy/paste SHIPPED; find-a-node DEFERRED.**
  *Type feedback:* while dragging a wire, the ghost turns green over a compatible input port and red
  over an incompatible one (`wire_drop_compat` / `ports_compatible`, port-category equality encoded by
  colour), so a mis-wire reads before the drop; the actual connect still validates server-side.
  *Copy/paste/duplicate:* `Cmd+C` copies the selected nodes + the wires among them to a graph
  clipboard; `Cmd+V` pastes them via `PasteNodesCommand` with fresh runtime ids, fresh stable
  `NodeId`s, deduped handles (`b` → `b_2`), an editor-position offset, and re-anchored internal wires
  (external wires dropped); undo removes exactly the pasted set. Duplicate is copy-then-paste (Cmd+D
  is already the node-dump shortcut). Tested: 1 canvas compat test + 2 editing paste tests (fresh
  identity + wire remap). *Find-a-node DEFERRED:* a search box needs the same canvas text-input mode
  as group-rename / free-text params — same deferred bucket.

## Final status (autonomous run, 2026-06-13)

All 7 phases are addressed end-to-end. **Shipped:** live on-canvas values (1), colour + vector
editors (2), control-node sparklines (3), jump-to-node + group colour + group-rename backend (4),
generator mapping drawer — found already complete (5), String legibility + path picker (6),
connection type feedback + node copy/paste (7). **Deferred to a pixels-in-hand / canvas-text-input
pass** (one coherent bucket, each documented above with its precise blocker — all are either GPU/
visual features the project gates on Peter's eyes, or need a canvas text-input mode that doesn't
exist yet, so building them blind would risk the instrument): the per-node image-thumbnail atlas
(3), the group-rename UI trigger (4), and the gradient/curve/table/free-text/wgsl-code editors and
find-a-node search (6/7). Every shipped item compiles clean, passes focused tests, and is clippy-clean
on the project's standard gate.

## Follow-up run, 2026-06-14 (branch `graph-editor-ux`)

The "pixels-in-hand / canvas-text-input" bucket above is now almost entirely shipped. The
canvas text-input mode it was waiting on turned out to be buildable blind (it's keyboard +
overlay plumbing, not visual-correctness work), which unblocked everything that rode on it.

- **Canvas / inspector text-input foundation. SHIPPED (a5f743dc).** The editor window now owns
  its own key routing + overlay render for graph text fields, reusing the existing
  `TextInputState` / `render_text_input_overlay`. Four `TextInputField` graph variants
  (`GraphGroupRename` / `GraphStringParam` / `GraphWgsl` / `GraphNodeSearch`, plus
  `GraphTableCell` below) dispatch to the content thread only (the canvas renders from
  content-thread snapshots). Multiline fields (WGSL) insert a newline on Enter and commit on
  Cmd+Enter; single-line fields commit on bare Enter.
- **Group rename (F2). SHIPPED (a5f743dc).** Single-selected group, inline field anchored over
  its header (`group_rename_target` + `editor_canvas_viewport`), routes to `RenameGroupCommand`
  at the canvas scope.
- **Free-text String editing. SHIPPED (a5f743dc).** The inspector value cell for a non-path
  String param (`render_text.text`) is now click-to-edit. `ParamSnapshot` / `GraphEditorParam`
  carry the raw untruncated `string_value` (the old `summary` is lossy), so the edit
  round-trips. Commit goes through `SetGraphNodeParamCommand` with a String value.
- **WGSL code editor. SHIPPED (a5f743dc).** `wgsl_compute` nodes show an "Edit Code" button
  that opens a multiline editor over the kernel. New `SetWgslSourceCommand` mutates
  `node.wgsl_source` with undo; a blank buffer clears the override back to the built-in kernel.
  `NodeSnapshot` / `GraphEditorNodeView` now carry `wgsl_source`. (Known limitation: the
  overlay does not scroll, so a very long kernel can clip past the window bottom — a later
  polish item; short edits are fine.)
- **Find-a-node search (Cmd+F). SHIPPED (a5f743dc).** Live search dims every non-matching node
  on the canvas (drawn last in `draw_node`); Esc clears, Enter keeps the highlight.
- **Table grid editor (gradient stops + numeric sequences). SHIPPED (2a488dc4).** Makes
  `ParamValue::Table` params editable — they were read-only ("N×M"). One numeric grid covers
  `gradient_ramp` (≤16 `[pos,r,g,b]` stops), `cycle_table_row`, and `scalar_array_accumulator`.
  `ParamSnapshot` / `GraphEditorParam` carry the full `table_value`; the inspector renders a
  header + a clickable cell per entry; a cell edit rebuilds just that cell into a full `Table`
  value (`SetGraphNodeParamCommand`, target-generic). The **response-curve editor** the brief
  listed turned out to be the mapping-popover's response curve, which already ships — no
  separate widget needed. (Columns are addressed by index; the richer gradient-swatch /
  draggable-stop presentation is the visual polish below.)

All four editors are `GraphTarget`-generic, so they work identically on effect and generator
graphs (no fork — `feedback_graph_editor_unified_surface`). New unit tests cover the
`SetWgslSourceCommand` round-trip and the panel's Table/String cell emits; focused suites and
the standard clippy gate are green.

### Per-node image-thumbnail atlas (§3) — SHIPPED (8a749247), visual QA pending

Built after all. The feasibility trace turned up a simpler path than the earlier plan
assumed: the executor already has a **dump mode** (`set_dump_request` / `dump_textures`, the
same capture Cmd+D uses) that keeps every watched-effect node output alive for one frame — so
no new executor multi-capture and no new `UIRenderer` primitive were needed. What landed:

- **Content** (`content_pipeline`): while the atlas is enabled, dump mode captures every
  watched-effect node output; each is blitted into a cell of one `ATLAS_GRID`×`ATLAS_GRID`
  atlas texture (`draw_fullscreen_viewport` per cell, Load after one clear). The
  `(node_id, cell)` layout + the atlas `SharedTextureBridge` are published like the single-node
  preview.
- **Plumbing**: `ContentState::node_atlas_layout` carries the layout to the UI;
  `ContentCommand::SetNodeAtlasEnabled` toggles capture — sent on editor open, cleared on
  close, so **a live show pays nothing** (authoring-only, the same gate the brief wanted).
- **UI** (`app`): allocates the atlas bridge + a `node_thumb_pipeline` whose fragment samples
  one cell (UV sub-rect via an inline `Bytes` uniform — no UIRenderer change).
- **Present pass** (`app_render`): for each visible canvas node with a layout cell, blits its
  atlas cell into the node body. `GraphCanvas::visible_node_thumbnails` gives the body rects.

Compiles + clippy clean; `visible_node_thumbnails` geometry is unit-tested. **Visual QA the
next run can confirm/tune** (only checkable with the app running): thumbnail placement fills
the body below the header and may overlap on-face param text; cells are raw blits, so non-colour
outputs (depth/normal/scalar) read dark — per-encoding "smart" thumbnails are a later pass.

**Effect/generator parity (aebf2cf5).** The first cut shipped effect-only because the
executor's dump was only exposed on the `Compositor` (effect) side; the generator
`PresetRuntime` has the same `set_dump_all` / `dump_textures_all` but no passthrough.
`GeneratorRenderer` gained `set_dump` / `dump_textures`, and `content_pipeline` packs a watched
generator's dump into the same atlas in the generator capture block (the effect block is now
gated on a watched effect so it can't clobber the generator's layout). Exactly one of
effect/generator fills the atlas per frame — they're mutually exclusive. The editor now behaves
identically for both (`feedback_graph_editor_unified_surface` / `feedback_effect_generator_binding_parity`).
