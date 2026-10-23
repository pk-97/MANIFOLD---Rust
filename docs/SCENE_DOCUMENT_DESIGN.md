# Scene document — scenes are the source of truth, the graph is the compiled backend

<!-- index: Authoritative design for the 3D scene redesign: a scene layer stores a SceneDoc of eleven item kinds, compiled to today's EffectGraphDef by the only writer of that graph; one physics setting with automatic collision shapes; one ordered modifier chain that works on any shape; pixel picking; cameras per display; starters as saved scenes; every scene feature main had restored. One branch; lanes with checklists in section 11. -->



**Status:** APPROVED · waves 1 and 2 built on `wave/scene-document` (df95dd7ce); fix lanes F1, F2 and L6 running · epic BUG-elwvu (Scene document: scenes are the source of truth, the graph is the compiled backend). Lane beads are children of that epic, listed in section 11.

**Document type:** Design and plan. A fresh session builds from this doc alone.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) section 5 (Phase briefs); [AGENT_ROUTING.md](AGENT_ROUTING.md) for seats, briefs and gates.
**Supersedes:** [SCENE_SETUP_PROGRAMME.md](SCENE_SETUP_PROGRAMME.md); [PHYSICS_ENGINE_BOUNDARY_DESIGN.md](PHYSICS_ENGINE_BOUNDARY_DESIGN.md) section 5.2 (One scene-object model) and section 5.5 (Copy groups); [SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md](SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md); the card, exposure and starter sections of [SCENE_SURFACES_DESIGN.md](SCENE_SURFACES_DESIGN.md) and [SCENE_SETUP_PANEL_DESIGN.md](SCENE_SETUP_PANEL_DESIGN.md); the fluid-role model of the water family; the open compound-model lanes G2b to G7.
**Judged by:** Peter in the app. No card render sets are made or kept.

Binding constraints:
- Persistence: the scene format changes outright. Old scenes and presets may break (D-119, no old-show compatibility). No migrations, no old-show proofs.
- Thread residency: the content thread owns the project and compiles. The UI reads `Arc<Project>` snapshots and sends commands. Unchanged.
- Performance surface: every value a card shows is an ordinary manifest param, bindable to MIDI, OSC, LFO, envelope and automation. Runtime unchanged.
- Hot path: compiling runs on a structural edit and at load, never per frame. The renderer, the physics engines and the param runtime see what they see today.

Calls marked "default, pending Peter" are built as written until Peter rules otherwise. Section 15.3 lists them.

## 1. Rulings

These decide the design. Where a source note disagrees, the ruling wins.

1. **Scenes are scenes (D-120, scenes are scenes).** A scene layer stores a `SceneDoc`, the source of truth. The node graph is the backend it compiles to.
2. **Clean cut.** One branch. The old graph-as-scene path is deleted in the same work. Land once. No dual paths, fallbacks, stopgaps or migrations. Old scenes and presets may break (D-119, no old-show compatibility). No interim versions: physics gets automatic collision shapes from the start.
3. **Stage versus setup.** Structural properties are item fields set by commands. They are never mapped, modulated or triggered. Live properties are manifest params and stay mappable. Section 7.4 is the one list of which is which. A param write never recompiles or resets anything. A structural edit may restart that item's simulation.
4. **Eleven kinds, exactly:** Group, Object, Camera, Light (Sun, Point, Spot, Environment), Material, Water, Water Source (Fill, Inflow, Drain), Swarm, Force, Modifier, World. A Modifier is one card in the scene's ordered modifier chain (ruling 20). An Object's Shape is a built-in solid (Cube, Sphere, Plane, Tetrahedron, Octahedron, Icosahedron, Dodecahedron), with Ocean and the procedural shapes added by their lanes. An imported part is an Object that draws its file's mesh; "Mesh from file" is not a Shape choice (default, pending Peter). World holds physics (Gravity, Speed, Reset, Air Resistance), Fog, Render and the default camera. A layer plane is not a kind (section 7.6).
5. **Imports.** Add → Mesh from file and a drop both run the one importer (default, pending Peter). A file with several meshes becomes a Group whose Source is the file; a file with one mesh becomes one Object. Children are ordinary Objects whose ids derive from the file's node path. Model File lives on the import Group and on a single imported Object (default, pending Peter). A Model File swap keeps children whose path still exists. A Combine Meshes switch on the group is off by default.
6. **Copies is a section** on Object and Group (Count, Layout, Spacing, Columns, Turn), not a kind.
7. **Five commands:** Add (with an optional source, which is Duplicate), Remove, Reparent, Rename, Set. Group and Ungroup are composites in one `ExecuteBatch`, on Cmd+G and Cmd+Shift+G. All go through `EditingService`, one undo step each. `ItemId` is a plain newtype. Param ids are `<item id>.<property>`.
8. **Compiler:** `compile(&SceneDoc)` gives an `EffectGraphDef` plus a manifest. It compiles the whole graph per structural edit, on the content thread, at structural change and at load. Node ids come from (item id, role). Forces and modifiers go to today's `scene_modifiers` and `load/expand`; bodies go to `physics_members`. Nothing below the loader changes, with one exception: a scene modifier instance addresses compiled nodes by node id instead of `SceneNodeRef` (lane L12).
9. **Physics:** one setting, None, Collider or Body. The card shows Mass, Bounce and Friction, with no Advanced section. One `CollisionShape` service gives automatic shapes: the exact mesh for static colliders in Box3D, a spatial split into hulls with automatic detail for bodies, and a distance lattice for water. Its cache key includes scale and water cell size. Water reads Colliders and Bodies whose bounds touch its domain. No kinematic setting. Speed clamps and never errors. A body whose shape is still being built joins when it is ready, and the world never pauses for it. The `CollisionShape` service and the physics install are the one gate into Box3D: nothing non-finite or degenerate ever reaches the solver, and a Box3D assert never aborts the app in the show build (section 7.8).
10. **Pixel clicking.** The renderer's selection mask pass is extended into an item-id buffer, so a click picks exactly what is drawn. Gizmo handles remain only for things with no pixels (lights, forces, water domains, sources). One selection store, shared by tree, viewport and preview, and it works with gizmos hidden.
11. **Cameras are repeatable.** A scene layer picks the camera it renders with a live Camera row on the World card: mappable, a cut lands on its frame, no recompile. Removing the picked camera falls back to the first camera in the same undo step. A pick per output or display (multi-display rigs) means rendering the scene once per camera and compositing per display; it needs its own design: BUG-lz538 (per-display camera pick).
12. **Cards by kind:** one table keyed by (kind, property), rows through the param surface recipe. No preset rows, sections, labels or `cardVisible`. The Lighting section stays, with every light and the environment together in one card. The HDRI file picker is always present in HDRI mode. Bake size and force detail are automatic. What is hidden, what is disabled and what toasts follows section 4. The cards are exactly what section 8 lists; no other layout, styling or naming change. The recipe: [WIDGET_TREE_DESIGN.md](WIDGET_TREE_DESIGN.md) section 5b (Agent contract and enforcement).
13. **Base scene:** camera, sun, environment (sky by default, HDRI one click away), World, one cube with its own Material (default, pending Peter). "Scene" is the first entry in the browser. No starters ship: only the base Scene; Peter builds scenes in the panel and saves them. A saved scene is a file in a closed format (`deny_unknown_fields`). The old scene presets are deleted, Lantern included.
14. **Testing:** the goal is that nothing Peter opens crashes, freezes or loses his work. One run-everything crash test for scenes, plus the short promise list in section 10. No tests of shape, digests or half-built features; no UI flows; no card render sets.
15. **Base:** the branch carries G1 (renderer instance space) and G2a (transform parent chain). Their task beads stay open until the branch lands.
16. **Plan:** one branch, the lanes in section 11, at most four parallel builds (disk). Peter tests a branch build after each fix round, with the exact launch command.
17. **Silence means Blender.** Where this doc is silent, an item behaves as it does in Blender (Unity for the default scene and components). The lane names each such call in its report.
18. **One writer per region.** The scene compiler writes the scene. On a scene layer's graph the scene is one sealed Scene node, read-only in the graph editor, with outputs for colour and the render's other passes (depth, normals, item id). Its only inputs are the layer's frame width and height (`SCENE_INPUTS`); the layer picture never enters it. Everything around that node is the user's: the graph editor opens a scene layer's graph so post-processing is wired after the Scene node, and a recompile never touches it (Blender's compositor Render Layers node). The Scene panel never edits a graph directly. Effect and generator graphs outside scene layers are written only by the graph editor. No hand-wired scene graphs: every scene capability is an item kind, a field, a modifier or a World setting. Node trees may later attach to items as assets (section 14), never replacing the scene.
19. **Nothing main had is dropped without Peter's yes.** Every scene feature a performer could use on main has a home and an owning lane in section 15. One that fits no kind is listed in section 15.3 for Peter.
20. **Modifiers are a kind, and forces are not modifiers.** A Modifier item is one card in the scene's single ordered chain, shown in its own section of the tree, like a layer's effect cards: drag to reorder, an on/off toggle per card, the type swappable. Order in the section is the order of application. Each card names its targets: one or more Objects or Groups, or the whole scene. No Object or Group holds a modifier list. Forces stay their own kind in their own section of the tree; they add up, so they are not a chain. Every mesh modifier works on any Object shape (built-in solids, planes, imported parts, Ocean) and on Groups, in any scene. Each item supplies its own rest-pose bounds, centre and vertex count, and every recipe reads them the same way. A built-in shape gets enough mesh detail automatically while a deforming modifier targets it. A Group target bends all its parts in one shared frame, as main did for scans. No modifier refuses a shape. Every modifier row is a performance row. Every modifier and force main shipped comes back as a Modifier, a Force or a World setting (section 15.2).
21. **Scene-wide modifiers.** Scene Loop, Math View and Scene Mirror are Modifier types that act on the whole scene. Scene Loop is the corridor loop, and every camera sees it; a camera's Loop rig rides it, and a camera cut keeps the tiling. A scene holds at most one Scene Loop and one Math View. Neither is on the World card. Scene Fog is removed: fog is World Fog, its Density a performance row.
22. **No hit parts.** Modifiers have no Hit variants, no Clip Trigger toggle and no hit-envelope rows. Shatter's break is a live param, mappable like Reset.
23. **No arbitrary limits.** No fixed count caps bodies, walls, copy groups, group parts, forces or modifiers. Buffers and ports grow as items are added, and growing never restarts a simulation. A limit stays only where the physics or the machine's memory requires it, with that reason in one line next to it and on the disabled control.

## 2. Verdict

A scene layer stores a tree of typed items. The render graph is compiled from it, so the graph becomes the backend it already is at runtime. Every card is defined once per item kind, so a saved scene is just its document. Each object gets one physics setting (None, Collider, Body) whose collision shape comes from the mesh automatically, and water, rigid bodies and swarms all read it. A click on the output picks the pixel's item. Every scene feature main had comes back as an item, a field, a modifier or a World setting.

On stage: Peter opens Scene from the browser, gets a camera, sun, sky and a cube, adds objects, groups and lights, picks an HDRI from the Lighting card, and sets physics with one control. Every card is the same card for the same kind, whoever built the item. Anything he maps or modulates (transforms, colour, forces, speed, camera cut) stays live and never rebuilds the scene. Anything structural (physics setting, Draw As, Emit From) changes on a click and may restart that one item's simulation, the way a Reset does.

## 3. Old-path survivors on the branch

What the deletion proof (section 10) still finds at df95dd7ce, and who removes it.

| Survivor | Where | Owner |
|---|---|---|
| `SceneNodeRef` (85 files) | Scene modifier addressing: `manifold-node-engine/src/load/expand/`, `manifold-core/src/scene_index.rs`, `scene_modifier_*.rs`, `manifold-nodes-scene/.../scene_compile/forces.rs`, their tests | L12 |
| `scene_modifier_edit` | `manifold-core/src/scene_modifier_edit.rs`, `manifold-app/src/scene_modifier_edit.rs` and its tests; 48 files name it | L12 |
| The old layer-card modifier picker. It writes into the compiled graph, a second writer (ruling 18) | `manifold-app/src/ui_bridge/projection/inspector.rs` | L12 |
| The generator-swap refusal path | `manifold-app/src/scene_modifier_transfer.rs` | L12 |
| Scene modifier load migrations | `manifold-nodes-scene/src/node_graph/scene_modifier_legacy_migration/` | L12 |
| `stamp_scene_node_exposures` | Callers: `manifold-io/src/migrations/scene_cinematic_tail_v1130.rs` and the catalog test `runtime_bound_param_survives_rebuild.rs` (L12); `manifold-nodes-water/src/presets/gpu_flip.rs` (L7). The definition in `manifold-core/src/scene_exposure.rs` goes last | L12, L7, then L10 |
| Fluid role wiring and the off-menu Collider role kept as a stopgap in `role_kind` | `fluid_role` in 34 files | L7 |
| Two comments naming `SceneVm::from_def` | `gltf_mesh_source.rs:222`, `gltf_skinned_mesh_source.rs:107` | L12 |
| The fifteen old `render_scene` starters (Lantern included; Scene.json is the base Scene and stays) | `crates/manifold-nodes/assets/generator-presets/` | L10 |

## 4. Refusals, hiding and toasts

One rule for every card, row and choice:
- **Hidden:** a row or choice that can never apply to the item as it is set up now (its kind, shape, rig, light type, source). Rows that do not apply never show.
- **Disabled, with its reason on hover:** a row or choice that applies but is blocked by something else in the scene (a Body inside a Body, a capacity reached, a target that cannot resolve). Through `ParamRow.spec.inactive_reason` (`manifold-ui/src/param_surface.rs`).
- **Toast:** an action that was tried and failed, or input the physics gate refuses while running. One toast names the item and the real reason, shows its whole text and stays up long enough to read. The item's card shows the same reason on the control it concerns.
- **Never:** standing refusal text on a card, a refusal only in a log line, or a wrong reason (a uniform-scale rule reported as "a body is out of the world").
- **Not offered:** a choice whose lane has not built it yet (Ocean until L16).
- **Limits:** a row never takes a value that breaks the view or the solver. Camera Distance stays above 0; Near Clip stays above 0 and below Far Clip; Field of View stays in its range; a water domain stays inside the solver's size limit (section 7.18).

## 5. What carries over unchanged

The node engine and every primitive; the renderer (including G1's instance space and G2a's transform parent chain), apart from the item-id buffer; Box3D; the GPU FLIP and MPM solvers; the force expansion passes (`load/expand`), apart from addressing compiled nodes by node id (ruling 8); the param manifest, bindings, modulation, OSC and MIDI runtime; the `ParamSurface` card rows; the gizmo core and shape declarations (their param ids become property ids); the Scene tab widgets; the frame planner. The compile target is today's `EffectGraphDef` plus its manifest, so nothing downstream of the compiler changes.

## 6. Industry practice, and the USD mapping

| Tool | Scene truth | Graphs |
|---|---|---|
| Blender | Objects in collections and parent trees; the startup file holds a cube, a light and a camera | Node trees attach to objects, materials and the world |
| Cinema 4D | Object manager tree; tags carry physics and materials | Scene Nodes and node materials attach to objects |
| Unity | GameObjects with components; the default scene has a camera and a directional light | Shader Graph and VFX Graph are assets that components reference |
| Houdini Solaris | USD stage | SOP networks feed prims |
| USD | Prims in a hierarchy with typed attributes and relationships | UsdShade node graphs attach to materials |

We follow USD for the model, Blender for the startup scene and for any behaviour this doc leaves unsaid (ruling 17), Unity and USD for physics.

| Our item | USD concept | Notes |
|---|---|---|
| Scene document | Stage, one root layer | One per scene layer |
| Group | `UsdGeomXform` with children | Nesting is free |
| Object, built-in Shape | `UsdGeomCube`, `Sphere`, `Cylinder`, `Cone`, `Capsule`, `Plane` | Platonic solids have no USD gprim: a `Mesh` with a generator (difference 1 below) |
| Object, imported part | `UsdGeomMesh` prim under an `Xform` that carries a reference to the file | A multi-mesh import is a Group whose Source is the file; children are keyed by file node path |
| Object, Shape Ocean | A generated surface | Ours; no USD equivalent |
| Material | `UsdShadeMaterial` with a `UsdPreviewSurface` shader; `material:binding` on items | Binding inherits down the tree, nearest wins. Texture source None, file, model or layer (a layer is ours) |
| Camera | `UsdGeomCamera` (focal length, aperture, focus distance, f-stop, clipping range) | The orbit rig is ours (difference 7) |
| Light: Sun, Point, Spot | `UsdLuxDistantLight`, `SphereLight`, `SphereLight` plus `ShapingAPI` | |
| Light: Environment | `UsdLuxDomeLight` with `inputs:texture:file` | The HDRI file picker is the dome's texture; procedural sky is ours (difference 8) |
| World, physics part | `UsdPhysicsScene` (gravity) | Speed and Reset are ours (difference 4) |
| Physics setting Collider | `UsdPhysicsCollisionAPI` plus `MeshCollisionAPI` approximation | |
| Physics setting Body | `UsdPhysicsRigidBodyAPI` plus `CollisionAPI`, `MassAPI` | Body on a group with colliders below it is one compound body |
| Copies section | `UsdGeomPointInstancer`, prototype = the object or group | Physics per copy: rigid body on the prototype, as UsdPhysics allows |
| Swarm | `UsdGeomPointInstancer` whose positions come from a simulation | Draw As = prototype; Emit From is ours |
| Force | None | Ours: a typed item with a target list, the `UsdCollectionAPI` shape (difference 5) |
| Modifier | None | Ours: one card in the scene's ordered chain; its targets are a `UsdCollectionAPI`-shaped list (difference 9) |
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
9. **Modifiers.** USD has no modifier stack. Ours is the Modifier kind: one ordered chain of cards per scene, each naming its targets (one or more Objects or Groups, or the whole scene), compiled as today (ruling 20, section 7.19). Forces are a separate kind and not a chain.
10. **Swarm emission.** A PointInstancer's positions are authored arrays; ours come from a GPU simulation each frame.
11. **Per-clip overrides.** A clip may override a scene layer's params, including file paths (today's clip `string_params`). Closest to a USD session layer; kept as clip data.

## 7. The design

### 7.1 The scene document

- `SceneDoc` lives on a scene layer's generator instance (`PresetInstance`, `manifold-core/src/effects/instance.rs`), beside `params`. For a scene layer the graph is never saved; it is compiled.
- `SceneDoc` is a tree of `Item { id, name, kind, fields, children }`. `kind` is the closed enum of eleven (ruling 4). `fields` are the structural properties of that kind, a closed set per kind, listed in section 7.4.
- References between items (a material binding, Draw As, Emit From, force and modifier targets, a Look At target, a Material's source layer) are fields, set by `Set`. Material binding inherits down the tree, nearest wins.
- The file format is closed: `deny_unknown_fields` everywhere. No preset-declared param, section, label, `cardVisible` or string binding exists in a scene layer. `presetMetadata.params`, `bindings`, `stringParams`, `stringBindings` are not part of a scene layer.
- Every live property is a param in the layer's manifest, id `<item id>.<property>`, created by the compiler from the kind's table. Structural fields are never params (section 7.4).
- Ownership: content thread. Mutation: `EditingService` commands only, each one undo step. Serialization: the document plus the manifest, as JSON inside the project. The UI reads the document from its `Arc<Project>` snapshot through one seam, `scene_of(project, layer) -> Arc<SceneVm>`; every host (project bridge, projection, gizmo, edit selection, setup check) uses it. There is no second constructor.
- The compiler is the only writer of a scene layer's graph (ruling 18).
- The layer card shows only what the user pinned (SCENE_SURFACES_DESIGN D4). A saved scene keeps its pins.

### 7.2 Identity and the five commands

- `ItemId` is a plain newtype (a stable unique id, never a path). It is the scene's identity: selections, gizmo owners, commands and force targets carry it. `SceneNodeRef` (a graph scope path) is deleted (section 3).
- Commands, all through `EditingService` via `ContentCommand::Execute` or `MutateProject`:
  - **Add** `(kind, parent, index, optional source)`. With a source it duplicates that item and its subtree with fresh ids. Adding an Object also adds its own Material and binds it, in the same undo step (default, pending Peter). Adding a Swarm also adds its default Attractor, core and gust Force items aimed at it, in the same undo step. Adding a Modifier aims it at the selected Objects and Groups, or at the whole scene when nothing is selected, and puts it at the end of the chain.
  - **Remove** `(id)`. Removes the subtree. Anything referencing a removed item (Draw As, Emit From, force targets, material bindings, a Look At target, a camera pick) falls back to its default in the same step. A Modifier just loses that target.
  - **Reparent** `(id, new parent, index)`. Also reorders, the modifier chain included. The item keeps its place on screen. Force and Modifier items stay at the top of the scene and never go inside a Group.
  - **Rename** `(id, name)`. A default name follows the shape until the user renames the item (default, pending Peter).
  - **Set** `(id, field or param, value)`. A structural field set is a structural edit (section 7.4). A param set is a param write.
- **Group** (Cmd+G) is Add Group plus Reparent of the selection, and **Ungroup** (Cmd+Shift+G) is Reparent of the children plus Remove, each in one `ExecuteBatch`, one undo step. Both keep every item where it is on screen.
- **Copy and paste** (Cmd+C, Cmd+V) capture an item and its subtree and paste it with Add with a source, in the same scene or another scene layer. Cmd+D is Duplicate.
- Param writes still go through today's param commands, addressed by `<item id>.<property>`. Renaming or reparenting never changes a param id.

### 7.3 The compiler

- `scene_compile::compile(&SceneDoc) -> CompiledScene { graph: EffectGraphDef, manifest }` in `manifold-nodes-scene`, shaped like `physics_members::compile` (`manifold-node-engine/src/load/physics_members.rs`): a pure function over a definition.
- One emitter per kind, one module per kind under `scene_compile/`. The bodies of the old builders and recipes move into them; nothing is invented.
- Node ids derive from (item id, role). A recompile after a structural edit keeps every unchanged node's id, so the runtime keeps what it keeps for node-id-matched state.
- It compiles the whole graph per structural edit, on the content thread, and at load. A param write never recompiles.
- Force and Modifier items compile to `scene_modifiers` instances and the `load/expand` passes, addressed by node id (ruling 8). Modifiers compile in chain order. Bodies ride `physics_members`.
- Every mesh source the compiler emits (built-in solids, Sphere, Plane, imported parts, Ocean, procedural shapes, a Group as the union of its parts, the whole scene as the union of everything) supplies its rest-pose bounds, centre and vertex count, and every modifier recipe reads them the same way (ruling 20). A recipe never takes its scale from the largest import in the scene.
- A scene layer's graph opens in the graph editor with the scene as one sealed Scene node; the user wires post-processing around it (ruling 18). [L12b]

### 7.4 Stage versus setup

| Structural (item fields, set by commands, never mapped) | Live (manifest params, mappable) |
|---|---|
| Kind; Object Shape; physics setting; Copies on or off; Modifier type, on or off, targets and place in the chain; Shatter's Pieces; Group Source, Combine Meshes and Clip; Camera Rig and the Look At target; World default camera and Fog on or off; Light type; Environment source and Sky style; Material binding, texture source, Type and features; Water Source role and shape; Swarm Draw As and Emit From; Force type and targets | Transforms; colour; intensity; every surface row; forces and impulses; Speed; Reset; Fog Density; visibility; each output's camera pick; Copies Count, Layout, Spacing, Columns and Turn; every other row of a Modifier card, its mask and the Scene Loop, Math View and Scene Mirror rows included; Shatter's break; camera Shake; animation Rate |

- A param write never recompiles or resets anything.
- A structural edit recompiles and may restart the edited item's simulation, like a Blender re-bake. The card marks the control as a setup control; it is not a mappable row.
- Carry: CPU rigid state is carried by node id across a recompile (`physics_carry.rs`). GPU FLIP and swarm state reseeds on a structural rebuild of their own item.
- Speed clamps to its range and never errors.
- A body whose collision shape is still being built joins when ready. The world keeps ticking meanwhile, and no body is ever held silently.

### 7.5 The base scene

What every new scene holds, as Blender's startup file and Unity's default scene do:
- **Camera:** orbit rig at a three-quarter view, 50 mm equivalent.
- **Sun** (Light, Sun).
- **Environment** (Light, Environment): source Sky (Softbox) by default, HDRI File one click away with the file picker always on the card in HDRI mode. Exposure 0 EV. One Environment per scene.
- **World:** Gravity -9.81, Speed 1, idle until an item has physics; Render settings; Fog off.
- **One cube** at the origin (Object, Shape Cube, physics None) with its own Material, so its colour is editable at once.

"Scene" is the first entry in the browser and opens exactly this. No other starter ships.

### 7.6 Materials and layer planes

- A Material is an item bound by reference, inherited down the tree, nearest wins. A group's material reaches every child with none of its own.
- Every new Object gets its own Material (section 7.2). Duplicate shares the original's Material ("used by 2"); Make Unique gives the item its own copy (Add with a source, then Set the binding, one `ExecuteBatch`).
- The Material card appears inline on the object that uses it: its look (Default, Matte, Coated, Brushed Metal, Glass, each applied in one undo step), colour and surface rows, "used by N" and Make Unique.
- The texture source is None, a file, a model file's material (imports, section 7.7), or a layer. A layer feeds Base Colour or Emission.
- Material Type is PBR, Unlit or Cel (restored by L17; section 15).
- A layer plane (a plane showing another layer's output) is not a kind. It is an ordinary Object with Shape Plane whose Material is Unlit and takes its image from a layer, with alpha cutout. Add → Layer Plane builds exactly that. Physics, Copies, picking and the card work as for any plane.
- Material features (Glass, Coat and the rest of "+ Add feature") are structural fields on the Material, set by one `Set` with their default values in the same `ExecuteBatch`. Adding a feature is one undo step, and undo removes it completely.
- Rule for every card: what the Scene panel shows comes only from the document and its params. No scene state lives in panel-only memory (open sections and scroll position are view state, not scene state).

### 7.7 Imports

- Add → Mesh from file opens the file picker at once and runs the same importer as a drop (ruling 5). A file with several meshes becomes a Group whose Source is the file; a file with one mesh becomes one Object. Children follow the file's node tree: one Object per mesh-bearing node, empty pass-through nodes collapsed, nested as groups where the file nests.
- An import lands at its true size and where the file puts it, in metres, as in Blender and Unity: glTF is metres; FBX and COLLADA convert their stated units to metres; OBJ, which has no units, is read as metres. Nothing rescales or recentres it.
- Each part and group keeps its file node's transform, as in Blender's glTF import: its card starts at the node's position, rotation and scale in its parent's frame, and its mesh stays in the node's own space, so the node's origin is its pivot (a wheel turns about its axle). A mesh-less node folds into its one child when the two make one exact transform; otherwise it stays a group. A skinned or animated part draws where its skin or animation puts it, seen from its node at rest, so its card moves it from there. Combine Meshes draws every part where the file puts it.
- An import looks as it does in the file: each part's Material reads the file's factors, maps and extensions (texture source Model); skins, morphs and animation play (the Group's Clip field; Rate, Loop Mode and Retrigger rows); the file's lights and cameras become Light and Camera items under the Group.
- A model file is read once, on the import worker: the parts and the facts the compiler reads (materials, rig, morphs, size) come from one parse. The facts travel on the document and are never saved; no compile on the UI or content thread opens a file. A load, paste, undo or relink that names a file without its facts reads it on the worker, and until they arrive its parts draw as plain meshes. What a file's materials hold that the surface cannot draw goes to the import warnings.
- Child ids derive from the file node path, so a Model File swap keeps every child whose path still exists. The rest are removed, with a toast naming the count.
- A Combine Meshes switch on the group merges the children into one mesh, keeping each part's material and animation. Off by default.
- Changing an imported Object's Shape to a built-in drops its file, in one undo step.
- Edits to a child (transform, material, physics) are ordinary item edits and survive a swap with their child, as do the Forces and Modifiers aimed at it. This fixes BUG-yuc2n (Paste across layers drops the source clip's Model File override) by construction, and copying an imported model is Add with a source, which makes a working clone: BUG-tt9ce (Copying an imported model never made a working clone) is fixed by design.

### 7.8 Physics and collision shapes

- **One setting on Object and Group: None, Collider, Body.** Collider is a static solid; Body is a dynamic rigid body. On a group, Body is one compound body of everything inside (where the body sits decides). A Body inside a Body is disabled with its reason.
- **The card shows** Physics, then (Body only) Mass, then Bounce and Friction (Collider and Body). No Advanced section. No Kinematic setting.
- **The shape comes from the mesh automatically**, from one `CollisionShape` service in `manifold-physics` that takes an item's mesh and world scale and returns what each solver asks for:
  - Static collider in Box3D: the exact triangle mesh (`manifold-physics/native/box3d/bridge.c`, `b3CreateMesh`).
  - Body in Box3D: a spatial split into hulls with automatic detail chosen from size and triangle count, with a 0.1% shell for open scans and the 64-hull cap (`MAX_BODY_HULLS`, `manifold-physics/src/lib.rs`).
  - Water (GPU FLIP, MPM): a signed distance lattice for a closed mesh, unsigned with a shell for an open one (`manifold-physics/src/sdf.rs`). Shell thickness is automatic: one water cell on each side of the surface (two cells through), or 0.1% of the mesh, whichever is larger.
  - Swarms: none in this cut (section 14).
- **Cache key** includes the mesh content, the item's scale and the water cell size. A late-arriving mesh (an async load) must flip the key: the key hashes a content-ready signal, not size and transform alone.
- **Water reads it directly.** Every Collider and every Body in the water's World whose bounds touch the water domain is a solid in the water; Bodies couple two ways as rigid bodies do. No fluid role on an object, no per-solver shape path.
- The solvers stay separate underneath.
- **One gate into Box3D.** The `CollisionShape` service and the physics install are the only way anything reaches Box3D. They validate every transform, scale and hull as finite and non-degenerate. Bad input is refused and shown as section 4 says, and nothing non-finite ever reaches the solver. A Box3D assert must never abort the app in the show build. This covers the broad-phase assertion `stackCount < B3_TREE_STACK_SIZE - 1` in `b3DynamicTree_Query` (`crates/manifold-physics/native/box3d/src/dynamic_tree.c`), a tree more than 1024 levels deep, which NaN or infinite bounds cause: BUG-k787p (Box3D tree query assertion), fixed in L4.
- A body whose scale breaks a copies rule is refused with that rule's own reason, never "a body is out of the world".

### 7.9 Copies

- A section on Object and Group: Count, Layout, Spacing, Columns, Turn. Count 1 means one copy. Copies on or off is the structural field; the numbers and Layout are live params.
- Layout offers every arrangement main offered (Grid, Ring, Scatter, Line, the `generate_instance_transforms` arrangements) plus On Surface of an item (restored by L19).
- Copies of a group draw through G1's instance space, spaced by the group's bounds; copies take their transform parent chain from G2a. Physics per copy: the prototype's setting applies to every copy.
- There is no Make Copies action and no Copies kind.

### 7.10 Water, Water Source, Ocean

- **Water** is a kind: Domain (its Transform is the box: Position and Size on the card, moved and resized by its gizmo), Resolution, Simulation dials, Surface, Whitewater (section 7.18); its sources as children. The water family template in `manifold-nodes-water` is the emitter.
- **Water Source** is a child item with Role Fill, Inflow or Drain; Shape Box or Sphere; Transform; Inflow speed. The role is shown in the header. There is no Collider role (section 7.8).
- **Ocean** is an Object Shape, built by L16. Its rows (the surface dials) appear on the Object card when Shape is Ocean. It is not offered until it draws.
- BUG-fbwu6 (Initial Fill refusal is not named by the domain setup warning) is fixed by design: Fill on a source that cannot fill is disabled with its reason.

### 7.11 Swarm

- Swarm is its own kind (not an Object). Emit From: Box volume (default), Sphere, or the surface of any Object or Group. Draw As: Tetrahedron, Cube, Octahedron, Icosahedron, Sphere, or any Object or Group, drawn per particle through G1's Group instance space. Both are structural fields, as is its Material binding.
- Count, Size, Air Drag and Look are live. Its Transform places the emitter; the force lattice bounds follow the emitter with a margin. Force detail is automatic.
- No hidden spawn cube or shape node.

### 7.12 Forces

- Force is a kind: its recipe type (Radial, Uniform, Vortex, Wind, Drag, Attractor, Explosion), the recipe's rows, Targets (everything with physics, or chosen items), Impulse. Forces compile to scene modifier instances and the `load/expand` passes. Drag compiles as the Wind recipe with speed and gusts at 0.
- A force is an item a user adds, with `Add`. A scene holds any number of forces (ruling 23).
- Forces are not modifiers and not a chain: they add up, so their order changes nothing. Force items sit at the top of the scene, in their own section of the tree.

### 7.13 Cameras

- Camera is a repeatable kind. Rig: Orbit, Free, Look At, or Loop (structural). Loop rides the scene's Scene Loop modifier (ruling 21, section 7.19) and arrives with L21.
  - Orbit: Orbit, Tilt, Distance (minimum above 0), the height it looks at, Field of View, Roll, Clipping.
  - Free: Yaw, Pitch, Position, Field of View, Roll, Clipping.
  - Look At: Position, Target (an Object or Group pick, a structural field; the camera follows the item as it moves; Target X, Y and Z offset from it) (default, pending Peter), Field of View, Clipping. Focus Distance starts at 10 m, never below 0.1 m.
  - Every rig has "Use this view".
  - Depth of Field: On, Focus, f-stop, Max Blur, Aperture Shape (default, pending Peter), Quality.
  - Motion Blur: On, Shutter, Max Blur. It shows whenever the camera or an object moves.
  - Shake: Amount, Speed, Position strength, Rotation strength, every one a performance row. It compiles to `node.transform_shake` on the camera's own transform, after the rig. Amount 0 leaves the camera still.
- A camera inside a group follows the group's transform.
- The World card's Camera row picks the camera the layer renders; it is live and mappable and never recompiles, so a mapped cut lands on its frame (ruling 11). Each camera keeps its own depth of field and motion blur; only the picked camera's run. [L8b]

### 7.14 Pixel clicking and selection

- The renderer's selection mask pass becomes an item-id buffer. The compiler publishes a table from item id to render slot. The content thread renders the buffer and resolves pixel to slot to `ItemId`. A click picks exactly what is drawn, including parts inside groups. Every pass that draws an item writes its id, the water surface included.
- Gizmo handles remain only for things with no pixels: lights, forces, water domains, sources. Their shape declarations stay.
- One selection store, the Scene panel's, shared by tree, viewport and preview. It works with gizmos hidden. No press rebuilds a scene view on the UI thread.

### 7.15 Saved scenes

- A saved scene is a file saved from the app (document plus manifest values), in the closed format.
- No starters ship: only the base Scene. The fifteen old `render_scene` starters leave the browser: six are deleted (Lantern, Swarm, Swarm Cloud and others), and the nine water, physics and ocean scenes move to test fixtures because the engine proofs load them.
- Changing a layer's generator always works, as in Resolume and Ableton: the old scene goes with the old generator, one undo step; one undo brings both back. BUG-vis7l (Scene preset hidden) and BUG-hkshf (generator change refused) are fixed by design.

### 7.16 Not changed

Section 5 lists what carries over.

### 7.17 Scale link toggle

- Every Transform section has a link toggle beside Scale X, Y and Z, on by default (Unity's constrain-proportions link; Cinema 4D and Figma proportion locks).
- Linked: dragging or typing any one axis scales all three in proportion, live during the drag, and the values stay on release; one undo step. Unlinked: each axis is free.
- It is a card editing helper over the existing three params (default, pending Peter). It adds no param, so modulation, MIDI and OSC still target each axis. The link state is a card setting and is not mapped.
- Built through the param surface recipe in WIDGET_TREE_DESIGN.md section 5b (Agent contract and enforcement), with no bespoke row infrastructure.

### 7.18 Fit with the water end game

The water end game ([GPU_FLIP_END_GAME_DESIGN.md](GPU_FLIP_END_GAME_DESIGN.md)) changes things below the loader: sparse tiles, no mesh in the live path, a memory budget. This design only fixes how water is set up and shown. These rules keep the two from colliding:
- **Domain size limit** is never a literal in the table. The row's hard limit comes from the solver's own constant (`manifold-core/src/fluid_domain.rs`, 0.5 to 20 m). When the end game removes the cap for GPU FLIP, only the solver changes. A value past the limit is stopped by the row and never reaches the solver.
- **Resolution** is cells across the longest side; the card shows the cell size in metres, read-only. When the end game stores cell size instead, only the Water table row changes.
- **Whitewater capacity** is one function in the Water emitter, of Resolution. The end game changes its input, not the card. A full pool replaces its oldest particles and never refuses.
- **Colliders in water:** water reads items whose bounds touch its domain (its box, its active tiles later). `CollisionShape` hands water a distance sampler cropped to the domain plus reach, never a lattice over a whole mesh. Past 255 coupled items (`MAX_LIQUID_BODIES`, `fluid_domain.rs`) the card says how many are not coupled.
- **Physics View** is a Render menu mode that draws the particle dots instead of the water surface, plus the hidden things: open and closed faces, colliders as the water sees them, sources and drains. It is not a card row.
- **Surface:** the Water emitter owns the whole surface path behind the Surface rows. When the end game draws the level set directly, it swaps the emitter's output in one lane and keeps the row ids.
- **Water Source** Role and Shape are open lists in the Water Source table, so mesh-shaped fills, an absorb zone and ocean-fed faces are added there later.
- **Setup check:** `scene_vm/setup_check.rs` and `manifold-app/src/setup_check.rs` (the problem list, red outline and card warning) run on `scene_of`. A setup warning (what the solver clipped) is a notice with a red outline on the item, not a refusal.

### 7.19 Modifiers

- **Modifier** is a kind. The scene's Modifier items are one ordered chain of cards in their own section of the tree, like a layer's effect cards: drag to reorder, an on/off toggle per card, the type swappable. Order in the section is the order of application. Modifier items sit at the top of the scene, never inside a Group.
- **Targets** is a structural field: one or more Objects or Groups, or the whole scene. Add aims a new card at the selection, or at the whole scene when nothing is selected. Each Object target bends in its own frame. A Group target bends all its parts in one shared frame, as main did for scans. The whole scene bends as one Group of everything. Removing an item drops it from every card's targets in the same undo step; a card left with no target acts on nothing.
- **Mesh modifiers:** Surface Peel, Ordered Recon, Surface Waves, Spatial Echoes, Elastic Sculpture, Vortex Fragments, Shatter (L15). L18 adds the deformers and Shake, and L19 the copy fields, as more types aimed the same way. Each works on any shape and on Groups (ruling 20). Shatter acts on Body targets: a target whose Physics is not Body is disabled with its reason. Its Pieces is setup; its break is a live param.
- **Scene-wide modifiers:** Scene Loop, Math View, Scene Mirror (L21). They always act on the whole scene, so their card has no Targets row. A scene holds at most one Scene Loop and one Math View; a second Add is disabled with that reason.
  - Scene Loop tiles the scene into a corridor that every camera sees. A camera on the Loop rig rides it, and a cut to another camera keeps the tiling. With no Scene Loop in the scene, the Loop rig is disabled with its reason.
  - Math View draws the scene's modifier maths over or instead of the picture.
  - Scene Mirror reflects the scene (`reflect_array`).
- **Stage and setup:** Type, on or off, Targets, Shatter's Pieces and the place in the chain are structural (section 7.4). Every other row of a card is a performance row, its mask included. A type swap keeps the card's place, toggle and targets; its rows start at the new type's defaults.
- Param ids are `<modifier item id>.<property>`, so reordering the chain never changes one.
- No Hit variants, no Clip Trigger toggle and no hit-envelope rows (ruling 22).
- Forces are not modifiers (section 7.12). No Object or Group holds a modifier list, and no Object or Group card has a Modifiers section.

## 8. Cards by kind

One table keyed by (kind, property), one file per kind under `manifold-core/src/scene_schema/`. It carries group, role, band, order, gate, unit and limit. Rows go through the param surface recipe: manifest params with ids, plus the structural field rows (`FieldRow`: field, section, choices, applies, disabled reason) rendered generically. No bespoke row, slider or drawer. Every row's section is declared in the table; there is no fallback section. Every engine param the compiler emits is a card row, automatic or internal in the table. Bake size and force detail are automatic and have no rows. Hidden, disabled and toast follow section 4.

| Kind | Card |
|---|---|
| Camera | Rig (Orbit, Free, Look At; Loop, which rides the scene's Scene Loop, from L21); the rig's rows (section 7.13); Use this view; Depth of Field (with Aperture Shape); Motion Blur; Shake (Amount, Speed, Position strength, Rotation strength; L8) |
| Lighting (one card) | Environment source (Sky, HDRI File, None, and Sea Horizon from L16); in HDRI mode: File picker (always present), Rotation (L13), Intensity, Background (Blur, Brightness), Aim Sun at HDRI; in Sky mode: Gradient or Softbox rows; Exposure; then every light |
| Light | Type, Colour, Intensity, Position, Target, Range, Size, Shadows (rows by type) |
| Object | Shape (built-in solids; Ocean from L16; Extrude, Revolve and Tube from L20); Model File (a single imported Object only); Transform (with the Scale link toggle); Material (inline: look, colour and surface rows, used by N, Make Unique); Look (Visible, Cast Shadows, Emission); Physics; Copies |
| Group | Transform (with the Scale link toggle); Visible; Material (applies to children with none); Physics (Body here is one compound body); Copies; when Source is a file: Model File, Combine Meshes, Clip, Rate, Loop Mode, Retrigger |
| Material | Look; Type (L17); surface rows; features; texture source (None, file, model, layer; a layer feeds Base Colour or Emission); used by N; Make Unique |
| Swarm | Emit From; Draw As; Count; Size; Air Drag; Material; Look; Transform places the emitter |
| Water | Domain: Position and Size (with the Scale link toggle; the size limit comes from the solver, section 7.18), moved and resized by its gizmo; Resolution (with the cell size in metres, read-only); Simulation: Speed first, then only dials a performer reads (solver internals such as Coarse Solve, Max Iterations and Sheet Fill Rate are automatic in the table); Surface: Surface Detail, Particle Scale, Stretch, Centre Smoothing, Fill Pits, Smoothing; Whitewater: Amount only |
| Water Source | Role (header); Shape; Transform; Inflow speed |
| Force | Its type's rows; Targets; Impulse |
| Modifier | Header: Type (swappable) and the on/off toggle; Targets (one or more Objects or Groups, or the whole scene; not on a scene-wide type); the type's rows, with a Mask section (Centre, Width, Feather in radius units) on each masked type (section 7.19) |
| World | Physics (Gravity, Speed, Reset, Air Resistance); Fog (Density a performance row); Render (render mode and its colours (L13), ray tracing, temporal upscale, Ambient Occlusion (L13)); Camera (the live pick, L8b) |

The tree: Camera and Lighting, World pinned at the top; then the item hierarchy, every item in its place with a kind icon, nested groups shown as a real tree; then the Forces section; then the Modifiers section, its cards in the order they apply.

### 8.1 Expected behaviour

Short statements a performer can check, one by one. Each names the one lane that owns it; "L11" means built or not yet owned by a building lane, and L11 checks it in the app and fixes what fails. A lane checks every line it owns before its commit.

**Scene panel**
- The tree mirrors the item hierarchy, nested groups included, with a kind icon per item. [L11]
- Forces and Modifiers each have their own section of the tree, and neither ever lands inside a Group. [L15]
- Dragging an item in the tree reparents or reorders it and keeps it where it is on screen. [L11]
- Double-clicking a tree row renames the item. [L11]
- Add puts the new item at the origin, selected, in view of the current camera. [L11]
- A choice that does not draw yet is not in the Add menu or any list. [L11]
- Delete or Backspace removes the selected items, one undo step. [L11]
- Cmd+D duplicates; Cmd+C and Cmd+V copy and paste items, also into another scene layer. [L9]
- Cmd+G groups the selection and Cmd+Shift+G ungroups; nothing moves on screen. [L11]
- Selecting in the tree selects in the viewport and the preview, and the other way round. [L9]
- Clicking the output picks the drawn item, a part inside a group included, with gizmos shown or hidden. [L9]
- The selection survives a structural edit. [L9]
- The gizmo moves, rotates and scales the selected item in its parent's space, inside groups too. Its axes are thick enough to see and grab. [F2]
- Every edit (row drag, gizmo drag, field change, Add, Remove, Reparent, Group) is one undo step; Cmd+Z restores exactly and Cmd+Shift+Z redoes. [L11]
- A row that does not apply is hidden, not disabled. A blocked control is disabled with its reason on hover. [L11]
- A toast shows its whole text, stays up long enough to read, and names the real reason. [F2]
- No row edits a value to one that breaks the view or the solver (section 4). [F2 for Distance; L8 for Clipping and Field of View; L7 for the domain]
- A knob, MIDI or OSC change reaches the item on the next frame and never rebuilds the scene. [L8]

**Object**
- A new Object has its own Material, and its colour is editable on its card. [F1]
- Its name follows its shape (Cube, Sphere, Icosahedron) until the user renames it. [F1]
- Shape offers Cube, Sphere, Plane, Tetrahedron, Octahedron, Icosahedron, Dodecahedron. [F1] Ocean [L16] and Extrude, Revolve and Tube [L20] join when they draw.
- An Extrude, Revolve or Tube shape changes live when its curve's rows move, with no recompile. [L20]
- Changing Shape keeps the item's transform, material, physics and copies, and every Force and Modifier aimed at it. [L11]
- Visible off hides the item from the output, from picking and from shadows. [L11]
- Body makes it fall on play at once, whatever was set before; nothing else needs toggling. [F2]
- Every modifier works on every shape. [L15]
- Copies work on every shape. [L11]

**Group**
- Moving, rotating or scaling a Group moves every child, lights and cameras included. [L11]
- Visible off hides everything inside. [L11]
- A Group's Material reaches every child with no Material of its own. [L11]
- Body on a Group makes one compound body of everything inside. [L11]
- A Modifier aimed at a Group bends all its parts in one shared frame, so a multi-part scan moves as one piece. [L15]
- Copies of a Group copy everything inside, spaced by the group's bounds. [L19]

**Imports**
- Add → Mesh from file opens the file picker at once. [F1]
- A file with several meshes arrives as a Group with one child per mesh; a file with one mesh as one Object. [F1]
- An import lands at its true size, in metres, where the file puts it. [F1]
- Each imported part's and group's card shows its file node's position, rotation and scale; turning a part turns it about the node's origin.
- An imported model looks as it does in the file: materials and textures [F1]; skinned geometry and animation [L14]; ambient occlusion [L13]; the importer's material handling of main [L14].
- The file's lights and cameras arrive as items under the Group. [L11]
- Model File shows on the import Group and on a single imported Object. A swap keeps the parts that still exist, with their edits, and toasts how many went. [F1]
- Changing an imported Object's Shape to a built-in drops the file in one undo step; undo brings it back. [F1]
- Combine Meshes keeps each part's material and animation. [L14]

**Material**
- The looks Default, Matte, Coated, Brushed Metal and Glass each apply in one undo step. [F1]
- Duplicating an Object shares its Material ("used by 2"); Make Unique gives it its own, one undo step. [F1]
- Adding or removing a feature is one undo step, and undo removes it completely. [L11]
- A texture can come from a file or a layer; a layer feeds Base Colour or Emission. [L17]
- Type offers PBR, Unlit and Cel. [L17]
- Add → Layer Plane makes a plane showing a chosen layer, unlit, with alpha cutout. [L17]

**Camera**
- Distance cannot reach 0; the view never goes black from a camera row. [F2]
- Near Clip stays above 0 and below Far Clip; Field of View stays in its range. [L8]
- Every rig has "Use this view". [F2]
- Look At targets an Object or Group, and the camera follows it as it moves. [F2]
- Aperture Shape sits under Depth of Field and shows on bright out-of-focus highlights. [F2]
- Motion blur shows while the camera orbits. [F2]
- The Camera card has a Shake section (Amount, Speed, Position strength, Rotation strength); every row is a performance row, so a shake mapped to a kick lands on the kick; Amount 0 leaves the camera still. [L8]
- A camera inside a group follows the group. [L11]
- The World's Camera row cuts between cameras; a mapped cut lands on its frame, with no rebuild. [L8b]
- Removing the camera on show falls back to the first camera, in the same undo step; adding or moving cameras keeps the shot. [L8b]
- Shake wobbles the camera about its own position, like a handheld camera; Amount 0 is still. [L8b]

**Light and Environment**
- A new light lights the scene at once. [L11]
- Changing a light's type keeps its position and colour. [L11]
- A light inside a group follows the group. [L11]
- In HDRI mode the file picker is always on the card, and a picked file shows at once. [L11]
- HDRI Rotation turns the environment and its lighting. [L13]
- Exposure reaches every camera. [L11]
- Sea Horizon is an environment source. [L16]

**World**
- Speed 0 freezes every simulation; Speed out of range clamps. [L11]
- Reset restarts every simulation, also on a mapped beat. [L11]
- A render mode change shows on the next frame. [L11]
- Ambient Occlusion is a World Render row, on by default. [L13]
- The render modes' clay colour, line colour and point size are rows. [L13]
- Fog Density is a performance row. [L13]

**Copies**
- Count 1 shows one; changing Count, Layout or Spacing is live and restarts nothing else. [L11]
- Layout offers every arrangement main had, plus On Surface of an item. [L19]

**Modifiers**
- The Modifiers section of the tree is one chain of cards in the order they apply; dragging a card reorders the chain; each card has an on/off toggle and a type that can be swapped. [L15]
- A card names its targets: one or more Objects or Groups, or the whole scene. Add aims it at the selection, or at the whole scene when nothing is selected. [L15]
- Adding, removing, reordering, toggling or swapping a card is one undo step. [L15]
- Any order of cards compiles and draws. [L15]
- No Object or Group card has a Modifiers section. [L15]
- Every modifier row is a performance row, mappable to MIDI, OSC and LFOs. [L15]
- A deforming modifier on a built-in shape bends smoothly; the shape gets the mesh detail it needs on its own. [L15]
- A modifier's scale follows its own target, not the largest model in the scene. [L15]
- Mask Centre, Width and Feather are in the target's radius units, and their labels say so. [L15]
- No modifier has a Clip Trigger toggle or a Hit variant. [L15]
- Shatter acts on Body targets; a target whose Physics is not Body is disabled with its reason; its break is a live, mappable param. [L15]
- Removing an item drops it from every card's targets in the same undo step; a card with no target left acts on nothing. [L15]
- Scene Loop tiles the scene into a corridor that every camera sees; a camera on the Loop rig rides it; a cut to another camera keeps the tiling. [L21]
- Math View draws the scene's modifier maths over or instead of the picture. [L21]
- A scene holds at most one Scene Loop and one Math View; a second Add is disabled with that reason. [L21]
- Scene Mirror reflects the whole scene. [L21]
- Every modifier main shipped is offered (section 15.2). [owner in section 15.2]

**Physics**
- A Body joins as soon as its shape is ready; no body is held silently. [F2]
- A refused body shows the real reason on its card and in one toast. [F2]

**Water and Water Source**
- Add → Water makes a domain box with a gizmo that moves and resizes it. [L7]
- Fill fills the domain on play; Inflow streams; Drain removes. [L7]
- A Collider in the domain is a wall; a Body in it floats and pushes the water. [L7]
- Domain size stops at the solver's limit. [L7]
- Physics View is in the Render menu. [L7]

**Swarm**
- Add → Swarm gives a moving swarm with its default forces, one undo step. [L6]
- Draw As takes any Object or Group; Emit From takes any surface. [L6]
- Count changes live, without a restart. [L6]
- Forces act on swarms. [L6]

**Force**
- A force acts on its targets at once. [L11]
- Forces add up: reordering them in their section changes nothing. [L11]
- Impulse fires exactly once per trigger. [L11]
- Removing a target falls back to the default in the same step. [L11]

## 9. Alternatives, priced

| Shape | Cost | What it forecloses |
|---|---|---|
| **A. Scene document compiled to today's graph (chosen)** | The lanes in section 11 | Hand-wired scene graphs (intended) |
| B. Keep the graph as truth; derive cards by kind from it and forbid custom graphs by validation | 4 lanes | Every Add, Duplicate, Copies and physics edit stays a graph rewrite; the graph trace stays; a starter can still differ from a user scene in wiring the validator did not foresee. Ruled out |
| C. Scene document executed directly by a scene renderer with no graph | Renderer, physics and modulation rewritten | Reuse of the whole backend; months |

Kill-pass on A:
- Strongest case against: the compile seam loses runtime state on recompile (a physics world restarting when a light is added). Promise 5 (section 10) holds it.
- Second: compile time on a large import (a 300-node kit) makes structural edits stutter. The compile is linear in items. No timing test: Peter sees a stutter in the app.
- Third: two scene paths. There are none by design (ruling 2). Until L6, L7 and L16 land, the branch has no Swarm, Water or Ocean.

## 10. Testing

The goal: nothing Peter opens crashes, freezes or loses his work. Anything he sees in two seconds in the app is checked in the app, not tested. No tests of shape (node counts, row lists, layouts), no digests, no goldens captured from the output under test, no tests of features still being built, no UI flows, no card render sets. A lane writes a test only when its checklist names one. Before writing it, it extends the test that already proves the behaviour.

**The scene crash test** (L10; it extends `manifold-nodes --test main scene_document`): from the base scene, for every kind and every choice of every field row, read from the table so a new kind or choice is covered without editing the test, it adds the item, compiles, installs, ticks 120 frames headless, saves, loads, and undoes back to the base. It fails on a panic, on a hang (nextest's terminate-after), on a loaded document that differs from the saved one, or on an undo that does not return the base exactly. A GPU variant renders one frame of each through `scripts/gpu_queue.py`, nightly.

**The promise list.** Each is one existing test, extended by the lane named when it changes the behaviour:
1. Every edit undoes exactly, material features and Modifier cards included. (`manifold-editing commands::scene_doc::tests`; F1 adds Add Object and Make Unique, L15 adds Modifier cards.)
2. Renaming or moving an item keeps its MIDI maps and modulation attached.
3. Save and load gives back the same scene.
4. Non-finite or degenerate input never reaches Box3D, and the world keeps stepping.
5. An unrelated structural edit (adding a light) does not restart physics, and a param write never recompiles.
6. Speed out of range clamps and never freezes the world.
7. Scaling an item refreshes its collision shape.
8. An impulse fires exactly once per trigger.
9. A knob, MIDI or OSC change reaches the item on the next frame, and a camera cut lands on its frame, with no rebuild. (L8)
10. A Model File swap keeps edits on the parts that still exist.
11. No engine param a scene uses is unreachable: each is a card row, automatic or internal in the table. (Every lane that adds a kind keeps it green.)
12. Every kind and every field choice runs without a crash: the scene crash test. (L10)
13. One GPU proof, through `scripts/gpu_queue.py`, that a click returns the drawn item, including a part inside a group. (L9)

**Deletion proof (L10):** `rg -n 'SceneVm::from_def|stamp_scene_node_exposures|scene_object_models|insert_scene_template|SCENE_VOCABULARY|SceneNodeRef|scene_modifier_edit' crates` returns zero.

## 11. Plan

Branch `wave/scene-document`. Land once, after L11.

### 11.1 Lane rules

- Each lane makes one commit and stops for review. It runs scoped nextest on the touched crates, regenerates the node catalog and docs index where it changed them, and one `--cheap` landing gate. At most four lanes build at once (disk).
- Files are disjoint. Shared list files take one-line entries from any lane: the enums in `manifold-core/src/scene_doc.rs`, `scene_schema/add_menu.rs`, the Shape choices in `scene_schema/object.rs`, and the registries in `scene_schema/mod.rs` and `scene_compile/mod.rs`. Anything bigger in another lane's file: stop and report.
- Where this doc is silent, the lane follows Blender (Unity for the default scene and components) and names each such call in its report (ruling 17).
- No new test unless the lane's checklist names one (section 10).
- **The report ends with the lane's checklist**, each item marked done, not done, or changed (with the reason), then the Expected behaviour lines it owns (section 8.1), marked the same way.
- The lead files a child bead of the epic for each lane that has none and updates the L7 bead (Ocean moved to L16).

### 11.2 Built

- **L1** (BUG-elwvu.1): `SceneDoc`, `ItemId`, the (kind, property) table, the five commands, the first compiler. World Fog on or off and the Environment's Sky style are structural fields; Exposure is on the Environment and reaches every camera's lens; one Environment per scene.
- **L2** (BUG-elwvu.2): the Scene tab on the document, `scene_of`, the tree, the Add menu, Cmd+G, the Lighting card, field rows, the old path deleted (survivors in section 3).
- **L3** (BUG-elwvu.3): imports as Groups, Model File swap, Combine Meshes, materials, skins, morphs, animation, the file's lights and cameras.
- **L4** (BUG-elwvu.4): the physics setting, `CollisionShape`, the Box3D input gate.
- **L5** (BUG-elwvu.5): Copies, Force items, four mesh modifiers on imported meshes as a per-object list (`ObjectFields.modifiers`, `MeshModifier` in `scene_doc.rs`). L15 moves them to the Modifier kind and deletes the list.
- **L2i** (BUG-elwvu.12): physics per copy, copies of any shape but Ocean, a per-object Modifiers section (L15 replaces it with the Modifier kind and deletes it), disabled reasons, lights and cameras following their group.

### 11.3 Order

| Round | Lanes (at most four build at once) |
|---|---|
| Running | F1, F2, L6 |
| Lead | Merge F1, F2, L6 and `lane/scene-doc-glb-sweep` (3d5849b57). Peter tests the build |
| A | L12, L13, L14, L17 |
| B | L15 (after L12), L7, L8 |
| C | L9 (after L7), L16, L18, L19, L20, L12b (each after L15) |
| D | L21 (after L15 and L8), L10 (after L6, L7, L9, L12, L16) |
| E | L22 (after every other lane) |
| F | L11 |

### 11.4 Lanes

**F1 (Opus, running): Add and import, materials, Shape list, names, Sphere.**
Files: `scene_schema/add_menu.rs`, `scene_schema/object.rs` (Shape and names), `scene_doc/base.rs`, `scene_doc/import.rs`, `commands/scene_doc/` (material and import), `manifold-app/src/scene_import.rs` and `scene_material_look.rs`, `scene_setup_panel/material_block.rs` and `properties.rs`, the sphere mesh source. Joins: L6 adds one Add menu line.
1. Add → Object adds the Object and its own Material in one undo step; the base cube has its own Material.
2. The Material block shows inline on every Object card: look, Colour and surface rows, "Used by N", Make Unique.
3. Make Unique gives the object its own copy in one undo step.
4. The looks Default, Matte, Coated, Brushed Metal and Glass apply to the Material item in one undo step.
5. Add → Mesh from file opens the file picker at once and runs `import_model_commands`; the empty-Object-then-swap path is gone.
6. A multi-mesh file imports as a Group with one child per mesh-bearing node; a single-mesh file as one Object.
7. An import from the Add menu lands centred and sized as a drop does.
8. Every import draws its file's materials and textures.
9. Shape lists the built-in solids only: Cube, Sphere, Plane, Tetrahedron, Octahedron, Icosahedron, Dodecahedron. No Ocean, no Mesh from file.
10. Model File shows on an import Group and on a single imported Object, and a pick swaps through `swap_model_file_commands`.
11. Changing an imported Object's Shape to a built-in drops its file in one undo step.
12. A default name follows the shape until the user renames the item.
13. Sphere draws, takes Collider and Body, and takes Copies.
14. `every_scene_edit_undoes_exactly` covers Add Object with its Material and Make Unique.
15. The release app is built in slot-5 and the report gives the launch command.

**F2 (Opus, running): Link Scale, the lone Body, camera, toast, row slop.**
Files: `scene_setup_panel/scale_link.rs`, `manifold-app/src/ui_bridge/scrub.rs`, `manifold-water-rigid/src/physics/admission.rs` and `copy_group.rs`, `manifold-app/src/setup_check.rs`, `scene_schema/camera.rs`, `scene_compile/camera.rs`, the camera header in `scene_setup_panel`, the motion blur path, `manifold-ui/src/panels/toast.rs`, `scene_schema/modifier.rs` (row visibility only), `viewport_gizmo.rs`. Joins: L15 replaces the per-object Modifiers section with the Modifier kind, which supersedes item 12.
1. With the link on, dragging one Scale axis moves all three in proportion during the drag, and the values stay on release; one undo step. The report names the root cause.
2. Link Scale stays a card helper with no new param.
3. The copies uniform-scale refusal names its real reason.
4. A lone Body falls on play after None → Body or a Shape change, with no Copies toggle. The report names the root cause.
5. No body is held silently: a pending shape joins when built, or the card shows why not.
6. Distance has a minimum above 0.
7. The Look At target is an Object or Group pick (a structural field set by Set), and the camera follows it.
8. Look At shows "Use this view".
9. Aperture Shape sits under Depth of Field.
10. Motion blur shows while the orbit camera moves, or the report names why not and the fix.
11. A toast shows its whole text and stays up long enough to read.
12. No Add Modifier row on built-in solids. (Superseded by ruling 20; L15 deletes the per-object Modifiers section.)
13. The gizmo axes are thicker, through the existing gizmo style constants.

**L6 (Sonnet, running; BUG-elwvu.6): Swarm.**
Files: `scene_compile/swarm.rs` (new, from the old `swarm_body`), `scene_schema/swarm.rs`, one Add menu line. Joins: forces through L5's `scene_compile/forces.rs`; Emit From and Draw As through the existing field rows.
1. Add → Swarm adds the Swarm and its default Attractor, core and gust Forces in one undo step.
2. `scene_compile/swarm.rs` ports the old `swarm_body`; nothing is invented.
3. Count, Size, Air Drag and Look are card rows; every param of the swarm's nodes is a row, automatic or internal.
4. Each Emit From choice (Box, Sphere, the surface of any Object or Group) emits.
5. Each Draw As choice (Tetrahedron, Cube, Octahedron, Icosahedron, Sphere, any Object or Group) draws.
6. The force lattice bounds follow the emitter with a margin.
7. Forces act on the swarm.
8. No Billboard or Add Modifier row on a Swarm card; no hidden spawn cube or shape node; the swarm preset builder is deleted.
9. Promise 11's test includes a Swarm.
10. One headless frame of a Swarm drawn as a two-part Group, through `scripts/gpu_queue.py`, with no crash.

**L12 (Opus): one writer per graph.**
Files: `manifold-node-engine/src/load/expand/`, the `scene_modifier_*` files in `manifold-core`, `manifold-app/src/scene_modifier_edit*`, `scene_modifier_transfer.rs`, the picker in `ui_bridge/projection/inspector.rs`, `manifold-nodes-scene/.../scene_modifier_legacy_migration/`, the addressing in `scene_compile/forces.rs`, the graph editor's open path, `manifold-io/src/migrations/scene_cinematic_tail_v1130.rs`. Joins: L15 builds on the node-id addressing.
1. A scene modifier instance addresses compiled nodes by node id; `rg -n SceneNodeRef crates` returns zero.
2. `scene_modifier_edit` is deleted in core and app, with its call sites and tests.
3. The old layer-card modifier picker is deleted; nothing but the compiler writes a scene layer's graph.
4. The graph editor refuses to open a scene layer's graph, with a toast naming why.
5. `scene_modifier_transfer.rs` and the scene modifier load migrations are deleted.
6. The `stamp_scene_node_exposures` callers in `manifold-io` and the catalog test are gone.
7. No comment names `SceneVm::from_def`.
8. Forces still compile and act: promise 8 passes.

**L13 (Opus; BUG-elwvu.13 (World Render ambient occlusion)): World Render.**
Files: `scene_compile/world.rs`, `scene_schema/world.rs`, `scene_compile/light.rs` and `scene_schema/light.rs` (HDRI Rotation only), the environment bake primitive (one new param, following docs/ADDING_PRIMITIVES). Joins: L14 shares the glb sweep.
1. World Render has an Ambient Occlusion row, on by default, compiling `node.ssao_gtao` and its blur as the old importer wired them.
2. The glb sweep goldens that missed for ambient occlusion (Emissive, Interpolation, SimpleInstancing) pass.
3. The render modes' clay colour, line colour and point size are World Render rows.
4. HDRI Rotation turns the environment and its lighting.
5. World Fog Density is a performance row.

**L14 (Opus; BUG-elwvu.14 (RecursiveSkeletons skinning against its golden)): imports look like the file.**
Files: `scene_compile/mesh.rs`, `scene_compile/model_material.rs`, the skinned mesh sources, the glb sweep harness. Joins: L13 shares the sweep.
1. RecursiveSkeletons draws the same skinned geometry as its golden.
2. Combine Meshes keeps each part's material and animation.
3. The report lists what main's `gltf_import/upgrade/` passes did and whether the branch does each; any missing one is restored in `model_material.rs`.
4. With L13, the glb sweep is green.
5. The `gltf_card_precedence` gate ownership red is gone.

**L17 (Sonnet): materials restore.**
Files: `scene_schema/material.rs`, `scene_compile/material.rs`, the Material field in `scene_doc.rs`, one Add menu line. Joins: after F1's material work.
1. Material Type offers PBR, Unlit and Cel, each compiling the material node main used.
2. A layer texture feeds Base Colour or Emission.
3. Add → Layer Plane adds a Plane Object with an Unlit Material whose texture is a chosen layer, with alpha cutout, in one undo step.
4. Promise 1's test covers a Type change.

**L15 (Opus): the Modifier kind and the mesh modifiers.**
The branch's per-object list (`ObjectFields.modifiers`, `MeshModifier`, `MeshModifierKind` in `scene_doc.rs`, from L5) and the per-object Modifiers section (L2i) move to the Modifier kind and are deleted.
Files: in `scene_doc.rs`, the Modifier kind and its fields and the per-object list's deletion; `scene_schema/modifier.rs`; `scene_compile/modifiers.rs` (new, from `emit_mesh_modifier` in `forces.rs`); `commands/scene_doc/modifiers.rs`; the bounds output in `scene_compile/object.rs`, `group.rs` and `mesh.rs`; the recipe source capture in `load/expand`; the Forces and Modifiers sections in `scene_setup_panel/outliner.rs`; the per-object Modifiers section in `scene_setup_panel/properties.rs` and `scene_setup_panel.rs` and its projection in `manifold-app/src/ui_bridge/projection/scene.rs` (deleted). Joins: after L12; L21 adds the scene-wide types, and L16, L18, L19 and L20 add types and shapes through its mechanism.
1. Modifier is a kind with Type, On and Targets fields. Force and Modifier items sit at the top of the scene; Reparent and Cmd+G never put one inside a Group.
2. The tree has a Forces section and a Modifiers section; the Modifiers section lists the cards in the order they apply, and dragging a card reorders the chain.
3. Each card has an on/off toggle and a swappable Type; a swap keeps the card's place, toggle and targets. Add, Remove, reorder, toggle and swap are one undo step each.
4. Targets take one or more Objects or Groups, or the whole scene. Add aims a new card at the selection, or at the whole scene when nothing is selected. Removing a target item drops it from every card in the same undo step.
5. `ObjectFields.modifiers`, `MeshModifier`, `MeshModifierKind`, `modifiers_unavailable` and the per-object Modifiers section are deleted; `rg -n 'MeshModifier|modifiers_unavailable' crates` returns zero.
6. Every item supplies its own rest-pose bounds, centre and vertex count; recipes read them the same way for every shape; `scene_bounds` is no longer `None` (`builder.rs`).
7. An Object target bends in its own frame; a Group target bends all its parts in one shared frame; the whole scene bends as one Group of everything.
8. A built-in shape gets enough mesh detail automatically while a deforming modifier targets it.
9. Every modifier row is a performance row (none is Setup role), with the param id `<modifier item id>.<property>`.
10. Each masked type has the mask rows main carried; Centre, Width and Feather are in radius units, labelled so.
11. No Clip Trigger toggle, Burst Strength, Burst Duration, Attack, Hold or Return row remains. Whether Ordered Recon's Stagger acts without a trigger is unconfirmed: keep it only if it does.
12. Every L15 row of section 15.2 is offered, in the home the table gives: Surface Peel, Ordered Recon, Surface Waves, Spatial Echoes, Elastic Sculpture, Vortex Fragments, Shatter.
13. Shatter acts on Body targets; a target whose Physics is not Body is disabled with its reason; Pieces is setup and the break is a live param.
14. Any order of cards compiles and draws.
15. Promise 1's test covers adding, reordering and swapping a card.
16. Surface Peel works on a Cube, a Sphere, a Plane, an imported part, a Group and the whole scene (one look in the app each).
17. No fixed count of modifiers per item or per scene (ruling 23): the 16 stage-carrying modifiers per owner cap and the Math View count cap go.

**L7 (Opus; BUG-elwvu.7): Water and Water Source.**
Files: `manifold-nodes-water/`, `scene_compile/water.rs` and `water_source.rs`, `scene_schema/water.rs` and `water_source.rs`, `manifold-water-rigid/` (water coupling), the Physics View menu entry. Joins: colliders through L4's `CollisionShape`; L9 writes item ids in the water passes.
1. Add → Water adds a domain; Add inside a Water adds a Water Source.
2. The Water card has the rows of section 8; solver internals are automatic in the table.
3. The domain size row's limit comes from the solver constant.
4. Every Collider and Body whose bounds touch the domain is a solid in the water, through `CollisionShape`.
5. Fill on a source that cannot fill is disabled with its reason.
6. Physics View is a Render menu mode.
7. Past 255 coupled items the card says how many are not coupled.
8. Fluid role wiring and the role 3 stopgap are deleted, with the Water in a Photoscan lifted chain and the `stamp_scene_node_exposures` call in `gpu_flip.rs`.
9. Shell thickness and section 7.8 agree.
10. The setup check covers water through `scene_of`.

**L8 (Sonnet; BUG-elwvu.8): cameras per display, camera Shake.**
Files: `manifold-app` output and display code, the camera switch and Shake in `scene_compile/camera.rs`, the clipping and Field of View limits and the Shake rows in `scene_schema/camera.rs`, `manifold-ui` display settings, `manifold-nodes-scene/src/node_graph/primitives/transform_shake.rs` (two params, following docs/ADDING_PRIMITIVES.md). Joins: L21's Scene Loop uses the camera pick; L18's Shake modifier uses the same node.
1. Each output and display has a camera pick, defaulting to the World's default camera.
2. The pick is a live param, mappable to MIDI, OSC, LFO and automation, and never recompiles.
3. Removing a picked camera falls back to the World's default in the same undo step.
4. Near Clip stays above 0 and below Far Clip; Field of View stays in its range.
5. No single-camera assumption is left in output and display code.
6. Promise 9's test exists and passes.
7. The Camera card has a Shake section: Amount, Speed, Position strength, Rotation strength, every one a performance row, compiled to `node.transform_shake` on the camera's transform after the rig. Amount 0 leaves the camera still.
8. `node.transform_shake` takes Position and Rotation strength in place of its fixed quarter ratio, with defaults that keep today's look; Speed drives its Frequency. The node catalog is regenerated.

**L9 (Opus; BUG-elwvu.9): pixel clicking, one selection store, the clipboard.**
Files: `manifold-nodes-scene/src/node_graph/primitives/render_scene.rs` and its selection pass, `viewport_gizmo.rs`, `physics_overlay.rs`, `window_input.rs`, `workspace.rs`, `scene_setup_panel.rs`, the scene clipboard. Joins: the water passes from L7 write item ids.
1. The selection mask pass writes item ids; the compiler publishes the item-to-slot table.
2. A click on the output picks the drawn item, parts in groups included, with gizmos shown or hidden.
3. Gizmo handles remain only for lights, forces, water domains and sources.
4. One selection store for tree, viewport and preview; one press host; no UI-thread scene rebuild on a press.
5. The selection survives a structural edit.
6. Cmd+C and Cmd+V copy and paste items within a scene and into another scene layer; Cmd+D duplicates.
7. Promise 13's GPU proof passes.

**L12b (Opus): the sealed Scene node.**
Files: `manifold-node-engine` (a sealed node the compiler owns), the install seam (`install_scene_document`), the graph editor's open path and its read-only node view. Joins: after L15. Reference: Blender's compositor Render Layers node. Search the graph engine for an existing group or sealed-node mechanism first and extend it.
1. A scene layer's graph opens in the graph editor. The compiled scene shows as one sealed Scene node: it can be moved, not opened, edited, deleted or rewired inside.
2. Its outputs are Colour plus whatever passes the render already produces (depth, normals, item id); a pass that does not exist yet is reported, not built.
3. Nodes wired between the Scene node and the layer output survive every recompile (any Scene panel edit), save and load.
4. Undo of a Scene panel edit never undoes a graph editor edit, and the reverse.
5. L12's refusal toast is gone; nothing writes inside the Scene node but the compiler.

**L20 (Opus): curve shapes.**
Files: `scene_compile/curves.rs` (new), `scene_schema/curves.rs` (new), Shape entries in `scene_doc.rs`. Joins: bounds through L15. Reference: Blender's curve objects (Extrude and Bevel depth) and its Screw modifier.
1. Shape offers Extrude (`extrude_curve`), Revolve (`revolve_curve`) and Tube (`tube_from_path`).
2. Each takes a Curve field with built-in curves: Circle, Polygon (Sides), Star, Spiral, Helix, Wave and Lissajous, built from the existing curve nodes (`generate_range`, `array_math`, `pack_curve_xy`, `frequency_ratio`). A curve that needs a node that does not exist is reported, not built.
3. Every curve row (Sides, Radius, Turns, ratios) and Depth, Thickness and Segments are performance rows, so the modulation, audio and trigger drawers can drive them. Nothing reacts to the beat on its own.
4. Each curve shape takes Material, physics, Copies and modifiers like any shape.
5. Drawing your own curve by hand is not in this lane.

**L16 (Sonnet): Ocean.**
Files: `scene_compile/ocean.rs` (new), `scene_schema/ocean.rs` (new), the Shape entry, the Sea Horizon environment source in `scene_compile/light.rs`. Joins: bounds through L15.
1. Shape offers Ocean, and it draws the ocean surface from main's Ocean starter nodes.
2. The Ocean rows show on the Object card when Shape is Ocean.
3. Modifiers and Copies on an Ocean: changed, pending Peter. An endless camera-projected sea has no bounds for a modifier frame or a copy layout, so both refuse an Ocean with that reason on the card and in the Targets picker, and scene-wide modifiers skip it.
4. Environment offers Sea Horizon.

**L18 (Sonnet): deformers.**
Files: `scene_compile/deformers.rs` (new), `scene_schema/deformers.rs` (new), Modifier type entries in `scene_doc.rs`. Joins: L15's Modifier kind; Shake uses the node L8 extends.
1. The deformers on main's Object card are Modifier types: Bend, Twist, Taper, Inflate, Rotate, Displace by Texture, Morph.
2. The mesh deformer nodes no preset or menu used are Modifier types: Boil, Ripple, Melt, Glitch Jitter, Shatter mesh, Slice, Voxelize, Fold, Facet Normals, Displace Mesh, Wave Shear, Normal Wave, Cut Out Box.
3. Shake (`transform_shake`) is a Modifier type for Objects and Groups. A camera's shake is its own Camera card section (L8), not this type.
4. Each works on every shape, on Groups and on the whole scene.

**L19 (Sonnet): copies restore.**
Files: `scene_compile/copies.rs`, `scene_schema/copies.rs`, Modifier type entries for the copy fields in `scene_doc.rs`. Joins: L15's Modifier kind.
1. Layout offers every arrangement main had (Grid, Ring, Scatter, Line, and each `generate_instance_transforms` arrangement) plus On Surface of an item (`scatter_on_mesh`).
2. The copy fields are Modifier types that act on their targets' copies: wave field, copy positions, displace, jitter, per-copy noise, field blend (`wave_field_3d`, `copy_positions`, `displace_copies`, instance jitter, per-copy fbm and simplex, `lerp_instance_fields`).
3. A Group's copies space by the group's bounds, and a group nested in a copying group places its own transform inside the instance.
4. Copies of a group take any number of parts (ruling 23): `MAX_GROUP_PARTS` goes.

**L21 (Opus): the scene-wide modifiers: Scene Loop, Math View, Scene Mirror.**
Files: `scene_compile/scene_loop.rs`, `math_view.rs` and `scene_mirror.rs` (new), `scene_schema/scene_wide.rs` (new; their rows), their Modifier type entries in `scene_doc.rs`, the Loop rig in `scene_schema/camera.rs` and `scene_compile/camera.rs`. Reads docs/SCENE_LOOP_DESIGN, docs/SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN and docs/SCENE_MIRROR_DESIGN first. Joins: after L15 (the Modifier kind and the scene's bounds) and L8 (camera files and the camera pick).
1. Scene Loop, Math View and Scene Mirror are Modifier types. They always act on the whole scene, and their card has no Targets row.
2. A scene holds at most one Scene Loop and one Math View; a second Add is disabled with that reason.
3. Scene Loop tiles every item into a corridor, and every camera sees it.
4. Camera Rig offers Loop, which rides the scene's Scene Loop and is disabled with its reason when the scene has none; a cut to another camera keeps the tiling.
5. Scene Loop's rows are performance rows; Cell Size, Home, Near and Far start from the scene's bounds.
6. Math View has Mode (Scene, Math, Overlay) and its rows, all performance rows, and draws the scene's modifier maths.
7. Scene Mirror reflects the whole scene (`reflect_array`), its rows performance rows.
8. Each works with every kind in the scene, physics and copies included, at any place in the chain.
9. Neither Scene Loop nor Math View has a row on the World card.

**L10 (Sonnet; BUG-elwvu.10): the crash test, deletions, docs.**
Files: `crates/manifold-nodes/tests/`, `crates/manifold-nodes/assets/generator-presets/` (deletions), `manifold-core/src/scene_exposure.rs` (deletion), `docs/`.
1. The scene crash test of section 10 exists and passes.
2. The fifteen old `render_scene` starters leave the browser: those no proof reads are deleted with their tests, and the water, physics and ocean scenes the engine proofs load move to test fixtures. Scene.json stays.
3. `scene_exposure.rs` is deleted.
4. The deletion proof returns zero.
5. Superseded docs are marked, the maps updated, the node catalog and docs index regenerated.

**L22 (Opus): clean-up.**
Files: the whole branch diff against main. Runs alone, after every other lane.
1. One simplify pass over the branch: dead code, duplicate helpers, comments that restate code, stale names, legacy verbose prose.
2. Every lane checklist line reported "not done" or "changed" is fixed, or logged as a typed bead with its reason.
3. Slop the per-phase reviews found and left is fixed.
4. Godfile ceilings raised during the run get their split.
5. No behaviour changes beyond those fixes; the crash test passes.

**L11 (Opus; BUG-elwvu.11): review, fixes, one landing.**
1. One read-only Fable review of the whole branch against this document: complete, no slop, no bugs. It is the run's only Fable use; L11 fixes what it finds.
2. Every Expected behaviour line (section 8.1) checked in the app; each L11 line fixed where it fails.
3. Review against the rulings: survivors of the old path, a second graph writer, a param write that recompiles, a structural property with a mappable row, standing refusal text, a second selection store, a shape path outside `CollisionShape`, anything non-finite that can reach Box3D, a modifier list on an Object or Group, a feature main had with no home.
4. The crash test and the promise list pass.
5. One landing gate; the lead lands on main without waiting for Peter when the review, the crash test and the promise list are green.

## 12. Forbidden moves

- A second scene path, fallback, stopgap, shim or migration. Old scenes may break (D-119, no old-show compatibility).
- A second writer of any region: the graph editor writing inside the Scene node, the Scene panel writing any graph, anything but the graph editor writing an effect or generator graph, or a hand-wired scene graph anywhere.
- Dropping a feature main had without Peter's yes, or leaving one with no owning lane.
- A preset-only param, section, label, `cardVisible` or string binding on a scene layer.
- A row the (kind, property) table does not declare, or a bespoke row, slider or drawer (the param surface recipe named in ruling 12).
- Hand-editing a compiled graph, or saving it.
- A structural property exposed as a mappable param.
- A param write that recompiles or resets anything.
- A refusal as standing card text, only in a log line, or with a wrong reason; a disabled row for something that never applies; a choice offered that draws nothing.
- A row that lets through a value that breaks the view or the solver.
- Scene state kept in panel-only memory, out of reach of undo.
- A fallback section for a row, or a capacity or budget row a performer has to manage.
- A physics setting beyond None, Collider, Body; an Advanced section; a Kinematic setting.
- A second collision-shape path per solver, or any route into Box3D that skips the input gate.
- An interim physics (hand-picked shapes) "until automatic shapes land".
- A modifier that works only on some shapes, or a shape refusal on modifiers.
- A modifier list or Modifiers section on an Object or Group; forces in a chain; a Force or Modifier inside a Group.
- A gizmo-box pick for something that has pixels; a second selection store.
- A special Layer Plane kind or path.
- A Scale link implemented as a new param or a bespoke row.
- A `SceneNodeRef` use, or a path as identity.
- A twelfth kind, or a Copies, ImportedModel or Lighting kind.
- A UI flow suite, a shape or digest test, or a test beyond the lane's checklist.

## 13. Costs, stated honestly

- **Values live in the manifest, not the item.** One home per value and an untouched runtime, but the document alone is not a complete scene: export and the crash test read both.
- **Structural properties cannot be modulated.** An LFO cannot switch Draw As on stage. Switching physics, Draw As or Emit From mid-show restarts that item's simulation, as a Reset does.
- **Whole-graph recompile per structural edit.** A large import's first structural edit is where a stutter would show; Peter checks it in the app.
- **Old scenes break.** Shows built on graph scenes play their saved graph but their Scene panel is empty (D-119). Landing does not wait on them and no migration is built; any critical one is rebuilt by hand later.
- **The branch is short of main until the restore lanes land.** Section 15 lists what is missing and who brings it back.
- **Stable ids instead of USD paths.** Right for live mappings; an id-to-path map is needed on export.
- **Water reads every Collider and Body in its World whose bounds touch the domain.** A collider that should not touch the water must be kept out of the domain; there are no collision groups in this cut.
- **Automatic collision detail** may miss on hard scans, and there are no manual overrides. The better method is a follow-up (section 14).
- **The input gate refuses bad physics input** instead of simulating it. A scene with a NaN scale shows a refusal on its card and that item stays out of the solver.
- **The scene is sealed in the graph editor.** A scene layer's graph shows the scene as one node with post-processing wired around it (ruling 18). Nodes inside the scene are edited only in the Scene panel and cannot be inspected.

## 14. Deferred

Each has a trigger; none is built in this work.
- **Convex decomposition by CoACD** (Wei et al., SIGGRAPH 2022) or V-HACD for bodies. Trigger: the automatic spatial split misses on a real scan. Ported code needs file-header credit and `THIRD_PARTY_NOTICES.md`.
- **Swarm collisions** using the water's distance lattice. Trigger: after the swarm lane lands and Peter asks.
- **Material and geometry node assets** (Material source = graph asset, a geometry graph on an Object, a sky graph on an Environment), attached to items, never replacing the scene. Hand-drawn curves for Extrude, Revolve and Tube would arrive this way; the built-in curves ship now (L20). Trigger: a node feature for materials or geometry exists, or Peter asks for hand-drawn curves.
- **A read-only view of the compiled graph.** Trigger: a debugging need the logs cannot meet.
- **Hover preview of a structural edit's effect** (keep, install, restart item, restart world) before the click. Trigger: Peter loses a simulation to a structural click on stage.
- **Water collision groups** and a "Water collision: off" switch. Trigger: a collider that must stay out of the water but sits in its domain.

## 15. Scene features on main, and where each lives

Every scene feature a performer could use on main (origin/main), its state on the branch at df95dd7ce, its home, and its owner. Every engine primitive still exists on the branch; "missing" means nothing in `scene_compile/` emits it or no row reaches it. Status read from code; not checked in the app.

### 15.1 Inventory

| Feature | Reached on main by | Branch | Home | Owner |
|---|---|---|---|---|
| Built-in solids: Cube, Plane, Tetrahedron, Octahedron, Icosahedron, Dodecahedron | Add → Object; graph | Works | Object Shape | built |
| Sphere | Swarm Draw As only | Missing as an Object shape | Object Shape | F1 |
| Ocean surface (`projected_grid`, `ocean_spectrum`, `inverse_fft_2d`, `ocean_displace`) | Ocean and Ocean Cliff starters | Offered in Shape, draws nothing | Object Shape Ocean | L16 |
| Procedural geometry: grid, hypercube, polytope edges, nested cubes, triangulated grid | Their own line generators (Wireframe, Tesseract, Nested Cubes, Blossom Wire, Lissajous) | Unchanged | Not a scene feature | none |
| Curve geometry: extrude curve, revolve curve, tube from path | Hand-wired | Missing | Object Shapes Extrude, Revolve, Tube | L20 |
| Wireframe lines (`render_lines`) | Their own line generators | Unchanged | Not a scene feature | none |
| Groups, nesting, visibility | Graph groups | Works | Group | built |
| Shake on objects (`transform_shake`) | Hand-wired | Missing | Modifier type for Objects and Groups | L18 |
| Camera shake (`transform_shake` on a camera's transform) | Hand-wired | Missing | Camera Shake section: Amount, Speed, Position strength, Rotation strength | L8 |
| Layer plane (layer onto Base Colour or Emission; unlit, cutout) | Add → Plane; Skin row | Partly: a layer feeds Base Colour only; no unlit or cutout default | Plane Object, Material from a layer; Add → Layer Plane | L17 |
| glTF import | Drop; Add → Import Model | Drop works; Add → Mesh from file takes the wrong path | Group or Object with a file | F1 |
| Skins, morphs, animation clips | Automatic | Works (`scene_compile/mesh.rs`; Clip, Rate, Loop Mode, Retrigger) | Import Group | built; RecursiveSkeletons L14 |
| The file's lights and cameras | Automatic | Works | Light, Camera items | built |
| The importer's material upgrade passes (`gltf_import/upgrade/`) | Automatic | Deleted | Model texture source | L14 |
| Photoscans | Import; Water in a Photoscan | Group with Combine Meshes | Group | L14 |
| Screen-space ambient occlusion (`ssao_gtao` and its blur) | Automatic in imported scenes | Nothing emits it | World Render row | L13 |
| PBR surface rows | Inline on every object | Only for a bound Material item; new objects bind a row-less default | Material, inline | F1 |
| Material looks: Default, Matte, Coated, Brushed Metal, Glass | Material Preset list | Deleted | Material look | F1 |
| Material features: Coat, Iridescence, Emission, Glass, Sheen, Anisotropy, Translucency, Subsurface | "+ Add feature" | Works once a Material exists | Material features | built |
| Texture from file; a model's maps | Material inspector | Works | Material texture source | built |
| Unlit and Cel materials | Water starters; scene table | Missing (PBR only) | Material Type | L17 |
| Mesh deformers on the Object card: Bend, Twist, Taper, Inflate, Rotate, Displace by Texture, Morph | Object card "+ Add Modifier" | Missing | Modifier types | L18 |
| Deformers in scene graphs: Boil, Ripple, Melt, Glitch Jitter, Shatter mesh, Slice, Voxelize, Fold, Facet Normals, Displace Mesh, Wave Shear, Normal Wave, Cut Out Box | Hand-wired | Missing | Modifier types | L18 |
| The twenty scene modifier presets | Layer card picker | Section 15.2 | Section 15.2 | Section 15.2 |
| Scene Mirror (`reflect_array`) | Hand-wired | Missing | Scene-wide Modifier type | L21 |
| Field masks (`mesh_spatial_mask`) | Inside recipes | Inside the built recipes; Centre labelled metres, though the primitive works in radius units | A Mask section on each masked Modifier type | L15 |
| Copies: Count, Layout, Spacing, Columns, Turn | Make Copies | Works; the Line layout is unconfirmed | Copies | built; L19 |
| Copy fields: `wave_field_3d`, `copy_positions`, `displace_copies`, instance jitter, per-copy fbm and simplex, `lerp_instance_fields` | Hand-wired | Missing | Modifier types acting on their targets' copies | L19 |
| Scatter on a surface; `generate_instance_transforms` arrangements | Hand-wired | Missing | Copies Layout | L19 |
| Rigid physics, World physics | Enable Physics; starters | Works | Physics setting; World | built |
| Forces: Radial, Uniform, Vortex, Wind, Attractor, Explosion | Add → Force; layer card | Works; Drag compiles as Wind | Force | built |
| Swarm | Add → Swarm; starters | Missing | Swarm | L6 |
| Forces on swarms | Swarm starters | Missing | Force targets | L6 |
| Water: GPU FLIP, MPM, particles, surface, whitewater | Add → Water; starters | Missing | Water | L7 |
| Water sources: Fill, Inflow, Drain | Add inside a Water | Fields only | Water Source | L7 |
| Physics View | Water card | Missing | Render menu mode | L7 |
| Camera rigs: Orbit, Free, Look At | Camera card | Works; Look At target is three numbers | Camera Rig | built; Look At target F2 |
| Camera switching | Hand-wired; Scene Loop | World Camera row, live cut | Per-layer live pick; per-display pick is BUG-lz538 (per-display camera pick) | L8b |
| Depth of field with Aperture Shape | Camera card | Works | Camera | built |
| Motion blur | Camera card | Rows work; invisible while orbiting in Peter's test | Camera | F2 |
| Exposure | Lighting card | Works | Environment | built |
| Sun, Point, Spot; shadows | Lighting card | Works | Light | built |
| Sky Gradient, Softbox; HDRI file, intensity, background, Aim Sun at HDRI | Lighting card | Works | Environment | built |
| HDRI Rotation | Not on main | Missing | Environment row (new) | L13 |
| Sea horizon environment (`sea_horizon_env`) | Ocean starters | Missing | Environment source | L16 |
| Render mode: Rendered, Solid, Wireframe, Points | World card | Works | World Render | built |
| Render mode clay colour, line colour, point size | Hand-wired | Missing | World Render rows | L13 |
| Ray tracing (reflections, shadows, ambient occlusion, global illumination), temporal upscale | World card | Works | World Render | built |
| Fog | Scene Fog preset; World | Works as World Fog; Density is Setup role | World Fog, Density a performance row | L13 |
| Bloom, chromatic aberration, grain, 2D depth of field | Layer effects | Unchanged; not scene features | Layer effects | none |
| The fifteen old `render_scene` starters | Browser | Play as plain graphs with an empty Scene panel | Gone from the browser (engine-proof scenes kept as test fixtures); scenes are built in the panel and saved | L10 |
| Graph scenes in saved projects | Load | Play as plain graphs with an empty Scene panel | No migration (D-119) | none |

### 15.2 Modifier restore

The twenty scene modifier presets main shipped in `crates/manifold-nodes/assets/scene-modifier-presets/`, plus Scene Mirror. "Hidden on main" means `available: false`. What each does and how it is played is in `scene_modifiers_intent.md` in the session notes. Every mesh modifier works on any Object, on Groups and on the whole scene (ruling 20), and every row is a performance row. Forces are their own kind, not modifiers.

| Preset | Built from on main | Branch | Home | Owner |
|---|---|---|---|---|
| Attractor, Explosion, Radial Force, Uniform Force, Vortex Force, Wind | Force recipes | Works as Force items | Force kind | built |
| Surface Peel | `transform_mesh_patches`, `morph_mesh`, `mesh_spatial_mask` | Imported parts only, in a per-object list; rows Setup role | Mesh Modifier with a mask; no burst rows (ruling 22) | L15 |
| Ordered Recon | `ordered_recon_mesh`, `mesh_stagger_envelope` | Imported parts only, in a per-object list; rows Setup role | Mesh Modifier with a mask; Progress is the gesture; no hit-envelope rows (ruling 22) | L15 |
| Surface Waves | `normal_wave_mesh` | Imported parts only, in a per-object list; rows Setup role | Mesh Modifier with a mask | L15 |
| Spatial Echoes | `analytic_echo_instances` | Imported parts only, in a per-object list; rows Setup role | Mesh Modifier; stacks on Copies | L15 |
| Elastic Sculpture | `wave_shear_mesh`, `morph_mesh`, `mesh_spatial_mask` | Missing | Mesh Modifier with a mask | L15 |
| Vortex Fragments | `transform_mesh_patches`, `morph_mesh`, `mesh_spatial_mask` | Missing | Mesh Modifier with a mask | L15 |
| Surface Peel — Masked (hidden on main) | Surface Peel with its mask on | Missing | No type: Surface Peel with its mask set | none |
| Surface Peel hit variant (hidden on main) | Surface Peel with Clip Trigger on | Missing | Dropped (ruling 22) | none |
| Ordered Recon — Clip Gesture (hidden on main) | Ordered Recon with Clip Trigger on | Missing | Dropped (ruling 22) | none |
| Waves + Echoes (hidden on main) | Surface Waves then Spatial Echoes in one card | Missing | No type: two cards, Surface Waves then Spatial Echoes | none |
| Shatter | `trigger_gate` on the rigid body's `release_count` | Missing | Mesh Modifier on Body targets; a target whose Physics is not Body is disabled with its reason; Pieces is setup, the break is a live param (ruling 22) | L15 |
| Scene Loop | `scene_array`, `loop_camera`, `camera_switch`, `beat_ramp` | Missing | Scene-wide Modifier, one per scene, plus the Loop camera rig (ruling 21) | L21 |
| Scene Fog | `atmosphere` | The same node as World Fog | Removed: World Fog, Density a performance row | L13 |
| Math View | Rows the renderer reads (Mode, Occlusion, Grid, Fragments, Ghosts, Vectors, Axes, Trails, brightness, Density, Line Width, hues) | Missing | Scene-wide Modifier, one per scene (ruling 21) | L21 |
| Scene Mirror (no preset) | `reflect_array` | Missing | Scene-wide Modifier (ruling 21) | L21 |

### 15.3 For Peter

Features that fit no kind:
- **Drawing a curve by hand** (point editing in the viewport). L20 ships built-in curves only; a hand-drawn curve needs its own design pass.

Calls built as defaults, pending Peter:
- Every new Object gets its own Material.
- Add → Mesh from file imports a Group, or one Object for a single-mesh file, through the real importer.
- Shape lists built-in solids only; Ocean returns as a Shape in L16.
- Model File lives on the import Group and on a single imported Object.
- Link Scale stays a card helper.
- The Look At target is an Object or Group pick.
- Aperture Shape stays, under Depth of Field.
- Default names follow the shape.
- Sphere is a built-in shape.
