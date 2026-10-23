# Scene refinement — simple, honest, never frozen

<!-- index: Refinement pass on the scene document after the first live tests: draw only real vertices, a last-resort layer cut instead of a frozen Mac, errors that show, physics on all cores, copies of any scale, materials whose features show and leave cleanly, everything updating on drag, the panel finished to its design, one Radial force, collider meshes for scans, smoother AO and a Scene Loop camera button. Water is out of scope. Three phases, eleven lanes. -->

**Status:** APPROVED · building, phase 1. Epic and lane beads are listed in section 5.
**Document type:** Design and plan. A fresh session builds from this doc plus the base doc.
**Execution contract:** [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) section 5 (Phase briefs); [AGENT_ROUTING.md](AGENT_ROUTING.md) for seats, briefs and gates.
**Base:** [SCENE_DOCUMENT_DESIGN.md](SCENE_DOCUMENT_DESIGN.md). Everything there stands unless a line below changes it.
**Amends:** base ruling 3 (a live row may re-lay its own item while dragged, section 2 ruling 6), base ruling 20 (automatic detail only where bends need it), base section 7.8 (a Collider's contact mesh), base section 7.12 (one Radial force), base section 8 (tree sections and Add), CINEMATIC_POST_DESIGN.md D9 (ambient occlusion, which may now use past frames).
**Judged by:** Peter in the app, once, after all three phases land. An agent's gate is a computed number or an `rg` that finds nothing, never a judgment of an image.
**Out of scope:** water (domain, sources, whitewater, MPM, water ray tracing); the water upgrade owns it. Hand-drawn curves. Copies under every modifier.
**Findings with code anchors:** session notes `video_review_r1.md`, `video_review_r2.md`, `review_refinement_doc.md` in `~/Library/Application Support/Claude/side-session-notes/local_fdcd1661-f045-4f70-ab0c-89f3458c1c62/`. Each lane reads the issues it owns there.

Binding constraints: those of the base doc. Two-thread model unchanged; no new shared state; all edits through `EditingService`; no per-frame allocation on hot paths.

## 1. Goal

A simple, easy panel that meets a professional standard, and an engine that never freezes the Mac. This is a refinement pass: it fixes what the first live tests found and removes more than it adds. No new categories of rows, no new error system, no cost readout. Nothing that works today gets worse.

## 2. Rulings

1. **No rogue Materials.** A Material used by one item shows only inline on that item's card and is removed with it. It shows as its own tree row only when two or more items share it. Duplicate still shares (base section 7.6).
2. **Copy modifiers stay, offered only where they act.** Copy Wave, Copy Jitter, Copy Noise and Copy Blend move whole copies. They are offered only for a target with Copies on; on a target without, they are disabled with the reason "Turn on Copies to move them".
3. **Swarm forces stay general forces,** aimed at the swarm and named after it.
4. **Per-section Add only.** Lighting, Objects, Forces and Modifiers each carry a +; the Camera row carries one for cameras. Objects' + holds the shapes, Group, Water, Swarm, Layer Plane and Mesh from file. The top Add goes.
5. **Heavy edits never block.** No refusal and no confirm; silly values are allowed and render slowly. A scene layer whose GPU frame runs too long stops drawing, says why, and offers Draw Again.
6. **Everything updates on drag.** No row waits for release. A row too slow to drag is made faster. Copies Count and Swarm Count add or remove copies and particles while the rest keep moving. On Body copies, Layout, Spacing, Columns and Turn re-lay that pile as they move, as they do today: the item re-lays, nothing else restarts.
7. **Scene Loop offers a Loop camera.** An Add Loop Camera button on the card adds a camera on the Loop rig and makes it the layer's camera. The World Camera row switches back.
8. **Ambient occlusion:** its hard edge is fixed first, then it uses past frames.
9. **One Radial force.** Radial Force, Attractor and Explosion become one Radial force with a signed Strength and the Impulse button. The Forces + offers Push, Pull and Burst as starting values for that card.
10. **Body and Collider copies may overlap.** Spacing must be above zero, from one constant shared by the row and the gate. Copies that start inside each other blast apart; that is an effect.

## 3. Phases and lanes

Each lane: its own slot and `lane/refine-<name>` branch, one commit, then stop and report. The report lists what it deleted beside what it added, and marks each of its section 4 checks pass, fail or not run.

### Phase 1: never frozen, never silent

**P1a Lockup and Math View** (Opus, high).
- The draw follows the live count: `fragment_cuts.rs` wires the last cut map's `live_count` into the object's `node.scene_object` as a new optional input; `SceneObject` carries it; `render_scene/frame_setup.rs` uses it when the mesh slot has no extent. The cut map's fixed reserve stays.
- Detail on request: a second per-type flag, `smooth_detail`, drives automatic detail; `deforms` keeps its Shatter meaning. On: Bend, Twist, Taper, Surface Waves, Wave Shear, Melt, Inflate, Ripple, Elastic Sculpture, Boil, Fold. Off: Vortex Fragments, Shatter, Shatter Mesh, Ordered Recon, Surface Peel, Slice, Voxelize, Cut Out Box. Glitch Jitter is the lane's call.
- Math View marks are sized from the frame radius the capture already holds (`MathViewCapture.radius`), never from a triangle. Its capture stays in `load/expand`.

**P1b Never frozen, never silent** (Opus, high).
- Last-resort cut: each scene layer ends its own command-buffer chunk (`commit_and_continue`); its GPU time is polled from the retained buffer, as `raytrace/trace_timing.rs` does. Over a named constant beside `TRACE_BUFFER_CEILING_SECONDS` (start 0.25 s) for three frames in a row, the layer stops drawing; a toast says why; the World card offers Draw Again.
- Errors show: the executor keeps the newest error per node and prints it to stderr once; `ContentState` carries the new ones, folded like `graph_edit_diagnostic`; the app toasts each once, naming the layer. `render_scene`'s subsurface abort and every other abort draw the magenta fallback, never a stale texture.
- `scripts/gpu_queue.py` waits a set time for MANIFOLD to quit before a GPU run, then reports the run as not run.

**P1c Copies and cores** (Opus, high).
- The uniform-scale and "positive scale up to 100" rules in `physics/copy_group.rs` are deleted. A copied Object's B matrix carries the prototype's per-axis scale only; position and rotation stay in the pose.
- Spacing: ruling 10, one constant in `copies_body.rs` used by `scene_schema/copies.rs` and `copy_group.rs`.
- Refusals name the item and the real reason, on the control they concern; no "Out of the physics world", no "can't move" on a Collider. A refused copy group draws at rest; a refused single Body stays where it was.
- Physics on the performance cores: `bridge.c` sets `workerCount` to the performance cores minus one (the UI thread's; the content thread counts as a worker), at least one, using Box3D's built-in scheduler.
- Copies share one collision shape instead of one cooked per copy.

**P1d Materials** (Opus, medium).
- Removing a feature keeps it in the list with mode Removed (the node runs it as Off); its rows hide; Add feature offers it again with its values.
- Adding Glass, Translucency or Subsurface at Metallic 1 sets Metallic 0 in the same undo step. At Metallic 1 those rows carry the reason "Metal lets no light through" through the projection's `row_reasons`.
- Sheen is seeded at Roughness 0.5.
- Remove also removes, in the same undo step, every Material whose only users were in the removed subtree.
- The extension-map sampler and UV rows show only while their map has a texture.
- Glass under ray tracing: reflected and refracted surfaces get the material's transmission.

### Phase 2: feel and panel

**P2a Live dragging** (Opus, high).
- The cut map's scan becomes a multi-block scan, so dragging Ordered Recon's Bands and Direction stays smooth.
- Copies Count is live. Line and Grid grow one way from the first copy, which sits on the origin, as Blender's Array does: no fill from the centre, no re-centring on Count. Ring spreads its copies evenly round the whole circle, up to Columns a ring, then starts the next ring, as Cinema 4D's radial Cloner does, so its copies slide round as Count changes. New tail copies install on the tick they land, live and in export; existing Body copies keep their motion (only the rest layout changes); physics carry adopts the live-added copies.
- Swarm Count is live: new particles spawn where a reseed would put those slots, seeded by index and epoch; the mover steps only active particles.

**P2b Panel** (Sonnet, high; strict checklist).
- Tree: World, Camera, Lighting, Objects, Forces, Modifiers, with the + buttons of ruling 4. The Lighting row's indent matches the others. The tree lists a Material only when two or more items use it. A Body inside a Group is marked.
- On/off is a switch everywhere: `on_off()` fields draw as switches. The Modifier card header carries its on/off switch and type.
- Dragging a modifier shows where it will land.
- Copy modifiers per ruling 2.
- A swarm card shows "Look: <item>", a link to its Draw As item.
- A value box shortens or drops its unit before it drops digits.

**P2c Forces and gizmos** (Opus, medium).
- One Radial force (ruling 9); swarm forces named after their swarm (ruling 3).
- Past the swarm's force lattice, a particle reads radial and vortex forces from their formulas, as `interaction.rs` does, and Uniform, Wind and Drag at their constant value. Water's lattice is unchanged.
- Gizmos: smooth anti-aliased lines at a readable width, round circles. A radial force shows its radius and two falloff shells (half strength and a tenth). A Vortex shows a cylinder round its axis, with the axis.

**P2d Scan colliders** (Opus, high).
- Box3D's per-contact triangle buffer is per-worker scratch that grows, so the exact mesh collides correctly at any density; the patch is listed in `native/box3d/README.md`.
- A Collider's contact mesh is the exact mesh, less the slivers Box3D drops (under 0.25 mm², far under its 5 mm contact skin). It is never simplified: simplification bought no step time. A small body or a splash rests on the surface it sees.
- Measured cost: worst case 0.78 ms per step, a 1.15 m cube on the cliff scan at 0.049 scale.
- Water keeps its own distance lattice from the exact mesh; it does not read the contact mesh.

### Phase 3: polish

**P3a Ambient occlusion** (Opus, high). A falloff at its radius and a per-frame rotation first; the lines must be gone before the next step. Then history: last frame's AO reprojected by the render's velocity output, rejected per pixel where depth disagrees, reset on a camera shot change, a resize and a state clear, never on a seek, a loop point or a geometry change.

**P3b Scene Loop** (Opus, medium). Ruling 7. Rows only a Loop camera reads are hidden until one exists. The cell is measured from the rest layout, the copies' layout box included, never from moving bodies.

**P3c Cut list and constants** (Opus, medium).
- Delete the variant presets Ordered Recon — Clip Gesture, Surface Peel - Clip Hit, Surface Peel — Masked and Waves + Echoes; move the tests that load them to the stock recipes. Scene Loop (copies) stays.
- Delete the material rows Baked Look (duplicates Type Unlit) and Ambient.
- `MAX_COPIES`, the swarm `MAX_COUNT`, `MAX_PIECES` and `CUT_MAP_EXTRA_RECORDS`: each grows, or gets its one-line physics or memory reason beside it and on its control.

### Every phase

One Opus adversarial review of the phase's lanes before it lands, then one merge and one landing. One Fable review at the end of phase 3.

## 4. Checks

A lane's own gate is a computed number or an `rg` that finds nothing. Peter checks the whole list in the app at the end.
1. P1a: one GPU proof through `gpu_queue`: Vortex Fragments on a cube; the `water_perf` probe's drawn vertices equal the cut's live count, not the capacity.
2. P1c: the same scene gives identical poses after 600 ticks with 1 and with 8 workers. `rg 'copies need one scale' crates` and `rg 'workerCount = 1' crates/manifold-physics/native/box3d/bridge.c` find nothing.
3. P2c: `rg 'ForceType::Attractor|ForceType::Explosion' crates` finds nothing.
4. In the app at the end:
   - 1,000 cube copies on a 100 m ground with Scene Loop, then Vortex Fragments on the whole scene: the Mac stays responsive and Undo works.
   - 65,536 copies of the cliff scan: the layer stops drawing, the Mac stays responsive, Draw Again works.
   - 5,000 Body copies in a Vortex: physics under 10 ms per tick on the overlay.
   - Copies Count 10 to 1,000 mid-play, and Swarm Count 1,000 to 50,000: old ones keep moving.
   - Drag Bands on the hornet: no stall.
   - Brushed Metal plus Glass turns to glass; Metallic back to 1, the Glass rows say why. Add Glass, remove it, add Subsurface: no white picture.
   - Add a Cube, remove it: no Material left. Duplicate a Cube: the Material shows once, shared.
   - A 10 × 0.16 × 10 Collider with Copies: copies draw and collide. A Cube Body on the cliff scan rests on its surface.
   - Ordered Recon then Math View on a photoscan, playing: visible marks and arrows.
   - Scene Loop, Add Loop Camera, play: the camera flies the corridor.
   - AO at a cube contact at 10 m: no lines.
   - Per-section +, switches everywhere, the modifier header switch, Copy modifiers only with Copies on.

## 5. Beads

Epic BUG-g6p6m (Scene refinement: simple, honest, never frozen). Its children .1 to .11 are the lanes P1a, P1b, P1c, P1d, P2a, P2b, P2c, P2d, P3a, P3b and P3c, in that order.

## 6. Forbidden moves

Those of the base doc, plus:
- A refusal or confirm prompt for a heavy edit; a fixed cap added to stop a lockup.
- A row that waits for release to apply.
- A new error, reason or refusal path beside `row_reasons`, `setup_check` and the toast.
- A cost readout, a new row category, or a feature this doc does not name.
- Any change to water behaviour.

## 7. Costs

- A project saved with an Attractor or an Explosion does not open at all after P2c: the loader reads the whole project file in one pass, so one unknown force fails the project, not only its scene layer. The closed format has no fallback (D-119, no old-show compatibility). Peter's clifTesting project holds an Attractor.
- Dragging Layout, Spacing, Columns or Turn on Body copies re-lays that pile each frame, as today.
