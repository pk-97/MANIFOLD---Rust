# Node Groups — Editor UI/UX Spec

<!-- index: Editor UI/UX spec for node groups (snapshot nesting, navigation, group/ungroup). Phases A-C shipped, D dropped, E pending. -->

Status: **Phases A–C shipped; D dropped; E pending** (authored 2026-06-02, status
corrected 2026-06-13). Phases A–C (snapshot nesting, read-only navigation +
breadcrumb, marquee select + collapse/ungroup) are built. **Phase D (interface
editing) is dropped** — groups are organisation-only, exposure stays direct-to-card
(Peter, 2026-06-13). Phase E (naming + color) is scoped in
`docs/GRAPH_EDITOR_UX_BUILD_BRIEF.md` §4. Builds on the backend in
`NODE_GROUPS_DESIGN.md` (schema + `flatten_groups`). Companion: `NODE_GRAPH_SYSTEM.md`,
`CARD_TARGET_UNIFICATION.md`.

The bar is professional node-editor UX — TouchDesigner / Blender for the network model,
Ableton (racks + macros) for the parameter interface. Not a toy.

---

## 1. Scope

The backend lets a group *exist, flatten, and run*. There is no way to see, make, enter, or edit
one in the canvas. This spec is that layer: the **embedded-group editor experience**.

**In scope:** seeing a group as a box; entering/exiting it; collapsing a selection into a group
with an auto-inferred interface; ungrouping; editing the interface (ports + exposed params);
naming. All for groups embedded in a single effect/generator document.

**Out of scope (later layers, unchanged by this):** recipes — saving a group to its own file, a
recipe browser, linked-vs-local instances, versioning. Those sit on top; this spec stays
embedded-only. Where a choice here would wall recipes off, §11 flags it.

---

## 2. The linchpin: the editor already lives in the document

The editor canvas is **not** driven by the flattened runtime graph. The content thread builds
`GraphSnapshot::from_def(&EffectGraphDef)` and pushes it to the UI ([content_thread.rs] caches it
as `Arc<GraphSnapshot>`; [app_render.rs] hands it to `GraphCanvas::set_snapshot`). Mutations flow
back as `PanelAction`s → editing `Command`s that mutate the `EffectGraphDef`. Flattening happens
*separately*, at runtime load (`into_graph`).

So the editor's source of truth is already the **authoring document** — which, with the backend,
can contain `group` nodes with nested bodies. Groups are therefore representable with **no new
editor architecture**: extend the snapshot to carry the nesting, teach the canvas to navigate and
render it, and add two restructuring commands. That's the whole shape.

This is the single most important fact in this document. It means the work is additive on three
existing layers (snapshot / canvas / commands), not a rebuild.

---

## 3. Mental model: the canvas becomes a network navigator

Every professional node tool models a group as *a sub-network you descend into*:

- **TouchDesigner** — Base/Container COMPs are subnetworks. `i` enters, `u` goes up; a path bar
  (`/project1/geo1`) shows depth; `In`/`Out` operators are the COMP's connectors;
  right-click → *Collapse Selected* makes one; a `.tox` is the saved component (the recipe analog).
- **Blender** — `Ctrl+G` groups a selection (auto-creating *Group Input* / *Group Output* nodes
  wired from the boundary); `Tab` enters/exits; the group shows as one node carrying the group's
  sockets; `Ctrl+Alt+G` ungroups; dragging to the empty socket on Group Input/Output adds a port.
- **Ableton** — `Ctrl+G` wraps devices into a Rack; the Rack's **8 macros** map to inner-device
  params. That macro layer is exactly our **interface params**: a small named surface on the box
  that drives chosen inner knobs. This is the best reference for our exposure model.
- **Resolume** — less of a node model, but the lesson holds: name and colour-code your groups so
  the structure is legible *at a glance under stage pressure*.

We adopt: descend-to-edit, a breadcrumb path bar (TD), auto-inferred interface on collapse
(Blender/TD), Group Input/Output boundary nodes (Blender/TD), macro-style exposed params on the box
(Ableton), `Ctrl+G` / `Ctrl+Alt+G` / double-click-enter / `Esc`-exit, and colour/name for
legibility (Resolume). None of this is invented; it's the conventions performers already know.

The canvas gains one piece of state: a **scope path** — the list of group node ids you've descended
into (`[]` = the effect's root graph, `[5]` = inside group node 5, `[5, 2]` = a nested group). The
canvas renders the graph *level at that path* and a breadcrumb of the names along it.

---

## 4. The interaction set (the actual UX)

Each item notes the reference convention and the concrete behaviour.

### 4.1 Select — marquee multi-select
Today the canvas is single-select (`selected: Option<u32>`). Collapse needs a set. Add:
- **Marquee**: left-drag on empty canvas draws a selection rectangle; nodes intersecting it select.
- **Modifiers**: `Shift`+marquee/click adds; `Alt`/`Cmd`+click toggles one.
- Selection is the current scope's nodes only (you can't select across group boundaries).
- Visual: selected nodes get the existing selected border; marquee is a translucent rect.

### 4.2 Collapse to group — `Ctrl+G`
The headline gesture. With ≥1 node selected:
1. Compute the interface by **inference** (§6): wires crossing the selection boundary become input
   / output ports; inner nodes' currently-exposed params carry over as the group's interface params
   (so card exposure survives collapse).
2. Replace the selection with a single **group node** placed at the selection's centroid; the
   selected nodes move into its body; boundary wires re-anchor to the new ports.
3. The group gets an auto name (`Group 1`, deconflicted) that's immediately inline-editable — the
   name is the handle, i.e. the namespace, so it must be unique and `/`-free.
4. Undoable in one step.

### 4.3 Enter / exit — double-click, `Tab`, breadcrumb, `Esc`
- **Enter**: double-click a group node (or select + `Tab` / `i`) → scope path pushes the group; the
  canvas swaps to its body and auto-frames it.
- **Breadcrumb** bar across the canvas top: `Bloom ▸ soft_focus ▸ inner`. Click any segment to jump
  to that depth. The leaf segment is inline-editable (rename the current group).
- **Exit**: `Esc` / `Tab` / click the parent breadcrumb / an explicit *up* affordance → scope pops.
- Navigation is **UI-local and instant** — no content-thread round-trip (§8). A performer/author
  flicking in and out of groups must feel zero lag.
- A faint tint or depth indicator on the canvas while inside a group (TD-style) so you always know
  you're not at the root.

### 4.4 The group node, collapsed (the box)
A group node renders distinctly from an atom:
- A header band in a **group tint** (Blender's green-header cue) with the group **name** and a small
  *network/stack* glyph plus an **enter** chevron.
- **Interface input ports on the left, output ports on the right** — drawn from the group's
  interface, so it wires exactly like any node.
- The collapsed face shows a short **exposed-param summary** (reusing the existing collapsed-node
  summary line) — the "macros" at a glance.
- Hover reveals the enter affordance; double-click anywhere on the body enters.

### 4.5 Inside a group — Group Input / Output nodes
When the scope is inside a group, the body shows two special boundary nodes (Blender/TD):
- **Group Input** (left edge): its *outputs* are the interface input ports; inner nodes wire *from*
  it.
- **Group Output** (right edge): its *inputs* are the interface output ports; inner nodes wire
  *into* it.
- These are **render-only** — they never become runtime primitives (the flattener folds them, per
  the backend spec). `from_def` synthesizes their ports from the interface declaration; no registry
  entry.
- **Add a port by wiring** (Blender's gesture): each boundary node shows a trailing empty `+`
  socket; dragging an inner output to Group Output's `+` adds a new output port (named from the
  source, dedup-suffixed); dragging Group Input's `+` to an inner input adds an input port. Emits an
  interface-edit command.
- Ports are renamable (inline) and removable (delete the wire to the boundary, or a context action).

### 4.6 Ungroup / dissolve — `Ctrl+Alt+G`
Inverse of collapse: with a group node selected, inline its body into the parent scope and re-anchor
the boundary wires to what they connected to inside. Inner handles lose the group prefix; positions
restore around where the group sat. One-step undoable. `group` then `ungroup` is identity (§9 test).

### 4.7 Naming, colour, framing
- **Name**: inline-edit on the breadcrumb leaf or the node header. Validated unique + `/`-free;
  rejected names shake/red-flash rather than silently no-op.
- **Colour/tint** (optional, Resolume/TD): a per-group accent for live legibility. Stored on the
  group node (a `tint` field); purely cosmetic.
- **Framing**: collapse frames nothing (you stay put); enter auto-frames the body; `F` frames
  selection/all (match the existing canvas framing if present).

---

## 5. Architecture — three additive layers

### 5.1 Data — snapshot carries the nesting
`crates/manifold-renderer/src/node_graph/snapshot.rs`:
- `NodeSnapshot` gains `group: Option<Box<GroupSnapshot>>`.
- New `GroupSnapshot { interface: InterfaceSnapshot, nodes: Vec<NodeSnapshot>, wires: Vec<WireSnapshot> }`
  (recursive — nested groups fall out).
- `InterfaceSnapshot` carries the input/output port names+types and the interface params (name →
  inner target + value) for the box face and the boundary nodes.
- `GraphSnapshot::from_def` recursion: for a node whose `def.group` is `Some`, set the
  `NodeSnapshot`'s `inputs`/`outputs` from `interface.{inputs,outputs}`, recurse `from_def` on the
  body into `NodeSnapshot.group`, and synthesize `group_input`/`group_output` body nodes' ports
  from the interface. Groupless defs produce today's exact snapshot (additive).

The snapshot is `Send` owned data, rebuilt only on `data_version` change and cached behind `Arc`
([content_thread.rs] `CachedGeneratorGraphSnapshot`), so carrying the full tree costs per-edit, not
per-frame.

### 5.2 Canvas — navigation, selection, rendering
`crates/manifold-app/src/graph_canvas.rs`:
- **Scope path** `Vec<u32>` + a helper that walks `Arc<GraphSnapshot>` to the current level.
- **Marquee**: new `DragMode::Marquee { origin }`; `selected: Option<u32>` → `selected: AHashSet<u32>`
  (audit every read — `selected_node_id`, `request_delete_selected`, draw highlight).
- **Group rendering**: a node-kind branch in `draw_node` for group nodes (tint header, side ports,
  enter chevron, exposed-param summary).
- **Group Input/Output rendering** when inside a group, with the `+` socket.
- **Breadcrumb** bar render + hit-testing at the canvas top.
- **Double-click** detection (the canvas already tracks a double-click distance threshold) → enter;
  `Esc`/`Tab` → exit; breadcrumb click → set scope.
- **New actions** (UI-local where possible): `EnterGroup`/`ExitGroup` are local (mutate scope, no
  command); `GroupSelection`, `Ungroup`, `AddInterfacePort`, `RenameGroup`, `RenameInterfacePort`
  become `PanelAction`s. Every *existing* mutation action (`AddGraphNodeAt`, `MoveGraphNode`,
  `ConnectPorts`, `RemoveGraphNode`, `DisconnectPorts`, `SetGraphNodeParam`) gains the current
  **scope path** so it targets the right sub-graph.

### 5.3 Commands — scope-aware mutation + restructuring
`crates/manifold-editing/src/commands/graph.rs`:
- Existing commands gain a `scope_path: Vec<u32>`; their `apply` descends to that sub-graph of the
  `EffectGraphDef` before mutating. (Root path `[]` = today's behaviour, so this is backward
  compatible.)
- **New** `GroupNodesCommand { scope_path, selected: Vec<u32>, handle, centroid }`,
  `UngroupNodeCommand { scope_path, group_node_id }`, `RenameGroupCommand`,
  `AddInterfacePortCommand` / `RenameInterfacePortCommand`. Each is reversible for undo (store the
  pre-image needed to invert — for group/ungroup, that's the affected sub-graph slice).
- The heavy logic lives in pure-core helpers (§6), so the commands are thin wrappers that locate the
  sub-graph and call them.

---

## 6. The pure-core restructuring helpers (the testable heart)

New module `crates/manifold-core/src/group_edit.rs` — pure data, no GPU/renderer/registry, unit-
testable exactly like `flatten`. The "magic" of collapse lives here, isolated and provable.

```rust
/// Infer a group interface from a selection within one graph level.
pub fn infer_interface(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    selected: &BTreeSet<u32>,
) -> InferredInterface;   // { inputs, outputs } with the inner endpoints they map to

/// Collapse `selected` (within the level) into a new group node `handle`.
/// Returns the rewritten level: the group node replaces the selection, its body
/// holds the selected nodes + group_input/output + rewired internal wires, and
/// boundary wires re-anchor to the new ports. Carries inner exposed_params up as
/// interface params.
pub fn group_selection(
    nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>,
    selected: &BTreeSet<u32>, handle: &str, centroid: (f32, f32),
) -> Result<(Vec<EffectGraphNode>, Vec<EffectGraphWire>), GroupEditError>;

/// Inverse: inline a group node's body back into the level, re-anchoring boundary wires.
pub fn ungroup(
    nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>, group_node_id: u32,
) -> Result<(Vec<EffectGraphNode>, Vec<EffectGraphWire>), GroupEditError>;
```

### Interface inference algorithm
For a selection `S` over a graph level:
- Wire `a → b`, `a ∉ S`, `b ∈ S`: a **boundary input**. Create one group input port per distinct
  inner sink `(b, to_port)`; name it from `to_port` (dedup-suffix on collision). Re-anchor: external
  `a → group.inN`; inside, `group_input.inN → b`. Two externals into the *same* inner sink is
  illegal anyway (an input takes one source) — so one port per inner sink is exact.
- Wire `a → b`, `a ∈ S`, `b ∉ S`: a **boundary output**. Create one group output port per distinct
  inner source `(a, from_port)`; inside `a → group_output.outN`; external `group.outN → b`
  (fan-out across multiple external consumers of the same inner source — one port, many external
  wires). Name from `from_port`.
- Wire both ends in `S`: stays in the body. Both ends out: stays in the parent.
- **Param carry-over**: for each selected node param currently in `exposed_params`, add an interface
  param routing the group name → that `(inner_handle, param)` and drop the inner `exposed_params`
  entry (the group now owns the exposure). This keeps Ableton/MIDI/driver bindings working: after
  flatten the handle is prefixed and the outer card's binding targets the prefixed handle (the
  backend spec's performance-surface guarantee).

### Errors (`GroupEditError`)
`EmptySelection`, `SelectionNotConnected` (allowed — a group can hold disjoint nodes; *not* an error,
just noted), `ReservedHandleChar`, `DuplicateHandle`, `NotAGroup` (ungroup target isn't a group),
`UnknownNode`. Keep it structured for tests.

### The property that proves it
`ungroup(group_selection(level, S, h)) ≅ level` — collapse then dissolve returns the original level
up to node-id renumbering and handle prefixing. A pure-CPU round-trip test, the analog of the
backend's `grouped_equals_handwired`. Plus: flattening the grouped form equals flattening the
original (collapse changes authoring shape, never runtime behaviour) — ties this spec to the backend
flattener as the cross-check.

---

## 7. Reference-grade details that separate "works" from "professional"

- **Inference must feel right or nobody groups.** Prototype §6 first (the spike Peter flagged):
  collapse a real 6–8-node selection and eyeball the inferred ports. Wrong-feeling ports kill the
  feature. Verify before committing to the command surface.
- **Instant navigation.** Enter/exit is UI-local; never block on the content thread. Cache the
  frame; don't rebuild on descend.
- **Naming is load-bearing, not chrome.** The name *is* the namespace. Inline-edit, validate live,
  reject duplicates/`/` visibly.
- **Exposed params = macros.** Treat the group's interface params as Ableton macros: the small
  surface that reaches the outer card. The existing per-node expose checkbox should, inside a group,
  add/remove interface params rather than card bindings directly.
- **Undo is one step per gesture.** Group, ungroup, add-port each undo atomically — performers
  experiment fast and `Ctrl+Z` constantly.
- **Don't fork Effect vs Generator.** Per `feedback_graph_editor_unified_surface`, the group UX must
  be identical whether the host is an effect or a generator. The snapshot/canvas/command path is
  shared, so this holds by construction — guard it with a test on both.
- **Legibility under pressure.** Colour + name + breadcrumb exist so that the night before a gig a
  busy graph reads as a few labelled boxes. That's the whole point (it's why we built this).

---

## 8. Data flow (fits the two-thread model, no new shared state)

- Content thread builds the **full nested** `GraphSnapshot` via `from_def` on `data_version` change,
  caches it `Arc`, pushes to UI (existing path, now recursive).
- UI navigates the tree **locally** (scope path) — entering a group is a pointer walk, not a message.
- Mutations emit `PanelAction`s carrying the **scope path**; [app_render.rs] maps them to scope-aware
  `Command`s → `ContentCommand::Execute` → the content thread mutates the `EffectGraphDef` at that
  path → `data_version` bumps → new snapshot. The existing `EditingService`/`UndoRedoManager` path is
  unchanged; no `Arc<Mutex>`, no model writes from UI.

---

## 9. Testing strategy

- **Pure-core (`group_edit.rs`)** — the provable heart, CPU-fast: `infer_interface` on every boundary
  shape; `group_selection` produces expected ports/handles/wires; `ungroup` inverts; the
  `ungroup∘group ≅ id` round-trip; param carry-over; nested collapse; error cases. This is where
  correctness is *guaranteed*.
- **Snapshot (`from_def`)** — a grouped def produces the expected nested `GraphSnapshot` (group node
  ports = interface, body recursed, boundary nodes synthesized). CPU.
- **Cross-check with backend** — `flatten(group_selection(level)) == flatten(level)`: collapsing
  never changes what runs. Reuses the landed flattener.
- **Canvas interaction** — visual inspection, per `feedback_graph_editor_is_authoring_not_perform`
  and `feedback_visual_effects_skip_gpu_parity`: load a hand-authored grouped preset, navigate,
  collapse a selection, ungroup, edit ports, on both an effect and a generator. The canvas is an
  authoring surface; Peter's eyes are the gate there, not pixel parity.

---

## 10. Build sequence for the ultracode session

Each phase is independently shippable and gated.

- **Phase A — Snapshot nesting.** `GroupSnapshot`/`InterfaceSnapshot` + `from_def` recursion + group
  node ports. Gate: snapshot unit tests; renderer compiles.
- **Phase B — Read-only navigation.** Canvas renders group boxes; double-click enter, breadcrumb,
  `Esc` exit; scope path; group/boundary rendering. *No mutation.* Gate: load a hand-authored grouped
  preset and navigate it (visual).
- **Phase C — Restructure.** `group_edit.rs` (infer/group/ungroup + full unit suite) → marquee
  multi-select → `GroupNodesCommand`/`UngroupNodeCommand` + scope-path on existing commands. Gate:
  core round-trip tests green; collapse/ungroup a real selection (visual).
- **Phase D — Interface editing.** Group Input/Output `+`-socket port add, rename ports, rename
  group, exposed-param-as-interface-param. Gate: visual + the param-carry-over test.
- **Phase E — Polish.** Auto-frame on enter, colour/tint, keyboard map (`Ctrl+G`/`Ctrl+Alt+G`/`Tab`/
  `Esc`), collapsed-face macro summary, depth tint. Gate: visual; the full workspace sweep (this
  touches the shared snapshot + editing commands → infrastructure).

The spike (§7) precedes Phase C: prove inference feels right before building the command surface
around it.

---

## 11. File-by-file checklist

- `crates/manifold-core/src/group_edit.rs` *(new)* — `infer_interface`, `group_selection`, `ungroup`,
  `GroupEditError`, the full unit suite.
- `crates/manifold-core/src/lib.rs` — `pub mod group_edit;`.
- `crates/manifold-renderer/src/node_graph/snapshot.rs` — `GroupSnapshot`/`InterfaceSnapshot`,
  `NodeSnapshot.group`, `from_def` recursion + boundary-port synthesis.
- `crates/manifold-editing/src/commands/graph.rs` — `scope_path` on existing commands;
  `GroupNodesCommand`, `UngroupNodeCommand`, `RenameGroupCommand`, `AddInterfacePortCommand`,
  `RenameInterfacePortCommand`.
- `crates/manifold-ui/src/panels/mod.rs` — new `PanelAction`s + `scope_path` on existing ones.
- `crates/manifold-app/src/graph_canvas.rs` — scope path, marquee + set-based selection, group /
  boundary rendering, breadcrumb, enter/exit, double-click, new `DragMode::Marquee`.
- `crates/manifold-app/src/app_render.rs` — route new actions → scope-aware commands.
- `content_thread.rs` — none beyond the (now recursive) `from_def` it already calls.

---

## 12. Risks & open questions

- **Inference ergonomics** — the make-or-break (§7). Spike first. Open: port *ordering* (source
  order? spatial top-to-bottom?) and whether to merge an external source feeding two inner sinks
  into one input port (v1: keep separate — simpler, predictable).
- **Undo of group/ungroup** — must invert cleanly. Storing the affected sub-graph slice as the
  pre-image is simplest; confirm it composes with the existing 200-cap undo stack.
- **`selected: Option<u32>` → set** — a small but cross-cutting change; audit every reader
  (`feedback_per_window_resource_writes` pattern: change a once-singular field, find *all* uses).
- **Node-id stability** — scope paths use def node ids; group/ungroup renumber. Keep the scope path
  valid across a restructure (a collapse that creates the group you're standing in should land you
  sensibly — likely: collapse keeps you at the parent scope with the new group selected; enter is a
  separate gesture).
- **Deeply nested snapshot size** — acceptable (per-edit, `Arc`-cached), but if authored graphs get
  pathological, consider lazy body snapshots (the Option-B scoped-snapshot fallback). Not for v1.

---

## 13. Forward hooks (recipes build on this, don't change it)

- A **"Save group as recipe"** action serializes the selected group's `GroupDef` to a disk preset —
  rides on the disk-load work (`project_bundled_presets_swap_deferred`) and the existing
  save-to-JSON command path.
- **Linked vs local**: this UI is local-embedded only. A recipe drop-in is a group node whose body
  came from a ref; the editor renders it identically. *Frozen-until-you-choose* (the decided policy)
  becomes an "update available" affordance on such nodes — a badge on the box header, no model
  change here.
- **Standalone group editing**: a recipe opened on its own canvas is just "enter a group whose
  parent is the document root" — the navigation built here already covers it once Group Input/Output
  are real registered nodes (the backend spec's deferred item).
