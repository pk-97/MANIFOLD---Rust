# Scene refinement — the panel tells the truth, the engine never locks, and less code does more

<!-- index: Refinement pass on the scene document after the first live tests: restart rows declared and applied on release, one reason per inert row, errors on the card, single-use materials that leave with their object, the draw following live counts with a last-resort GPU cut, physics on all cores, modifiers that move shapes or whole copies, Math View drawing real pieces, and a cut list of duplicates. Water is out of scope. -->

**Status:** DRAFT · adversarially reviewed once (findings folded in); pending Peter's sign-off. No epic yet; the lead files one at sign-off with a child bead per lane.
**Document type:** Design and plan. A fresh session builds from this doc plus the base doc.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) section 5 (Phase briefs); [AGENT_ROUTING.md](AGENT_ROUTING.md) for seats, briefs and gates.
**Base:** [SCENE_DOCUMENT_DESIGN.md](SCENE_DOCUMENT_DESIGN.md). Everything there stands unless a line below changes it.
**Amends:** base ruling 3 and section 12 (restart rows, section 3.1), base ruling 20 (automatic detail), base section 7.4 (the restart rows leave the live list), base section 7.8 (a Collider's contact mesh), base section 7.12 (one Radial force), base section 7.19 (copy fields become a Moves choice), base section 8 (tree sections and Add), CINEMATIC_POST_DESIGN.md D9 (ambient occlusion, which may now use past frames).
**Judged by:** Peter in the app, against section 9. An agent's gate is a computed number, never a judgment of an image.
**Out of scope:** water (domain, sources, whitewater, MPM, water ray tracing, source naming); the water upgrade owns it. Hand-drawn curves.

Binding constraints: those of the base doc. Two-thread model unchanged; no new shared state; all edits through `EditingService`; no per-frame allocation on hot paths.

## 1. What the tests showed

The first live tests on main held up the foundation: one document, one compiler, live rows that never recompile, physics with copies and forces at a thousand bodies. Everything that went wrong falls into six causes:

1. **The panel shows what is declared, never what is in effect.** Rows that do nothing say nothing: Glass at Metallic 1, Sheen at roughness 0, Copy modifiers without Copies, Scene Loop rows with no Loop camera. A removed feature keeps shading. Errors land in stderr.
2. **Restarts hide behind live-looking rows.** Body copies teleport to rest when Spacing or Layout moves; Swarm Count lands only at the next reseed; Bands re-cuts the whole mesh per drag frame.
3. **Ownership has two rules.** An Object's Material outlives it; a Water's leaves with it.
4. **Nothing budgets work that grows with a count.** One Add of Vortex Fragments drew about 6.4 billion vertices per pass (each cut mesh drawn at its padded capacity × 1,000 copies × 31 loop tiles). The GPU saturated, the window server starved and the Mac stopped responding. Box3D runs on one worker.
5. **Hidden caps and stale rules.** Box3D reads 256 triangles per contact and drops the rest, so bodies sink into scans. The copies uniform-scale rule guards a need that no longer exists. Spacing's row and gate disagree.
6. **The panel was not finished.** Per-section Add buttons were removed against the design; on/off is a pick list on structural rows and a switch on live ones; gizmos are coarse.

The full findings live in the session notes (`video_review_r1.md`, `video_review_r2.md`, `review_refinement_doc.md`). This doc fixes the causes and deletes before it adds.

## 2. Rulings

1. **No rogue Materials.** A Material used by one item shows only inline on that item's card and leaves with it. It shows as its own tree row only when two or more items share it.
2. **Every shape modifier can move the shape or each copy.** A structural field, Moves: Shape or Each copy. Copy Wave, Copy Jitter, Copy Noise and Copy Blend are deleted.
3. **Swarm forces stay general forces,** aimed at the swarm and named after it.
4. **Per-section Add only.** The top Add goes.
5. **Heavy edits never block.** No refusal and no confirm. Silly values are allowed; they render slowly. The machine is protected by a last-resort cut of the offending layer, with one click to draw it again.
6. **Counts are live where cheap.** Copies Count and Swarm Count add or remove copies and particles while everything else keeps moving. Ordered Recon's Bands and Direction apply on release.
7. **Scene Loop offers a Loop camera.** An Add Loop Camera button on the card adds a camera on the Loop rig and makes it the layer's camera. The World Camera row switches back.
8. **Ambient occlusion uses past frames,** after its hard edge is fixed.
9. **One Radial force.** Radial Force, Attractor and Explosion become one Radial force with a signed Strength and the Impulse button. The Forces + offers Push, Pull and Burst as starting values for that card.
10. **Body and Collider copies may overlap.** Spacing must be above zero, from one constant shared by the row and the gate. Copies that start inside each other blast apart; that is an effect.

## 3. The row contract

**3.1 Restart rows.** A new exposure, `restarts()`, separate from today's `setup()`. A `setup()` row is a card-placement marker and stays a live, mappable row; transforms are `setup()` rows and stay mappable. A `restarts()` row applies only when the gesture ends (release, Enter, a menu pick): the scrub skips the live write on Move and sends one write on Commit. It shows no mapping buttons and carries a small "restarts <item>" note. A restart-row write restarts only its own item, once.

The restart rows in this pass, declared with `when` predicates where they depend on physics:
- Ordered Recon: Bands, Direction X, Y, Z (each re-runs the cut).
- Copies: Layout, Spacing, Columns, Turn when the item's physics is Body (each re-lays every copy at rest). On Collider and drawn-only copies they stay live.

A live row never resets. A solver restart caused by a live row is a bug in that row's declaration. Follow-up bead: the cut map's scan becomes a multi-block scan; once a re-cut is cheap, Bands and Direction go back to live.

**3.2 One reason per inert row.** The UI projection's `row_reasons` is the one answer for a row that is present but cannot change the picture. It reads the document and each param's base value, as the mode gates already do, and the panel shows it through `inactive_reason`. `setup_check` stays the channel for what the running solver refused. Each lane adds the reasons for the rows it owns, for example:
- Glass, Translucency and Subsurface at Metallic 1: "Metal lets no light through."
- Moves: Each copy on an item with Copies off: "Turn on Copies to move them."
- Scene Loop rows that only a Loop camera reads: hidden until one exists.

**3.3 Errors reach the card.** The executor keeps the newest error per node and prints each error to stderr once. `ContentState` carries a per-layer list, folded like `graph_edit_diagnostic`. A `render_scene` error names its object slot, which the app maps to the item through the pick table, and the item's card shows it plus one toast. A failing node draws its fallback (magenta), never a stale texture.

**3.4 Features.** Removing a material feature keeps it in the Material's list with mode Removed, which the node runs as Off. Its rows hide; + Add feature offers it again and brings back its values. Adding Glass, Translucency or Subsurface at Metallic 1 sets Metallic 0 in the same undo step; removing it leaves Metallic alone. Sheen is seeded at Roughness 0.5 (Blender's default) so it shows on the base cube.

## 4. Ownership

- **Materials (ruling 1):** Remove also removes, in the same undo step, every Material whose only users were in the removed subtree, for an Object and a Water alike. The tree lists a Material only when two or more items use it. `can_hold` is unchanged and Duplicate still shares, as base section 7.6 says.
- **Swarm look:** the swarm draws with its Draw As item's look. Its card shows "Look: <item>", a link that selects that item.
- **Bodies in Groups:** the tree marks the Body item. The parent's disabled Physics row already gives the reason.

## 5. Budget

**5.1 Draw what is real.** `fragment_cuts.rs` wires the last cut map's `live_count` into the object's `node.scene_object` as a new optional input; `SceneObject` carries it; `frame_setup.rs` uses it when the mesh slot has no extent of its own. Rejected: forwarding live extents through every element-wise and fused primitive, which reaches into the freeze compiler. The cut map's fixed reserve stays; once the draw follows the live count it costs only memory.

**5.2 Detail on request.** A second per-type flag, `smooth_detail`, drives automatic detail; `deforms` keeps its meaning for which parts Shatter may break. Smooth detail on: Bend, Twist, Taper, Surface Waves, Wave Shear, Melt, Inflate, Ripple, Elastic Sculpture, Boil, Fold. Off: Vortex Fragments, Shatter, Shatter Mesh, Ordered Recon, Surface Peel, Slice, Voxelize, Cut Out Box. Glitch Jitter is the lane's call, named in its report. Amends base ruling 20.

**5.3 The build and the content thread.** The compile stays inside the edit: it rebuilds the manifest, which is what makes Undo exact. The lockup was GPU-bound, and 5.1 and 5.5 remove it. After R1 lands, the lead profiles the original repro. Only if the runtime build (`registry.create`) takes longer than one frame does it move to a `BackgroundWorker`, newest wins, with carry and install on the content thread at swap time. Otherwise this is dropped and logged.

**5.4 Show the cost.** An item's card header shows an estimate computed in the projection from the document and base values: vertices × copies × Scene Loop tiles × passes. It is right-aligned and shown only above a threshold. No GPU readback.

**5.5 Last-resort cut.** Each scene layer ends its own command-buffer chunk (`commit_and_continue`). Its GPU time is polled from the retained buffer, as `raytrace/trace_timing.rs` does. A layer over a named constant beside `TRACE_BUFFER_CEILING_SECONDS` (start at 0.25 s) for three frames in a row stops drawing. Its World card says why and offers Draw Again. This protects the machine; it is not a quality limit. Follow-up bead, the root fix BUG-vtjx3 (heavy water plus RT frames are killed by the macOS GPU watchdog) already names: split heavy instanced draws across command buffers.

**5.6 Physics on all cores.** `bridge.c` sets `workerCount` to the machine's cores minus two, at least one, and uses Box3D's built-in scheduler. Box3D orders contacts the same at any worker count; a test proves our use of it (section 11).

**5.7 Live counts.** A copy's place depends on its index and the layout rows, never on Count, so layouts grow from the first copy. A Count change installs the new tail on the tick it lands, live and in export; existing copies keep their motion, and re-enabled tail copies return to their places. Physics carry adopts the whole world node, live-added copies included. A new copy shares its siblings' collision shape. New swarm particles spawn where a reseed would put those slots, seeded by index and epoch; the mover steps only active particles.

## 6. Caps and stale rules

- **Contact meshes for scans.** The `CollisionShape` service builds a Collider's contact mesh by vertex clustering at a cell size set from world scale, cached by scale as today. Amends base section 7.8 (the exact mesh). Box3D's per-contact triangle buffer moves to per-worker scratch in the world's task contexts and grows; the patch is listed in `native/box3d/README.md`.
- **Copies keep any scale.** The uniform-scale rule and the "positive scale up to 100" rule in `copy_group.rs` are deleted. A copied Object's B matrix carries the prototype's per-axis scale only; position and rotation stay in the pose.
- **Spacing:** ruling 10.
- **Refusals** name the item and the real reason, on the control they concern. A refused copy group still draws at rest; a refused single Body stays where it was.
- **Forces beyond the swarm's lattice.** The lattice covers each aimed force's reach (the radius for an Edge curve) plus the emitter. Past it, a particle reads radial and vortex forces from their formulas, as `interaction.rs` does, and Uniform, Wind and Drag at their constant value. Water's lattice is unchanged.
- **Readouts keep their digits.** A value box shortens or drops its unit before it drops digits.
- **Named constants.** `MAX_COPIES`, the swarm `MAX_COUNT`, `B3_MAX_MESH_CONTACT_TRIANGLES`, `MAX_PIECES` and `CUT_MAP_EXTRA_RECORDS`: each grows, or gets its one-line physics or memory reason beside it and on the control.

## 7. The panel and modifiers

- **Tree:** World, Camera, Lighting, Objects, Forces, Modifiers. Lighting, Objects, Forces and Modifiers each carry a +; the Camera row carries one for cameras. Objects' + holds the shapes, Group, Water, Swarm, Layer Plane and Mesh from file. The Lighting row's indent matches the others.
- **On/off is a switch everywhere.** `on_off()` fields draw as switches. The Modifier card header carries its on/off switch and type, as base section 8 says. Dragging a modifier shows where it will land.
- **Gizmos:** smooth lines at a readable width and enough segments that circles look round. A radial force shows its radius and two falloff shells (half strength and a tenth). A Vortex shows a cylinder round its axis, with the axis drawn.
- **Moves: Each copy (ruling 2).** The card's recipe runs on a point mesh of the copy centres (normal = the copy's up axis) and writes the moved centres back, in `copies_plan`'s frame: two small element-wise primitives and one expander branch, after the DECOMPOSING_GENERATORS.md section 2.5 (audit by analogy) audit. It moves copies; Shake on Each copy also turns them, as it already shakes whole transforms. Hidden on Ordered Recon, Surface Peel, Vortex Fragments, Shatter, Shatter Mesh, Slice, Voxelize, Cut Out Box and Facet Normals. Disabled with its reason on Body copies, which physics places. Homes: Copy Wave → Surface Waves, which gains a Push direction row while Moves is Each copy; Copy Noise → Boil; Copy Jitter → Shake; Copy Blend → a Copies "Blend To" second layout with a live Mix, its rows hidden while Blend To is None. Copies do not turn to follow Bend or Twist in this pass.
- **Math View draws the pieces the chain moves.** Capture and cut-map alignment stay in `load/expand`. The 18 value controls become ordinary card params. A piece is a cut's cell or band (the cut map gains a piece-id output), one copy for copies, and for smooth deforms a cell of a k × k × k grid over the frame's rest box, k from Density. Arrows come from a per-piece centroid reduction, a new primitive audited under section 2.5. Marks are sized from the frame radius the capture already holds. A topology mismatch shows on the card through 3.3.
- **Scene Loop:** ruling 7. The cell is measured from the rest layout, never from moving bodies; the corridor takes the copies' layout box as a live input computed from the Copies params. This closes BUG-dsasx (Scene Loop cell size follows live Copies rows and swarm spread).
- **Glass under ray tracing:** reflected and refracted surfaces get the material's transmission, as the first surface already does.
- **Ambient occlusion (ruling 8):** first a falloff at its radius and a per-frame rotation, then a check that the lines are gone. Then history: last frame's AO reprojected by the render's velocity output, rejected per pixel where depth disagrees, reset on a camera shot change and a seek, never on a geometry change. With AO on, the main pass writes velocity.

## 8. Cut list

Each line is struck or kept at sign-off. A lane that finds another duplicate adds a line here, not code.
- Copy Wave, Copy Jitter, Copy Noise, Copy Blend (section 7).
- Attractor and Explosion as separate force types (ruling 9).
- The variant presets Ordered Recon — Clip Gesture, Surface Peel - Clip Hit, Surface Peel — Masked and Waves + Echoes. The tests that load them move to the stock recipes. Scene Loop (copies) stays: the Scene Loop compiler uses it.
- Automatic detail on shapes that do not need smooth bends (section 5.2).
- Material rows: Baked Look (duplicates Type Unlit); Ambient (a non-physical fudge); the 168 extension-map sampler and UV rows while their map has no texture.
- The copies uniform-scale and scale-100 rules (section 6).
- Pick lists for on/off; refusals only in stderr; the Spacing row and gate disagreeing.
- Kept on purpose: Shatter and Shatter Mesh (one breaks bodies, the other explodes faces); Glitch Jitter and Shake (a stepped per-vertex hash versus smooth whole-transform noise).

## 9. Acceptance

Peter checks these in the app. A lane's own gate is a computed number or an `rg` that finds nothing.
1. 1,000 cube copies on a 100 m ground with Scene Loop, then Vortex Fragments on the whole scene: the Mac stays responsive and Undo works.
2. 65,536 copies of the cliff scan: the layer stops drawing, the Mac stays responsive, Draw Again works.
3. 5,000 Body copies in a Vortex: physics under 10 ms per tick on the overlay (about 58 ms today).
4. Raise Copies Count from 10 to 1,000 mid-play: existing copies keep moving. Swarm Count from 1,000 to 50,000: new particles appear, old ones keep moving.
5. Drag Bands on the hornet: no stall during the drag, one re-cut on release.
6. Brushed Metal plus Glass: the cube turns to glass. Metallic back to 1: the Glass rows say why.
7. Add Glass, remove it, add Subsurface: no white picture.
8. Add a Cube, remove it: no Material left behind. Duplicate a Cube: the Material shows once as shared.
9. A 10 × 0.16 × 10 Collider with Copies on: copies draw and collide.
10. A Cube Body dropped on the coastal cliff scan Collider: it rests on the surface.
11. Ten cube copies, Surface Waves with Moves: Each copy: the cubes ride a wave and stay cubes.
12. Ordered Recon, then Math View, on a photoscan, playing: one arrow per piece, visible.
13. Scene Loop, Add Loop Camera, play: the camera flies the corridor.
14. AO on the cube contact at 10 m: no lines.
15. Forces + Push, Pull, Burst: one Radial card each.
16. Per-section +, switches everywhere, the modifier header toggle.

## 10. Plan

**10.1 Lane rules.** Base section 11.1, plus:
- Opus seats for lanes with a judgment call; Sonnet only for struck cut-list deletions.
- Each brief lists the base-doc and this-doc requirements the lane must keep, and the section 9 items it owns. Review checks those.
- Each report lists what the lane deleted, with line counts, beside what it added.
- `scripts/gpu_queue.py` waits a set time for MANIFOLD to quit, then reports the proof as not run; the lane reports it unverified and the lead reruns it (wave 1, lead).
- Landing in batches, with known reds named up front. Opus review per wave; one Fable review at the end.

**10.2 Wave 1.** At most four build at once.
- **R1 Lockup** (sections 5.1, 5.2). Files: `load/expand/fragment_cuts.rs`, `primitives/scene_object.rs`, `manifold-node-engine/src/scene/scene_object.rs`, `render_scene/frame_setup.rs`, `scene_schema/modifier.rs`, `scene_schema/deformers.rs`, `scene_compile/object.rs`. Gate: the GPU proof in section 11.
- **R2 Materials** (sections 3.4, 4 Materials removal, the material rows in section 8, Glass under ray tracing). Files: `manifold-app/src/scene_edit.rs` (material functions and remove), `scene_compile/material.rs`, `manifold-nodes-scene/src/node_graph/material_inspector.rs`, `primitives/pbr_material.rs` (row exposure only), `render_scene/rt_frame.rs`.
- **R3 Copies rules and cores** (section 6 copies scale, Spacing, refusals; section 5.6). Files: `physics/copy_group.rs`, `copies_body.rs`, `scene_compile/copies.rs`, `render_scene/instance_space.rs`, `primitives/physics_world.rs`, `scene_schema/copies.rs`, both `setup_check.rs`, `physics.rs` publish, `native/box3d/bridge.c`. Gate: `rg 'copies need one scale' crates` and `rg 'workerCount = 1' crates/manifold-physics/native/box3d/bridge.c` find nothing.
- **R4 Panel** (section 7 tree, switches, header toggle, drag feedback; readout digits; the tree hiding single-use Materials). Files: `scene_schema/fields.rs` (`on_off`), `scene_schema/add_menu.rs`, `manifold-ui` `outliner.rs`, `scene_setup_panel.rs`, `item_header.rs`, `properties.rs`, `param_surface.rs`, `param_slider_shared/state.rs`, `projection/scene.rs`.
- **R5 Last-resort cut** (section 5.5, delivered as a toast and a `ContentState` flag; the card line comes with R7). Files: `content_pipeline.rs`, `manifold-gpu` encoder chunking, the new constant beside `TRACE_BUFFER_CEILING_SECONDS`.

**10.3 Wave 2,** after sign-off and wave 1's landing.
- **R6 Restart rows** (section 3.1). Files: `scene_schema/mod.rs`, `scene_schema/modifier.rs`, `scene_schema/copies.rs`, `ui_bridge/scrub.rs`, `param_surface.rs`.
- **R7 Errors on the card and the cost estimate** (sections 3.3, 5.4, and R5's card line). Files: `exec/execution.rs`, `content_state.rs`, `primitives/render_scene.rs`, `projection/scene.rs`.
- **R8a Forces and presets** (ruling 9, swarm force names, the variant preset cuts). Files: `scene_doc.rs` ForceType, force presets, `scene_schema/force.rs`, `add_menu.rs`, `scene_edit.rs` swarm forces, the moved tests. Gate: `rg 'ForceType::Attractor|ForceType::Explosion' crates` finds nothing.
- **R8b Moves and Blend To** (section 7 Moves). Files: the two new primitives, the expander branch, `scene_schema/modifier.rs` Moves field, `scene_schema/copies.rs` Blend To, the four Copy presets deleted. Gate: `rg 'CopyWave|CopyJitter|CopyNoise|CopyBlend' crates` finds nothing.
- **R9 Math View** (section 7 Math View).
- **R10 Live counts** (section 5.7). Files: `physics/copy_layout.rs`, `physics.rs` relayout, `physics_carry.rs`, `scene_compile/swarm.rs`, `swarm_forces.rs`.

R6, R8a and R8b share schema files and run one after another in that order; R7, R9 and R10 run beside them.

**10.4 Wave 3.**
- **R11 Contact meshes, the Box3D buffer, forces beyond the lattice, named constants** (section 6).
- **R12 Gizmos.**
- **R13 Ambient occlusion** (section 7): falloff first, history second.
- **R14 Scene Loop** (ruling 7, the cell measure).
- The lead's 5.3 measurement, after R1.

## 11. Testing

Base ruling 14 stands. New tests only where a silent break would reach the stage:
- R1: one GPU proof through `gpu_queue`: Vortex Fragments on a cube; the `water_perf` probe's drawn vertices equal the cut's live count, not the capacity.
- R3: the same scene gives identical poses after 600 ticks with 1 and with 8 workers.
- R6: the run-everything crash test writes every live row mid-play and checks body poses stay continuous (no jump to rest) and the swarm epoch does not advance.

## 12. Forbidden moves

Those of the base doc, plus:
- A refusal or confirm prompt for a heavy edit.
- A fixed cap introduced to stop a lockup.
- A restart row that applies per drag frame, or a solver restart caused by a live row.
- Building restart rows on today's `setup()` marker.
- A second reason path beside `row_reasons` and `setup_check`.
- A modifier card that duplicates another's maths for copies.

## 13. Costs

- **Saved scenes with removed types do not load that layer.** Copy Wave, Copy Jitter, Copy Noise, Copy Blend, Attractor and Explosion have no fallback in the closed format. Peter's clifTesting project holds an Attractor. No migration (D-119, no old-show compatibility); R8a checks with `project_tool` that the loss is the layer, not the project.
- **On Body copies, Layout, Spacing, Columns and Turn are no longer mappable.** They re-lay the pile, so they apply on release.
- **Bands and Direction are not mappable** until the cut's scan is fast.
- **Copies only move under Moves: Each copy,** except Shake, which also turns them.
