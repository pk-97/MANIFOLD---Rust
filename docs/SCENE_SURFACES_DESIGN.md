# Scene Surfaces — the scene card and the Scene Setup panel as one system

**Status:** PROPOSED · 2026-10-13 · Opus 5.5 (lead session with Peter). Awaiting Peter's approval; P0–P7 not started. Owed: Peter's yes on D2 (the overturned "persist only user choices" direction). Beads: BUG-pmbd3 (3D scene inspector panel upgrades), children mapped in section 5.
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) through section 6 (Seam briefs) before starting any phase.

<!-- index: Scene card (performance) and Scene Setup panel (setup) as two views of one scene item list and one stamped param descriptor table; naming pass, pins, navigation. -->

**The governing insight: the card and the panel already share one row widget, one edit path and stable param ids. What they do not share is the answer to three questions — what is this control called, which scene item owns it, and does it belong on stage. Today eight places answer those questions, and each surface asks a different subset.** This design moves the answers into one classification table, stamped into the manifest beside the label the way material roles already are, and one scene item list. The card and the panel become two filters over the same rows: the card shows the performance role, the panel shows the selected item.

Peter, 2026-10-13, on the split and the order: "Camera first, most useful controls out of the box that get used frequently." On camera rows: orbit, translate, focus, distance and FOV "are actually all very useful on the inspector card." On lights: "'Main Light' is confusing and doesn't mean anything, multiple lights are possible in a scene." On pins: "YES love this!!! also needs right click to remove from card." On the panel tree: "Physics should be it's own thing, not under world." On master force strength: "sounds like a lot of new work? Let's focus on what we have already."

**Stage translation.** The card is what Peter grabs mid-set. After this design, a scene's card opens with the camera, then the scene's own toys (box count, water speed, the preset's macros), then forces with a Kick button each, then one fader per light named after the light, then depth of field and motion blur as two switches. A mapped control never disappears. Right-click moves anything from the panel onto the card and back. Nothing on the card is a placeholder for a control the scene does not have.

**Binding constraints** (docs/DESIGN_AUTHORING.md section 1 (the intake)): *hot path* — none; both projections run on the structural sync path, the per-frame value channel is untouched (id join, WIDGET_TREE INV-6). *Thread residency* — projection on the UI thread from `Arc<Project>` snapshots; every mutation through `EditingService` or `MutateProject`. *Time model* — not involved. *Persistence* — yes: two new optional fields on `ParamSpecDef`, one on `PresetInstance`, refreshed by the existing idempotent load migration. The canonical fixture (`Liveschool Live Show V6 LEDS.manifold`) is pre-3D and must load byte-identically. Param ids never change, so mappings, automation lanes, OSC addresses and macros survive every phase. *Performance surface* — the card is the performance surface; MIDI, OSC, Ableton and automation rows are part of every phase that touches it.

Companions: docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md (the predecessor; its section 5 (Decisions — resolved by Peter) binds here except where D2 and D13 say otherwise). docs/WIDGET_TREE_DESIGN.md section 5b (agent contract and enforcement) (the row recipe every row change follows). docs/MATERIAL_INSPECTOR_DESIGN.md (D3 there is the precedent for D2 here). docs/SCENE_SETUP_PANEL_DESIGN.md (outliner and structural verbs, still standing).

---

## 1. Audit — what exists (verified 2026-10-13)

Extend, don't redesign. Every row below was read in the tree on 2026-10-13.

| Piece | Where | State |
|---|---|---|
| Shared row model (`ParamRow`, `RowSpec` with `disabled`, `inactive_reason`, `section`) | `crates/manifold-ui/src/param_surface.rs:140-176`, `:299-322` | exists. Dim with reason renders on both surfaces: `param_slider_shared/builders.rs:1664-1673`, `:2381-2384` |
| Card projection, one manifest walk | `manifold-app/src/ui_bridge/projection/cards.rs:355-542`; modulation attached at `:519-538`, scene curation called after it at `:540-542` | exists |
| Scene card curation | `projection/scene_performance.rs:26-195` — twelve hard-coded `StandardTarget`s (`:60-169`), relabels and re-sections rows (`:227-228`), renames every kept extra to "Scene Controls" (`:186-188`) | replaced (P2) |
| Placeholder rows with synthetic ids `scene_performance.{key}` | `scene_performance.rs:232-272`, `:428-438` | deleted (P2) — a private id scheme |
| Kept-extra filter ignores modulation | `scene_performance.rs:394-426` | the live bug BUG-pmbd3.7 (card hides mapped rows); fixed P0 |
| Card-visibility table | core `scene_exposure.rs:66-111` `card_visible_for` | replaced by the descriptor table (P1) |
| Stamp-time param filters (which params get exposed at all) | `manifold-nodes-scene/src/node_graph/scene_exposure.rs:115-130`; liquid dial lists `manifold_core::liquid_domain::liquid_dial_params` | replaced by `SurfaceRole::Hidden` (P1) |
| Section naming | `nodes-scene scene_exposure.rs:421-462` `section_name_for_node`; persisted into `ParamSpecDef.section`, never refreshed on load (core `scene_exposure.rs:598-687` refreshes label, range, angle, value labels only) | scene surfaces stop reading it (P2, P4) |
| Load migration, idempotent, stamps and refreshes | core `scene_exposure.rs:432-690` (passes 1–4); called on project load at `manifold-app/src/project_io.rs:538` and bundled-preset cache build at `manifold-nodes/src/bundled_presets.rs:86` | exists; extended (P1) |
| Label refresh: exact auto-stamps re-read the primitive's `ParamDef.label` on every load | core `scene_exposure.rs:598-650` | exists — a `ParamDef.label` edit already reaches old projects |
| Classification-table precedent stamped into the manifest | `nodes-scene node_graph/material_inspector.rs` `material_param_role`; written by the provider at `nodes-scene scene_exposure.rs:176`; refreshed in place at core `scene_exposure.rs:216-246` | precedent for D2 |
| Primary-binding ownership rule (fan-outs keep one owner) | `projection/scene.rs:229-248` `owned_specs`; tests `:494-512`, `:957-1012` | exists; becomes the ownership rule for both surfaces |
| Scene discovery | `nodes-scene node_graph/scene_vm.rs:113-138` (`objects`, `lights`, `camera_controls`, `world_controls`); light name is the raw handle at `:1797` | extended with `scene_items` (P2) |
| Panel item selection | `manifold-ui/src/panels/scene_setup_panel.rs:713-723` `SceneSelection` (Object, Light, Force, Camera, World, Physics, Rendering) | exists |
| Panel outliner | `scene_setup_panel.rs:1787-1976`: Camera loose row, "Lighting & Environment" fold (World + lights), Objects fold with "Import Model…", "Motion & Physics" fold with "+ Force", Rendering loose row, then a separate add row (`scene_setup_actions.rs` `build_add_action_row`); emoji glyphs, one `■` for every object | rebuilt (P4) |
| Panel stats line | `scene_setup_panel.rs:1652-1675` | deleted (P4) |
| Card → panel navigation | `scene_setup_panel/parameter_navigation.rs:56-130` `reveal_clip_response` (requires a clip trigger, `:64-72`); lights and world matched by section string `:28-50` | generalised to ids (P5) |
| Card section header | `param_card/render.rs:1140-1166`, one fold button; fold state UI-local, never saved (`param_card/mod.rs:388-396`) | extended (P5) |
| Param right-click menu | `manifold-app/src/ui_root/dropdowns.rs:1666-1731`; scene panel rows never emit it (comment `:1716`) | extended (P5) |
| Panel expose action | `ui_bridge/project.rs:1223-1255` `SceneSetupExposeParam` — no-op when already exposed; nothing in `manifold-ui` sends it | superseded by pins (P5) |
| Non-undoable view-state write precedent | `ui_bridge/dispatch/params.rs:337-374` (`EffectCollapseToggle` via `MutateProject`) | precedent for fold persistence |
| RGB swatch | only in the panel's material inspector: `scene_setup_panel/material_inspector.rs:853`, `:911`; `RowRole::ColourSwatch(MaterialColour)` at `param_surface.rs:409`; `rgb_members` set only by `projection/material.rs:432` | moved into the shared row host (P6) |
| Atlas icon vocabulary | `manifold-ui/src/icons.rs:19-41` | extended with scene item icons (P4) |
| Item commands | `RemoveSceneObjectCommand` (`commands/graph/scene.rs:980`), `DuplicateSceneObjectCommand` (`graph/scene/duplicate.rs:18`), `RemoveSceneLightCommand` (`scene.rs:1847`), `DeleteSceneModifierCommand` / `DuplicateSceneModifiersCommand` (`graph/modifier_stack.rs:167`, `:275`); no light duplicate | exist |
| Viewport camera | `nodes-scene node_graph/viewport_session.rs:1-27` (isolated from the project), camera at `:226`, moves at `:256-274` | read by P7 |
| `ParamDef` | `manifold-node-engine/src/parameters.rs:233-253`: name, label, ty, default, range, enum_values; 1,891 struct literals in 339 files (`rg -c 'ParamDef \{' crates/`) | labels edited in place (P3); no new fields |
| Flow harness | 36 scene flows under `scripts/ui-flows/` (`ls scripts/ui-flows | rg -c '^scene'`), scenes mapped in `manifest.json:176-266`; fixtures `ui_snapshot/fixtures.rs:28-104` (gltfscene, physicsscene, waterdambreak, waterdambreakmatter, fluidauthoring, mushroomscene); no free-camera scene | exists; `oceanscene` added (P2) |
| Godfile ceilings | `manifold-app/tests/godfile_regrowth.rs:116` caps `scene_setup_panel.rs` at 4300; `scene_vm.rs` (3300), core `scene_exposure.rs` (2825), `material_inspector.rs` (2320), `cards.rs` (1935) are uncapped | ratcheted (P1) |

**Where a control's name and place are decided today** (the disease, counted): (1) `ParamDef.label`; (2) the stamp filters in `nodes-scene scene_exposure.rs:115-130` plus `liquid_dial_params`; (3) `card_visible_for`; (4) `section_name_for_node`, and creation sites that pass their own section strings (`projection/scene.rs:199-200` says creation and load name the same kind differently); (5) the persisted copies in `ParamSpecDef`; (6) `scene_performance.rs`; (7) `look_metadata` relabelling radius to "Size" (`nodes-scene scene_exposure.rs:184-189`); (8) the panel's section-string matching in `parameter_navigation.rs:28-50`. The lead's count of four was low.

**Agreed-plan claims the code contradicts** (checked, so phases don't inherit them):
- "Parent Visible → hidden": already hidden. `parent_visible` is never stamped (`nodes-scene scene_exposure.rs:124`); the water family shows it as an authored control labelled "Visible" (`manifold-nodes-water/src/presets/gpu_flip.rs:795`), which stays.
- White Furnace (`node.bake_environment` `uniform`, `bake_equirect_envmap.rs:219`) and RT Denoise Feed (`render_scene.rs:6847`) are visible today in the panel, so hiding them is a real change.
- "Light colour swatch is one wire" (BUG-pmbd3.10 (light colour swatch)): the card has no swatch renderer; it lives in the panel's material inspector. Real work, P6.
- Light names: the readable GLB light name never reaches `LightRow.name`, which reads the handle (`scene_vm.rs:1797`); the importer builds the readable `label` at `gltf_import/scene.rs:407` and does not store it as a title.
- Forces are not on the card today: `SceneModifier` bindings are dropped unless user-added (`scene_performance.rs:400-402`, `:422`). The nine duplicate force rows in BUG-pmbd3.9 (rows keep their section) only happen with user-added force bindings.
- Some labels in the naming table are not `ParamDef` literals: "Solve Level" and "Max Iterations" are authored labels hard-coded in io migrations (`manifold-io/src/migrations/solve_level_card_v1180.rs:56,70`, `pressure_cap_card_v1190.rs:66,80`), "Max Blur (px)" also at `scene_cinematic_tail_v1130.rs:316`; force labels live in recipe JSON (`manifold-nodes/assets/scene-modifier-presets/UniformForce.json:42-56`). The label refresh does not reach authored labels.
- "The panel groups by `sections_for_doc_ids`": that function is now `sections_for_nodes` (`projection/scene.rs:205`); ownership is already by binding target node, section strings only label sub-groups.

---

## 2. Decisions

**D1 — The card is the performance surface; the Scene Setup panel is the setup surface. Same items, same names.** Every row on the card is a row the panel also shows, under the same label, except the two card label rules in D4 (light name, group name), whose words the panel also displays. Rejected: a card with its own wording ("Horizontal Angle" beside the panel's "Orbit") — that is the "Intensity" vs "Sun Intensity" split the predecessor named as a bug.

**D2 — One descriptor table beside the vocabulary, stamped into the manifest and refreshed on load.** A new module `manifold-nodes-scene/src/node_graph/scene_surface.rs` holds `scene_param_desc(type_id, param) -> Option<SceneParamDesc>` for every param of every scene-vocabulary type: role, panel group, card band and order, card label rule, unit, gate. Stamping copies it into `ParamSpecDef.scene`; the load migration refreshes it on every non-user binding whose primary target is a vocabulary param. Both projections read `ParamSpecDef.scene` from the manifest and nothing else. Labels stay on `ParamDef.label` (and on recipe and preset JSON for authored controls); the table never carries a label. A coverage test fails on any unclassified vocabulary param.

This refines the direction Peter liked ("each node type declares, next to its primitive, each param's label, unit, role"). The facts are declared once, per type, in one table — not inside each primitive — and they are persisted the way material roles are. **It overturns one part of that direction: "projects persist only user choices, not display copies."** Reason, with evidence: the manifest is the only source rows may read (WIDGET_TREE INV-1: "sourced ONLY from the manifest walk — no registry re-reads, no hand tables"), and the persisted label is read far beyond the two surfaces — automation lane names (`projection/timeline.rs`), `project_tool` (`timeline_verbs.rs`), the content thread, OSC naming (`preset_definition_registry.rs`). Stop persisting it and each of those needs its own live resolver; keep it and the load refresh already holds it true. Peter decides this at approval.

Rejected alternatives, priced (DESIGN_AUTHORING section 4 (alternatives)):
- *Live descriptors at projection time (the literal "persist only user choices").* Implementation: cheap in the two projections, which already call `SceneVm::from_def`. Hot path: none. Migration: old projects' copies stay for every other reader, so the label refresh survives anyway and every value now has two homes, live for the card and stamped for automation lanes. Forecloses: the manifest as the single row authority, and the pure-function projection tests (WIDGET_TREE D8a) that need no registry. Killed by INV-1 and the reader count above.
- *Descriptor fields on `ParamDef`.* Puts the fact inside the primitive, which is what Peter pictured. Costs a field on 1,891 struct literals in 339 files for facts that matter to about 30 types, and the graph editor would carry scene-only vocabulary. Rejected on cost.
- *A trait method per scene primitive.* Thirty implementations across six crates (scene, rigid, liquid, FLIP, MPM, whitewater) for one policy, and no single place to audit coverage. Rejected: the coverage test needs one table to iterate.
- *A saved card layout per project (Resolume-style row list).* New scenes still need a default generator — the role table again — and adding a light would not add its row without re-running it, so two systems. Rejected.

Kill-pass on the chosen shape. Strongest case against: persisted descriptors go stale on named and fan-out exposures the refresh never reaches, so old imported scenes show wrong rows. Answer: the refresh keys on the primary binding's target `(type_id, param)`, not the id shape, so named fan-outs (the importer's environment intensity fanning to `hdri_gain`) get the same descriptor as exact stamps. The ownership half is already proven by `projection/scene.rs` tests `world_sections_exclude_the_fanned_out_sun_section` and `duplicated_ground_binding_uses_its_clone_target_for_section_ownership`. The cheapest catching test is P1's `every_bundled_scene_row_has_one_owner_and_one_role` over every bundled scene preset plus the gltfscene fixture; it is a P1 gate, not run at design time (it needs the table to exist).

**Zero-new-systems count** (DESIGN_AUTHORING section 3 (shaping the architecture)): new identity, addressing or dispatch systems = 0. The descriptor table replaces six of the eight deciders above; the item list replaces three ownership functions (`camera_controls`, `world_controls`, `object_controls`) with one; the synthetic `scene_performance.*` id scheme is deleted. Net: one fewer id scheme.

**D3 — One scene item list, ownership by primary binding.** `scene_items(def, &vm)` returns Camera, then each light, each object, each force, each physics world, then World, in that order. A row belongs to the item whose owned nodes contain its primary binding's target node (the `owned_specs` rule, `projection/scene.rs:229-248`), or to a Force item when the primary target is `SceneModifier { modifier_id }`. Rows owned by no item (composite macros, authored controls on non-vocabulary nodes) form the scene's authored group. Both surfaces partition rows with this one function. Rejected: keeping per-surface ownership (the card's `StandardTarget` node lists beside the panel's `owned_specs`), which is how the two surfaces disagree today.

**D4 — Card layout.** Bands in fixed order: **Camera, Scene, Forces, Lighting, Look** (Peter: camera first). Within a band, rows sort by item order, then `card_order`. Section titles:

| Band | Section(s) | Rows (performance role) |
|---|---|---|
| Camera | "Camera" | Orbit camera: Orbit, Tilt, Distance, Target Height, Field of View, Focus. Free camera: Pan, Tilt, Position X, Position Y, Position Z, Field of View, Focus. Look-at and loop cameras: Field of View, Focus (default; revisit when Peter asks for more). Focus dims while depth of field is off. |
| Scene | one section per owning item, titled by item name; authored rows titled by the preset's display name | Physics world: Count, Reset. Liquid domains: Emission, Inflow Speed, Speed, Reset where the domain has them; Matter additionally Cohesion, Stiffness, FLIP Ratio. Authored preset controls (Ocean's wind and waves, composite macros): as authored. Water mesher controls (Surface Detail, Smoothing, Whitewater Budget, Particle Scale): setup role. |
| Forces | one section per force, titled by `scene_modifier_display_name` (`cards.rs:938`, adds "#2") | Enabled, Strength, Kick (the recipe's `fire` trigger, usable by T and audio triggers), Kick Size (`impulse_strength`). Centre, axis, radius: setup. Rows dim with "No physics objects in this force's targets" when the force's eligible objects are empty (the fact `cards.rs:849-855` already computes). |
| Lighting | "Lighting" | Exposure (lens `exposure_ev`), Environment (bake `intensity`, card label = group name), then one Intensity row per light, labelled with the light's name, in item order. |
| Look | "Look" | Depth of Field (switch), then F-Stop and Max Blur only while it is on. Motion Blur (switch), then Shutter Angle and Max Blur only while it is on. |

Camera translate: Target Height (`look_y`) is the only translate the orbit camera has (`camera_orbit.rs:67-129`); lateral pan is Deferred (new primitive params). Rejected: "Main Light Intensity" (Peter, quoted above). Rejected: placeholder rows for absent targets (BUG-pmbd3.3 (hide controls the scene lacks)).

**D5 — Hide what the scene lacks; dim what does nothing now.** A row whose param the scene does not have is not built. A row that exists but has no effect right now renders with `inactive_reason` (Focus while depth of field is off; a force with no physics targets). Hidden-role params stay unstamped (today's filter semantics); already-stamped hidden params keep their exposure and ids and simply build no row.

**D6 — Attached rows are always shown.** A row with a driver, envelope, audio mod, Ableton mapping, macro mapping or automation lane builds on the card whatever its role, its pin or its gate, and builds in the panel even when its role is Hidden. "Remove from card" is disabled on it, reason "Mapped — unmap to remove". Fixes BUG-pmbd3.7 (card hides mapped rows) as a class.

**D7 — User choices are stored apart from stamped facts.** `ParamSpecDef.card_pin: Option<CardPin>` (`Shown` / `Removed`; `None` follows the role) is written only by `SetCardPinCommand` and never by stamping or refresh. Card section fold state persists as `PresetInstance.folded_sections`, written through `MutateProject` like `EffectCollapseToggle` (not undoable — view state). Rejected: pinning through `card_visible` (refresh pass 2 rewrites it on every load, core `scene_exposure.rs:529-563`). Rejected: pinning through `user_added` (un-exposing a user binding deletes it, `expose.rs` `unexpose_user_binding_freezes_effective_value_into_def_slot`). Rejected: pinning through `SceneSetupExposeParam` (no-op on an exposed param, which every scene param is).

**D8 — Panel tree.** Fixed groups in this order: **Camera, Lights, Objects, Forces, Physics, World** (Peter: physics is its own group). A group with no items is not drawn, except Lights and Objects, which always show so their add button is reachable. Each row: type icon, readable name, eye toggle where the item has `visible`. Each group owns its own add button: Lights "+" adds a light; Objects "+" opens one menu (Object, Plane, Water, Import Model…); Forces "+" opens the force picker. World holds Environment, Fog and Ray Tracing (the old Rendering row). Below the tree, the selected item's header shows its name once (click to rename where a rename command exists), its kind, and copy and delete where commands exist: objects copy and delete, forces copy and delete, lights delete only, Camera, Physics and World neither. Sections under the header follow the item's `ControlGroup`s in declaration order. The stats line is deleted.

**D9 — One word list, on the declarations.** Labels change only where they are declared: `ParamDef.label`, recipe JSON, bundled preset JSON, and the io migrations that hard-code authored labels. Units move to `unit` and render in the value readout. The agreed table, plus the six meanings resolved from code:

| Current | New | Where declared |
|---|---|---|
| FOV Y · Look Y · Near / Far | Field of View · Target Height · Near Clip / Far Clip | `camera_orbit.rs`, `free_camera.rs`, `look_at_camera.rs`, `loop_camera.rs` |
| Pos X/Y/Z · Yaw / Pitch (free camera) | Position X/Y/Z · Pan / Tilt | `free_camera.rs`, `loop_camera.rs` |
| Horizontal / Vertical Angle (card) | Orbit / Tilt (Pan / Tilt on free camera) | deleted with `scene_performance.rs` |
| Exposure (EV) · Aperture (card) · DoF Max Radius · Max Blur (px) | Exposure + unit EV · F-Stop · Max Blur · Max Blur + unit px | `camera_lens.rs`, `nodes-image motion_blur.rs:102`, `scene_cinematic_tail_v1130.rs:316`; find DoF Max Radius with the command below |
| Mode · Aim X/Y/Z · Colour R/G/B · Light Size (light) | Type · Target X/Y/Z · one colour swatch · Size | `light.rs` |
| Uniform (White Furnace) · RT Denoise Feed · Parent Visible | hidden (role) · hidden (role) · already hidden | table |
| Copy Count / Spacing / Columns · Copy Layout · Simulation Speed | Count / Spacing / Columns · Arrangement · Speed | `physics_world.rs:350-355`, `matter_domain.rs:317` |
| Physics · Motion · Density (kg/m³) (rigid body) | Enabled · Body Type · Density + unit kg/m³ | `rigid_body.rs:155-158` |
| Closed −X/+X · Bottom/Top · −Z/+Z | Left/Right Wall · Floor/Ceiling · Back/Front Wall | `matter_domain.rs:307-312` and the FLIP domain twins |
| Initial Fill Height · Particle Capacity · Matter Points per Cell · Grid Budget (M cells) | Fill Height · Max Particles · Particles per Cell · Grid Budget + unit M cells | `matter_domain.rs:305,313`, `gpu_flip_domain.rs:415`, `liquid_fill.rs:163` |
| RT Enabled · RT Reflections · RT Shadows · RT Ambient Occlusion · RT Global Illumination | Ray Tracing · Reflections · Shadows · Ambient Occlusion · Global Illumination (inside the Ray Tracing group) | `render_scene.rs:6771-6832` (no clash with other `render_scene` labels, checked) |
| Base Colour R/G/B · Emission R/G/B · Normal Scale · Cel Bands | swatches (already, material inspector) · Normal Strength · Bands | material primitives, `cel_material.rs:80` |
| Vector X/Y/Z · Strength (m/s²) · Impulse (m/s) · Fire · Center (forces) | Direction X/Y/Z · Strength + unit m/s² · Kick Size + unit m/s · Kick · Centre | recipe JSON |
| **Azimuth Variation** — brightness ripple around the horizon, `1 + v·sin(2·azimuth)` (`bake_equirect_envmap.wgsl` via `bake_equirect_envmap.rs:97`) | Brightness Variation | `bake_equirect_envmap.rs` |
| **Release** — any value above 0 breaks the body into its Pieces once (`manifold-water-rigid/src/physics.rs:1007-1011`) | Break Apart | `rigid_body.rs:161` |
| **Solve Level** — pressure solve on a grid 2^k coarser, 0 = full grid (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)) | Coarse Solve | `gpu_flip_domain.rs:411`, `solve_level_card_v1180.rs` |
| **Max Iterations** — the pressure solve's round cap (default 900, `gpu_flip_domain.rs:412`); the name already says what it caps inside the water's solver controls | Max Iterations (kept) | — |
| **Liveliness** (Matter) — PIC/FLIP velocity blend (`grid_to_matter_body.wgsl:138`) | FLIP Ratio (the industry term) | `matter_domain.rs:322`, `grid_to_matter.rs:83` |
| **Appearance Gain** — below 1 fades coverage, above 1 brightens (`render_scene.wgsl:1027-1033`) | Level | `scene_object.rs:118` |

FLIP Fluids' own terms stay (Sheet Fill Rate). Re-derivation for any row: `rg -n '"<current label>"' crates/ -g '*.rs' -g '*.json'`; a label not found as a literal is composed — find its parts, never guess.

**D10 — Cross-surface gestures.** Right-click on any row in either surface opens the one param menu (`dropdowns.rs:1666`), which gains: **Show on Card** (panel row whose card state is off), **Remove from Card** (card row; disabled on attached rows, D6), **Show in Scene Setup** (card row). Clicking a card section's title (not its fold triangle) opens the Scene Setup panel at the first row of that section. All navigation is by `(GraphParamTarget, ParamId)` against the panel's full surface, never by section string. Rejected: a second menu for scene rows (one menu, WIDGET_TREE section 5b).

**D11 — Lights.** Display name: node `title`, else the handle humanised (`fill_light` → "Fill Light"), else "Light N". The importer writes the GLB light name into `title`; a load rung copies it into `title` for already-imported lights whose intensity stamp's section string differs from the handle. Every light gets a card row; no cap. The Lighting section starts folded when it holds more than eight light rows; the user's fold choice then persists (D7). Rejected: a light-count cap ("never add a size rule").

**D12 — Row sets follow authored values.** Which Look detail rows exist depends on the switch's base value at structural projection, never the per-frame effective value — a driver flickering depth of field does not rebuild the card. Precedent: MATERIAL_INSPECTOR D4 ("layout uses the authored mode/attachments, never the per-frame sample").

**D13 — Migration stays load-time and idempotent** (predecessor section 5 (Decisions — resolved by Peter) item 5, kept). `migrate_scene_exposures` gains: write `ParamSpecDef.scene` for every non-user binding whose primary target is a vocabulary param; keep `card_visible` equal to `role == Performance` for those specs; the light-title rung (D11). It never writes `card_pin`, `folded_sections`, user-added bindings, or authored labels. The one superseded part: section strings no longer decide anything on scene surfaces, so the predecessor's "named collapsed sections per scene item" become display data the projections ignore. Water graphs follow the decision-log rule ("Water changes need no old-project upgrades"): water presets are regenerated, no water rungs.

**D14 — "Use this view" copies the viewport camera into the scene camera** as one undoable command. Free scene camera: copy position, pan, tilt, field of view. Orbit scene camera: Target Height = height where the view ray passes closest to the vertical axis through the origin; Distance = eye to that point; Orbit = `atan2(x, z)` of the eye about the axis; Tilt = elevation angle; Field of View copied. A view ray parallel to the axis keeps the current Target Height. Look-at and loop cameras: button disabled with reason "Not supported for this camera type".

**Consequences, stated honestly:** (1) Two more optional fields per scene exposure in saved projects (about 60 bytes of JSON each before zip). (2) P3 renames labels that automation lanes, OSC docs and Ableton display — Peter will see new names on lanes in old shows after one load. Ids do not change, so nothing unmaps. (3) The descriptor table is a large hand-written data file (about 300 params); its correctness is a coverage test plus Peter's look at the card, not a proof. (4) Four phases touch the most-used surface in the show; P0's flow baseline is the net that tells a new red from an old one.

---

## 3. Design body

### 3.1 Vocabulary — `manifold-foundation/src/scene_surface.rs` (new; shared because both core and ui read it)

```rust
// serde camelCase; Copy + Eq + Hash on every enum. Declaration order is display order.
pub enum SurfaceRole { Performance, Setup, Hidden }
pub enum CardBand { Camera, Scene, Forces, Lighting, Look }
pub enum ControlGroup {
    Camera, Lens, DepthOfField, MotionBlur,          // camera item
    Light, Shadows,                                  // light item
    Transform, Material, Body, Simulation, Whitewater, Source, Look, Modifier, // object item
    Force,                                           // force item
    PhysicsWorld,                                    // physics item
    Environment, Fog, RayTracing,                    // world item
}
pub enum CardLabel { Param, ItemName, GroupName }
pub enum Feature { DepthOfField, MotionBlur }
pub enum Gate { Dim(Feature), Fold(Feature) }
pub enum Unit { Metres, MetresPerSecond, MetresPerSecondSquared, KilogramsPerCubicMetre, Ev, Pixels, MillionCells, Seconds }
impl Unit { pub fn suffix(self) -> &'static str; }   // " m", " m/s", " m/s²", " kg/m³", " EV", " px", " M cells", " s"
pub enum SceneItemKind { Camera, Light, Object(ObjectKind), Force, Physics, World }
pub enum ObjectKind { Mesh, Plane, Water, Model }

pub struct SceneParamDesc {
    pub role: SurfaceRole,
    pub group: ControlGroup,
    pub band: Option<CardBand>,     // Some iff role == Performance
    pub card_order: u8,             // order within (item, band)
    pub card_label: CardLabel,
    pub unit: Option<Unit>,
    pub gate: Option<Gate>,
    pub switch: Option<Feature>,    // this row turns the feature on and off
}
```

`ui` already depends on foundation, so no mirror enums are needed.

### 3.2 The table — `manifold-nodes-scene/src/node_graph/scene_surface.rs` (new)

```rust
pub fn scene_param_desc(type_id: &str, param: &str) -> Option<SceneParamDesc>;
```

Owner: the scene crate, beside `SCENE_VOCABULARY` (`scene_exposure.rs:37-71`). Pure data, a `match` on `type_id` then `param`. Water type ids are matched by string as the vocabulary already does. Rules the table encodes: everything `card_visible_for` lists today is Performance unless D4 says otherwise; everything the stamp filters drop today is Hidden; `look_metadata`'s "Size" becomes `CardLabel::Param` with the platonic radius label edited in P3. Shape precedent: `node_graph/material_inspector.rs` `material_param_role`.

**How core gets it.** Core cannot call the scene crate. The descriptor travels the way `material_role` does: `SceneParamMetadata` (core `scene_exposure.rs:22`) gains `pub scene: Option<SceneParamDesc>`, filled by the provider (`metadata_for_node_type_with_registry`, `nodes-scene scene_exposure.rs:106`) from `scene_param_desc`. The provider drops Hidden params in place of its per-type filter lists. Core's stamp (`:289`) and refresh copy `meta.scene` into the spec; the refresh matches bindings by `(node_id, param)` target, not by id shape, so named fan-outs are covered (D13). Pass 2 (`:529-563`) becomes the descriptor refresh.

Authored controls (recipe JSON, bundled preset JSON, io-migration specs) carry `"scene": {...}` in their own `ParamSpecDef` when they need a role other than the default. Default for a spec with `scene: None`: Performance in band Scene when `card_visible`, else Setup; group `Simulation` for water presets, `Force` for recipe params, otherwise the owning item's first group.

### 3.3 Persisted fields — `manifold-core/src/effect_graph_def.rs`

```rust
// on ParamSpecDef (both #[serde(default, skip_serializing_if = "Option::is_none")]):
pub scene: Option<manifold_foundation::scene_surface::SceneParamDesc>, // stamped + refreshed (D13)
pub card_pin: Option<CardPin>,                                         // user choice only (D7)
pub enum CardPin { Shown, Removed }                                    // serde camelCase

// on PresetInstance (#[serde(default, skip_serializing_if = "Vec::is_empty")]):
pub folded_sections: Vec<String>,   // card section titles the user folded
```

`RowSpec` (`param_surface.rs:140`) gains `unit: Option<Unit>`; `format_param_value` (`param_slider_shared/state.rs:462`) appends `unit.suffix()` after the number. Material roles keep their own field.

### 3.4 Scene items — `manifold-nodes-scene/src/node_graph/scene_items.rs` (new)

```rust
pub struct SceneItem {
    pub kind: SceneItemKind,
    pub id: SceneItemId,
    pub name: String,             // D11 rule for lights; object name as today; force via scene_modifier_display_name
    pub owned_nodes: Vec<NodeId>, // replaces camera_controls / world_controls / object_controls
}
pub enum SceneItemId { Camera, World, Light(NodeId), Object(NodeId), Force(NodeId), Physics(NodeId) }

/// Pure function of the def; built on the UI thread at structural sync,
/// same cadence as SceneVm. Order: Camera, lights, objects, forces, physics, World.
pub fn scene_items(def: &EffectGraphDef, vm: &SceneVm) -> Vec<SceneItem>;
/// Index of the item owning `param_id` by its primary binding; None = authored group.
pub fn owner_of(items: &[SceneItem], meta: &PresetMetadata, param_id: &str) -> Option<usize>;
```

`object_controls`' sharing rules (`projection/scene.rs:100-137`: shared transforms and modifiers belong to the first consumer, materials to every consumer) move into `scene_items` unchanged. `scene_modifier_display_name` moves from `cards.rs:938` into this module so the force name has one home. UI side: `SceneSetupVm` gains `items: Vec<SceneItemVm>` with `kind`, `selection: SceneSelection`, `name`, `parent: Option<u32>`; other fields interior-free. `SceneSelection::Rendering` is deleted (World owns Ray Tracing).

### 3.5 Card projection — `manifold-app/src/ui_bridge/projection/scene_card.rs` (new; replaces `scene_performance.rs`)

```rust
pub(super) fn scene_card_rows(rows: &mut Vec<ParamRow>, inst: &PresetInstance, def: &EffectGraphDef) -> bool;
```

Called at the same site (`cards.rs:540-542`), after modulation is attached. Per row: read `inst.params[id].spec.scene` and `card_pin`; decide on-card = attached (D6) or `card_pin == Shown` or (`role == Performance` and `card_pin != Removed`); drop rows whose `Gate::Fold` feature switch has base value off (D12); set `inactive_reason` for `Gate::Dim` and force no-physics; set `spec.name` from `card_label`; set `spec.section` to the band's section title (D4); set `spec.unit`; sort by (band, item index, card_order, manifest order). User-added rows with no `scene` desc are on-card unless `Removed`. Attached rows with role Setup or Hidden join their item's section in their band (Scene band when the item has none).

### 3.6 Panel — `scene_setup_panel/outliner.rs` and `scene_setup_panel/item_header.rs` (new modules; `scene_setup_panel.rs` only shrinks)

The tree reads `SceneSetupVm.items` (D8). Icons are new `Icon` atlas entries (`icons.rs`): SceneCamera, SceneLight, SceneMesh, ScenePlane, SceneWater, SceneModel, SceneForce, ScenePhysics, SceneWorld, bitmaps added where `native_text::generate_atlas_icons` builds the others. Properties for the selected item: rows owned by the item, minus Hidden (unless attached), grouped by `ControlGroup`, each group a fold section titled by the group's display name. The material inspector keeps its own layout inside the Material group.

### 3.7 Commands and actions

| Gesture | Action (manifold-ui `actions.rs`) | Command / write | Precedent |
|---|---|---|---|
| Show on Card / Remove from Card | `ParamsAction::SetCardPin(GraphParamTarget, ParamId, Option<UiCardPin>)` | `SetCardPinCommand { target: GraphTarget, param_id, pin: Option<CardPin>, previous }` in `manifold-editing/src/commands/graph/card_pin.rs`; writes `preset_metadata.params[..].card_pin` and the manifest spec; undoable | `ToggleNodeParamExposeCommand` (`graph/expose.rs:41`) for target resolution and manifest refresh |
| Show in Scene Setup | `ParamsAction::RevealInSceneSetup(GraphParamTarget, ParamId)` | UI-only: open panel, `reveal_param` | `reveal_clip_response` (`parameter_navigation.rs:56`) |
| Section title click | `ParamsAction::RevealSectionInSceneSetup(GraphParamTarget, ParamId)` (first row of the section) | same | same |
| Section fold | existing fold toggle, plus `MutateProject` write of `folded_sections` | non-undoable | `EffectCollapseToggle` (`dispatch/params.rs:337`) |
| Use this view | `SceneSetupAction::CopyViewportCamera(LayerId)` | `CopyViewportCameraCommand` — one undo unit setting the D14 params | `SetGraphNodeParamCommand::with_previous` batching |

`reveal_param(target, param) -> bool` is `reveal_clip_response` with the clip-trigger requirement removed and the section-string matching (`parameter_navigation.rs:28-50`) replaced by `items` ownership. `UiCardPin` lives in `manifold-ui` (`ui` cannot see core); the app maps it exhaustively. ⚠ VERIFY-AT-IMPL (P5): how `ToggleNodeParamExposeCommand` refreshes `inst.params` after a metadata write — `rg -n "build_param_manifest|reconcile" crates/manifold-editing/src/commands/graph/expose.rs`; `SetCardPinCommand` uses the same path.

### 3.8 Seam brief — what deletes (old → new)

| Old | New | Re-derive with |
|---|---|---|
| `card_visible_for(type, param)` (core `scene_exposure.rs:66`), 3 non-test call sites: core `:289`, `:555`, `scene_performance.rs:416` | `meta.scene` (provider-filled) at stamp + refresh; `spec.scene` in the projection | `rg -n 'card_visible_for\(' crates/ -g '*.rs'` |
| Stamp filter chain in `metadata_for_node_type_with_registry` (`nodes-scene scene_exposure.rs:115-130`) | drop params whose desc role is Hidden | read the function |
| `look_metadata` relabel (`:184-189`) | `ParamDef.label` edit | `rg -n 'look_metadata' crates/` |
| `curate_scene_rows`, `StandardTarget`, `synthetic_id`, `is_retained_extra`, `best_candidate` | `scene_card_rows` | `rg -n 'scene_performance' crates/` |
| `SceneVm.camera_controls`, `SceneVm.world_controls`, `object_controls` | `SceneItem.owned_nodes` | `rg -n 'camera_controls|world_controls|object_controls' crates/` |
| `property_selection_for_row` section matching, `reveal_clip_response` | `reveal_param` by item ownership | `rg -n 'reveal_clip_response|property_selection_for_row' crates/` |
| `SceneSelection::Rendering`, `OutlinerFold("Motion & Physics")`, `"Lighting & Environment"` | D8 groups | `rg -n 'Rendering|Motion & Physics|Lighting & Environment' crates/manifold-ui/src/panels/scene_setup_panel*` |

Compiler-driven: delete the old symbol first; the errors are the checklist. Re-run each command at execution time; if the count differs from what the phase brief recorded, stop and list the new sites first.

### 3.9 Performance

Structural sync only: `scene_items` and `scene_card_rows` run once per data change, on the UI thread, over at most a few hundred rows. No per-frame allocation is added; the per-frame value channel is unchanged. Load migration adds one descriptor lookup per scene binding.

**The plausible-wrong architecture, forbidden by name.** You will want to fix the card by adding cases to the projection — a `match scene_kind` in `scene_card.rs` choosing rows for "physics scenes" and "water scenes". No: every per-scene and per-type fact is a row in `scene_param_desc` or an authored `scene` field; `scene_card.rs` contains no type ids and no param names (negative gate in P2). You will also want a label map in the projection so the card can say "Field of View" without touching the primitive. No: labels change only where they are declared (D9). And you will want to give the panel its own row builder for the new header or the light swatch. No: WIDGET_TREE section 5b (agent contract and enforcement); `no_bespoke_row_infra` fails it.

---

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| INV-S1 Every param of every vocabulary type is classified exactly once | `scene_surface::tests::every_vocabulary_param_is_classified` — iterates `SCENE_VOCABULARY` × `PrimitiveRegistry` `parameters()`, fails on `None` (P1) |
| INV-S2 Scene surfaces read descriptors only from the manifest | negative gate: `rg -n 'scene_param_desc' crates/manifold-app crates/manifold-ui` → 0 (P1 onward) |
| INV-S3 One curation decider | negative gate: `rg -n 'card_visible_for|StandardTarget|scene_performance' crates/` → 0 (P2) |
| INV-S4 Card and panel partition rows identically | `scene_items::tests::card_and_panel_agree_on_owners` over gltfscene, physicsscene, waterdambreak, waterdambreakmatter, oceanscene fixtures (P2) |
| INV-S5 Attached rows always on the card | `scene_card::tests::attached_rows_stay_on_card` — driver, envelope, audio, Ableton, macro, automation, each on a Setup and a Hidden row (written P0, kept green through P2) |
| INV-S6 Refresh never touches user choices | `scene_exposure::tests::refresh_preserves_card_pin_folds_and_user_bindings` plus `migrate_twice_is_noop` (P1) |
| INV-S7 Row set follows base values | `scene_card::tests::look_rows_follow_base_not_effective` (P2) |
| INV-S8 Same label on both surfaces | `scene_card::tests::card_and_panel_labels_match` — every row on both surfaces has equal `spec.name` except `CardLabel::ItemName`/`GroupName` rows, whose text equals the item or group title shown in the panel (P2) |
| INV-S9 No units in scene labels | `scene_surface::tests::no_unit_in_scene_labels` — vocabulary `ParamDef` labels, recipe and bundled scene preset names: no `(` and no unit suffix tokens (P3) |
| INV-S10 Navigation by id only | negative gate: `rg -n 'spec\.section' crates/manifold-ui/src/panels/scene_setup_panel/parameter_navigation.rs` → 0 (P5) |
| INV-S11 No private id schemes on scene surfaces | negative gate: `rg -n 'scene_performance\.' crates/` → 0 (P2) |
| INV-S12 Godfiles do not regrow | `godfile_regrowth::no_register_listed_file_regrows` with ceilings added at current size for `scene_vm.rs`, core `scene_exposure.rs`, `projection/cards.rs`, `scene_setup_panel/material_inspector.rs` (P1), and `scene_setup_panel.rs` lowered to its post-P4 size (P4) |
| INV-S13 Card header title never overlaps its controls | `param_card` geometry test `generator_title_clears_header_controls` (P2) |

---

## 5. Phasing

Phased by layer: P1 data, P2 card projection (the first visible phase runs model → manifest → projection → card pixels end to end), P3 words, P4 panel, P5 gestures between them, P6–P7 two small new affordances. Every child of BUG-pmbd3 (3D scene inspector panel upgrades) lands in a phase:

| Child | Phase |
|---|---|
| .1 scene section, forces and camera layout | P2 (layout), P3 (labels) |
| .2 one intensity row per light | P2 |
| .3 hide controls the scene lacks | P2 |
| .4 right-click show on / remove from card | P5 |
| .5 section name opens the panel | P5 |
| .6 units in the value readout | P2 (field), P3 (labels) |
| .7 card hides mapped rows (bug) | P0 stopgap, P2 class fix |
| .8 camera rows follow camera type | P2 |
| .9 rows keep their section | P2 |
| .10 light colour swatch | P6 |
| .11 right-click Show in Scene Setup | P5 |
| .12 dim controls that do nothing | P2 |
| .13 remember card fold state | P1 (field), P5 (behaviour) |
| .14 Use this view | P7 |

Common to every phase. **Entry:** the status line reads APPROVED, including D2 (blocking; decider Peter — no phase is briefed before it); `git -C "/Users/peterkiemann/MANIFOLD - Rust" log --oneline -1 origin/main`; re-run the section 1 anchors the phase names. **Forbidden in every phase:** type ids or param names in `scene_card.rs` or the panel tree; a label map anywhere outside the declarations; bespoke rows, sliders or swatches (WIDGET_TREE section 5b (agent contract and enforcement)); new `Arc<Mutex>`; per-frame work; silently dropping an unresolved row on load; widening scope into material inspector internals. **GPU:** none in this design; land with `--skip-gpu`. **Reports** carry `Shortcuts taken:` and `Demo artifact:`.

### P0 — Baseline and the live bug
- **Entry:** `ls scripts/ui-flows | rg -c '^scene'` = 36 (stop if not; list the new ones).
- **Read-back:** this doc's sections 1, D6; `scene_performance.rs` whole; `scripts/run_ui_flows.py` docstring. Restate D6 and the forbidden list.
- **Deliverables:** (1) run every scene flow (`.claude/scripts/with-build-lock.sh scripts/run_ui_flows.py scene`) and file one typed bead per red, with the flow name and failing step. (2) `scripts/ui-flows/scene-surfaces-clickthrough.json` (gltfscene) and `-physics.json` (physicsscene): select every outliner item in turn and assert its properties header text and at least one `param_row.*` under it; every failing step is a bead, not a fix. (3) **Named stopgap** for BUG-pmbd3.7 (card hides mapped rows): `is_retained_extra` keeps any row whose modulation, audio, mapping or automation facts are set. Deleted with the function in P2. (4) `attached_rows_stay_on_card` test (INV-S5), shaped to survive P2.
- **Gate:** positive — `cargo nextest run -p manifold-app scene_performance` green; INV-S5 test fails on the pre-fix code (show it red first) and passes after; flow counts reported as `N pass / M fail / 36 total + 2 new`. Negative — none.
- **Demo:** L3 — the two click-through flows; their failures are the bug list. Performer gesture: map a light's position to an LFO, check it stays on the card.
- **Forbidden:** fixing anything the click-through finds (beads only); inventing bugs not observed in a flow run.
- **Test scope:** `-p manifold-app`, plus the flow suite filtered to `scene`.

### P1 — The descriptor layer (data, no visible change)
- **Entry:** P0 landed. Anchors: core `scene_exposure.rs:66`, `:432`, `:598`; nodes-scene `scene_exposure.rs:37`, `:115-130`, `:421`; `effect_graph_def.rs` `ParamSpecDef`.
- **Read-back:** D2, D3, D5, D7, D13, sections 3.1–3.3, 3.8; docs/MATERIAL_INSPECTOR_DESIGN.md section 4.1 (manifest descriptors). Restate why the descriptor is stamped and not read live.
- **Deliverables:** foundation `scene_surface.rs` (3.1); `ParamSpecDef.scene`, `card_pin`, `CardPin`, `PresetInstance.folded_sections` (3.3); `SceneParamMetadata.scene` filled by the provider (3.2); nodes-scene `scene_surface.rs` table covering every vocabulary param; stamping writes `scene` and derives `card_visible`; provider drops Hidden params; `migrate_scene_exposures` refresh per D13 including the light-title rung; the importer writes the GLB light name into the light node's `title` (`gltf_import/scene.rs:407`); `is_retained_extra` reads `spec.scene` where it called `card_visible_for` (the function itself goes in P2); delete `card_visible_for`, the filter chain, `look_metadata`'s relabel; godfile ceilings (INV-S12). Tests: INV-S1, INV-S6, `every_bundled_scene_row_has_one_owner_and_one_role` (D2 kill-pass test, using today's `owned_specs` ownership rule), `scene_surface_round_trip`.
- **Gate:** positive — the tests above; `load_liveschool_live_show_v6` and `liveschool_roundtrip_preserves_addressing_sites` green unchanged; card row ids and order for physicsscene and gltfscene identical before and after (dump diff, zero lines). Negative — INV-S2; `rg -n 'card_visible_for|fn look_metadata' crates/` → 0.
- **Round trip:** save gltfscene and physicsscene, reload through `project_io`, assert `scene` descriptors equal and a driver armed after reload modulates its row.
- **Demo:** none — L1 (deliberately no visible change).
- **Forbidden:** a label field in `SceneParamDesc`; changing any `ParamDef.label` (that is P3); reading the table from `manifold-app`.
- **Test scope:** `-p manifold-foundation -p manifold-core -p manifold-nodes-scene -p manifold-nodes -p manifold-app` (shared vocabulary crosses these five) plus `-p manifold-io --test load_project`.

### P2 — Card on items and descriptors (the vertical slice)
- **Entry:** P1 landed; `rg -n 'scene: Option<' crates/manifold-core/src/effect_graph_def.rs` → 1.
- **Read-back:** D3–D6, D11, D12, sections 3.4–3.5, 3.8; WIDGET_TREE section 5b (agent contract and enforcement). Restate the forbidden projection cases.
- **Deliverables:** `scene_items.rs` (3.4) with `owner_of`; `SceneVm.camera_controls`/`world_controls`/`object_controls` replaced; `scene_card.rs` (3.5) replacing `scene_performance.rs` (deleted, with the P0 stopgap); `RowSpec.unit` and the value-readout suffix; force rows from recipe `scene` fields (Kick, Kick Size); light rows named per D11; camera rows per camera type; Look switches; no-physics dim; card title ellipsis (INV-S13); the water preset builder (`manifold-nodes-water/src/presets/gpu_flip.rs`) gives its mesher controls an authored Setup `scene` field and the water presets are regenerated; `oceanscene` fixture (`physics_scene("Ocean", "Ocean")` in `fixtures.rs`). ⚠ VERIFY-AT-IMPL: where a recipe param's host `ParamSpecDef` is minted when a force is applied — `rg -n 'BindingTarget::SceneModifier \{' crates/manifold-editing crates/manifold-core -g '*.rs'`; the host spec copies the recipe's `scene` field there.
- **Gate:** positive — INV-S4, S5, S7, S8, S13 tests; `scene_card::tests::{orbit_card_order, free_camera_card_order, physics_card_bands, water_card_bands, matter_card_bands, three_lights_three_named_rows, absent_dof_builds_no_row}`. Negative — INV-S3, INV-S11; `rg -n '"node\.|"orbit"|"intensity"' crates/manifold-app/src/ui_bridge/projection/scene_card.rs` → 0.
- **Demo:** L3 — `scripts/ui-flows/scene-card-bands.json` on physicsscene (assert section titles Camera, Physics, Lighting, Look in order, a "Count" row, no "Main Light"), `scene-card-free-camera.json` on oceanscene (Pan, Tilt present; no Distance), `scene-card-dof-switch.json` on gltfscene (click Depth of Field on, assert F-Stop appears; off, assert gone). Performer gesture: drag Orbit and Field of View, then fire a force's Kick.
- **Forbidden:** per-scene-type layout code; keeping `scene_performance.rs` beside the new module; reading `spec.section` for ownership.
- **Test scope:** `-p manifold-nodes-scene -p manifold-app -p manifold-ui`; flows `scene-card`, `scene-setup`.

### P3 — The naming pass (labels and units)
- **Entry:** P2 landed. Re-derive each D9 row with its command and record the hit count.
- **Read-back:** D1, D9; INV-S8, INV-S9. Restate: labels change where declared, nowhere else.
- **Deliverables:** every D9 label edit in `ParamDef`s, recipe JSON and bundled scene presets; one new idempotent io rung relabelling the cinematic-tail spec `scene_cinematic_tail_v1130.rs:316` hard-coded ("Max Blur (px)" → "Max Blur"); the water card label "Solve Level" changes in the regenerated water presets and in the literal `solve_level_card_v1180.rs` writes, with no new water rung (decision log, water upgrades); "Max Iterations" is kept; units moved to `unit` (in the table for primitives, in the authored `scene` field for JSON specs); INV-S9 test; regenerate water presets (no water rungs, D13).
- **Gate:** positive — INV-S8, INV-S9; `migrate_twice_is_noop`; Liveschool tests unchanged. Negative — `rg -n '"(FOV Y|Look Y|Exposure \(EV\)|Light Size|Copy Count|Simulation Speed|Initial Fill Height|Particle Capacity|RT Enabled|Appearance Gain|Azimuth Variation)"' crates/ -g '*.rs' -g '*.json'` → 0 outside `migrations/` history rungs.
- **Round trip:** an old-label gltfscene project saved before P3 loads with new labels on card, panel and automation lane; its OSC address and Ableton mapping still resolve.
- **Demo:** L3 — `scene-naming.json` on gltfscene: assert "Field of View", "Target Height", "Exposure" and value text ending " EV". Performer gesture: read the card at arm's length — no truncated label on a default-width card.
- **Forbidden:** a display-name map; renaming any `ParamDef.name` (the wire key).
- **Test scope:** `-p manifold-nodes-scene -p manifold-water-gpu-flip -p manifold-water-gpu-mpm -p manifold-water-rigid -p manifold-nodes-image -p manifold-io -p manifold-app` (labels are declared across these crates).

### P4 — Panel tree and item header
- **Entry:** P2 landed (items exist). `wc -l crates/manifold-ui/src/panels/scene_setup_panel.rs` recorded.
- **Read-back:** D8, section 3.6; SCENE_SETUP_PANEL_DESIGN.md section 7 (Decided — do not reopen) for outliner verbs still standing; P0's bead list for this panel.
- **Deliverables:** `outliner.rs`, `item_header.rs`; icons; per-group add buttons with the Objects menu; Physics group; World with Ray Tracing; `SceneSelection::Rendering` deleted; stats line deleted; item header with rename, kind, and copy and delete per D8; force item header owns the name, the force card's own header keeps enable, targets and trigger chips only; properties grouped by `ControlGroup`, Hidden rows omitted unless attached (D6); `scene_setup_panel.rs` ceiling lowered. Rewrite every scene flow that asserted the old tree.
- **Gate:** positive — `scene_setup_panel::tests::tree_groups_in_fixed_order`, `add_buttons_one_per_group`, `header_names_item_once`; full scene flow suite green with the P0 count reconciled (every flow on disk accounted for). Negative — `rg -n 'Motion & Physics|Lighting & Environment|SceneSelection::Rendering|\\u\{1F4F7\}|\\u\{25A0\}' crates/manifold-ui/src` → 0.
- **Demo:** L3 — `scene-panel-tree.json` on gltfscene and physicsscene: assert group order, select a light, assert header text equals its readable name once. Performer gesture: add a light from the Lights "+", rename it, see the new name on its card row.
- **Forbidden:** a new row or header widget outside the shared builders; keeping the old add row "for now".
- **Test scope:** `-p manifold-ui -p manifold-app`; scene flows.

### P5 — Cross-surface gestures and fold persistence
- **Entry:** P2 and P4 landed.
- **Read-back:** D6, D7, D10, section 3.7; `dropdowns.rs:1666-1731`; `parameter_navigation.rs` whole.
- **Deliverables:** scene panel rows emit `ParamLabelRightClick`; menu items Show on Card, Remove from Card (disabled on attached rows), Show in Scene Setup; `SetCardPinCommand` with undo; section title click → panel; `reveal_param`; `folded_sections` persistence and the D11 default fold; delete `reveal_clip_response`, `property_selection_for_row`, the `SceneSetupExposeParam` action and arm.
- **Gate:** positive — `card_pin::tests::{pin_unpin_undo_redo, remove_disabled_on_mapped_row}`, `reveal_param_selects_owning_item_for_light_object_force_world`. Negative — INV-S10; `rg -n 'SceneSetupExposeParam|reveal_clip_response' crates/` → 0.
- **Round trip:** pin a material colour row and unpin Orbit, fold Lighting, save, reload: pins and fold hold, a mapping on the pinned row still drives it.
- **Demo:** L3 — `scene-pin-roundtrip.json` (right-click Roughness in the panel → Show on Card → assert a card row; right-click it on the card → Remove from Card → assert gone), `scene-card-to-panel.json` (click the Lighting title → assert the panel selects a light). Performer gesture: mid-set, pin an object's Y position to the card and drive it.
- **Forbidden:** a second context menu; navigation by section string; pins stored in `card_visible` or `user_added`.
- **Test scope:** `-p manifold-editing -p manifold-ui -p manifold-app`.

### P6 — Light colour swatch on the card
- **Entry:** P5 landed. `rg -n 'ColourSwatch' crates/manifold-ui/src` recorded.
- **Read-back:** WIDGET_TREE_DESIGN.md section 5b (agent contract and enforcement) recipe; `material_inspector.rs:853-920`.
- **Deliverables:** the swatch moves into the shared row host (recipe steps 1–5); `RowRole::ColourSwatch` payload widens from `MaterialColour` to a `ColourGroup` enum adding `Light`; the light intensity row carries `rgb_members` of the light's colour params; the material inspector uses the shared swatch.
- **Gate:** positive — dispatch test: swatch gesture on a light row emits one undoable RGB write to the three colour ids. Negative — `rg -n 'fn .*swatch' crates/manifold-ui/src/panels/scene_setup_panel/material_inspector.rs` → 0; `no_bespoke_row_infra` green.
- **Demo:** L3 — `scene-light-swatch.json` on gltfscene: pick a colour on a light row, assert the row's colour value changed. Performer gesture: recolour the key light from the card.
- **Forbidden:** a card-only swatch copy.
- **Test scope:** `-p manifold-ui -p manifold-app`.

### P7 — Use this view
- **Entry:** P4 landed. ⚠ VERIFY-AT-IMPL: where the app's live viewport camera pose is readable from the UI thread (`rg -n 'SceneViewportPass' crates/manifold-app/src`); `viewport_session.rs:226` is the headless twin.
- **Read-back:** D14; `viewport_session.rs:1-27`.
- **Deliverables:** "Use this view" button in the Camera item header; `CopyViewportCameraCommand`; the orbit mapping as a pure function `orbit_from_view(eye, forward, current_look_y) -> OrbitParams` with tests.
- **Gate:** positive — `orbit_from_view` round trip: orbit params → eye/forward → back, equal within 1e-4; free-camera copy exact; one undo restores all params. Negative — none.
- **Demo:** L3 — `scene-use-this-view.json` on gltfscene: orbit the viewport, click the button, assert Orbit changed. Performer gesture: fly the viewport to a framing, click, then perform from that framing on the card.
- **Forbidden:** writing the viewport camera into the project on every move.
- **Test scope:** `-p manifold-nodes-scene -p manifold-editing -p manifold-app`.

---

## 6. Decided — do not reopen

1. Card = performance, panel = setup; same items, same names (D1).
2. One descriptor table in the scene crate, stamped into `ParamSpecDef.scene`, refreshed on load; projections read only the manifest (D2). Labels live only where declared.
3. One scene item list; ownership by primary binding; both surfaces use it (D3).
4. Card bands Camera, Scene, Forces, Lighting, Look; no "Main Light"; no placeholders (D4).
5. Hide absent, dim inactive (D5). Attached rows always shown (D6).
6. Pins in `card_pin`, folds in `folded_sections`; refresh never writes them (D7).
7. Panel groups Camera, Lights, Objects, Forces, Physics, World; one add button per group (D8).
8. The D9 word list, including Brightness Variation, Break Apart, Coarse Solve, Max Iterations, FLIP Ratio, Level.
9. One right-click menu; navigation by param id (D10).
10. Every light gets a row; Lighting folds by default past eight (D11).
11. Row sets follow base values (D12).
12. Load-time idempotent migration kept; water gets no rungs (D13).

## 7. Deferred

- **Master force strength** — Peter: "sounds like a lot of new work? Let's focus on what we have already." Revive when Peter asks for one fader over all forces.
- **Orbit camera lateral pan (target X/Z)** — new primitive params. Revive when Peter asks for pan on an orbit camera live.
- **Light duplicate** — no command exists. Revive when Peter asks to copy a light.
- **More performance rows for look-at and loop cameras** — revive when a show uses one of them.
- **Card pins on non-scene generator and effect cards** — revive when Peter asks for pins outside scenes.
- **Compact three-value rows (position X/Y/Z on one line)** — predecessor section 5 (Decisions — resolved by Peter) item 4 parked it; still parked.
