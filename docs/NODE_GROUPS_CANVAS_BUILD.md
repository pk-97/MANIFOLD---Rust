# Node Groups — Canvas Build (continuation handoff)

Status: **in progress.** This is the live continuation doc for the editor-canvas work, written
2026-06-02 right before a context compaction. The clean-context instance starts here.

Companion docs: `NODE_GROUPS_DESIGN.md` (backend), `NODE_GROUPS_UI_DESIGN.md` (full UX spec). This
doc is the *build state + how to finish it debug-friendly*, not the design rationale.

---

## Where it lives

- Branch **`node-groups`** in a worktree at **`/Users/peterkiemann/MANIFOLD-node-groups`** (NOT
  merged; sits on top of node-graph-system HEAD). All commands: `git -C "<worktree>" …`,
  `cargo … --manifest-path "<worktree>/Cargo.toml"`. The worktree has `settings.local.json` seeded,
  so a session rooted there (or this main-rooted session, which has the dir trusted) edits without
  prompts.
- Backend agents work in the main checkout on `node-graph-system`. Stay in the worktree; don't
  touch the main tree.

## Done + tested (don't rebuild)

| Piece | Where | Proof |
|---|---|---|
| Schema + `flatten_groups` | `manifold-core/flatten.rs`, `effect_graph_def.rs` | 16 tests |
| Loader integration | `graph_loader.rs` (`instantiate_def` hook) | e2e load test |
| Collapse/ungroup logic | `manifold-core/group_edit.rs` | round-trip + flatten cross-check |
| Group/Ungroup commands | `manifold-editing/commands/graph.rs` | real-Project tests |
| **Snapshot preserves groups** | `manifold-renderer/snapshot.rs` `from_def` | `from_def_preserves_group_structure` |

So: the data the canvas reads (`GraphSnapshot` with `NodeSnapshot.group: Option<Box<GroupSnapshot>>`)
is correct and tested, and the commands the canvas will call (`GroupNodesCommand`,
`UngroupNodeCommand` — both take `scope_path`, `selected`, `handle`, `centroid`) work and undo.
**Every hard correctness question is already answered.** The canvas only renders a correct model and
emits commands that already work — it cannot make collapse produce wrong ports or break undo.

## Remaining = the canvas (this is the blind, visual part)

Files: `manifold-app/src/graph_canvas.rs` (~1700 lines, custom bitmap UI), `manifold-ui/src/panels/mod.rs`
(PanelAction enum), `manifold-app/src/app_render.rs` (action → command routing). Keys: **Ctrl+G group,
Ctrl+Shift+G ungroup.**

---

## THE MANDATE: build it debug-friendly (Peter's explicit ask)

Peter builds this blind and does one visual pass at the end. So the build must make its own state
legible and its failures obvious. Bake these in from the first line — they are not optional polish:

1. **Pure, unit-tested helpers for everything that isn't pixels.** The math doesn't need eyes, so
   test it: scope-path navigation (walk a `GraphSnapshot` to the current level), marquee
   rect∩node-rect intersection, port hit-testing (screen point → port), centroid-of-selection. Put
   these as free functions with `#[cfg(test)]` tests. When the canvas misbehaves, you'll know
   instantly whether it's logic (caught by a test you can add) or rendering (eyes only).
2. **A debug overlay, toggleable.** A corner readout drawn on the canvas: current scope path,
   selection count + ids, hovered node id, current `DragMode`. Optionally outline hit-zones. Gate it
   on a `debug_overlay: bool` field flipped by a key (e.g. backtick) or env var. This is how Peter
   sees "what the canvas thinks is happening" without a debugger.
3. **Structured logging behind one flag.** `eprintln!` the gesture pipeline — collapse
   (`GroupSelection {ids:?} -> "{handle}"`), enter/exit (`scope: {old:?} -> {new:?}`), marquee
   commit. One `GROUP_CANVAS_LOG` env check or a const. So a failing interaction leaves a trail.
4. **Isolate the new code.** Group rendering = one clearly-named branch in `draw_node`
   (`draw_group_node`), navigation = a small `GroupNav` helper, not scattered `if is_group` checks.
   Named visual constants at the top (`GROUP_TINT`, `GROUP_HEADER_H`, `PORT_SPACING`) so tweaking the
   look is editing one block.
5. **Ship a load-on-launch fixture FIRST.** Hand-author a small grouped preset (Source → a 2–3-node
   group → FinalOutput, like the `from_def` test fixture) under `assets/effect-presets/` so the
   moment any rendering exists, Peter can open the editor and see/navigate a real group. No fixture =
   nothing to look at. This is step 0.

The goal: when Peter jumps in, his punch-list is "nudge this / wrong colour", and anything deeper is
self-diagnosed by the overlay + logs, not a debugger spelunk.

---

## Build order (each layer compiles + clippy-clean before the next)

**Layer 0 — fixture + plumbing.** Author the grouped fixture preset; confirm it loads via
`check-presets` and the bundled_presets test. Add `debug_overlay` + the log flag scaffolding. Nothing
visual yet, but the target exists.

**Layer 1 — see + navigate (read-only).** In `graph_canvas.rs`: add `scope: Vec<u32>` + a
`GroupNav` helper that walks `Arc<GraphSnapshot>` to the current level's nodes/wires. Render group
nodes distinctly (`draw_group_node`: tint header, side ports from `NodeSnapshot.inputs/outputs`, enter
chevron, exposed-param summary). Render `group_input`/`group_output` when inside. Double-click a group
→ push scope; breadcrumb bar → set scope; `Esc`/`Tab` → pop. Navigation is UI-local (no command).
First milestone Peter can *look at*: load the fixture, fly in and out of the box.

**Layer 2 — selection.** `selected: Option<u32>` → `AHashSet<u32>` (audit every reader:
`selected_node_id`, `request_delete_selected`, highlight draw). Add `DragMode::Marquee { origin }` +
rect drawing + intersection select; Shift = additive. Unit-test the intersection math.

**Layer 3 — the gestures.** New `PanelAction`s (`GroupSelection { scope_path, node_ids, ... }`,
`Ungroup { scope_path, group_id }`; `EnterGroup`/`ExitGroup` stay UI-local). `Ctrl+G` on a selection
emits `GroupSelection`; `Ctrl+Shift+G` on a selected group emits `Ungroup`. `app_render.rs` routes
them to the **already-built** `GroupNodesCommand` / `UngroupNodeCommand`, passing the canvas's current
scope path + selection + the selection centroid + an auto name (`Group N`, inline-editable). Existing
mutation actions (`AddGraphNodeAt`, `MoveGraphNode`, …) gain the scope path so editing inside a group
targets the right level (the commands already accept `scope_path`).

**Layer 4 — interface editing + polish.** Group Input/Output `+`-socket to add a port (drag), rename
ports/group, exposed-param-as-interface-param, colour/tint, auto-frame on enter, the full keymap.

---

## Code anchors (so you don't re-hunt)

- `snapshot.rs` — `GraphSnapshot { nodes, wires, outer_routings }`, `NodeSnapshot.group:
  Option<Box<GroupSnapshot>>`, `GroupSnapshot { nodes, wires }`. **Ready.** Group node's `inputs`/
  `outputs` are the interface ports; `group_input`/`group_output` nodes carry synthesized ports.
- `graph_canvas.rs` — `GraphCanvas` struct (~line 400); `selected: Option<u32>` (~414, → set);
  `DragMode` enum (~349, add `Marquee`); `set_snapshot` (~507, stores `Arc<GraphSnapshot>` +
  preserves positions); `draw_node` (~1406); double-click distance threshold already exists;
  `drain_actions` emits `PanelAction`s.
- `panels/mod.rs` — `PanelAction` enum (has `AddGraphNodeAt`, `ConnectPorts`, `MoveGraphNode`,
  `RemoveGraphNode`, `SetGraphNodeParam`, `ToggleNodeParamExpose`, `OpenGraphEditor`). Add the group
  actions here.
- `app_render.rs` — `PanelAction::* =>` match arm (~line 706+) maps actions to `Command`s and
  `ContentCommand::Execute`. Add arms for the group actions → the two commands.
- `editing/commands/graph.rs` — `GroupNodesCommand::new(target, scope_path, selected, handle,
  centroid, catalog_default)`, `UngroupNodeCommand::new(target, scope_path, group_node_id,
  catalog_default)`. **Ready to call.**

## Verification

- Logic helpers (nav, marquee, hit-test): unit tests, CPU.
- `group_edit` / commands / `from_def`: already tested — re-run `cargo test -p manifold-core --lib
  group_edit`, `-p manifold-editing --lib`, `-p manifold-renderer --lib snapshot::` to confirm green
  before/after.
- Canvas rendering + interaction: **Peter's visual pass** (the editor is eyeball-verified per
  `feedback_graph_editor_is_authoring_not_perform`). The overlay + logs + fixture make that pass
  efficient.
- Blast radius touches the shared snapshot + editing commands → run the full workspace sweep once at
  the end. Known pre-existing red (not ours): WireframeDepthGraph copy-texture mismatch, DepthOfField
  prewarm submission.

## Don't forget

- Keys: **Ctrl+G group, Ctrl+Shift+G ungroup.**
- Commit per layer with `git -C "<worktree>"`; commit-message backticks must be escaped/quoted.
- Recipes (save group to disk, link/unlink, versioning) are a LATER layer on top of this — out of
  scope. This is embedded-group editing only.
