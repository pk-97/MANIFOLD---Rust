# Scene document — scenes are the source of truth, the graph is the compiled backend

<!-- index: Authoritative design for the 3D scene redesign: a scene layer stores a SceneDoc of ten item kinds, five commands compile to today's EffectGraphDef, one physics setting with automatic collision shapes, pixel picking, cameras per display, starters as saved scenes. Clean cut on one branch, four waves, eleven lanes. -->



**Status:** APPROVED · not started · 2026-10-22 · epic BUG-elwvu (Scene document: scenes are the source of truth, the graph is the compiled backend). Lane beads BUG-elwvu.1 to BUG-elwvu.11, listed in section 11.

**Document type:** Design and plan. A fresh session builds from this doc alone.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) section 5 (Phase briefs); [AGENT_ROUTING.md](AGENT_ROUTING.md) for seats, briefs and gates.
**Supersedes:** [SCENE_SETUP_PROGRAMME.md](SCENE_SETUP_PROGRAMME.md); [PHYSICS_ENGINE_BOUNDARY_DESIGN.md](PHYSICS_ENGINE_BOUNDARY_DESIGN.md) section 5.2 (One scene-object model) and section 5.5 (Copy groups); [SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md](SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md); the card, exposure and starter sections of [SCENE_SURFACES_DESIGN.md](SCENE_SURFACES_DESIGN.md) and [SCENE_SETUP_PANEL_DESIGN.md](SCENE_SETUP_PANEL_DESIGN.md); the fluid-role model of the water family; the open compound-model lanes G2b to G7.
**Evidence:** `docs/evidence/scene_document_2026-10-22/before/` holds every card of every scene preset as it renders today (`<Preset>__NN_<item>.png`, and `<Preset>.txt` with every row as text, offscreen rows included). The `after/` folder beside it is filled before landing (section 10).

Binding constraints:
- Persistence: the scene format changes outright. Old scenes and presets may break (D-119, no old-show compatibility). No migrations, no old-show proofs.
- Thread residency: the content thread owns the project and compiles. The UI reads `Arc<Project>` snapshots and sends commands. Unchanged.
- Performance surface: every value a card shows is an ordinary manifest param, bindable to MIDI, OSC, LFO, envelope and automation. Runtime unchanged.
- Hot path: compiling runs on a structural edit and at load, never per frame. The renderer, the physics engines and the param runtime see what they see today.

## 1. Rulings

These decide the design. Where a source note disagrees, the ruling wins.

1. **Scenes are scenes (D-120, scenes are scenes).** A scene layer stores a `SceneDoc`, the source of truth. The node graph is the backend it compiles to. Hand-wired graphs are only for future geometry, shader and material node assets.
2. **Clean cut.** One branch. The old graph-as-scene path is deleted in the same work. Land once. No dual paths, fallbacks, stopgaps or migrations. Old scenes and presets may break (D-119, no old-show compatibility). No interim versions: physics gets automatic collision shapes from the start.
3. **Stage versus setup.** Structural properties are item fields set by commands. They are never mapped, modulated or triggered: kind, shape, physics setting, Draw As, Emit From, light type, environment source, Copies on or off. Live params stay mappable: transforms, colour, intensity, forces, impulses, speed, reset, visibility and the active camera. A param write never recompiles or resets anything. A structural edit may restart that item's simulation.
4. **Ten kinds, exactly:** Group, Object (Shape: built-ins, Ocean, or Mesh from file), Camera, Light (Sun, Point, Spot, Environment), Material, Water, Water Source (Fill, Inflow, Drain), Swarm, Force, World. World holds physics (Gravity, Speed, Reset, Air Resistance), Fog and Render. A layer plane is not a kind (section 7.6).
5. **Imports are a Group whose Source is a file.** The children are ordinary Objects whose ids derive from the file's node path. A Model File swap keeps children whose path still exists. A Combine Meshes switch on the group is off by default.
6. **Copies is a section** on Object and Group (Count, Layout, Spacing, Columns, Turn), not a kind.
7. **Five commands:** Add (with an optional source, which is Duplicate), Remove, Reparent, Rename, Set. Group and Ungroup are composites in one `ExecuteBatch`, on Cmd+G and Cmd+Shift+G. All go through `EditingService`, one undo step each. `ItemId` is a plain newtype. Param ids are `<item id>.<property>`.
8. **Compiler:** `compile(&SceneDoc)` gives an `EffectGraphDef` plus a manifest. It compiles the whole graph per structural edit, on the content thread, at structural change and at load. Node ids come from (item id, role). Forces go to today's `scene_modifiers` and `load/expand`; bodies go to `physics_members`. Nothing below the loader changes.
9. **Physics:** one setting, None, Collider or Body. The card shows Mass, Bounce and Friction, with no Advanced section. One `CollisionShape` service gives automatic shapes: the exact mesh for static colliders in Box3D, a spatial split into hulls with automatic detail for bodies, and a distance lattice for water. Its cache key includes scale and water cell size. Water reads Colliders and Bodies whose bounds touch its domain. No kinematic setting. Speed clamps and never errors. A body whose shape is still being built joins when it is ready, and the world never pauses for it. The `CollisionShape` service and the physics install are the one gate into Box3D: nothing non-finite or degenerate ever reaches the solver, and a Box3D assert never aborts the app in the show build (section 7.8).
10. **Pixel clicking.** The renderer's selection mask pass is extended into an item-id buffer, so a click picks exactly what is drawn. Gizmo handles remain only for things with no pixels (lights, forces, water domains, sources). One selection store, shared by tree, viewport and preview, and it works with gizmos hidden.
11. **Cameras are repeatable.** Each output or display picks its own camera. The active camera is a live param (camera cuts on a beat; multi-display rigs).
12. **Cards by kind:** one table keyed by (kind, property), rows through the param surface recipe ([WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) section 5b (Agent contract and enforcement)). No preset rows, sections, labels or `cardVisible`. The Lighting section stays, with every light and the environment together in one card, as today. The HDRI file picker is always present in Environment mode. Bake size and force detail are automatic. Rows that do not apply never show. A refusal is a disabled control with its reason on hover, never standing text. No layout, styling or naming changes beyond this list: same card, same kind; pixel clicking; no refusal text; the one physics row; rows that do not apply hidden; the real hierarchy tree with nested groups and Cmd+G; Copies section; shared materials with "used by N" and Make Unique; camera per display; generator swaps always work; the Scale link toggle (section 7.17).
13. **Base scene:** camera, sun, environment (sky by default, HDRI one click away), World, one cube. "Scene" is the first entry in the browser. Starters are scene files saved from the app, in a closed format (`deny_unknown_fields`). Ocean is the Ocean shape. Lantern is dropped unless it rebuilds from normal items.
14. **Testing:** tests only for breaks a performer would notice, each lane with a stated test budget. No new UI flow suites; flows on removed behaviour are deleted. A GPU proof runs once per finished stage (swarm Draw As a group, water colliders, pixel picking). The starter test is small: load, compile validates, values within limits, base items present. Before landing, every card is rendered from the new build into `docs/evidence/scene_document_2026-10-22/after/`, beside the before-set, for Peter to judge.
15. **Starting point:** the built branches `lane/g1-instance-space` (fce17d15c, renderer instance space) and `lane/g2a-nested-groups` (4ccea112a, transform parent chain) are merged into the redesign branch first. They conflict by one line in the `frame_setup.rs` closure, where G2a's `parent.world_with_rotation` wins inside G1's composition. G2a's old-path UI half is replaced by the new panel.
16. **Plan:** four waves on one branch, eleven lanes (section 11). About 8 to 10 hours of agent wall clock, at most four parallel builds (disk). Peter tests a branch build after wave 2.

## 2. Verdict

A scene layer stores a tree of typed items. The render graph is compiled from it, so the graph becomes the backend it already is at runtime. Every card is defined once per item kind, so a starter is just a saved scene. Each object gets one physics setting (None, Collider, Body) whose collision shape comes from the mesh automatically, and water, rigid bodies and swarms all read it. A click on the output picks the pixel's item.

On stage: Peter opens Scene from the browser, gets a camera, sun, sky and a cube, adds objects, groups and lights, picks an HDRI from the Lighting card, and sets physics with one control. Every card is the same card for the same kind, whoever built the item. Anything he maps or modulates (transforms, colour, forces, speed, camera cut) stays live and never rebuilds the scene. Anything structural (physics setting, Draw As, Emit From) changes on a click and may restart that one item's simulation, the way a Reset does.

## 3. Where every Scene panel row comes from today

Verified 2026-10-22 at main 256ff292f. The Scene panel is derived from the graph, which is the reverse of every 3D tool. A row exists only if the preset's stored metadata exposed it.

| Fact | Where it comes from | Anchor |
|---|---|---|
| Which rows an item has | Every entry in the preset's `presetMetadata.params` whose first binding targets one of the item's nodes. A node nobody exposed has no rows | `manifold-app/src/ui_bridge/projection/scene.rs:295-323` (`parameter_ids_for_nodes`, `owned_specs`); `SceneItemVm.parameter_ids`, `manifold-ui/src/panels/scene_setup_panel/view_model.rs:357-365` |
| How exposures get there | Stamped into the preset at insert (`stamp_scene_node_exposures_into`) and again at load for a fixed list of node types | `manifold-core/src/scene_exposure.rs:126`; vocabulary `manifold-nodes-scene/src/node_graph/scene_exposure.rs:36-73`; load stamp `:171-205` |
| What a template exposes | Add Object stamps material, transform and object; Add Swarm stamps only the nodes its row owns | `scene_templates.rs:104-119`, `owned_card` `:444-485` |
| Group, order, role, gates, units | The scene table, per node type and param. Already by kind, but by node kind, not item kind | `manifold-nodes-scene/src/node_graph/scene_surface.rs:121-156` |
| Section of a row the table does not describe | The item's first group (fallback) | `scene_setup_panel.rs:1425-1432` (`property_group`) |
| Labels, ranges, sections of hand-authored rows | The preset JSON | `manifold-nodes/src/starter_scenes.rs:252-276`, `:310-321` |
| What the layer card shows | Pins, plus any authored control with `card_visible` | `projection/scene_card.rs:180-196` |
| File pickers (HDRI, Model File, Scan Model) | Only a preset-level string param with a string binding to the node's `path` | `projection/scene.rs:140-162`; `scene_setup_panel/lighting.rs:79-115`; `starter_scenes.rs:226-235` |
| Item kinds | `SceneItemKind` has Camera, Light, Object(Mesh, Plane, Water, Model), Force, Physics, World, Source; no Swarm | `manifold-foundation/src/scene_surface.rs:242-287` |
| Refusals on cards | Capability reasons printed as standing text | `manifold-editing/src/commands/graph/scene/physics.rs:150` |
| Per-preset graph wiring the panel depends on | Swarm's hidden spawn cube and shape node; Water in Scan's lifted cliff chain and hand-wired collider; Rubble's Blast; every water preset's role wiring | `scene_templates.rs:371,386`; `starter_scenes.rs:157-237`, `:287-321` |

The browser cannot offer an empty scene because `Scene.json` says `"available": false`. BUG-vis7l (Scene preset hidden) and BUG-hkshf (generator change refused) are fixed by design here: Scene is the first browser entry, and swapping a layer's generator always works.

A Layer Plane object (a plane showing another layer's output) is a special object kind today. It refuses physics with "Enable Physics requires a supported static mesh source", printed twice on its card. Under this design it is an ordinary Object (section 7.6).

Click handling today (what pixel picking replaces):
- A geometric pick against declared gizmo boxes, not pixels. The pick shape is a unit box sized by Scale params (`scene_surface.rs:170-179`), hit within 8 pt of an edge or 22 pt of origin (`viewport_gizmo.rs:59-61`, `:626-660`). Parts inside groups are never pickable (`scene_vm/shapes.rs:175,199`).
- Three selection stores (`scene_setup_panel.rs:541`, `physics_overlay.rs:92`, `workspace.rs:130`) and two press hosts (`window_input.rs:873`, `physics_overlay.rs:230`). The preview one is gated on Show Gizmos (`scene_setup_panel.rs:903`, `physics_overlay.rs:183`).
- Each press rebuilds `SceneVm::from_def` on the UI thread (`physics_overlay.rs:240`).
- The GPU selection mask (`render_scene.rs:2979`) is drawn and never read.

Runtime keep, reset and rebuild rules today are spread over `physics_carry.rs:12-60` (CPU rigid state by node id), `physics.rs:319` (`slot_needs_rebuild`), `physics_world.rs:441` (count change rebuilds), `physics_world.rs:479-489` (`hold_pending` freezes the clock while a collider cooks or Speed is out of range, `:319-325`). Rulings 3 and 9 settle the stage behaviour; section 7.4 says what changes.

## 4. Starters today, and the refusals

Sixteen bundled presets contain a `render_scene`. What each contains that a user could not build with the Scene panel's own actions:

| Starter | What a user could not build |
|---|---|
| Scene (the empty one) | Nothing; it is the base scene. Hidden from the browser |
| Swarm, Swarm Cloud | Hidden spawn cube and particle shape with no rows; the item's Transform is really the force lattice bounds; listed under Objects with a "Model" header; "Billboard" and "+ Add Modifier" rows that do nothing for a swarm; standing text "Enable Physics requires a shared group transform"; HDRI mode with no file picker; Air Drag on both the swarm and the Physics World; Swarm Cloud's Burst force renamed and moved by the builder |
| Rubble Pile | A "Size" row fanned out to three scale params; an Explosion card with a preset-only section "Rubble" holding "Blast" |
| Physics Boxes, Physics Solids | Copies rows hand-edited in the JSON; standing text "Make Copies needs one scale on all three axes" |
| Water in a Photoscan | A cliff scan chain lifted from Ocean Cliff by node id, a hand-wired collider, a preset-only "Scan Model" string param bound to five nodes; the object card prints "Remove this object's Fluid Role before enabling Physics…" twice |
| Water Dam Break (GPU FLIP, Matter, Particles), Floating Box, Still Pool, Sea Wall | Fluid roles hand-wired to the water; "HDRI File: (empty)" with no HDRI wired; Width and Height (the bake size) on Lighting; a stray "No physics in this scene" line above the Camera card on Sea Wall |
| Ocean Cliff | "Environment: Custom (edit in graph)"; six "Object k" rows; "Enable Physics requires an authored transform" on graph-built objects |
| Ocean | "Environment: Custom (edit in graph)"; its surface is "Object 0", a custom producer |
| Lantern | "HDRI File: (empty)" with no HDRI; one material-named object |

Every lighting card shows Width and Height, the environment bake size (`scene_surface.rs:465`).

Refusals that live only in a log line today:
- `content_commands.rs:1192`: a force's Impulse that cannot fire.
- `app_lifecycle.rs:778` and `:867`: a glTF import with no metadata, or a layer command that made no layer, abort silently.
- `ui_bridge/projection/cards.rs:896`: a force card's target list that cannot resolve shows empty.
- `ui_bridge/dispatch/presets.rs:56,71,85` and `dispatch/browser.rs:68,96`: preset import, export, push, duplicate and delete failures.
- The generator change refusal (`scene_modifier_transfer.rs:158`). It cannot occur under this design.

Rule from here on: a refusal is never only a log line, and a card never shows a refusal as standing text. An action that cannot happen is drawn disabled, with its reason on hover, through `ParamRow.spec.inactive_reason` (`manifold-ui/src/param_surface.rs:195`). A failed action that was attempted toasts.

## 5. Distance from today, deleted against added

Measured in non-test lines, from the files read.

| Area | Lines now | What it does now | Fate |
|---|---|---|---|
| `SceneVm::from_def` and its trace (`scene_vm.rs` 3,202 plus `scene_vm/` 1,447) | 4,650 | Reads cameras, lights, objects, water, physics back out of graph wiring | The view types (about 900) stay, built from the document; the trace is deleted (about 3,750) |
| `scene_object_model.rs`, `scene_items.rs` | 1,028 + 286 | Derive object rows, kinds, capabilities and the item list from graph shape | Deleted; the item tree replaces them (1,310) |
| Exposure stamping and its load migrations (`manifold-core/src/scene_exposure.rs`, nodes-scene `scene_exposure.rs`, `compound.rs`, `fluid_objects.rs`) | 2,505 | Copy node params into preset metadata so rows exist | Deleted; rows come from the (kind, property) table |
| Template insertion (`scene_template.rs`) and scene modifier graph edits (`scene_modifier_edit.rs`) | 2,459 | Splice template groups and force graphs into the scene graph | Builders move into the compiler; splicing deleted (about 2,000) |
| Scene graph commands (`editing/commands/graph/scene.rs` and `scene/`) | 7,271 | Every Add, Duplicate, Copy, Physics, Copies, Role edit as a graph rewrite | Deleted; replaced by about 1,500 lines of item commands |
| Scene projections (`projection/scene.rs`, `scene_card.rs`) | 2,488 | Join stamped metadata to items | Metadata joins deleted (about 1,200); card bands and values stay |
| glTF importer (`gltf_import/`) | 7,047 | Loads files and builds graph groups | Loading stays; about 3,000 lines of graph building become item building |
| `starter_scenes.rs` and the swarm preset builder | about 500 | Hand-built starter graphs | Deleted; starters are saved scenes |
| Fluid role collider path, rigid shape enum and Collider Detail param, `physics_match.rs`, Fluid Role and authored-transform refusals | about 600 | Contradicting collision settings | Deleted |
| The Layer Plane object kind and its "supported static mesh source" physics refusal | not measured | A special object with its own rules | Deleted; a plane Object with a layer-sourced Material replaces it |
| G2a's old-path UI half | not measured | Group rows on the graph-derived panel | Deleted; the new panel replaces it |
| Scene tab panel | 6,821 | Draws the tree and cards | Stays; its data source changes |

| Deleted by the end of wave 4 | Lines |
|---|---|
| `SceneVm::from_def` trace | 3,750 |
| `scene_object_model.rs`, `scene_items.rs` | 1,310 |
| Exposure stamping and its load migrations | 2,505 |
| Template splicing and scene modifier graph edits | 2,000 |
| Scene graph commands | 7,271 |
| Scene projections' metadata joins | 1,200 |
| Importer graph building | 3,000 |
| `starter_scenes.rs` and the swarm preset builder | 500 |
| Fluid role collider path, rigid shape enum, refusals | 600 |
| **Total (Layer Plane path and G2a's UI half not counted)** | **about 22,000** |

| Added | Lines |
|---|---|
| `SceneDoc`, ids, (kind, property) table, five commands | 2,000 |
| Compiler (moved builders plus new glue) | 3,000 |
| `SceneVm` from the document | 1,000 |
| `CollisionShape` service and the Box3D input gate | 900 |
| Importer to items | 1,200 |
| Item-id buffer and one selection store | 600 |
| Cameras per display | 300 |
| Scale link toggle | 100 |
| **Total** | **about 9,100** |

Sixteen preset JSON files (about 1.9 MB, Swarm alone 357 KB of stamped metadata) become small scene files.

What carries over unchanged: the node engine and every primitive; the renderer (including G1's instance space and G2a's transform parent chain); Box3D; the GPU FLIP and MPM solvers; the force expansion passes (`load/expand`); the param manifest, bindings, modulation, OSC and MIDI runtime; the `ParamSurface` card rows; the gizmo core and shape declarations (their param ids become property ids); the Scene tab widgets; the frame planner. The compile target is today's `EffectGraphDef` plus its manifest, so nothing downstream of the compiler changes.

## 6. Industry practice, and the USD mapping

| Tool | Scene truth | Graphs |
|---|---|---|
| Blender | Objects in collections and parent trees; the startup file holds a cube, a light and a camera | Node trees attach to objects, materials and the world |
| Cinema 4D | Object manager tree; tags carry physics and materials | Scene Nodes and node materials attach to objects |
| Unity | GameObjects with components; the default scene has a camera and a directional light | Shader Graph and VFX Graph are assets that components reference |
| Houdini Solaris | USD stage | SOP networks feed prims |
| USD | Prims in a hierarchy with typed attributes and relationships | UsdShade node graphs attach to materials |

We follow USD for the model, Blender for the startup scene, Unity and USD for physics.

| Our item | USD concept | Notes |
|---|---|---|
| Scene document | Stage, one root layer | One per scene layer |
| Group | `UsdGeomXform` with children | Nesting is free |
| Object, built-in Shape | `UsdGeomCube`, `Sphere`, `Cylinder`, `Cone`, `Capsule`, `Plane` | Platonic solids have no USD gprim: a `Mesh` with a generator (difference 1 below) |
| Object, Shape Mesh from file | `UsdGeomMesh` prims under an `Xform` that carries a reference to the file | The import is a Group whose Source is the file; children are keyed by file node path |
| Object, Shape Ocean | A generated surface | Ours; no USD equivalent |
| Material | `UsdShadeMaterial` with a `UsdPreviewSurface` shader; `material:binding` on items | Binding inherits down the tree, nearest wins. Texture source None, file or layer (a layer is ours) |
| Camera | `UsdGeomCamera` (focal length, aperture, focus distance, f-stop, clipping range) | The orbit rig is ours (difference 7) |
| Light: Sun, Point, Spot | `UsdLuxDistantLight`, `SphereLight`, `SphereLight` plus `ShapingAPI` | |
| Light: Environment | `UsdLuxDomeLight` with `inputs:texture:file` | The HDRI file picker is the dome's texture; procedural sky is ours (difference 8) |
| World, physics part | `UsdPhysicsScene` (gravity) | Speed and Reset are ours (difference 4) |
| Physics setting Collider | `UsdPhysicsCollisionAPI` plus `MeshCollisionAPI` approximation | |
| Physics setting Body | `UsdPhysicsRigidBodyAPI` plus `CollisionAPI`, `MassAPI` | Body on a group with colliders below it is one compound body |
| Copies section | `UsdGeomPointInstancer`, prototype = the object or group | Physics per copy: rigid body on the prototype, as UsdPhysics allows |
| Swarm | `UsdGeomPointInstancer` whose positions come from a simulation | Draw As = prototype; Emit From is ours |
| World, Render part | `UsdRenderSettings` | Ray tracing switches as custom attributes |

Where we deliberately differ, and why:
1. **Platonic solids.** USD has no gprim for them; ours are built-in Shapes backed by generated meshes.
2. **Stable item ids.** USD names a prim by its path; renaming or moving it changes the path. Live mappings, modulators and automation must survive a rename mid-show, so every item has a stable id and a param id is `<item id>.<property>`. The path is for display and export.
3. **Values live in the layer's param manifest.** A USD attribute holds its value and time samples. Ours holds the base value in the manifest entry the performance surface already owns (modulation, drivers, envelopes, pins, OSC). One home per value; export reads it from there. Time is beats; export bakes samples.
4. **The physics clock.** `UsdPhysicsScene` has no Speed or Reset. Ours does: live performance needs to slow, freeze and restart physics on a beat.
5. **Forces.** USD has no standard force-field schema. `Force` is our typed item (Radial, Uniform, Vortex, Wind, Drag, Attractor, Explosion) with a target list, the `UsdCollectionAPI` shape.
6. **Water.** USD has no fluid schema. `Water` is our typed item (Houdini's FLIP domain, Blender's Mantaflow domain), with Water Source children.
7. **Orbit camera rig.** USD cameras are transforms. Our Orbit, Tilt, Distance and Target drive it; export bakes the transform.
8. **Procedural sky and fog.** The dome may take a procedural sky instead of a file; fog is on the World.
9. **Mesh modifiers.** USD has no modifier stack. Ours is an ordered list on the object, compiled as today.
10. **Swarm emission.** A PointInstancer's positions are authored arrays; ours come from a GPU simulation each frame.
11. **Per-clip overrides.** A clip may override a scene layer's params, including file paths (today's clip `string_params`). Closest to a USD session layer; kept as clip data.

## 7. The design

### 7.1 The scene document

- `SceneDoc` lives on a scene layer's generator instance (`PresetInstance`, `manifold-core/src/effects/instance.rs`), beside `params`. For a scene layer the graph is never saved; it is compiled.
- `SceneDoc` is a tree of `Item { id, name, kind, fields, children }`. `kind` is the closed enum of ten (ruling 4). `fields` are the structural properties of that kind, a closed set per kind: Object's Shape and physics setting, Copies on or off, Light's type, Environment's source, Water Source's role, Swarm's Draw As and Emit From, a Group's Source file and Combine Meshes, a Material's texture source.
- References between items (a material binding, Draw As, Emit From, force targets, a Material's source layer) are fields, set by `Set`. Material binding inherits down the tree, nearest wins.
- The file format is closed: `deny_unknown_fields` everywhere. No preset-declared param, section, label, `cardVisible` or string binding exists in a scene layer. `presetMetadata.params`, `bindings`, `stringParams`, `stringBindings` are not part of a scene layer.
- Every live property is a param in the layer's manifest, id `<item id>.<property>`, created by the compiler from the kind's table. Structural fields are never params (section 7.4).
- Ownership: content thread. Mutation: `EditingService` commands only, each one undo step. Serialization: the document plus the manifest, as JSON inside the project. The UI reads the document from its `Arc<Project>` snapshot through one seam, `scene_of(project, layer) -> Arc<SceneVm>`; every host (project bridge, projection, gizmo, edit selection, setup check) uses it. There is no second constructor.
- The layer card shows only what the user pinned (SCENE_SURFACES_DESIGN D4). A starter that wants a performer row on the layer card ships with the pin set, which a user can also do.

### 7.2 Identity and the five commands

- `ItemId` is a plain newtype (a stable unique id, never a path). It replaces `SceneNodeRef` as the scene's identity: selections, gizmo owners, commands and force targets carry it. `SceneNodeRef` (a graph scope path) is deleted with the old path.
- Commands, all through `EditingService` via `ContentCommand::Execute` or `MutateProject`:
  - **Add** `(kind, parent, index, optional source)`. With a source it duplicates that item and its subtree with fresh ids. Adding a Swarm also adds its default Attractor, core and gust Force items aimed at it, in the same undo step.
  - **Remove** `(id)`. Removes the subtree. Anything referencing a removed item (Draw As, Emit From, force targets, material bindings) falls back to its default in the same step.
  - **Reparent** `(id, new parent, index)`. Also reorders.
  - **Rename** `(id, name)`.
  - **Set** `(id, field or param, value)`. A structural field set is a structural edit (section 7.4). A param set is a param write.
- **Group** (Cmd+G) is Add Group plus Reparent of the selection, and **Ungroup** (Cmd+Shift+G) is Reparent of the children plus Remove, each in one `ExecuteBatch`, one undo step.
- Param writes still go through today's param commands, addressed by `<item id>.<property>`. Renaming or reparenting never changes a param id.

### 7.3 The compiler

- `scene_compile::compile(&SceneDoc) -> CompiledScene { graph: EffectGraphDef, manifest }` in `manifold-nodes-scene`, shaped like `physics_members::compile` (`manifold-node-engine/src/load/physics_members.rs`): a pure function over a definition.
- One emitter per kind. Today's builders become the emitters: `object_group`, `light_template`, `swarm_body` (`scene_templates.rs:84,257,336`), the water family template in `manifold-nodes-water`, the force recipes. The bodies move; nothing is invented.
- Node ids derive from (item id, role). A recompile after a structural edit keeps every unchanged node's id, so the runtime keeps what it keeps today for node-id-matched state.
- It compiles the whole graph per structural edit, on the content thread, and at load. A param write never recompiles. Editing today already clones the whole graph per transaction (`scene_transaction.rs:164`); the cost is the same order.
- Forces compile to today's `scene_modifiers` instances and the `load/expand` passes. Bodies ride `physics_members`. Nothing below the loader changes.
- The compiler's allowed node vocabulary is named in one list and tested: a compiled graph containing a node type outside it fails the compile test.
- A scene layer has no editable graph and no inspection view in this cut.

### 7.4 Stage versus setup

| Structural (item fields, set by commands, never mapped) | Live (manifest params, mappable) |
|---|---|
| Kind; Object Shape; physics setting; Draw As; Emit From; light type; environment source; Copies on or off; Water Source role; Group Source and Combine Meshes; Material texture source | Transforms; colour; intensity; forces and impulses; speed; reset; visibility; the active camera; Count, Spacing, Columns, Turn |

- A param write never recompiles or resets anything.
- A structural edit recompiles and may restart the edited item's simulation, like a Blender re-bake. The card marks the control as a setup control; it is not a mappable row.
- Carry: CPU rigid state is carried by node id across a recompile as today (`physics_carry.rs`). GPU FLIP and swarm state reseeds on a structural rebuild of their own item.
- Speed clamps to its range and never errors. The out-of-range hold at `physics_world.rs:319-325` is replaced by a clamp.
- A body whose collision shape is still being built joins when ready. The world keeps ticking meanwhile. The `hold_pending` freeze (`physics_world.rs:479-489`) is deleted.

### 7.5 The base scene

What every new scene holds, as Blender's startup file and Unity's default scene do:
- **Camera:** orbit rig at a three-quarter view, 50 mm equivalent.
- **Sun** (Light, Sun).
- **Environment** (Light, Environment): source Sky (Softbox) by default, HDRI File one click away with the file picker always on the card in HDRI mode. Exposure 0 EV.
- **World:** Gravity -9.81, Speed 1, idle until an item has physics; Render settings; Fog off.
- **One cube** at the origin (Object, Shape Cube, physics None).

"Scene" is the first entry in the browser and opens exactly this. Every starter is this base plus items added by the panel's own actions, saved.

### 7.6 Materials and layer planes

- A Material is an item bound by reference, inherited down the tree, nearest wins. A group's material reaches every child with none of its own.
- The Material's texture source is None, a file, or a layer. This is today's Skin Source.
- A layer plane (a plane showing another layer's output) is not a kind. It is an ordinary Object with Shape Plane whose Material takes its image from a layer. Physics, Copies, picking and the card work as for any plane; the "supported static mesh source" refusal and the special Layer Plane object path are deleted.
- The Material card appears inline on the object that uses it, with "used by N" and Make Unique. Make Unique duplicates the material for that object (Add with a source, then Set the binding, one `ExecuteBatch`).
- Today one material belongs to each object (`scene_templates.rs:98`); after this, any number of items bind one material.
- Material features (Glass, Coat and the rest of "+ Add feature") are structural fields on the Material, set by one `Set` with their default values in the same `ExecuteBatch`. Adding a feature is one undo step, and undo removes it completely. Today a feature is a batch of param writes plus a panel-only record of which features were added (`material_menus.rs:95`), which undo cannot see, so Cmd+Z leaves the feature behind.
- Rule for every card: what the Scene panel shows comes only from the document and its params. No scene state lives in panel-only memory (open sections and scroll position are view state, not scene state).

### 7.7 Imports

- A dropped glTF becomes a Group whose Source is the file (one Object if the file has a single mesh). Children follow the file's node tree: one Object per mesh-bearing node, empty pass-through nodes collapsed, nested as groups where the file nests.
- Child ids derive from the file node path, so a Model File swap keeps every child whose path still exists. The rest are removed, with a toast naming the count.
- A Combine Meshes switch on the group merges the children into one mesh. Off by default.
- Edits to a child (transform, material, physics) are ordinary item edits and survive a swap with their child. This fixes BUG-yuc2n (Paste across layers drops the source clip's Model File override) by construction, and copying an imported model is Add with a source, which makes a working clone: BUG-tt9ce (Copying an imported model never made a working clone) is fixed by design.

### 7.8 Physics and collision shapes

- **One setting on Object and Group: None, Collider, Body.** Collider is a static solid; Body is a dynamic rigid body. On a group, Body is one compound body of everything inside (where the body sits decides). A Body inside a Body is refused as a disabled control with its reason.
- **The card shows** Physics, then (Body only) Mass, then Bounce and Friction (Collider and Body). No Advanced section. No Kinematic setting. Rows that do not apply never show.
- **The shape comes from the mesh automatically**, from one `CollisionShape` service in `manifold-physics` that takes an item's mesh and world scale and returns what each solver asks for:
  - Static collider in Box3D: the exact triangle mesh (`manifold-physics/native/box3d/bridge.c:358-411`, `b3CreateMesh`).
  - Body in Box3D: a spatial split into hulls with automatic detail chosen from size and triangle count. Today's `prepare_colliders` (`manifold-water-rigid/src/physics_mesh.rs:83-135`) is the method, with a 0.1% shell for open scans and the 64-hull cap (`MAX_BODY_HULLS`, `manifold-physics/src/lib.rs:350`).
  - Water (GPU FLIP, MPM): a signed distance lattice for a closed mesh, unsigned with a shell for an open one (`manifold-physics/src/sdf.rs`). Shell thickness is automatic: one water cell, or 0.1% of the mesh, whichever is larger.
  - Swarms: none in this cut (section 14).
- **Cache key** includes the mesh content, the item's scale and the water cell size (`physics.rs:1363`). A late-arriving mesh (an async load) must flip the key: the key hashes a content-ready signal, not size and transform alone.
- **Water reads it directly.** Every Collider and every Body in the water's World whose bounds touch the water domain is a solid in the water; Bodies couple two ways as rigid bodies do today. No fluid role on an object, no per-solver shape path.
- The solvers stay separate underneath.
- **One gate into Box3D.** The `CollisionShape` service and the physics install are the only way anything reaches Box3D. They validate every transform, scale and hull as finite and non-degenerate. Bad input is refused with a reason shown on the card (a disabled control or a toast), and nothing non-finite ever reaches the solver. A Box3D assert must never abort the app in the show build. This covers the broad-phase assertion `stackCount < B3_TREE_STACK_SIZE - 1` in `b3DynamicTree_Query` (`crates/manifold-physics/native/box3d/src/dynamic_tree.c:1142`), a tree more than 1024 levels deep, which NaN or infinite bounds cause. It is tracked by BUG-k787p (Box3D tree query assertion), fixed in lane L4.
- Deleted: the Collider role and its Geometry, Builtin Shape, Radius, Exact Mesh and Shell Thickness params on `node.fluid_role_source`; the rigid body's own Shape enum and Collider Detail param (`rigid_body.rs:191`); "+ Fluid Role" and "Target: Water" rows; the role wiring in `scene/fluid/roles.rs`; `physics_match.rs`; the Fluid Role and authored-transform refusals.

### 7.9 Copies

- A section on Object and Group: Count, Layout (Grid, Ring, Scatter), Spacing, Columns, Turn. Count 1 means no copies. Copies on or off is the structural field; the numbers are live params.
- Copies of a group draw through G1's instance space; copies take their transform parent chain from G2a. Physics per copy: the prototype's setting applies to every copy.
- There is no Make Copies action and no Copies kind.

### 7.10 Water, Water Source, Ocean

- **Water** is a kind: Domain (its Transform is the box: Position and Size on the card, moved and resized by its gizmo), Resolution, Simulation dials, Whitewater; its looks and sources as children. Today the Water card has no domain placement rows; this is the fix. The water family template in `manifold-nodes-water` is the emitter.
- **Water Source** is a child item with Role Fill, Inflow or Drain; Shape Box or Sphere; Transform; Inflow speed. The role is shown in the header. The Collider role is gone (section 7.8).
- **Ocean** is an Object Shape. Its rows (the surface dials) appear on the Object card when Shape is Ocean.
- BUG-fbwu6 (Initial Fill refusal is not named by the domain setup warning) is fixed by design: Fill on a source that cannot fill is a disabled control with its reason.

### 7.11 Swarm

- Swarm is its own kind (not an Object). Emit From: Box volume (default), Sphere, or the surface of any Object or Group. Draw As: Tetrahedron, Cube, Octahedron, Icosahedron, Sphere, or any Object or Group, drawn per particle through G1's Group instance space. Both are structural fields.
- Count, Size, Air Drag, Material, Look are live. Its Transform places the emitter; the force lattice bounds follow the emitter with a margin. Force detail is automatic.
- The hidden spawn cube and shape node of today's Swarm are gone.

### 7.12 Forces

- Force is a kind: its recipe type (Radial, Uniform, Vortex, Wind, Drag, Attractor, Explosion), the recipe's rows, Targets (everything with physics, or chosen items), Impulse. Forces compile to today's scene modifier instances and the `load/expand` passes.
- The "scene modifiers attached to a preset" concept goes. A force is an item a user adds, with `Add`.

### 7.13 Cameras

- Camera is a repeatable kind. Rows: Rig (Orbit, Free, Look At); Orbit, Tilt, Distance, Target, Roll, Field of View, Clipping; Depth of Field; Motion Blur.
- The active camera is a live param. Each output or display picks its own camera, so cuts happen on a beat and multi-display rigs show different views. The pick is mappable and never recompiles. World holds the scene's default active camera.

### 7.14 Pixel clicking and selection

- The renderer's selection mask pass (`render_scene.rs:2979`) becomes an item-id buffer. The compiler publishes a table from item id to render slot. The content thread renders the buffer and resolves pixel to slot to `ItemId`. A click picks exactly what is drawn, including parts inside groups.
- Gizmo handles remain only for things with no pixels: lights, forces, water domains, sources. Their shape declarations stay.
- One selection store, the Scene panel's, shared by tree, viewport and preview. It works with gizmos hidden. The other two stores and the second press host are deleted. No press rebuilds a scene view on the UI thread.

### 7.15 Starters

- A starter is a scene file saved from the app (document plus manifest values), under `crates/manifold-nodes/assets/scene-presets/`, in the closed format.
- Ocean is the Ocean shape. Lantern is dropped unless it rebuilds from normal items.
- Changing a layer's generator always works, as in Resolume and Ableton: the old scene goes with the old generator, one undo step; one undo brings both back. There is nothing to transfer, so `scene_modifier_transfer`'s refusal cannot occur.

### 7.16 Not changed

The renderer apart from the item-id buffer, every primitive, both solvers and Box3D, the param runtime and modulation, the gizmo core and shape declarations, the Scene tab widgets, the frame planner.

### 7.17 Scale link toggle

- Every Transform section has a link toggle beside Scale X, Y and Z, on by default (Unity's constrain-proportions link; Cinema 4D and Figma proportion locks).
- Linked: editing any one axis scales all three proportionally, in one undo step. Unlinked: each axis is free.
- It is a card editing helper over the existing three params. It adds no param, so modulation, MIDI and OSC still target each axis as today. The link state is a card setting and is not mapped.
- Built through the param surface recipe in WIDGET_TREE_DESIGN.md section 5b (Agent contract and enforcement), with no bespoke row infrastructure.

## 8. Cards by kind

One table keyed by (kind, property) replaces the per-node scene table (`scene_surface.rs:121-156`) and preset exposures. It carries the same facts: group, role, band, order, gate, unit, limit. Rows go through the param surface recipe: manifest params with ids, plus the existing capability-driven action rows (the physics setting, Draw As, Emit From, Shape, file pickers). No bespoke row, slider or drawer. The table lives in one file per kind so lanes do not collide.

| Kind | Card |
|---|---|
| Camera | Rig; Orbit, Tilt, Distance, Target, Roll, Field of View, Clipping; Depth of Field; Motion Blur |
| Lighting (one card, as today) | Environment source (Sky, HDRI File, None); in HDRI mode: File picker (always present), Rotation, Intensity, Background (Blur, Brightness), Aim Sun at HDRI; in Sky mode: Gradient or Softbox rows; Exposure; then every light |
| Light | Type, Colour, Intensity, Position, Target, Range, Size, Shadows (rows by type) |
| Object | Shape (built-ins, Ocean, Mesh from file); Transform (with the Scale link toggle); Material (inline, used by N, Make Unique); Look (Visible, Cast Shadows, Emission); Modifiers; Physics; Copies. Textures, today's Skin, are the Material's texture source |
| Group | Transform (with the Scale link toggle); Visible; Material (applies to children with none); Physics (Body here is one compound body); Copies; when Source is a file, Model File and Combine Meshes |
| Material | Surface rows; texture source (None, file, layer); used by N; Make Unique |
| Swarm | Emit From; Draw As; Count; Size; Air Drag; Material; Look; Transform places the emitter |
| Water | Domain: Position and Size (with the Scale link toggle), moved and resized by its gizmo in the viewport; Resolution; Simulation dials; Whitewater (Amount only: the particle budget, today's "Whitewater Budget", is automatic from Resolution). Physics View is not a card row: it is a Render menu mode in the header beside Solid and Wireframe |
| Water Source | Role (header); Shape; Transform; Inflow speed |
| Force | Its type's rows; Targets; Impulse |
| World | Physics (Gravity, Speed, Reset, Air Resistance); Fog; Render (ray tracing, temporal upscale, render mode); default active camera |

No preset rows, sections, labels or `cardVisible`. Every row's section is declared in the table; there is no fallback section. Today an undeclared row falls into the item's first group (`scene_setup_panel.rs:1425-1432`), which is why Whitewater Budget and Physics View sit under Transform. Rows that do not apply never show. Bake size and force detail are automatic and have no rows. A refusal is a disabled control with its reason on hover.

The tree is the item hierarchy: Camera and Lighting, World pinned at the top as today, then every other item in its place with a kind icon, nested groups shown as a real tree. A Swarm is its own kind, not an Object.

## 9. Alternatives, priced

| Shape | Cost | What it forecloses |
|---|---|---|
| **A. Scene document compiled to today's graph (chosen)** | 11 lanes, 8 to 10 hours of agent time | Hand-wired scene graphs (intended) |
| B. Keep the graph as truth; derive cards by kind from it and forbid custom graphs by validation | 4 lanes | Every Add, Duplicate, Copies and physics edit stays a graph rewrite (7,271 lines); the graph trace stays; a starter can still differ from a user scene in wiring the validator did not foresee. Ruled out |
| C. Scene document executed directly by a scene renderer with no graph | Renderer, physics and modulation rewritten | Reuse of the whole backend; months |

Kill-pass on A:
- Strongest case against: the compile seam loses runtime state on recompile (a physics world restarting when a light is added). Cheapest check: L1 includes a non-gating check that adds a light to a compiled scene with a running physics world and reports whether the tick count continues. Carry is by node id and CPU-only today; ruling 3 allows a structural edit to restart that item's simulation, but an unrelated light must not restart the body.
- Second: compile time on a large import (a 300-node kit) makes structural edits stutter. Check in L3 with Peter's largest file; the compile is linear in items and the importer already builds a graph of the same size.
- Third: two scene paths. There are none by design: the old path is deleted in the same branch (ruling 2). The cost is that between wave 2 and wave 3 the branch has no Swarm, Water or Ocean items, and the old water and swarm starters do not load. Wave 3 rebuilds them.

## 10. Testing

Tests exist for breaks a performer would notice. Each lane states a budget (section 11); going over it needs the lead's yes.

- **No new UI flows at all.** Flows that exercise removed behaviour are deleted in the lane that removes it. Peter's hands-on test after wave 2 is the click-through.
- **CPU first:** command undo exactness, id stability across rename and reparent, save and load round trip, compile determinism, node-id stability across an unrelated edit, group material inheritance, shape-service cache keys, and the Box3D input gate (a NaN scale and a degenerate mesh are refused and the world keeps stepping).
- **GPU proofs, once per finished stage, one run each, through `scripts/gpu_queue.py`:** swarm Draw As a two-part group (L6), water with a collider and a body (L7), pixel picking returns the drawn item (L9). Never nextest for GPU proofs.
- **Starter test (L10), small:** every scene file in `scene-presets/` loads under `deny_unknown_fields`; its compile validates; values are within their limits; the base items (camera, sun, environment, World) are present.
- **After-renders:** before landing, L10 renders every card of every starter from the new build into `docs/evidence/scene_document_2026-10-22/after/`, using the same names as the before-set, so each before/after pair can be viewed side by side. Peter judges them.
- **Deletion proof (after L10):** `rg -n 'from_def|stamp_scene_node_exposures|scene_object_models|insert_scene_template|SCENE_VOCABULARY|SceneNodeRef' crates` returns zero outside the doc.

## 11. Plan: four waves, eleven lanes, one branch

Branch `wave/scene-document`. Step zero, done by L1 before anything else: merge `lane/g1-instance-space` (fce17d15c) and `lane/g2a-nested-groups` (4ccea112a) in. The one conflict is in the `frame_setup.rs` closure: G2a's `parent.world_with_rotation` wins inside G1's composition. Drop G2a's old-path UI half. Merge main into the branch at the end of each wave. Land once, after wave 4.

Each lane makes one commit and stops for review. Each runs scoped clippy and nextest on the touched crates, regenerates the node catalog and docs index where it changed them, and one `--cheap` landing gate. At most four lanes build in parallel (disk).

The compiler is built as one module per kind under `scene_compile/`, and the (kind, property) table as one file per kind, so parallel lanes add files and do not edit each other's.

The task beads for G1 (renderer instance space) and G2a (transform chains) stay open until this branch lands, since the branch carries them.

### Wave 1

**L1 (Opus; BUG-elwvu.1): the document, ids, table, commands, and the first compiler.**
- Does: merge G1 and G2a; `SceneDoc`, `ItemId`, the closed ten-kind enum and fields, the (kind, property) table for Group, Object, Camera, Light, Material, World; the five commands plus Group and Ungroup composites; the compiler for Group, Object (including Shape Plane with a layer-sourced Material), Camera, Light (all four types), Material (nearest-wins binding; texture source None, file or layer), World (Gravity, Speed with clamp, Reset, Air Resistance, Fog, Render); `PresetInstance` gains the document; the allowed-vocabulary list; the base scene as a file in code. Physics emits as None for now.
- Files: `manifold-core/src/scene_doc.rs` (new), `manifold-core/src/scene_schema/` (new, one file per kind), `manifold-editing/src/commands/scene_doc/` (new), `manifold-nodes-scene/src/node_graph/scene_compile/` (new, moving `object_group` and `light_template` from `scene_templates.rs`), `manifold-core/src/effects/instance.rs`.
- Deletes: nothing yet beyond G2a's old-path UI half. The old path stays compiling until L2 so the branch builds.
- Test budget: about 12 tests. Every command undoes exactly; ids survive rename and reparent; save and load round trip; deny_unknown_fields rejects an unknown field; compile is deterministic and node ids are stable across an unrelated edit; a group's material reaches a child with none; compiled graph stays inside the vocabulary; Speed clamps. Plus the non-gating state-continuity check (section 9).
- Depends on: none.

### Wave 2 (four lanes in parallel, after L1: three Sonnet, one Opus)

**L2 (Sonnet; BUG-elwvu.2): Scene tab, Add menu and Lighting card on the document; the old path is deleted.**
- Does: `scene_of(project, layer)` seam and `SceneVm` from the document, every host moved to it; the tree as the real hierarchy with nested groups and kind icons; Add menu (Object, Light, Camera, Group, Material and what other lanes add later); Cmd+G and Cmd+Shift+G; the Lighting card (environment source, always-present HDRI picker, every light); cards by kind for Group, Object, Camera, Light, Material, World; the Scale link toggle in every Transform section (section 7.17); disabled-with-reason action rows; the base Scene as the first browser entry; generator swaps always work (BUG-hkshf, BUG-vis7l); one selection store for the panel.
- Deletes: the `SceneVm::from_def` trace, `scene_object_model.rs`, `scene_items.rs`, exposure stamping and its load migrations, template splicing, the graph scene commands, the `scene_surface.rs` per-node table, scene projections' metadata joins, `starter_scenes.rs`, standing refusal text, `Scene.json`'s `available: false`, the Layer Plane object path and its physics refusal, flows on removed behaviour. Old Swarm, Water, Ocean items and their starters stop loading here; waves 2 and 3 rebuild them.
- Files: `manifold-app/src/ui_bridge/` (project.rs, projection/), `manifold-ui/src/panels/scene_setup_panel/`, `manifold-nodes-scene` deletions, `manifold-editing/src/commands/graph/scene*` deletions, `manifold-core/src/scene_exposure.rs` deletion, `manifold-nodes/src/starter_scenes.rs` deletion.
- Test budget: about 7 tests, no new flows. Adding a material feature and undoing it leaves the material exactly as before. Card rows come out right for each kind and never show standing refusal text. A layer-sourced plane accepts the physics setting. A linked Scale edit scales all three axes in one undo step; unlinked it changes one.
- Depends on: L1.

**L3 (Sonnet; BUG-elwvu.3): imports as groups.**
- Does: a dropped glTF becomes a Group with a file Source, children keyed by file node path; Model File swap keeps surviving children and toasts the rest; Combine Meshes switch; paste and duplicate of an import.
- Deletes: importer graph building (about 3,000 lines); the copy-an-import refusal.
- Files: `manifold-nodes-scene/src/node_graph/gltf_import/`, `manifold-editing/src/commands/scene_doc/`.
- Test budget: about 4 tests. Ids derive from node path and are stable; a swap keeps a surviving child and drops a vanished one; Combine Meshes off by default; duplicate of an import makes a working clone. Compile time of Peter's largest file is measured and reported, not gated.
- Depends on: L1.

**L4 (Opus; BUG-elwvu.4): physics setting, the CollisionShape service, and the Box3D input gate.** Opus because it touches the physics install, the native bridge and the open Box3D crash in section 7.8.
- Does: the None, Collider, Body setting and card (Mass, Bounce, Friction); emitters for Collision and RigidBody; `CollisionShape` with automatic shapes (exact mesh, spatial split with automatic detail, distance lattice) and the cache key (mesh content ready, scale, water cell size); pending shape joins when ready and the world never pauses; Speed clamp; group Body as one compound body; validation of every transform, scale and hull as finite and non-degenerate before Box3D, with refusals shown on the card; no Box3D assert aborts the show build. Fixes BUG-k787p (Box3D tree query assertion).
- Deletes: the Collider role params, the rigid body's Shape enum and Collider Detail, `physics_match.rs`, `hold_pending`, the Fluid Role and authored-transform refusals.
- Files: `manifold-physics/` (new `collision_shape`; the install path and the native bridge), `manifold-water-rigid/src/physics_mesh.rs`, `manifold-node-engine/src/load/physics_members.rs`, `manifold-nodes-scene/.../scene_compile/physics.rs`, `rigid_body.rs`, `scene/fluid/roles.rs` deletions.
- Test budget: about 7 tests. A static scan collider is a Box3D mesh shape; a body's hulls never span two parts; automatic detail gives the same count for a scaled copy; the cache key changes with scale and with cell size; rows that do not apply are absent; a cooking shape does not pause the world; one test feeds a NaN scale and a degenerate mesh and asserts the world keeps stepping.
- Depends on: L1.

**L5 (Sonnet; BUG-elwvu.5): Copies section plus Force items.**
- Does: Copies on Object and Group (Count, Layout, Spacing, Columns, Turn), drawn through G1's instance space; physics per copy from the prototype; Force kind with all seven types, Targets and Impulse, compiled to scene modifier instances; the force target list resolves or shows a disabled reason.
- Deletes: `scene_modifier_edit.rs` splicing, the Make Copies action path, copy-group graph rewrites, the Impulse log-only refusal.
- Files: `manifold-nodes-scene/.../scene_compile/copies.rs` and `forces.rs`, `manifold-editing/src/commands/scene_doc/`, `manifold-ui` Copies and Force cards.
- Test budget: about 5 tests. Copies of a group place correctly; Count 1 means none; a Body prototype gives every copy a body; a force reaches its targets; the Impulse fires once.
- Depends on: L1.

Peter tests a branch build after wave 2 (base scene, objects, lights, groups, imports, physics, copies, forces). The lead gives the exact launch command with the worktree binary path.

### Wave 3

**L6 (Sonnet; BUG-elwvu.6): Swarm.**
- Does: Swarm kind with Emit From and Draw As (any Object or Group, through G1's Group instance space); force lattice bounds follow the emitter; default forces added with the Swarm; Add menu entry.
- Deletes: the swarm preset builder, the hidden spawn cube and shape node, Billboard and Add Modifier rows on swarms.
- Files: `manifold-nodes-scene/.../scene_compile/swarm.rs` (from `swarm_body`, `scene_templates.rs:336`), swarm card table, `manifold-ui`.
- Test budget: about 3 tests plus one GPU proof: Draw As a two-part group renders both parts per particle (one frame).
- Depends on: L2, L4, L5.

**L7 (Sonnet; BUG-elwvu.7): Water, Water Source, Ocean.**
- Does: Water and Water Source emitters (Fill, Inflow, Drain); water reads Colliders and Bodies whose bounds touch the domain through `CollisionShape`; Ocean Shape on Object; disabled-with-reason Fill refusal (BUG-fbwu6).
- Deletes: fluid role wiring, `fluid_objects.rs`, the Water in Scan lifted chain, the bake-size rows.
- Files: `manifold-nodes-water/`, `scene_compile/water.rs`, `manifold-water-rigid/`, water card tables.
- Test budget: about 4 tests plus one GPU proof: water with a collider and a body, one frame; a collider outside the domain does not reach the water.
- Depends on: L2, L4.

**L8 (Sonnet; BUG-elwvu.8): cameras per display.**
- Does: repeatable Camera items; the active camera as a live param; each output or display picks its own camera; mappable to MIDI and OSC; camera cut on a beat.
- Deletes: the single-camera assumption in output and display code.
- Files: `manifold-app` output and display code, `scene_compile/camera.rs`, `manifold-ui` display settings.
- Test budget: about 3 tests. Two cameras, two displays show different cameras; changing the active camera does not recompile; a cut on a beat lands on that frame.
- Depends on: L2.

**L9 (Opus; BUG-elwvu.9): pixel clicking.**
- Does: the item-id buffer from the selection mask pass; the compiler's item-to-slot table; content-thread pixel resolve; click picks exactly what is drawn, including parts in groups; one selection store for tree, viewport and preview; gizmo handles only for no-pixel things; works with gizmos hidden.
- Deletes: the geometric pick against gizmo boxes for pixel items, the second and third selection stores, the second press host, the UI-thread scene rebuild on press.
- Files: `manifold-nodes-scene/src/node_graph/primitives/render_scene.rs` and its selection pass, `viewport_gizmo.rs`, `physics_overlay.rs`, `window_input.rs`, `workspace.rs`, `scene_setup_panel.rs`.
- Test budget: about 3 tests plus one GPU proof: a click on a drawn pixel returns that item, a click on a part inside a group returns that part, and a click works with gizmos hidden.
- Depends on: L2.

### Wave 4

**L10 (Sonnet; BUG-elwvu.10): starters, the starter test, after-renders, docs.**
- Does: build the starters in the app (or with the same commands) and save them: Scene, Swarm, Swarm Cloud, Rubble Pile, Physics Boxes, Physics Solids, Water in a Photoscan, the water set, Ocean Cliff, Ocean (as the Ocean shape); Lantern only if it rebuilds from normal items; the small starter test; the after-render of every card into `docs/evidence/scene_document_2026-10-22/after/` with the before-set's names; docs sweep (mark superseded docs, update maps, regenerate the node catalog and docs index); the deletion proof.
- Deletes: the old preset JSON files, any remaining dead scene code the deletion proof finds, docs text that describes the deleted path.
- Files: `crates/manifold-nodes/assets/scene-presets/`, `crates/manifold-nodes/tests/`, `docs/`.
- Test budget: the one small starter test (section 10), nothing else.
- Depends on: L3, L6, L7, L8, L9 (and so L2, L4, L5).

**L11 (Opus; BUG-elwvu.11): adversarial review, fixes, one landing.**
- Does: review the whole branch against this doc and the rulings, looking for survivors of the old path, a param write that recompiles, a structural property that got a mappable row, a standing refusal text, a second selection store, a shape path outside `CollisionShape`, anything non-finite that can reach Box3D; fix what it finds; run the landing gate once; the lead lands.
- Files: whatever the review finds.
- Test budget: a regression test only for a performer-visible break it finds.
- Depends on: L10.

Bead ids: BUG-elwvu.1 to BUG-elwvu.11 are L1 to L11 in order. Dependencies are set in beads.

## 12. Forbidden moves

- A second scene path, fallback, stopgap, shim or migration. Old scenes may break (D-119, no old-show compatibility).
- A preset-only param, section, label, `cardVisible` or string binding on a scene layer.
- A row the (kind, property) table does not declare, or a bespoke row, slider or drawer (the param surface recipe named in ruling 12).
- Hand-editing a compiled graph, or saving it.
- A structural property exposed as a mappable param.
- A param write that recompiles or resets anything.
- A refusal as standing card text, or only in a log line.
- Scene state kept in panel-only memory, out of reach of undo.
- A fallback section for a row, or a capacity or budget row a performer has to manage.
- A physics setting beyond None, Collider, Body; an Advanced section; a Kinematic setting.
- A second collision-shape path per solver, or any route into Box3D that skips the input gate.
- An interim physics (hand-picked shapes) "until automatic shapes land".
- A gizmo-box pick for something that has pixels; a second selection store.
- A special Layer Plane kind or path.
- A Scale link implemented as a new param or a bespoke row.
- A new `SceneNodeRef` use, or a path as identity.
- An eleventh kind, or a Copies, ImportedModel or Lighting kind.
- A UI flow suite for new behaviour, or a test beyond the lane's budget.

## 13. Costs, stated honestly

- **Values live in the manifest, not the item.** One home per value and an untouched runtime, but the document alone is not a complete scene: export and the starter test read both.
- **Structural properties cannot be modulated.** An LFO cannot switch Draw As on stage. Switching physics, Draw As or Emit From mid-show restarts that item's simulation, as a Reset does.
- **Whole-graph recompile per structural edit.** Same order as today's graph clone per transaction, but a large import's first structural edit is where a stutter would show (checked in L3).
- **Old scenes break.** Shows built on graph scenes stop loading their scene contents (D-119). Landing does not wait on them and no migration is built; any critical one is rebuilt by hand later.
- **Between wave 2 and wave 3 the branch has no Swarm, Water or Ocean.** Peter's wave-2 test covers the rest.
- **Stable ids instead of USD paths.** Right for live mappings; an id-to-path map is needed on export.
- **Water reads every Collider and Body in its World whose bounds touch the domain.** A collider that should not touch the water must be kept out of the domain; there are no collision groups in this cut.
- **Automatic collision detail** may miss on hard scans, and there are no manual overrides. The better method is a follow-up (section 14).
- **The input gate refuses bad physics input** instead of simulating it. A scene with a NaN scale shows a refusal on its card and that item stays out of the solver.
- **No graph view of a scene layer.** Power users lose free graph editing of a whole scene and any inspection of the compiled graph.
- **Ocean and Lantern.** Ocean returns as a Shape; Lantern is dropped unless it rebuilds from normal items.

## 14. Deferred

Each has a trigger; none is built in this work.
- **Convex decomposition by CoACD** (Wei et al., SIGGRAPH 2022) or V-HACD for bodies. Trigger: the automatic spatial split misses on a real scan. Ported code needs file-header credit and `THIRD_PARTY_NOTICES.md`.
- **Swarm collisions** using the water's distance lattice. Trigger: after the swarm lane lands and Peter asks.
- **Material and geometry node assets** (Material source = graph asset, a geometry graph on an Object, a sky graph on an Environment). Trigger: a node feature for materials or geometry exists.
- **A read-only view of the compiled graph.** Trigger: a debugging need the logs cannot meet.
- **Hover preview of a structural edit's effect** (keep, install, restart item, restart world) before the click. Trigger: Peter loses a simulation to a structural click on stage.
- **Water collision groups** and a "Water collision: off" switch. Trigger: a collider that must stay out of the water but sits in its domain.
