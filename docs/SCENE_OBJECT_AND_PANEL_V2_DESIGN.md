# Scene Object & Panel v2 — objects become graph vocabulary; the panel becomes outliner + properties

**Status: SHIPPED — all 5 phases landed on main 2026-07-17 (P1+P2 @ `5c5dacfe`,
P3 @ `da452351`, P4+P5 @ landing SHA below). Landing reports:
`docs/landings/2026-07-17-scene-object-v2-p1-p2.md`,
`docs/landings/2026-07-17-scene-object-v2-p3.md`,
`docs/landings/2026-07-17-scene-object-v2-p4-p5.md`. The object model, the
Object wire, and the outliner+properties panel are all live on main.
Three known, tracked gaps outside what P1-P5 committed to fix (none are
regressions from this landing — all found BY this wave's own gates, all
logged before the session ended, per house rule): BUG-218 ("Add modifier"
chip is a dead affordance against any real grouped object — the D6
modifier-stack commands still splice at the pre-D12 group_output.vertices
port, fix owed to `manifold-editing`, out of every phase's committed blast
radius); BUG-212 (`DuplicateSceneObjectCommand`'s fresh NodeIds break an
imported object's string-bound model-file path — Duplicate on a
hand-built object works, on an imported one does not); BUG-199 (dock
scroll — explicitly out of scope for this whole design, owned elsewhere).
BUG-210 (AddSceneObjectCommand pre-migration wires) FIXED by P3. A fourth,
more severe gap was found AND FIXED during P4+P5's own landing (not left
open): the D5 migration was never actually wired into the real project-load
path, and `SceneVm`'s transform/material/vertex-count tracing didn't
understand the migrated-project topology — both fixed in the same landing,
see that report for the full diagnosis.**
**(APPROVED design 2026-07-17 · Fable 5, design session with Peter)**
**Prerequisites:** SCENE_SETUP_PANEL_DESIGN P1–P5 (SHIPPED 2026-07-17 — this design revises its
object model and panel layout in place). BUG-199 (dock scroll) is explicitly OUT of this set —
another session owns it (Peter, 2026-07-17: "another agent has BUG-199 planned for fixing so we
can leave it out of this set").
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any
phase. Executor: Sonnet, orchestrated (Sonnet → Sonnet). Peter, verbatim: **"Sonnet must not
make any decisions, it must just execute mechanical spec."** Anything that feels like a
decision is an escalation, full stop.

Peter's directives (2026-07-17, verbatim — these opened and decided the design):

- "We should reuse UX and UI from blender. My main thought is that each object doesn't need
  it's own full unique set of tooling, should be select the object to use the tools."
- "Input cells should still have click and drag to adjust their values … It should be a basic
  'widget' that all numerical input fields use so any input field can be click and dragged to
  adjust the value as well as double clicked to keyboard enter a value."
- On per-object identity via importer grouping: "feels like a bit of a hack and probably not
  the correct long term solution." (Ratified the `scene_object` direction in the same session.)

**The governing insight: object identity moves from a UI-side inference to graph vocabulary.**
Today an "object" is *whatever named group happens to wrap the wires feeding `mesh_k`* — a
convention `SceneVm` reverse-engineers (scene_vm.rs's group-output trace) and the one part of
the shipped panel that was never a fact in the model. This design makes the object a typed
node: **`node.scene_object`** consumes mesh + transform + material + maps + instances and
emits ONE **`Object`** wire; `render_scene` takes `object_k: Object` instead of 3–9 parallel
per-object wires. Name and visibility become ordinary node facts (`handle`, a port-shadowed
`visible` param). Groups return to what GROUPING_GRAPHS designed them as: legibility, never
identity. On top of that model, the panel adopts the Blender split — a compact **outliner**
(every scene item, one row each, eye toggles) over a single **properties** region showing the
selection's controls — and every numeric value cell in the app's docks gains the standard
gesture set: **drag to scrub, double-click to type, right-click to reset, Shift for fine.**

Binding constraints (DESIGN_AUTHORING §1): **persistence** binds hardest — `render_scene`'s
port surface changes, so every existing scene def (projects, bundled/reference presets, user
library) load-migrates via one idempotent def rewrite (D5). **Hot path**: `scene_object` is a
CPU struct emit per frame (a `Copy` struct, no allocation — the `node.light` cost class);
`render_scene`'s per-frame work is unchanged in kind (same slot resolutions, one indirection
earlier). **Performance surface**: `visible` is port-shadowed — muting an object is bindable
to MIDI/LFO from birth. Thread residency and time model: untouched.

Companions: `SCENE_SETUP_PANEL_DESIGN.md` (v1 — its D1 view-not-model doctrine, D5 merge, D6
modifier stack, D7 empty states, D8 scope fence all CARRY FORWARD unchanged; this doc
supersedes only its object-identity mechanism, §4 "no new port types" invariant, panel layout,
and the §9 visibility deferral), `UI_WIDGET_UNIFICATION_DESIGN.md` (SHIPPED — the gesture
contract pattern P4 extends), `GROUPING_GRAPHS.md` (groups = legibility, restored),
`FREEZE_COMPILER_MAP.md` (cut-rule treatment of CPU-struct wires, which `Object` joins).

---

## 1. Audit — what exists (verified 2026-07-17, this session, main checkout)

Instruction to executors: **extend, don't redesign.** Re-verify anchors at phase entry —
concurrent lanes were landing on main the day this was written; a moved line is fine, a
missing symbol is an escalation.

| Piece | Where | State |
|---|---|---|
| CPU-struct wire pattern (the `Object` precedent, end to end) | `ports.rs:17-57` (`PortType::{Camera,Light,Material,Transform}`), `bindings.rs:125` (`NodeInputs::camera` accessor), `execution.rs:77-85` (per-type `*_write_scratch` drains), `light.rs:38-60` (CPU-only `primitive!`, struct output) | SHIPPED ×5 (Camera/Light/Material/Transform/Atmosphere). `Object` is the sixth, same shape everywhere |
| Slot-level resource resolution | `bindings.rs:190-196` (`texture_2d_slot(Slot)`, `array_slot(Slot)`) — and `render_scene.rs:2436-2450` already resolves `mesh_k` port→Slot→buffer, reads `slot_generation_of(slot)` for its shadow cache | SHIPPED. `SceneObject` carrying `Slot`s changes *where the slot comes from*, not how it resolves |
| Unbound-mesh & zero-vertex tolerance | `render_scene.rs:2437` (`let Some(vertices) = … else` skip), `:3078-3083` (vertex_count == 0 fallback) | SHIPPED — `visible: false` → skip-the-draw lands on existing tolerance |
| Planner lifetime extension for dynamically-consumed inputs | `effect_node.rs:595-599` (`variadic_skip_passthrough_out`: "planner extends every wired texture input's lifetime to the output's last reader") | SHIPPED precedent for D2's `carries_resources` hook |
| Fusion cut-rule exemption for CPU-struct wires | `docs/FREEZE_COMPILER_MAP.md` §4 ("a wire into a `Camera`-typed CPU-struct port does NOT cut…"); code: `freeze/install.rs`, `freeze/region.rs` (`rg -n "PortType::Camera" crates/manifold-renderer/src/node_graph/freeze/`) | SHIPPED — `Object` joins the same list |
| `render_scene` per-object dynamic ports | `render_scene.rs:742` (`rebuild(objects, lights)`), `:777-782` (`mesh_{i}` et al. port construction), `:1905` (full port list in `composition_notes`), `OBJECT_SAFETY_MAX` `:743` | SHIPPED — P2 rewrites the per-object list to `object_{i}` |
| Def load-migration precedents | type-id level: `graph_loader.rs:243` (`migrate_def_type_ids`, runs at `instantiate_def:321`); model level at project load: `binding_migration.rs:40` (`migrate_user_param_bindings_to_node_id(&mut Project)`, called from `manifold-app/src/project_io.rs`); JSON ladder: `manifold-io/src/migrate.rs` (v-rung functions) | SHIPPED ×3. D5's migration is model/def-level (binding_migration's residency), NOT the io ladder — the panel reads unflattened defs from snapshots, so migration must land in the stored def, not only at instantiate |
| Object identity today (the thing being replaced) | `scene_vm.rs:102-129` (`SceneObjectVm::Known` = group-output trace; `Custom` = everything else), SceneStarter.json (objects "Floor"/"Cube" are groups exporting `vertices`/`material`/`transform` group-output ports — the proto-bundle this design formalizes) | SHIPPED — the group-output port bundle is exactly `scene_object`'s input list, proven in every scene asset |
| Importer per-object emission | `gltf_import.rs:1901` (`build_import_graph` — one group per material: source/material/texture/transform), merge assembler (v1 D5, `merge_import_into_graph`), `AddSceneObjectCommand` (`commands/graph.rs:2172`, carries a `catalog_default: EffectGraphDef` spliced per add) | SHIPPED — P3 re-points all three producers at the new shape |
| Node display names | `effect_graph_def.rs:119-124` (`handle: Option<String>` — "Display / search name … NOT an addressing key") | SHIPPED — the object/light name home (D6). No new field |
| Rename machinery | `RenameGroupCommand` (`commands/graph.rs:2755`, sweeps card sections via the D5 walk at `:1868`); panel rename field `TextInputField::SceneObjectRename(u32)` (`text_input.rs:99-104`) | SHIPPED — P3 extends into `RenameSceneObjectCommand` (adds handle write); lights get a plain `SetNodeHandleCommand` |
| Numeric-cell gestures today | Drag-scrub: SHIPPED panel-wide (`scene_setup_panel.rs:561-616` `ValueDrag` + drag-armable value cells for steppers AND triplets; audio dock's gain calibration drag). Double-click type-in: SHIPPED for inspector (`TextInputField::InspectorParam`, text_input.rs:43-47) and graph editor (`GraphNumericParam`, `:69-77`, widget-unification P5d) — **absent from both docks** (`rg -n "DoubleClick" scene_setup_panel.rs` → 0). Reset: right-click via stepper/slider contracts. Shift-fine: nowhere (`Modifiers.shift` exists, `input.rs:20`) | The P4 gap is exactly: dock type-in + Shift-fine + a shared contract module so hosts stop hand-rolling |
| Gesture-contract pattern | `slider.rs:230-247` + `stepper.rs:44-52` (pure `intent_for` fns, per-host translation, D2/D3 of UI_WIDGET_UNIFICATION — SHIPPED 2026-07-13, P1–P8) | The pattern P4's `value_cell.rs` module follows |
| Dropdown widget | `panels/dropdown.rs:1-11` — generic floating menu, `open()` + `DropdownAction`, app-routed | SHIPPED — enum value cells reuse it; **no new widget kinds** |
| Text sessions | `text_input.rs:22` (`TextInputField` enum + per-field ctx structs; commit paths in `app_render.rs`) | SHIPPED — P4 adds two variants, same shape |
| Panel structure | `scene_setup_panel.rs:738-2000` (`build_docked` → `build_live` → per-section builders `build_objects_section`/`build_light_row`/`build_camera_section`/…; UI-local fold state; `SceneSetupVm` DTO from `state_sync`) | SHIPPED — P5 reorganizes `build_live` into outliner + properties, **reusing the row builders' bodies** |
| Node catalog | `docs/NODE_CATALOG.md` §"Registered node index" — generated by `cargo run -p manifold-renderer --bin gen_node_catalog`, drift-guarded | Regenerate in P1 |
| Open bugs this design absorbs or touches | BUG-193 (no remove-object command — **absorbed, P3 is the root fix**); BUG-194 (vertex counts — untouched, stays open); BUG-195 (merge reference radius — untouched, stays open); BUG-198 (headless `Key`/text gap — constrains P4's gate levels, see brief); BUG-199 (dock scroll — excluded per Peter) | Backlog Status lines updated at each absorbing landing |

Negative claims (searches run 2026-07-17): no object/bundle-shaped primitive exists among the
228 registered (`rg -i 'purpose: "[^"]*(object|bundle)' primitives/` — only renderers and
`transform_3d`'s prose); no duplicate-object command (`rg -n "Duplicate" commands/graph.rs` →
0); no node-handle rename command (`rg -n "SetNodeHandle" crates/manifold-editing/` → 0); no
Shift-fine drag anywhere in `manifold-ui`; `SliderIntent::EditValue` has no dock consumer.

**§2.5 audit verdict (run this session):** `Object` port + `node.scene_object` +
`render_scene`'s `object_k` surface are *genuinely new*; every other ingredient (CPU-struct
wires, slot resolution, drains, migration hooks, rename sweep, dropdown, type-in, drag
sessions) *exists* and is reused. The SceneStarter/importer group-output bundle
(`vertices`/`material`/`transform`) is the proof that this decomposition is the shape scenes
already want — `scene_object` is that implicit bundle, made a typed fact.

## 2. Decisions

**D1 — Objects are nodes: `node.scene_object` emits one `Object` wire; `render_scene`
consumes `object_k: Object`.** The structural fact "object k = (mesh, transform, material,
maps, instances)" today exists only as parallel wires that must stay index-coherent by luck.
`scene_object` binds them at one node; the Object wire carries the bundle to the renderer.
Name (`handle`) and visibility (`visible` param) get a real home. Discovery stops being
convention archaeology: find the `scene_object` feeding `object_k`, read ITS input wires —
grouped or not, a scene built from `scene_object`s gets first-class panel rows.
*Rejected:* **pass-through marker node** (scene_object re-emits mesh/transform/… on mirrored
output ports, no new PortType) — visibility has no mechanism: skip-alias machinery is
texture-only (`metal_backend.rs` `alias_2d`/`install_texture_2d`), and "decline to write the
output" fights the prebind invariant (`feedback_node_graph_prebind_all_textures`).
*Rejected:* **per-object params on `render_scene`** (`visible_k`/`name_k`) — re-opens
SCENE_BUILD D3 (render_scene carries no per-object state), gives modifiers/discovery no
anchor, and grows an unbounded param array on one node. *Rejected:* **keep groups as
identity** — Peter, verbatim, this session: "feels like a bit of a hack." Groups stay purely
cosmetic (GROUPING_GRAPHS restored).

**D2 — `SceneObject` is a `Copy` CPU struct carrying values for CPU facts and `Slot`s for GPU
resources.** Committed shape (new module `crates/manifold-renderer/src/node_graph/scene_object.rs`,
sibling of `camera.rs`/`light.rs`):

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneObject {
    pub visible: bool,
    pub transform: Transform,            // identity when the port is unwired
    pub material: Option<Material>,
    pub mesh: Option<Slot>,              // Array<MeshVertex>
    pub base_color_map: Option<Slot>,    // Texture2D — and the four siblings below
    pub normal_map: Option<Slot>,
    pub mr_map: Option<Slot>,
    pub occlusion_map: Option<Slot>,
    pub emissive_map: Option<Slot>,
    pub instances: Option<Slot>,         // Array<InstanceTransform>
}
```

`Copy`, zero allocation — hot-path legal by construction. `render_scene` resolves the slots
with the calls it already makes (`array_slot`, `texture_2d_slot`, `slot_generation_of` —
`bindings.rs:190-196`, `render_scene.rs:2436-2450`); the slots come from the Object instead
of from its own port index. Ordering is transitively correct (render_scene's wire-dependency
on scene_object ⊇ scene_object's on the producers). **The one genuinely new mechanism,
stated honestly:** resources referenced *through* a struct escape the planner's wire-based
lifetime view. Fix at the seam: a new default-false hook on the node trait (beside
`skip_passthrough`, `effect_node.rs`) — `fn carries_resources(&self) -> bool` —
`scene_object` returns `true`; the planner extends every wired texture/array input's lifetime
to the node's *output's* last reader, the exact rule `variadic_skip_passthrough_out` already
implements for muxes (`effect_node.rs:595-599`). Object wires never chain (see Invariants),
so one transitive hop is the whole problem.

**D3 — `node.scene_object`, committed signature.** CPU-only `primitive!` shaped like
`node.light` (`light.rs:38`):

```
type_id: "node.scene_object"
inputs:  vertices: Array<MeshVertex> optional,
         transform: Transform optional,
         material: Material optional,
         base_color_map / normal_map / mr_map / occlusion_map / emissive_map: Texture2D optional,
         instances: Array<InstanceTransform> optional,
         visible: ScalarF32 optional          (port-shadow, like every node.light scalar)
outputs: object: Object
params:  visible — transcribe node.light's `cast_shadows` ParamDef shape verbatim
         (same ty/enum-labels convention; default ON)
```

Evaluate: read own input slots via a new `NodeInputs::slot_of(&self, port) -> Option<Slot>`
(the port→Slot map already exists internally — this is an accessor, not new state), read
`transform`/`material` by value, read `visible` (port-shadow then param), emit via
`NodeOutputs::set_object` → a new `object_write_scratch` drain in the executor
(`execution.rs:77-85` sixth sibling). Codegen class: CPU bridge/IO — exempt from the
wgsl-fragment mandate by the standing rule's own class list (same as `node.light`), state it
in the primitive's comment. **Instrument meaning:** `visible` port-shadowed means "mute the
statue on the drop" is a MIDI binding, not a feature request.

**D4 — `render_scene` v2 per-object surface: `object_{i}` only.** `rebuild()`
(`render_scene.rs:742`) emits `object_{i}` for `i < objects` and DELETES the nine per-object
port families (`mesh_{i}`, `transform_{i}`, `material_{i}`, the five map ports,
`instances_{i}`). `lights`/`camera`/`envmap`/`atmosphere` unchanged. Draw assembly: per k,
`inputs.object("object_k")` → skip when `None` or `!visible` (skip = no draw AND no shadow
cast — an invisible object leaves no shadow); otherwise resolve slots exactly as today.
`objects`/`OBJECT_SAFETY_MAX`/caster-cap semantics unchanged. **Consequences, stated
honestly:** this is a breaking def-surface change; D5's migration is what makes it safe, and
the two must land in the same batch. The v1 design's §4 invariant "no new port types, no
model change" is formally superseded by this doc — that invariant protected the v1 *panel*
wave from scope creep; this design IS the model change, decided by Peter.

**D5 — One idempotent def migration, run at every def entry point.**

```rust
// manifold-core (beside flatten.rs; defs are core vocabulary)
pub fn migrate_scene_object_wires(def: &mut EffectGraphDef) -> bool   // true = changed
```

Rule, purely structural (no version gate — idempotence IS the gate): for every
`node.render_scene` node at any group depth, for every index k where any of the nine legacy
per-object ports has a wire: mint one `node.scene_object` (doc id above the def's max — the
merge assembler's convention; fresh `NodeId`; `handle` = the enclosing group's name of the
`mesh_k` producer when one exists, else `"Object {k}"`; canvas position = midpoint of the
mesh producer and the render_scene node), re-point the nine wires' consumers onto the
scene_object's inputs, wire `object → object_{k}`. A def with no legacy wires is untouched
(`false`). Placement: inside the mesh producer's group when the whole triple lives in one
group (the importer shape), else at root scope. Call sites: project load (beside
`migrate_user_param_bindings_to_node_id`, `project_io.rs`), bundled/reference preset load,
user-library preset load, `graph_tool` gains a `migrate` verb (used to regenerate the in-repo
JSON) — re-derive the full list at P2 entry: every place a def is deserialized,
`rg -n "EffectGraphDef" crates/manifold-io/src crates/manifold-app/src/project_io.rs crates/manifold-renderer/src/node_graph/bundled_presets.rs`.
**Forbidden on this path:** dropping a triple the rule can't parse — an unparseable object's
wires stay exactly as loaded (render_scene simply has no `object_k` for it; the panel shows
the honest custom row). Silent data loss is the loader's one banned move.

**D6 — Names: the object IS its `scene_object` node; the name is its `handle`.** Outliner
label precedence: `handle` → else `"Object {k}"`. `RenameSceneObjectCommand` (extends the
`RenameGroupCommand` walk, `graph.rs:2755`): one undo unit that sets the scene_object's
`handle`, renames the enclosing group *when one exists* (graph-view coherence — a sweep, not
a second home: the command is the single writer of both), and runs the existing card-section
sweep. Lights: name = the light node's `handle`, written by a new plain
`SetNodeHandleCommand { target, scope_path, node_doc_id, new_handle }` (no sweep — nothing
downstream displays light names today). Importer/starter emit handles at build time (same
string they already put on the group).

**D7 — The panel becomes outliner + properties (the Blender split).** Peter, verbatim: "each
object doesn't need it's own full unique set of tooling, should be select the object to use
the tools."

- **Outliner** (top, fixed): one row per item — glyph + name — in committed order: Camera ·
  World · each light · each object. Object rows carry an eye toggle (writes
  `scene_object.visible` through the fourth-surface path; the eye renders the driven style
  read-only when `visible` is wire-driven). Selected row uses the tree-selection styling
  precedent. Footer buttons (always visible): **+ Object · + Light · Import Model…**
  (existing dispatches).
- **Properties** (below, the only place detail rows ever appear): content = the selection's
  rows, built by the EXISTING builders relocated intact — Object → name row + visible row +
  transform triplets + material knobs + modifier stack (`build_object_row` body); Light →
  `build_light_row` body + name row; Camera → `build_camera_section` + lens; World →
  Environment + Fog sections (including the v1 "Add fog"/"Add environment" composites).
  A properties header row shows the selection's name plus, for objects and lights,
  **Duplicate** and **Remove** buttons (P3's commands).
- **Selection** is UI-local workspace state (like fold state, never serialized):
  `BTreeMap<LayerId, SceneSelection>` where
  `enum SceneSelection { Object(u32), Light(u32), Camera, World }` (u32 = node doc id — a
  removal-stable key, indices are not). Default (and on any dangling id after a graph edit):
  first object, else World. Header counts line and D7-v1 empty states unchanged.
- *Rejected:* Blender's icon tab strip in properties (new widget kind for zero information —
  collapsible section headers already do it); both-regions-as-tabs (hides the scene list
  while editing — the outliner is the map, it stays); keeping per-object expanded sections
  (the v1 layout this replaces — a 2-object scene already overflows a 1216px window,
  BUG-199's own repro).
- **Consequences, stated honestly:** one selected item at a time means cross-object A/B
  tweaking takes two clicks instead of one scroll. That trade is the point (the scroll
  stopped working at real scene sizes), and multi-select stays in Deferred with its trigger.
  The layout also collapses panel height to ~one item's rows — most scenes fit a 1216px
  window again, which *mitigates* BUG-199's exposure but does not fix scroll; that bug stays
  owned elsewhere.
- **Instrument meaning:** mid-set you click the thing and its knobs are in the same place
  every time — no scrolling past four objects' worth of rows while the room watches.

**D8 — Every numeric value cell speaks one gesture contract.** New module
`crates/manifold-ui/src/value_cell.rs`, the UI_WIDGET_UNIFICATION pattern (pure `intent_for`,
per-host translation, D2/D3 of that doc):

```rust
pub enum ValueCellIntent { Scrub { fine: bool }, EditValue, ResetToDefault }
// (Cell, Drag{shift})       -> Scrub { fine: shift }   — drags stay host-stateful (the
//                              existing ValueDrag / DragController sessions ARE the impl)
// (Cell, DoubleClick)       -> EditValue
// (Cell, RightClick)        -> ResetToDefault
```

`stepper.rs`'s contract table amends: `(Value, DoubleClick) → EditValue` (its last dead stop
falls, exactly like widget-unification P5d fell for the canvas). Scrub sensitivity: keep each
host's existing per-row scale; `fine` multiplies the applied delta by **0.1**. Hosts and
their EditValue targets, committed: scene dock numeric cells (steppers, triplets, modifier
params, camera, env/fog) → new `TextInputField::SceneNumericParam` + ctx
`{ layer_id, addr: ParamAddr, degrees: bool }` (commit parses f32 — degrees rows convert —
and dispatches the row's existing commit path, one undo unit; NO clamp, per
PARAM_RANGE_CONTRACT P1's no-clamp rule on type-in); audio dock gain/numeric cells → sibling
variant, same shape (⚠ VERIFY-AT-IMPL: enumerate audio dock numeric cells —
`rg -n "stepper\|drag" crates/manifold-ui/src/panels/audio_setup_panel.rs`); inspector and
graph editor already ship EditValue — unchanged. Sliders with tracks keep track-drag as their
scrub; their value cells already type. *Rejected:* per-panel hand-rolled gestures (today's
state — the audit found three private implementations of the same drag).

**D9 — Enum value cells with 3+ labels open the dropdown; 2-label enums stay steppers.**
Click on the value cell of a 3+-label enum row (light `mode` is 2 — stays; `shadow_softness`
4 — dropdown; environment `mode`, modifier axis selectors — dropdown) calls the existing
overlay (`panels/dropdown.rs` `open()`, items = the row's label set, anchored under the
cell); `DropdownAction` routes to the row's existing enum write. `[−]/[＋]` cycling keeps
working on every enum row. On/Off rows are a 2-label cycle — a click toggles; a menu of two
is noise. No new widget kinds.

**D10 — Angles display in degrees at the panel boundary; storage stays radians.** Committed
row table: `transform_3d.rot_*` (all three), `orbit_camera.orbit`/`tilt`, `free_camera`
euler triplet, and `fov_y` on all three camera atoms. Display format `%.1f°`; type-in parses
degrees; scrub steps in degrees (0.5°/px, fine 0.05°/px). Conversion lives ONLY in the
panel's format/commit boundary (`state_sync` row curation + the SceneNumericParam commit) —
graph defs, commands, cards, node faces are untouched. *Rejected:* unit metadata on
`ParamDef` (right idea, app-wide blast radius — Deferred with trigger). **Instrument
meaning:** "Tilt 17.2°" reads on a dark stage; "0.30" does not.

**D11 — Duplicate and Remove are first-class object/light verbs (Remove closes BUG-193).**
Both shaped like `AddSceneObjectCommand` (`graph.rs:2172`; one undo unit, `prev` snapshot):
- `RemoveSceneObjectCommand { target, scope_path, render_scene_node_id, object_index }` —
  delete the object's `scene_object` + its exclusive upstream subgraph (nodes reachable only
  from it, its enclosing group when emptied), renumber `object_{k>i}` wires down, decrement
  `objects`. Sibling `RemoveSceneLightCommand` for `light_k` (delete node, renumber,
  decrement `lights`).
- `DuplicateSceneObjectCommand { …, source_index }` — deep-clone the object's subtree
  (doc ids above max — merge-assembler convention; FRESH `NodeId`s — bindings are identity,
  never cloned; handle + `" 2"` suffix — the importer's collision convention), wire to index
  `objects`, bump `objects`, and offset the clone's `transform_3d.pos_x` by **+0.5** (a
  deliberate, visible, undoable default so the copy doesn't vanish inside the original —
  stated arbitrary, tune-by-feel later). Card sections/exposes are NOT cloned (expose is a
  deliberate act; fresh NodeIds make cloned bindings dangle by construction).

**D12 — `SceneVm` v2: discovery anchors on `scene_object`, tolerance doctrine unchanged.**
For each `object_k` wire: producer is `node.scene_object` (traced through group boundaries,
as today) → `Known` row — name from `handle`, `visible` value+driven flag, transform =
its `transform` input's producer (`node.transform_3d` → triplets), material = its `material`
input's producer (four curated atoms), modifier chain = the mesh-side chain between source
and the scene_object's `vertices` input (D6-v1 walk, re-anchored), map-presence flags.
Anything else → `Custom` row, never hidden, never an error. Lights/camera/environment/
atmosphere traces unchanged. `SceneObjectVm::Known.group_node_id: u32` becomes
`object_node_id: u32` (the scene_object) + `group_node_id: Option<u32>` (rename sweep
target). The Vm stays a pure function of the def — same purity gates as v1.

## 3. What it buys on stage

The skull/warehouse walkthrough of v1 still holds; what changes: open Scene Setup and you see
the *scene* — Camera, World, two lights, Hero, Warehouse — as six rows, not six hundred
pixels of stacked steppers. Click Hero: its transform, material, modifiers appear below,
every number scrubbable, double-click to type 42.0, right-click to reset, Shift for the fine
landing. Click the eye: Hero mutes — and because `visible` is port-shadowed, bind it to a
MIDI pad and the mute is a performance gesture. Duplicate the rock, drag it 3 meters, done —
no graph editor, no re-import. And a scene hand-built in the graph out of `scene_object`
nodes gets the same first-class panel, grouped or not: the panel reads the instrument's own
vocabulary now, not the importer's wrapping paper.

## 4. Invariants & enforcement

| Invariant | Enforcement (machine check, named) |
|---|---|
| `Object` wires never chain: `node.scene_object` is the sole `Object` producer and takes no `Object` input; consumers are renderer boundary nodes | Unit test `object_port_single_hop` walking the registry: every registered primitive's ports — `Object` outputs only on `node.scene_object`, `Object` inputs only on `node.render_scene` (extend when a second renderer consumes it) |
| `SceneObject` stays `Copy` (hot-path: no per-frame allocation) | Compile-time: `const _: () = { fn assert_copy<T: Copy>() {} … };` in `scene_object.rs` |
| Migration is idempotent and lossless | `migrate_scene_object_wires_idempotent` (apply twice, def-equality) + `migrate_unparseable_triple_left_intact` (a mangled def round-trips byte-identical); flatten-equivalence on a migrated grouped def |
| An invisible object leaves no shadow | `gpu_tests` PNG pair in P2's gate: caster `visible` off → its shadow gone from the receiver |
| Planner keeps carried resources alive | `carries_resources_extends_lifetimes` unit test on a synthetic plan: texture into scene_object, render_scene as last reader, assert no pool release between (shape it like the variadic-mux lifetime test — ⚠ VERIFY-AT-IMPL: `rg -n "variadic" crates/manifold-renderer/src/node_graph/execution_plan.rs` and mirror its test) |
| Panel introduces no new mutation path; one value, four surfaces (v1 §4, carried forward) | Same gates: `rg -n "MutateProject\|Arc<Mutex\|Arc<RwLock" scene_setup_panel.rs` → 0; `scene_panel_slider_emits_card_identical_command` stays green |
| `SceneVm` purity (v1 §4, carried) | Same: takes `&EffectGraphDef` only; `rg -n "Project\b" scene_vm.rs` → 0 outside doc comments |
| Every numeric value cell built by the dock registers all three gestures | Unit test `dock_numeric_cells_register_full_contract`: after a `build_docked` on the azalea-shaped synthetic Vm, every drag-armable cell id is also present in the type-in registration set (and vice versa) |
| Selection never dangles | Unit test: remove the selected object from the Vm, rebuild — selection falls back to first-object-else-World |
| No stored modifier list / no panel-owned scene state (v1, carried) | `rg -n "modifier" crates/manifold-core/` → no stored stack; selection map lives in the panel struct only, `rg -n "SceneSelection" crates/manifold-io crates/manifold-core` → 0 |

## 5. Phasing (Sonnet, one session each; landing batches P1+P2 · P3 · P4+P5)

Forbidden, all phases: keeping any legacy per-object port alive "for compatibility" (the
migration exists so the old surface can DIE — parallel old paths are the named killer) ·
panel-owned scene values · new widget kinds · `Arc<Mutex>` anywhere · scope-widening into
viewport/gizmos/physics/splats/animation (D8-v1 fence stands) · silent dropping on any load
path · `cd` prefixes, `add -A`, unpathspec'd commits (house rules).

### P1 — The Object wire (plumbing vertical: type → atom → executor → tests)

- **Entry state:** clean worktree off current `origin/main` (slot ring acquire); anchors
  re-run: `ports.rs:17` PortType enum, `bindings.rs:125`, `execution.rs:77-85`,
  `light.rs:38`, `effect_node.rs:547/587`.
- **Read-back:** this doc D1–D3 + Invariants; `docs/ADDING_PRIMITIVES.md`; `light.rs` whole
  (the template); FREEZE_COMPILER_MAP §4 + §9.
- **Deliverables:** `PortType::Object` (ports.rs, doc comment naming the single-hop
  invariant); `scene_object.rs` struct per D2; `primitives/scene_object.rs` per D3 +
  registration; `NodeInputs::{object, slot_of}` + `NodeOutputs::set_object` +
  `object_write_scratch` drain; `carries_resources` hook (default false) + planner lifetime
  extension; fusion cut-rule listing (`Object` joins the Camera line in
  `freeze/install.rs`/`region.rs`); `object_port_single_hop`, the Copy assert, the lifetime
  test; NODE_CATALOG regenerated (`cargo run -p manifold-renderer --bin gen_node_catalog`).
- **Gate.** *Positive:* `cargo nextest run -p manifold-renderer --lib`; GPU run (executor
  touched): `cargo test -p manifold-renderer --features gpu-proofs` — including a new
  `gpu_tests` graph: synthetic def wiring mesh→scene_object→(test consumer via
  `inputs.object`), asserts the struct arrives with correct slots and the mesh resolves.
  *Negative:* `rg -n "String" scene_object.rs` → no owned strings in the struct;
  `rg -n "Arc<Mutex" crates/manifold-renderer/src/node_graph/scene_object*` → 0.
  **Demo:** none — L1 (plumbing; the visible surface arrives with P2, same landing batch).
- **Forbidden:** `skip_passthrough` on scene_object (D1 rejected the alias shape); touching
  `render_scene` (P2's).
- **Test scope:** focused `-p manifold-renderer`; clippy `-p manifold-renderer`.

### P2 — render_scene v2 surface + the migration (lands WITH P1)

- **Entry state:** P1 committed in the same worktree; anchors: `render_scene.rs:742/777/1905/2436`,
  `binding_migration.rs:40`, `graph_loader.rs:321`, the def-deserialize entry-point sweep
  (D5's re-derivation command — write the fresh list into the phase notes).
- **Read-back:** D4–D5 + Invariants; `render_scene.rs` module doc + `rebuild` + draw-assembly
  region; D5's forbidden move.
- **Deliverables:** `rebuild()` emits `object_{i}` only (nine legacy families deleted); draw
  assembly + shadow-caster path read through `SceneObject` (visible=false → no draw, no
  shadow); `migrate_scene_object_wires` in manifold-core + call sites per D5;
  `graph_tool migrate` verb; ALL in-repo defs regenerated (re-derive the list:
  `rg -l '"mesh_0"' crates/manifold-renderer/assets tests/` — expect ≈10 preset files + any
  fixture defs); render_scene unit + gpu tests updated to the new wiring (re-derive:
  `rg -ln 'mesh_0' crates/manifold-renderer/src crates/manifold-renderer/tests`); migration
  test trio (idempotent, lossless-unparseable, flatten-equivalence); the invisible-caster PNG
  pair.
- **Gate.** *Positive:* full `-p manifold-renderer --lib` + `-p manifold-core --lib`; GPU
  suite `cargo test -p manifold-renderer --features gpu-proofs` (render_scene runtime
  touched — mandatory); `check-presets` clean; `graph_tool validate` + `fusion` on
  SceneStarter + one migrated reference preset; **held-out round-trip:** load the azalea
  fixture project (pre-migration def), assert migration fires once, scene renders
  pixel-comparably (PNG diff against a pre-change golden), save → reload → migration is a
  no-op (`false`). *Negative:* `rg -n '"mesh_\{i\}"|format!\("mesh_' render_scene.rs` → 0;
  `rg -l '"mesh_0"' crates/manifold-renderer/assets` → 0.
  **Demo (L2):** the before/after PNG pair (same scene, pre-migration golden vs
  post-migration render) + the invisible-caster pair, read by the orchestrator.
- **Forbidden:** version-gating the migration (idempotence is the gate); leaving any legacy
  port emission behind a flag; regenerating presets by hand-editing JSON (use
  `graph_tool migrate`).
- **Test scope:** focused + GPU as above; **P1+P2 land as one batch** — full workspace sweep
  + `cargo clippy --workspace -- -D warnings` + `cargo deny check bans` in the main checkout
  at landing; landing report per §8.10, quoting this doc's status line update.

### P3 — Producers and verbs: importer, merge, add, remove, duplicate, rename

- **Entry state:** P1+P2 landed; anchors: `gltf_import.rs:1901`, merge assembler,
  `AddSceneObjectCommand` (`graph.rs:2172`), `RenameGroupCommand` (`graph.rs:2755`),
  `text_input.rs:99` (SceneObjectRename).
- **Read-back:** D6 + D11; v1 doc D5 (merge invariants carry: chrome skipped, id offsetting,
  10× normalize); BUG-193's backlog entry.
- **Deliverables:** `build_import_graph` + merge assembler + `AddSceneObjectCommand`'s
  `catalog_default` emit scene_object-shaped objects (handle stamped); `RemoveSceneObjectCommand`
  + `RemoveSceneLightCommand` (BUG-193 root fix — update its backlog Status line this
  session); `DuplicateSceneObjectCommand` per D11; `RenameSceneObjectCommand` (handle +
  group-when-present + card sweep; the panel's SceneObjectRename field re-routes to it);
  `SetNodeHandleCommand` for lights; inverse-pair unit tests for every new command
  (execute+undo, def byte-equality), importer/merge unit tests updated.
- **Gate.** *Positive:* `-p manifold-renderer --lib -p manifold-editing --lib`; **held-out
  import gate:** import `the_rosetta_stone.glb` (the fixture v1's briefs never used for
  emission tests), assert every object row is scene_object-shaped (no migration fires on a
  fresh import: `migrate_scene_object_wires` returns `false`), renders (PNG); merge
  skull-into-warehouse still passes v1's P4 gate unchanged. *Negative:* importer emits zero
  legacy-port wires (`rg -n '"mesh_\{' gltf_import.rs` → 0 in emission code); no cloned
  NodeIds in duplicate's output (unit test asserts freshness).
  **Demo (L2):** rosetta import PNG + a duplicate-command PNG pair (original, then original +
  offset copy), read by the orchestrator.
- **Forbidden:** cloning card exposes/bindings in duplicate (D11 says not); re-running the
  whole importer for duplicate (clone the subtree); touching the panel (P5's).
- **Test scope:** focused; lands solo (batch 2); sweep + clippy + deny at landing.

### P4 — The numeric value-cell contract (app-wide widget work, panel-independent)

- **Entry state:** P1–P3 landed (only for repo currency — this phase touches `manifold-ui` +
  `manifold-app` text-input/commit paths and does not depend on the Object model); anchors:
  `slider.rs:230`, `stepper.rs:44`, `text_input.rs:22`, `input.rs:20`,
  `scene_setup_panel.rs:561-616`, `panels/dropdown.rs`.
- **Read-back:** D8–D10 + Invariants; UI_WIDGET_UNIFICATION_DESIGN D2/D3/D13/P5d (the
  pattern and the precedent for retiring a dead stop); BUG-198's backlog entry (why type-in
  gates at L2).
- **Deliverables:** `value_cell.rs` contract module + tests (D8's table verbatim);
  `stepper.rs` table amendment + test update; Shift-fine in the dock's `ValueDrag` apply
  path (×0.1) and the audio dock's calibration drag; `TextInputField::SceneNumericParam` +
  ctx + commit path (mirror `InspectorParam`'s, `app_render.rs`) wired to EVERY dock numeric
  cell (steppers, triplets, modifier/camera/env/fog rows); the audio-dock sibling variant
  (VERIFY-enumerate its cells first); D9 dropdown-on-click for 3+-label enum cells; D10
  degrees table applied (display, type-in parse, scrub steps);
  `dock_numeric_cells_register_full_contract` invariant test.
- **Gate.** *Positive:* `-p manifold-ui --lib -p manifold-app --lib`; L3 flow
  (`scripts/ui-flows/scene-setup-scrub-fine.json`): drag a light-intensity value cell with
  and without shift, assert the fine delta is 0.1× the coarse one and undo restores; L3 flow:
  click `shadow_softness` value cell, assert dropdown opens, click "Contact", assert the
  param write. Type-in gates at **L2 + unit** (BUG-198: headless text injection has no seam —
  do NOT fake an L3): unit tests on the commit parse (incl. degrees rows: type "45" into a
  rot cell → 0.7853981 rad lands), PNG of the open type-in box over a value cell,
  affordance-checked. *Negative:* `rg -n "Gesture::DoubleClick" scene_setup_panel.rs` → 0
  outside the `value_cell::intent_for` call (no raw gesture matching).
  **Demo (L3 + PNG):** the two flows + the type-in PNG. **Performer gesture:** shift-drag
  Fog density to land exactly 0.42 without overshooting, mid-set, one hand.
- **Forbidden:** clamping typed values (PARAM_RANGE_CONTRACT P1); a new text-editing widget
  (TextEditModel + the existing overlay ARE the surface); converting degrees anywhere but
  the panel boundary.
- **Test scope:** focused; lands with P5 (batch 3).

### P5 — Outliner + properties (the visible payoff; closes the wave)

- **Entry state:** P1–P4 landed in-tree (P4 same worktree is fine); anchors:
  `scene_setup_panel.rs:738/895/1120-2000` (build_live + section builders), `scene_vm.rs`
  (D12's rewrite target), tree-selection styling precedent
  (⚠ VERIFY-AT-IMPL: `rg -n "selected" crates/manifold-ui/src/tree.rs` — transcribe the
  selected-row colors).
- **Read-back:** D6, D7, D12 + Invariants; v1 doc D7 (empty states, verbatim carried);
  GROUPING_GRAPHS one-pager skim (what groups are again).
- **Deliverables:** `SceneVm` v2 per D12 (+ its synthetic-def unit tests: scene_object
  scenes grouped/ungrouped/mixed/custom); `SceneSelection` + the UI-local map + fallback
  rule; `build_outliner` (rows, glyphs, eye, footer buttons) + `build_properties`
  (dispatch to relocated builders — bodies reused, a diff that MOVES code, not rewrites
  it); properties header with name + Duplicate/Remove buttons dispatching P3's commands;
  light rename UI (double-click light row name → SetNodeHandleCommand via a
  `SceneLightRename` text field variant); eye toggle wired; existing
  `scripts/ui-flows/scene-setup-*.json` flows updated for the new layout (rows now live
  under a selection — each flow selects its outliner row first).
- **Gate.** *Positive:* `-p manifold-ui --lib -p manifold-app --lib -p manifold-renderer --lib`;
  L3 flows: (a) click the light's outliner row → assert its intensity row appears in
  properties, drag it, undo; (b) click an object's eye → assert `visible` param flipped AND
  a PNG pair shows the object gone from the preview, undo restores; (c) v1's fog-drag flow
  re-pointed through World selection — **expected GREEN again** (one selection's rows fit
  the window; if still unreachable, that's BUG-199's remit — do not fix scroll here, note it
  in the landing report). Selection-fallback unit test. **Held-out:** the merged
  warehouse+skull scene — outliner shows both objects + lights; PNG read by orchestrator,
  affordance check (eye/buttons read as clickable). *Negative:* §4 carried gates;
  `rg -n "SceneSelection" crates/manifold-io crates/manifold-core` → 0.
  **Demo (L3 + PNGs):** flows above + full-panel PNG on the merged scene.
  **Performer gesture:** click Warehouse's eye — the venue vanishes, the skull hangs in fog;
  click again — it's back. One glance, one click, reversible.
- **Forbidden:** a generic param-tree properties renderer (v1 D3's named wrong turn — still
  the tempting one); serializing selection; rewriting row builders that only needed moving.
- **Test scope:** focused; **wave close at the P4+P5 landing:** full workspace sweep +
  `cargo clippy --workspace -- -D warnings` + `cargo deny check bans` + the GPU suite iff
  any P4/P5 diff touched shader/runtime files (none should — escalate if the diff disagrees);
  landing report per §8.10; this doc's Status line updated; supersession sweep per §8 below.

**Phasing-completeness walk:** Object type/atom/executor/planner/fusion (P1) · render_scene
surface + migration + preset regen + invisible-shadow proof (P2) · importer/merge/add emit +
remove (BUG-193) + duplicate + rename object/light commands (P3) · scrub-everywhere contract +
Shift-fine + dock type-in + enum dropdowns + degrees (P4) · outliner + eye + selection +
properties + duplicate/remove/rename UI + flows (P5). Every §2 commitment lands in exactly one
phase; everything else sits in §9 with a trigger.

## 6. Performance (stated honestly)

Per frame added: one `Copy` struct emit per object (≤64) on the content thread — the
`node.light` cost class, nanoseconds against a 16ms frame; render_scene does the same slot
resolutions as today. Load-time: the migration walk is linear in nodes and runs once per def
generation (idempotent no-op after). The panel remains UI-thread view + dispatch; P2's PNG
byte-diff (panel open vs closed, carried from v1) re-certifies the show path. No
`MANIFOLD_RENDER_TRACE` gate owed; a phase that finds itself adding content-thread work
beyond the struct emit is off-brief — stop and escalate.

## 7. Decided — do not reopen

1. Object identity = `node.scene_object` + `Object` wire; groups are legibility only
   (Peter, 2026-07-17). The v1 "no new port types" invariant is superseded by this doc.
2. `SceneObject` carries CPU facts by value, GPU resources as `Slot`s; `Copy`; single-hop.
3. `visible` lives on `scene_object`, port-shadowed; invisible = no draw AND no shadow.
4. Migration is idempotent, structural, version-gate-free, and lossless-or-inert — never
   silently dropping.
5. Object/light names = node `handle`; object rename sweeps handle + group + card sections
   in ONE command.
6. Panel = outliner (Camera · World · lights · objects, eye on objects, footer add buttons)
   + one properties region; selection UI-local, doc-id-keyed, fallback first-object-else-World.
7. Numeric value cells: drag scrub / double-click type / right-click reset / Shift = ×0.1,
   via `value_cell.rs`; type-in never clamps.
8. 3+-label enum cells open the dropdown; 2-label cells stay click-cycle.
9. Angle rows display degrees at the panel boundary only (D10's row table).
10. Duplicate offsets `pos_x` +0.5, suffixes the name, never clones exposes/bindings;
    Remove renumbers and is BUG-193's fix.
11. BUG-199 (dock scroll) is out of scope — owned elsewhere; v1's D1/D5/D6/D7/D8 doctrine
    carries forward unchanged.
12. All v1 "Decided" items (its §7) stand except #3's layout description and the §4
    invariant this doc names — the graph stays the only model.

## 8. Execution notes for the orchestrator

- Fresh session per phase; the phase brief + this doc are the context (standard §8). Gates
  run by the orchestrating session; PNGs read, not assumed; landing reports in
  `docs/landings/`, status line updated per §8.9.
- Landing batches: P1+P2 (the model flip is atomic — never land P2 without P1 or vice
  versa), P3, P4+P5. One worktree slot for the workstream
  (`python3 scripts/agent-worktree.py acquire scene-object-v2 wave/scene-object-v2`).
- Escalations that PAUSE a phase: any need for a second `Object` consumer/producer, any
  migration topology D5's rule can't express, any command with no v1 precedent shape, any
  render_scene draw-path change beyond D4's read-through, anything touching `layer.rs` /
  serialization formats / new shared state.
- **Supersession sweep at the final landing (house rule):** update this doc's Status line;
  v1 doc header gains "object model + layout superseded by SCENE_OBJECT_AND_PANEL_V2 (D1/D7)";
  BUG-193 Status → fixed-by pointer; BUG_BACKLOG lines for 194/195 gain "still open under
  v2" notes if touched; `rg -n "group.*identity\|mesh_k\|mesh_0" docs/ memory/` and fix or
  tombstone every hit asserting the old model; `python3 scripts/gen_docs_index.py`.

## 9. Deferred (explicitly not v1, each with its revival trigger)

- **Multi-select + batch edit** (shift-click outliner rows, edit common rows). Trigger: the
  first real set-dressing session that wants to move five props at once; design the
  batch-command semantics then.
- **Per-light eye / solo** (needs an `enabled` on `node.light` + renderer skip — small but
  it touches the Light struct; not worth riding this wave). Trigger: first "kill that light
  on the break" ask.
- **Drag-reorder in the outliner** (object order = port index today; reorder = renumber
  command, mechanical under v2). Trigger: a real scene where draw/occlusion inspection
  order matters to Peter.
- **Camera "frame selected"** (write orbit params from the selected object's transform).
  Trigger: viewport navigation (REALTIME_3D P5) landing — do it there, where the math
  already lives.
- **Unit metadata on `ParamDef`** (degrees/ms/Hz app-wide, cards included). Trigger: the
  second surface that wants D10's table.
- **Outliner icons beyond text glyphs**. Trigger: the UI design-system pass
  (UI_SOTA_UPGRADE_PLAN) reaching the docks.
- **Audio-dock full row audit under the value-cell contract** (P4 covers its numeric cells;
  any exotic rows found get listed, not improvised). Trigger: the VERIFY-enumerate in P4
  finding non-numeric-cell shapes.
- Everything in v1's §9 (clip scene-states, per-scene ms, asset embedding, hot reload,
  multi-camera, multi-scene picker, animation section, video textures, scene presets,
  splats) — unchanged owners, unchanged triggers.
