# Scene Surfaces — the scene card and the Scene tab as one system

**Status:** SHIPPED · 2026-10-14 · all phases P0–P10 on main; kept as the contract for the param lookup (D2, D22–D24), the scene table and D15. Owed: BUG-tly4a (slider range popover).
**Lifecycle: contract**
**Prerequisites:** none.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs) through section 6 (Seam briefs) before starting any phase.

<!-- index: Scene card (performance) and Scene tab in the inspector (setup) as two views of one scene item list; one live param lookup (projects save only id, value and user choices); slider range vs hard limit; naming pass, pins, navigation; one scene per layer. -->

**The governing insight: the card and the panel already share one row widget, one edit path and stable param ids. What they do not share is the answer to three questions — what is this control called, which scene item owns it, and does it belong on stage. Today eight places answer those questions, and the answers are copied into every saved project, where they go stale.** This design answers them once, in one lookup in code that every reader uses, the way a DAW reads a plugin's parameter names at load. A saved project keeps only each control's id, its value and the user's own choices. The card and the panel become two filters over the same rows: the card shows the performance role, the panel shows the selected item. After D16 the panel is the Scene tab of the inspector, not a separate dock; in this doc "the panel" and "the Scene tab" name that one surface.

Peter, 2026-10-13, on the split and the order: "Camera first, most useful controls out of the box that get used frequently." On camera rows: orbit, translate, focus, distance and FOV "are actually all very useful on the inspector card." On lights: "'Main Light' is confusing and doesn't mean anything, multiple lights are possible in a scene." On pins: "YES love this!!! also needs right click to remove from card." On the panel tree: "Physics should be it's own thing, not under world." On master force strength: "sounds like a lot of new work? Let's focus on what we have already."

**Stage translation.** The card is what Peter grabs mid-set. After this design, a scene's card opens with the camera, then the scene's own toys (box count, water speed, the preset's macros), then forces with a Kick button each, then one fader per light named after the light, then depth of field and motion blur as two switches. A mapped control never disappears. Setting up the scene happens in a Scene tab beside the clip, layer and master tabs in the inspector, so there is no second panel to open, dock or lose; clicking a card section title jumps to that item in the Scene tab. One layer holds one scene, so the tab never has to guess which scene it shows. Right-click moves anything from the Scene tab onto the card and back, and sets how wide its slider is. A slider's end is no longer a wall: drag or type past it and the value keeps going until the real limit. A label fixed in code shows up in every old show on next load, on the card, the panel, the automation lanes and the tools. Nothing on the card is a placeholder for a control the scene does not have.

**Binding constraints** (docs/DESIGN_AUTHORING.md section 1 (the intake)): *hot path* — none; the lookup runs when a manifest is built (load, structural edit), both projections run on the structural sync path, and the per-frame value channel is untouched (id join, WIDGET_TREE INV-6). *Thread residency* — projection on the UI thread from `Arc<Project>` snapshots; every mutation through `EditingService` or `MutateProject`. *Time model* — not involved. *Persistence* — yes, and the largest change here: saved projects stop carrying names, ranges and roles for any control the lookup can describe, and carry the user's choices (rename, slider range, curve, invert, section, pin, fold) per control instead. The canonical fixture (`Liveschool Live Show V6 LEDS.manifold`) is pre-3D, its controls already track their templates, and it must load and save byte-identically. Param ids never change, so mappings, automation lanes, OSC addresses and macros survive every phase. *Performance surface* — the card is the performance surface; MIDI, OSC, Ableton and automation rows are part of every phase that touches it.

Companions: docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md (the predecessor; its section 5 (Decisions — resolved by Peter) binds here except where D2 and D13 say otherwise). docs/archive/PARAM_STORAGE_DESIGN.md (bundled params already track their template; D2 here finishes that model for graph-local exposures). docs/WIDGET_TREE_DESIGN.md section 5b (agent contract and enforcement) (the row recipe every row change follows). docs/MATERIAL_INSPECTOR_DESIGN.md (its D3 classification table is the shape of the scene table here). docs/SCENE_SETUP_PANEL_DESIGN.md (outliner and structural verbs, still standing).

---

## 1. Audit — what exists (verified 2026-10-13)

Extend, don't redesign. Every row below was read in the tree on 2026-10-13.

| Piece | Where | State |
|---|---|---|
| Shared row model (`ParamRow`, `RowSpec` with `disabled`, `inactive_reason`, `section`) | `crates/manifold-ui/src/param_surface.rs:140-176`, `:299-322` | exists. Dim with reason renders on both surfaces: `param_slider_shared/builders.rs:1664-1673`, `:2381-2384` |
| Card projection, one manifest walk | `manifold-app/src/ui_bridge/projection/cards.rs:355-542`; modulation attached at `:519-538`, scene curation called after it at `:540-542` | exists |
| Scene card curation | `projection/scene_performance.rs:26-195` — twelve hard-coded `StandardTarget`s (`:60-169`), relabels and re-sections rows (`:227-228`), renames every kept extra to "Scene Controls" (`:186-188`) | replaced (P4) |
| Placeholder rows with synthetic ids `scene_performance.{key}` | `scene_performance.rs:232-272`, `:428-438` | deleted (P4) — a private id scheme |
| Kept-extra filter ignores modulation | `scene_performance.rs:394-426` | the live bug BUG-pmbd3.7 (card hides mapped rows); fixed P0 |
| Card-visibility table | core `scene_exposure.rs:66-111` `card_visible_for` | replaced by the scene table (P3) |
| Stamp-time param filters (which params get exposed at all) | `manifold-nodes-scene/src/node_graph/scene_exposure.rs:115-130`; liquid dial lists `manifold_core::liquid_domain::liquid_dial_params` | replaced by `SurfaceRole::Hidden` (P3) |
| Section naming | `nodes-scene scene_exposure.rs:421-462` `section_name_for_node`; persisted into `ParamSpecDef.section`, never refreshed on load | scene surfaces stop reading it (P4, P6b) |
| Bundled params already follow the DAW model | `manifold-core/src/params.rs:28` `ParamOrigin::Bundled` ("tracks the template"); the wire entry is value, exposed, base, calibration, clip trigger (`effects/instance_serde.rs:47-63`) | extended to every control the lookup describes (P1) |
| Where graph-local copies come from | generators with a per-instance graph take `meta.params` as "the full ordered descriptor authority" (`instance_serde.rs:272-300` `gather_known_params`); graph-local auto exposures inline their spec (`:135-145`); `meta.params` is re-derived from the manifest on save (`:155-196`) | the copies D2 removes |
| Load refresh of the copies | core `scene_exposure.rs:432-690` (passes 1–4); pass 2 rewrites `card_visible` (`:529-563`); pass 4 rewrites label, range, angle and value labels of exact auto-stamps, because "range/label tweaks in code never reached already-saved projects" (`:598-687`). Called on project load (`manifold-app/src/project_io.rs:538`) and bundled-preset cache build (`manifold-nodes/src/bundled_presets.rs:86`) | passes 2 and 4 deleted (P2); passes 1 and 3 (structural) stay |
| Scene-scaled slider ranges | core `scene_exposure.rs:381` `scene_scaled_range(type_id, param, radius)`, radius from the saved `meta.scene_bounds`; written into copies at import (`nodes-scene gltf_import/scene_scale.rs:170-196`) and again by pass 4 | moves into the lookup (P1) |
| Node param metadata source | core `scene_exposure.rs:22-48` `SceneParamMetadata` and `SceneExposureMetadataProvider` (per-type metadata built from `PrimitiveRegistry`); passed as an argument to the migration, not installed globally | becomes the lookup's node half (P1) |
| Preset template registry | core `preset_definition_registry.rs:340` `try_get` over the global `PRESET_DEFINITIONS`, filled at startup; a manifest built before it is filled stashes its wire entries in `pending_wire` and re-joins later (`effects/instance.rs:375` `reconcile_manifest`); live twin `refresh_manifest_from_graph` (`:413`) | becomes the lookup's template half (P1) |
| One value clamp, one range | `params.rs:167` `constrain_to_range(value, min, max, wraps)`, called by automation (`manifold-playback/src/automation.rs:267`, `:361`, `:454`) and modulation (`modulation/composition.rs:214`) with `spec.min/max`; the slider drag clamps to the track (`param_slider_shared/row_host.rs:342`, `:373`, `:499`) | split into slider range and hard limit (P8) |
| User range override | calibration: `CalibrationWire` min, max, curve, invert (`instance_serde.rs:34`); set by `EditParamMappingCommand` (`manifold-editing/src/commands/effects.rs:1263`, `calibrated = true` at `:1399`) through `BindingMappingEdit::apply_to_manifest_spec` (`:1146-1185`), which writes the user's label, min, max, curve, invert and section straight into `spec` | becomes `ParamChoices` (P2) |
| Card-to-node reshape | `manifold-node-engine/src/runtime/build.rs:383-411`: invert and curve normalise the card value over `spec.min/max`; standalone builds read `meta.params` copies (`:386-395`) | reads the declared slider range (P2) |
| OSC addresses | `preset_definition_registry.rs:401-422`: `/master/{prefix}/{param_id}` and `/layer/{layer_id}/{prefix}/{param_id}`; the router keys targets by `p.id()` (`manifold-playback/src/osc_param_router.rs:91`, `:150`, `:258`). `ParamSpecDef.osc_suffix` is only linted for duplicates (`node-engine validate.rs:524-537`) | **id-based already — a rename never moves an address** |
| `project_tool` | `manifold-app/src/bin/project_tool.rs:29-33`: no registry, so it edits `project.json` as raw JSON and validates through `load_project_from_json` (`:178`), whose typed load drops what it cannot resolve. `manifold-io` cannot gain the registry: `manifold-nodes` → `node-engine` → `playback` → `io` is already a dependency chain, so the reverse edge is a cycle | moves to `manifold-app` (P1), like `graph-tool` (`manifold-app/Cargo.toml:16-17`) |
| Primary-binding ownership rule (fan-outs keep one owner) | `projection/scene.rs:229-248` `owned_specs`; tests `:494-512`, `:957-1012` | becomes the ownership rule for both surfaces |
| Scene discovery | `nodes-scene node_graph/scene_vm.rs:113-138` (`objects`, `lights`, `camera_controls`, `world_controls`); light name is the raw handle at `:1797` | extended with `scene_items` (P4) |
| Panel item selection | `manifold-ui/src/panels/scene_setup_panel.rs:713-723` `SceneSelection` (Object, Light, Force, Camera, World, Physics, Rendering) | exists |
| Panel outliner | `scene_setup_panel.rs:1787-1976`: Camera loose row, "Lighting & Environment" fold (World + lights), Objects fold with "Import Model…", "Motion & Physics" fold with "+ Force", Rendering loose row, then a separate add row (`scene_setup_actions.rs` `build_add_action_row`); emoji glyphs, one `■` for every object | rebuilt (P6b) |
| Panel stats line | `scene_setup_panel.rs:1652-1675` | deleted (P6b) |
| Card → panel navigation | `scene_setup_panel/parameter_navigation.rs:56-130` `reveal_clip_response` (requires a clip trigger, `:64-72`); lights and world matched by section string `:28-50` | generalised to ids (P7) |
| Card section header | `param_card/render.rs:1140-1166`, one fold button; fold state UI-local, never saved (`param_card/mod.rs:388-396`) | extended (P7) |
| Param right-click menu | `manifold-app/src/ui_root/dropdowns.rs:1666-1731`; scene panel rows never emit it (comment `:1716`) | extended (P7, P8) |
| Panel expose action | `ui_bridge/project.rs:1223-1255` `SceneSetupExposeParam` — no-op when already exposed; nothing in `manifold-ui` sends it | superseded by pins (P7) |
| Non-undoable view-state write precedent | `ui_bridge/dispatch/params.rs:337-374` (`EffectCollapseToggle` via `MutateProject`) | precedent for fold persistence |
| RGB swatch | only in the panel's material inspector: `scene_setup_panel/material_inspector.rs:853`, `:911`; `RowRole::ColourSwatch(MaterialColour)` at `param_surface.rs:409`; `rgb_members` set only by `projection/material.rs:432` | moved into the shared row host (P9) |
| Atlas icon vocabulary | `manifold-ui/src/icons.rs:19-41` | extended with scene item icons (P6b) |
| Item commands | `RemoveSceneObjectCommand` (`commands/graph/scene.rs:980`), `DuplicateSceneObjectCommand` (`graph/scene/duplicate.rs:18`), `RemoveSceneLightCommand` (`scene.rs:1847`), `DeleteSceneModifierCommand` / `DuplicateSceneModifiersCommand` (`graph/modifier_stack.rs:167`, `:275`); no light duplicate | exist |
| Viewport camera | `nodes-scene node_graph/viewport_session.rs:1-27` (isolated from the project), camera at `:226`, moves at `:256-274` | read by P10 |
| `ParamDef` | `manifold-node-engine/src/parameters.rs:233-253`: name, label, ty, default, range, enum_values; 1,891 struct literals in 339 files (`rg -c 'ParamDef \{' crates/`) | labels edited in place (P5); no new fields |
| Flow harness | 22 scene flows under `scripts/ui-flows/` (`ls scripts/ui-flows | rg -c scene`), scenes mapped in `manifest.json:176-266`; fixtures `ui_snapshot/fixtures.rs:28-104` (gltfscene, physicsscene, waterdambreak, waterdambreakmatter, fluidauthoring, mushroomscene); no free-camera scene | exists; `oceanscene` added (P4) |
| Godfile ceilings | `manifold-app/tests/godfile_regrowth.rs:116` caps `scene_setup_panel.rs` at 4300; `scene_vm.rs` (3300), core `scene_exposure.rs` (2825), `material_inspector.rs` (2320), `cards.rs` (1935) are uncapped | ratcheted (P3) |

**Where a control's name and place are decided today** (the disease, counted): (1) `ParamDef.label`; (2) the stamp filters in `nodes-scene scene_exposure.rs:115-130` plus `liquid_dial_params`; (3) `card_visible_for`; (4) `section_name_for_node`, and creation sites that pass their own section strings (`projection/scene.rs:199-200` says creation and load name the same kind differently); (5) the saved copies in `ParamSpecDef` and `BindingDef.label`, kept roughly true by pass 4; (6) `scene_performance.rs`; (7) `look_metadata` relabelling radius to "Size" (`nodes-scene scene_exposure.rs:184-189`); (8) the panel's section-string matching in `parameter_navigation.rs:28-50`. The lead's count of four was low.

**Agreed-plan claims the code contradicts** (checked, so phases don't inherit them):
- "Parent Visible → hidden": already hidden. `parent_visible` is never stamped (`nodes-scene scene_exposure.rs:124`); the water family shows it as an authored control labelled "Visible" (`manifold-nodes-water/src/presets/gpu_flip.rs:795`), which stays.
- White Furnace (`node.bake_environment` `uniform`, `bake_equirect_envmap.rs:219`) and RT Denoise Feed (`render_scene.rs:6847`) are visible today in the panel, so hiding them is a real change.
- "Light colour swatch is one wire" (BUG-pmbd3.10 (light colour swatch)): the card has no swatch renderer; it lives in the panel's material inspector. Real work, P9.
- Light names: the readable GLB light name never reaches `LightRow.name`, which reads the handle (`scene_vm.rs:1797`); the importer builds the readable `label` at `gltf_import/scene.rs:406` and passes it only as a section string (`:461`), never as a title.
- Forces are not on the card today: `SceneModifier` bindings are dropped unless user-added (`scene_performance.rs:400-402`, `:422`). The nine duplicate force rows in BUG-pmbd3.9 (rows keep their section) only happen with user-added force bindings.
- Some labels in the naming table are not `ParamDef` literals: "Coarse Solve" and "Max Iterations" are authored labels hard-coded in io migrations (`manifold-io/src/migrations/solve_level_card_v1180.rs:56,70`, `pressure_cap_card_v1190.rs:66,80`), "Max Blur (px)" also at `scene_cinematic_tail_v1130.rs:316`; force labels live in recipe JSON (`manifold-nodes/assets/scene-modifier-presets/UniformForce.json:42-56`). After D2 the io-migration labels are inert: those bindings target node params, so their label comes from the lookup.
- "The panel groups by `sections_for_doc_ids`": that function is now `sections_for_nodes` (`projection/scene.rs:205`); ownership is already by binding target node, section strings only label sub-groups.
- "OSC addresses come from labels": they don't. Addresses are type prefix, layer id and param id; labels never enter them (table above).

---

## 2. Decisions

**D1 — The card is the performance surface; the Scene tab (D16) is the setup surface. Same items, same names.** Every row on the card is a row the panel also shows, under the same label, except the two card label rules in D4 (light name, group name), whose words the panel also displays. Rejected: a card with its own wording ("Horizontal Angle" beside the panel's "Orbit") — that is the "Intensity" vs "Sun Intensity" split the predecessor named as a bug.

**D2 — One live lookup; a saved project keeps only each control's id, its value and the user's choices.** This is the decided architecture. Name, unit, slider range, hard limit and surface role come from one lookup in code, and every reader uses it: the card, the panel, automation lanes, OSC, `project_tool` and the content thread. A DAW does the same with plugin parameter names: it reads them from the plugin at load and never trusts a name saved in the song.

*The lookup.* `manifold_core::param_lookup::describe(key) -> Option<ParamSpecDef>` (section 3.2). It has three keys, and each one reads a source that exists today:
- `Template` (preset type, param id) reads `PRESET_DEFINITIONS`, which holds bundled presets, recipes and registered user presets. This is what bundled params already do.
- `Node` (type id, param, scene radius) reads the node param provider: today's `SceneExposureMetadataProvider`, built from `PrimitiveRegistry` and installed once at startup instead of being passed as an argument. Scene-scaled ranges come from `scene_scaled_range` with the radius taken from the saved `meta.scene_bounds`, which is a fact about the imported model, not a copy. Scene params also get their scene descriptor from the table in section 3.3.
- `Modifier` (recipe type, param) covers `SceneModifier` targets and reads the recipe's template.

A control resolves by the first key that answers: template first, then its primary binding target. `Composite` targets, and graph-local params that no template or node declares, are *authored*. The user built them, so their whole spec is the user's choice and is saved in full. That matches the rule rather than breaking it.

*The join.* The lookup is read in exactly one place: `build_param_manifest` / `gather_known_params` (`instance_serde.rs:272-356`), where each saved control is joined with its description. That function already runs at load, re-runs when the registry fills (`reconcile_manifest`) and runs on structural edits (`refresh_manifest_from_graph`). The joined `Param.spec` is what every reader already reads, so the card, the panel, automation lanes, OSC ranges and the content thread change nothing for names (section 3.8 lists each one). Readers that go around the manifest to saved copies are the work: they are listed with file and line in section 3.8 and moved in P2.

*What is saved per control:* id, value, base, exposed, clip trigger (all as today), plus `ParamChoices` (section 3.4): rename, slider range, curve, invert, section, card pin. Fold state is saved per card. `meta.params` keeps one entry per control, for order only. An entry the lookup describes is saved as `{"id": …}` and nothing else.

*How WIDGET_TREE INV-1 changes.* The words "rows are sourced ONLY from the manifest walk — no registry re-reads, no hand tables" stay true. What changes is what the manifest is. Each entry is the saved control (id, value, choices) joined with the live lookup, inside core, when the manifest is built. Projections still read only the manifest and stay pure functions of it, so WIDGET_TREE D8a's registry-free projection tests still work: they build manifests directly. P2 adds that one sentence to WIDGET_TREE INV-1.

*Old projects.*
- On load, a saved name, range, angle flag, value labels, `card_visible` or section copy on a control the lookup describes is ignored. Which controls the lookup names: D24.
- One case is converted instead. A user-added exposure (`user.*`) whose saved label or range differs from the lookup becomes a rename or slider-range choice, once. That is the only place a user rename could live in old files.
- Auto stamps lose nothing. Pass 4 already overwrites their saved labels and ranges on every load.
- A drawer rename of a bundled preset's curated control in an old per-instance graph goes back to the template's label. This is rare, and it is the same rule pass 4 already applies to auto stamps.
- The calibration block becomes slider range, curve and invert. It keeps its wire key and shape, so Liveschool saves byte-identically.
- On save, the ignored copies are dropped.
- A control the lookup cannot describe right now keeps its value, choices and saved copy, and is saved back word for word. This covers a removed primitive, a project-local preset not registered yet, or any tool without the registry, and it is the `pending_wire` path that BUG-036 (manifest dropped when a template is missing) already proves. Nothing is dropped because a lookup failed.

*OSC.* Addresses are `/layer/{layer_id}/{prefix}/{param_id}` and `/master/{prefix}/{param_id}`, and routing is keyed by param id. No label enters an address, so renames, including the whole P5 naming pass, never break a performer's controller. Only the meaning of a normalised 0–1 message depends on the range, and it maps over the slider range (D15).

*`project_tool`.* It moves from `manifold-io` to a `manifold-app` binary and installs the registry and lookup at start, the same way `graph-tool` does. Its raw-JSON edit path stays. Its validation load now resolves controls instead of dropping them, and `info` can print names. This is a move, not a blocker: `manifold-io` cannot depend on `manifold-nodes` (section 1 names the cycle).

No hard blocker was found. Three things carry risk and each is gated in P1 or P2:
- The lookup is a process-wide install. It is read-only after startup, with no lock.
- A file saved after P1 holds id-only entries. An older build would show those controls as placeholders. Bump the project format version (⚠ VERIFY-AT-IMPL (P1): whether an older build refuses a newer version or loads it; `rg -n 'CURRENT_VERSION|format_version' crates/manifold-io/src`).
- The importer's authored labels have to become table rules (light names → `CardLabel::ItemName`, environment → `CardLabel::GroupName`) or primitive labels. ⚠ VERIFY-AT-IMPL (P1): `rg -n 'label' crates/manifold-nodes-scene/src/node_graph/gltf_import/*.rs`.

Rejected alternatives, priced (DESIGN_AUTHORING section 4 (alternatives)):
- *The half version: live for the card, stamped for everyone else.* Projections read the lookup; automation lanes, OSC, tools and the content thread keep reading saved copies. Cheap, about two files. Every fact then has two homes, live on the card and stale on the lane, and pass 4 has to stay to keep the second home roughly true. Rejected: two homes.
- *Stamped copies refreshed on load* (this doc's first D2). One home in code, but a copy on disk that only a refresh pass keeps true. Pass 4 exists because those copies went stale ("f-stop sliders stuck at 0.5–1000", core `scene_exposure.rs:601-604`), and it still cannot reach named fan-outs, authored io-migration labels, or the scene-scaled ranges written at import. Every new descriptor field needs its own refresh rule and its own staleness test. Rejected: copies on disk are the disease. Not kept as an interim step either, because nothing blocks the full version.
- *Descriptor fields on `ParamDef`.* Puts the fact inside the primitive, which is what Peter pictured. Costs a field on 1,891 struct literals in 339 files for facts that matter to about 30 types, and the graph editor would carry scene-only vocabulary. Rejected on cost. The scene table sits beside the vocabulary and feeds the lookup.
- *A trait method per scene primitive.* Thirty implementations across six crates (scene, rigid, liquid, FLIP, MPM, whitewater) for one policy, and no single place to audit coverage. Rejected: the coverage test needs one table to iterate.
- *A saved card layout per project (Resolume-style row list).* New scenes still need a default generator — the role table again — and adding a light would not add its row without re-running it, so two systems. Rejected.

Kill-pass. Strongest case against: when the lookup is missing, a control has no readable name or range, where a stamped copy would have stayed readable. The three ways that happens are a primitive param renamed in code, a registry-less tool, and a project loaded before its local presets register. Answers, in that order:
- A param rename already goes through the alias map (`resolve_param_alias`), which `build_param_manifest` applies before the join.
- Registry-less tools move to `manifold-app`.
- Unresolved controls keep their saved entry word for word and re-join when the registry fills (`reconcile_manifest`).

The cheapest catching tests are P1 gates: `every_bundled_and_fixture_param_resolves` (every control of every bundled preset and every `ui_snapshot` fixture resolves, none falls to authored by accident) and `unresolved_control_round_trips_verbatim`.

**Zero-new-systems count** (DESIGN_AUTHORING section 3 (shaping the architecture)): new identity, addressing or dispatch systems = 0. The lookup is the existing template registry plus the existing node param provider, joined at the existing manifest build. It replaces deciders 2, 3, 5, 6 and 7 and two load-refresh passes. The scene table replaces 4 and 8 on scene surfaces. The item list replaces three ownership functions (`camera_controls`, `world_controls`, `object_controls`) with one. The synthetic `scene_performance.*` id scheme is deleted. Net: one fewer id scheme and no saved display copies.

**D3 — One scene item list, ownership by primary binding.** `scene_items(def, &vm)` returns Camera, then each light, each object, each force, each physics world, then World, in that order. A row belongs to the item whose owned nodes contain its primary binding's target node (the `owned_specs` rule, `projection/scene.rs:229-248`), or to a Force item when the primary target is `SceneModifier { modifier_id }`. Rows owned by no item (composite macros, authored controls on non-vocabulary nodes) form the scene's authored group. Both surfaces partition rows with this one function. Rejected: keeping per-surface ownership (the card's `StandardTarget` node lists beside the panel's `owned_specs`), which is how the two surfaces disagree today.

**D4 — Card layout.** Bands in fixed order: **Camera, Scene, Forces, Lighting, Look** (Peter: camera first). Within a band, rows sort by item order, then `card_order`. Section titles:

| Band | Section(s) | Rows (performance role) |
|---|---|---|
| Camera | "Camera" | Orbit camera: Orbit, Tilt, Distance, Target Height, Field of View. Free camera: Pan, Tilt, Position X, Position Y, Position Z, Field of View. Look-at and loop cameras: Field of View (default; revisit when Peter asks for more). |
| Scene | one section per owning item, titled by item name; authored rows titled by the preset's display name | Physics world: Count, Reset. Liquid domains: Emission, Inflow Speed, Speed, Reset where the domain has them; Matter additionally Cohesion, Stiffness, FLIP Ratio. Authored preset controls (Ocean's wind and waves, composite macros): as authored. Water mesher controls (Surface Detail, Smoothing, Whitewater Budget, Particle Scale): setup role. |
| Forces | one section per force, titled by `scene_modifier_display_name` (`cards.rs:938`, adds "#2") | Enabled, Strength, Kick (the recipe's `fire` trigger, usable by T and audio triggers), Kick Size (`impulse_strength`). Centre, axis, radius: setup. Rows dim with "No physics objects in this force's targets" when the force's eligible objects are empty (the fact `cards.rs:849-855` already computes). |
| Lighting | "Lighting" | Exposure (lens `exposure_ev`), Environment (bake `intensity`, card label = group name), then one Intensity row per light, labelled with the light's name and "intensity" ("Sun intensity"), in item order. |
| Look | "Look" | Depth of Field (switch), then Focus, which dims while it is off, then F-Stop and Max Blur only while it is on. A switch always sits above the rows it controls. Motion Blur (switch), then Shutter Angle and Max Blur only while it is on. |

Camera translate: Target Height (`look_y`) is the only translate the orbit camera has (`camera_orbit.rs:67-129`); lateral pan is Deferred (new primitive params). Rejected: "Main Light Intensity" (Peter, quoted above). Rejected: placeholder rows for absent targets (BUG-pmbd3.3 (hide controls the scene lacks)).

**D5 — Hide what the scene lacks; dim what does nothing now.** A row whose param the scene does not have is not built. A row that exists but has no effect right now renders with `inactive_reason` (Focus while depth of field is off; a force with no physics targets). Hidden-role params stay unstamped (today's filter semantics); already-stamped hidden params keep their exposure and ids and simply build no row.

**D6 — Attached rows are always shown, and stay.** Attaching a driver, envelope, audio mod, Ableton mapping, macro mapping or automation lane to a control the card leaves off (by its role, a fold gate or a Remove from Card) pins it Shown in the same undo unit (`KeepOnCard` in each attach command, `PresetInstance::keep_on_card`). Switching the modulator off or deleting it never takes the control off the card; Remove from Card is the only way off, and undoing the attach takes the pin back with it unless another live modulator or a macro still drives the control. The card reads the stored pin, never the live attachment; a pinned row stays through its fold gate, dimmed with the feature's off reason. Projects saved earlier pin their modulated, unpinned rows at load, and a removed row a live modulator drives. The panel shows an attached row even when its role is Hidden. "Remove from card" is disabled while a modulator is attached, reason "Mapped — unmap to remove". Fixes BUG-pmbd3.7 (card hides mapped rows) as a class.

**D7 — User choices are the only per-control display state a project saves.** `ParamChoices` (section 3.4) holds rename, slider range, curve, invert, section and card pin. They are written only by commands the user triggers: `SetCardPinCommand`, `SetSliderRangeCommand`, `EditParamMappingCommand`, and the scene-object and group renames (`RenameSceneObjectCommand`, `RenameGroupCommand`), which record the new section as a section choice through `Param::choose_section`. The join never writes them. The pin is `Option<CardPin>` (`Shown` / `Removed`); `None` follows the role. Card section fold state is saved as `PresetInstance.folded_sections` and `unfolded_sections` (a title in neither takes its default, so an unfold of the D11 default-folded Lighting holds), written through `MutateProject` like `EffectCollapseToggle` (not undoable, because it is view state). Rejected: pinning through `card_visible`, a lookup output that is no longer saved. Rejected: pinning through `user_added`, because un-exposing a user binding deletes it (`expose.rs` `unexpose_user_binding_freezes_effective_value_into_def_slot`). Rejected: pinning through `SceneSetupExposeParam`, a no-op on an exposed param, which every scene param is.

**D8 — Scene tab tree.** The tree sits at the top of the Scene tab (D16). Fixed groups in this order: **Camera, Lights, Objects, Forces, Physics, World** (Peter: physics is its own group). A group with no items is not drawn, except Lights and Objects, which always show so their add button is reachable. Each row: type icon, readable name, eye toggle where the item has `visible`. Each group owns its own add button: Lights "+" adds a light; Objects "+" opens one menu (Object, Plane, Water, Import Model…); Forces "+" opens the force picker. World holds Environment, Fog and Ray Tracing (the old Rendering row). Below the tree, the selected item's header shows its name once (click to rename where a rename command exists), its kind, and copy and delete where commands exist: objects copy and delete, forces copy and delete, lights delete only, Camera, Physics and World neither. Sections under the header follow the item's `ControlGroup`s in declaration order. The stats line is deleted. The tree and the selected item's settings are two panes, each its own scroll column, with a draggable split between them (Blender's Outliner over Properties): scrolling the settings never moves the tree. The tree pane takes only the height its rows need, up to the split; a double-click on the split resets it. The split and both scroll offsets are UI-local, never saved.

**D9 — One word list, on the declarations.** Labels change only where they are declared: `ParamDef.label`, recipe JSON and bundled preset JSON. After D2 a label edit reaches every old project on next load with no migration rung. Units move to `unit` and render in the value readout. The agreed table, plus the six meanings resolved from code:

| Current | New | Where declared |
|---|---|---|
| FOV Y · Look Y · Near / Far | Field of View · Target Height · Near Clip / Far Clip | `camera_orbit.rs`, `free_camera.rs`, `look_at_camera.rs`, `loop_camera.rs` |
| Pos X/Y/Z · Yaw / Pitch (free camera) | Position X/Y/Z · Pan / Tilt | `free_camera.rs`, `loop_camera.rs` |
| Horizontal / Vertical Angle (card) | Orbit / Tilt (Pan / Tilt on free camera) | deleted with `scene_performance.rs` |
| Exposure (EV) · Aperture (card) · DoF Max Radius · Max Blur (px) | Exposure + unit EV · F-Stop · Max Blur · Max Blur + unit px | `camera_lens.rs`, `nodes-image motion_blur.rs:102`; find DoF Max Radius with the command below |
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
| **Solve Level** — pressure solve on a grid 2^k coarser, 0 = full grid (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)) | Coarse Solve | `gpu_flip_domain.rs:411` |
| **Max Iterations** — the pressure solve's round cap (default 900, `gpu_flip_domain.rs:412`); the name already says what it caps inside the water's solver controls | Max Iterations (kept) | — |
| **Liveliness** (Matter) — PIC/FLIP velocity blend (`grid_to_matter_body.wgsl:138`) | FLIP Ratio (the industry term) | `matter_domain.rs:322`, `grid_to_matter.rs:83` |
| **Appearance Gain** — below 1 fades coverage, above 1 brightens (`render_scene.wgsl:1027-1033`) | Level | `scene_object.rs:118` |

FLIP Fluids' own terms stay (Sheet Fill Rate). Re-derivation for any row: `rg -n '"<current label>"' crates/ -g '*.rs' -g '*.json'`; a label not found as a literal is composed — find its parts, never guess. Where a card label today differs from the target primitive's label only because an io migration or the importer authored it, the primitive's label is the one home (the migration's label is inert after D2).

**D10 — Cross-surface gestures.** Right-click on any row in either surface opens the one param menu (`dropdowns.rs:1666`). It gains **Show on Card** (on a panel row whose card state is off), **Remove from Card** (on a card row; disabled on attached rows, D6), **Show in Scene tab** (on a card row), and **Set slider range…** / **Reset slider range** (D15). Show in Scene tab, and clicking a card section's title (not its fold triangle), switch the inspector to the Scene tab, select the item that owns the row and scroll the row into view. A section title reveals the section's first row owned by a light, object or force item, and its first row only when none is, so Lighting opens the first light and Camera or Look opens Camera. Show on Card from the Scene tab flashes a brief highlight on that card row the next time the card is shown. All navigation is by `(GraphParamTarget, ParamId)` against the Scene tab's full surface, never by section string. Rejected: a second menu for scene rows (one menu, WIDGET_TREE section 5b).

**D11 — Lights.** Display name: node `title`, else the handle humanised (`fill_light` → "Fill Light"), else "Light N". The importer writes the GLB light name into `title`; a load rung copies it into `title` for already-imported lights whose intensity stamp's section string differs from the handle. The title is the imported scene's own data, not a display copy. Every light gets a card row; no cap. The Lighting section starts folded when it holds more than eight light rows; the user's fold choice then persists (D7). Rejected: a light-count cap ("never add a size rule").

**D12 — Row sets follow authored values.** Which Look detail rows exist depends on the switch's base value at structural projection, never the per-frame effective value — a driver flickering depth of field does not rebuild the card. Precedent: MATERIAL_INSPECTOR D4 ("layout uses the authored mode/attachments, never the per-frame sample").
- Membership: `ChangeGraphParamCommand::touches` returns Values|Effects when the target spec has `scene.switch` or a gate, so a base-value edit that changes the row set rebuilds it; every other param edit stays Values (agreed with UI_INVALIDATION_DESIGN.md).

**D13 — Migration shrinks to structure.** Predecessor section 5 (Decisions — resolved by Peter) item 5 kept load-time idempotent migration, and that stays for structure only.
- `migrate_scene_exposures` keeps pass 1 (stamp missing exposures: binding id and target) and pass 3 (`default_mirrors_node_param`).
- It loses pass 2 and pass 4, because the join replaces them. The stamp stops writing specs beyond an id stub.
- It gains the light-title rung (D11).
- The default re-seed pass (BUG-303 stamped-default repair) stays until P3 moves defaults into the scene table.
- It never writes `ParamChoices`, `folded_sections` or user-added bindings.
- Section strings no longer decide anything on scene surfaces, so the predecessor's "named collapsed sections per scene item" become display data the projections ignore.
- Water graphs follow the decision-log rule ("Water changes need no old-project upgrades"): water presets are regenerated, no water rungs.

**D14 — "Use this view" copies the viewport camera into the scene camera** as one undoable command. Free scene camera: copy position, pan, tilt, field of view. Orbit scene camera: Target Height = height where the view ray passes closest to the vertical axis through the origin; Distance = eye to that point; Orbit = `atan2(z, x)` of the eye about the axis (the eye formula in `camera_orbit.rs`); Tilt = elevation angle; Roll = 0, since the viewport never rolls; Field of View copied. A view ray parallel to the axis keeps the current Target Height. Look-at and loop cameras: button disabled with reason "Not supported for this camera type".

**D15 — Slider range and hard limit are two facts.** Today one min/max both sets the slider span and caps the value (`constrain_to_range`, `params.rs:167`), so ranges are either huge or limiting. Following Blender (soft and hard limits), Houdini and TouchDesigner (slider range vs clamp), every control's description in the lookup carries:
- a **slider range**: what the slider spans. Today's min/max, and today's scene-scaled bands, widened to include the control's default as the stamp rule does now.
- a **hard limit**: what the value may never pass. `HardLimit::{Slider, Unlimited, AtLeast(x), AtMost(x), Between(a, b)}`. `Slider` is the default for every param the tables do not mention: drags stop at the declared range, and typed and OSC values stay free, since a range hint never restricts what a user enters (PARAM_RANGE_CONTRACT_DESIGN.md D3 (hints never restrict)). Toggles, triggers, enums and whole-number indices are always `Slider`.

The scene table declares limits for scene params, for example positions `Unlimited`, distances and intensities `AtLeast(0)`, field of view `Between(1, 179)`. Preset JSON can declare `limit` for template params.

Behaviour:
- Typing a value clamps only to a declared limit (`Unlimited`, `AtLeast`, `AtMost`, `Between`); under `Slider` it is unclamped.
- Dragging is relative to where the press started, so it carries on past the slider end until the hard limit. The slider shows its thumb pinned at the end with an overflow mark, and the readout shows the true value.
- Automation and modulation (LFOs, drivers, envelopes, audio) stay inside the slider range: the user's when one is set, else the declared range, and never past a declared limit. Only a hand drag or a typed value goes past the slider's ends. Modulation depth is measured over the slider range.
- OSC and MIDI 0–1 values map over the slider range (the user's, if set), as calibration does today, and clamp only to a declared limit; an OSC value outside 0–1 on an exact 0–1 range passes through raw as it does today.
- Wrapping params wrap over the declared slider range, never the user's.
- The card-to-node reshape (invert and curve) normalises over the declared slider range, never the user's, so a user's slider range never changes what reaches the node.

"Set slider range…" is in the shared right-click menu (D10). It works on any row on the card or the scene panel, because both surfaces build rows through the shared row widget. It is saved per control as `ParamChoices.slider`, and the old calibration block migrates into it. "Reset slider range" clears it. Its min and max edit the same field as the graph editor mapping drawer's range. The drawer stays as it is for modulation reshaping (invert, curve, scale, offset), so there is one home for the range and two places to edit it. Rejected: one range field with a "soft" flag (it can't express "slider 0–10, limit ≥0"). Rejected: auto-widening the slider when a value goes past its end (the slider would jump under the performer's hand).

**D16 — The scene is a tab in the inspector.** Peter, 2026-10-14: one inspector column with categories clips, layers, master, and scene when the selected layer has one; the scene surface and the inspector never need to be open at the same time.
- The Scene Setup dock stops being a separate surface. Today it is its own docked region beside the inspector (`ui_root/mod.rs:767-776` builds it into `layout.scene_setup()`; `layout.scene_setup_width`, `layout.rs:34`), opened by `toggle_scene_dock` (`ui_root/mod.rs:1412`; callers `ui_bridge/mod.rs:319`, `dispatch/params.rs:105`, `app_render.rs:1198`) and mutually exclusive with the Audio Setup dock (`:1340-1353`). Its content becomes a **Scene** tab beside Clip, Layer, Group and Master (`InspectorTab`, `manifold-ui/src/panels/mod.rs:243-248` gains `Scene`): the D8 tree on top, the selected item's header and properties below.
- The Scene tab exists only when the selected layer is a generator layer whose graph holds a `node.render_scene`. The layer is picked by today's rule (`projection/inspector.rs:258-263`: `selected_layer_id_for_clip`, else `primary_selected_layer_id`, else the active layer), which already binds the dock today (`:1030-1053`). When the layer changes to one without a scene, the inspector falls back to the Layer tab.
- External selection writes (`edit_selection.rs:205-217`, `set_selection` for lights and objects) switch the inspector to the Scene tab instead of assuming an open dock.
- Navigation into the tab is D10's: by param id, switching the tab and revealing the owning item.
- The 3D viewport is not affected. It lives in the graph editor workspace (`workspace.rs:84-104`, `viewport_open` toggled by `v`; its rect is set in `editor_bridge.rs:1641`), rendered by `SceneViewportPass` (`nodes-scene render_scene/scene_viewport.rs:18`), not inside the dock.

Rejected: keeping the dock beside the inspector. It costs width on a laptop on stage and shows two param columns for one layer. Rejected: a floating scene window (Peter never needs both open at once).

**D17 — One scene per layer.** A layer's generator graph holds at most one `node.render_scene`.
- One shared check in the graph validator (`manifold-node-engine/src/validate.rs:244` `validate_def`, the function `graph-tool validate --kind generator` runs) reports two or more `node.render_scene` nodes as an error. Every bundled preset must pass it.
- Editing commands that add, paste, duplicate or import a node into a layer's generator graph refuse when the result would hold a second `render_scene`, with the user-visible reason "This layer already has a scene". The refusal happens before the command enters undo, so the graph and the undo stack are unchanged.
- Projects that already hold two load unchanged (CLAUDE.md invariant: reading the past never changes it). No migration deletes nodes. The Scene tab shows the lowest-id one, as `scene_vm.rs:113-118` does today, with a one-line notice in the tab ("This layer has more than one scene; showing the first").

Rejected: deleting extra scenes on load (rewrites the past). Rejected: a scene picker in the tab (supports a shape Peter ruled out).

**D18 — The lookup's sources register at link time.** The node source (`param_lookup::NodeParamSource`, from `manifold-nodes-scene`) and the template-graph source (`param_lookup::TemplateGraphSource`, from `manifold-nodes`) are `inventory` submissions, not an `install()` into a `OnceLock`. A binary that links the crates has the lookup with no start-up call, and nothing can run before it is installed. Lead ruling, 2026-10-14.

**D19 — The metadata renames wait for P3.** `SceneExposureMetadataProvider` / `SceneParamMetadata` keep their names, and `NodeParamMetadata` gains `scene`, `unit` and `limit` in P3, where the scene filters move into the scene table. P1 reads the existing types unchanged. Lead ruling, 2026-10-14.

**D20 — Stubs bump the project version to 1.21.0.** An older build reads a stub as a nameless 0..0 control and could save that back; refusing the newer file is the safe failure. The rung is a structural no-op. One radius per graph: an import or merge saves `meta.scene_bounds` (the target's own win, else the incoming asset's) and the lookup scales node ranges by it.

**D21 — Which names the lookup supplies.** D24 decides which controls take the lookup's name; P5 changes the declared labels themselves.

**D22 — A fan-out is described by its first binding.** A control with several bindings under one id takes range, flags and scene scaling from its first binding's target in authored order, as the import range stamp did; everything else stays authored. If that target cannot be described, the control stays authored. Lead ruling, 2026-10-14.

**D23 — Scene facts follow the bound node.** A template control bound to exactly one node target takes that target's scene placement, unit and limit from the scene table wherever the template declares none. Section 3.3's defaults apply only when neither declares. Fan-outs and user-added controls keep their own. Lead ruling, 2026-10-14.

**D24 — Who owns a control's name.** The lookup names a control only when it is a template control or an exact auto-stamp (id `{doc_id}_{param}`, not user-added, targeting that node's param); those save without a name. Every other node-bound control (curated ids, fan-outs, `user.*`) owns its name: authored, saved, never replaced by the node's label. Range, flags and scaling still come from the lookup (D22).

**Consequences, stated honestly:**
1. Scene projects get smaller on disk: no names or ranges per control. A file saved after P1 shows placeholders in an older build (format version bump, P1).
2. Label fixes reach old shows on next load, on the card, the panel, automation lanes, OSC docs and Ableton displays. Peter will see new names on lanes in old shows after P5. Ids do not change, so nothing unmaps.
3. A drawer rename of a bundled preset's curated control in an old per-instance graph goes back to the template label (D2, old projects).
4. A user's slider range changes what a full controller sweep covers. That is true of calibration today.
5. The scene table is a large hand-written data file (about 300 params). Its correctness rests on a coverage test plus Peter's look at the card, not a proof.
6. Eight phases touch the most-used surface in the show. P0's flow baseline is the net that tells a new red from an old one.

---

## 3. Design body

### 3.1 Vocabulary — `manifold-foundation/src/scene_surface.rs` and `param_range.rs` (new; shared because core and ui both read them)

```rust
// scene_surface.rs — serde camelCase; Copy + Eq + Hash on every enum. Declaration order is display order.
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
pub enum CardPin { Shown, Removed }

pub struct SceneParamDesc {
    pub role: SurfaceRole,
    pub group: ControlGroup,
    pub band: Option<CardBand>,     // Some iff role == Performance
    pub card_order: u8,             // order within (item, band)
    pub card_label: CardLabel,
    pub gate: Option<Gate>,
    pub switch: Option<Feature>,    // this row turns the feature on and off
}

// param_range.rs
pub enum HardLimit { Slider, Unlimited, AtLeast(f32), AtMost(f32), Between(f32, f32) }
impl HardLimit { pub fn clamp(self, v: f32, slider: (f32, f32)) -> f32; }
```

`ui` already depends on foundation, so no mirror enums are needed.

### 3.2 The lookup — `manifold-core/src/param_lookup.rs` (new)

```rust
pub enum ParamKey<'a> {
    Template { preset: &'a PresetTypeId, id: &'a str },
    Node { type_id: &'a str, param: &'a str, scene_radius: Option<f32> },
    Modifier { recipe: &'a PresetTypeId, param: &'a str },
}
/// The node half: every param of every node type, registered once by manifold-nodes-scene (D18).
pub struct NodeParamSource { pub params_for_type: fn(&str) -> Vec<NodeParamMetadata> }
/// The source read as-is, and minus the scene table's Hidden params (what scene nodes stamp).
pub fn node_params(type_id: &str) -> Vec<NodeParamMetadata>;
pub fn stamped_node_params(type_id: &str) -> Vec<NodeParamMetadata>;
/// What the code declares about one control: name, unit, slider range, hard limit, scene descriptor,
/// value labels, angle/toggle/trigger flags. Never a user choice.
pub fn describe(key: ParamKey<'_>) -> Option<ParamSpecDef>;
/// Template id first, then the binding's primary target. Composite → None (authored).
pub fn key_for<'a>(preset: &'a PresetTypeId, binding: &'a BindingDef, nodes: &'a NodeTypes, scene_radius: Option<f32>) -> Option<ParamKey<'a>>;
```

`describe` for `Node` applies `scene_scaled_range` when a radius is given, and widens the slider range to include the binding's default the way the stamp does now. Section strings come from `section_name_for_node` behind the same call, so the panel's non-scene sub-groups keep their titles without saving them. The install is a `OnceLock`. It is not shared mutable state; hot-reloaded presets keep using `PRESET_DEFINITIONS`'s own swap. ⚠ VERIFY-AT-IMPL (P1): that the preset hot-reload path calls `refresh_manifest_from_graph` on open instances (`rg -n 'refresh_manifest_from_graph' crates/`). If it doesn't, a reloaded preset's names update on the next structural edit, which is acceptable.

### 3.3 The scene table — `manifold-nodes-scene/src/node_graph/scene_surface.rs` (new)

```rust
pub struct SceneParamFacts { pub desc: SceneParamDesc, pub unit: Option<Unit>, pub limit: HardLimit }
pub fn scene_param_facts(type_id: &str, param: &str) -> Option<SceneParamFacts>;
```

Owner: the scene crate, beside `SCENE_VOCABULARY` (`scene_exposure.rs:37-71`). Pure data: a `match` on `type_id`, then on `param`. Water type ids are matched by string as the vocabulary already does. Shape precedent: `node_graph/material_inspector.rs` `material_param_role`. Rules the table encodes:
- Everything `card_visible_for` lists today is Performance unless D4 says otherwise.
- Everything the stamp filters drop today is Hidden.
- `look_metadata`'s "Size" becomes `CardLabel::Param`, with the radius label edited in P5.

The provider fills `NodeParamMetadata.scene`, `.unit` and `.limit` from it, and drops Hidden params from stamping in place of its per-type filter lists. The table never carries a label.

Authored controls (recipe JSON, bundled preset JSON) carry `"scene"`, `"unit"` and `"limit"` in their own `ParamSpecDef` when they need something other than the default. They are templates, so the lookup reads them from `PRESET_DEFINITIONS`. Defaults for a spec with `scene: None`:
- role: Performance in band Scene when `card_visible`, else Setup;
- group: `Simulation` for water presets, `Force` for recipe params, otherwise the owning item's first group;
- limit: `Slider`.

### 3.4 Saved and in-memory fields — `manifold-core`

```rust
// params.rs
pub struct ParamChoices {                 // the only per-control display state a project saves (D7)
    pub rename: Option<String>,
    pub slider: Option<(f32, f32)>,       // "Set slider range…", mapping-drawer range, old calibration min/max
    pub curve: Option<MacroCurve>,
    pub invert: Option<bool>,
    pub section: Option<Option<String>>,  // mapping-drawer section override
    pub pin: Option<CardPin>,
}
pub enum SpecSource { Lookup, Authored, Unresolved }
// Param: + choices: ParamChoices, + source: SpecSource; - calibrated (now choices.slider/curve/invert.is_some())
// Param.spec = describe(key) joined with choices, except the slider range: spec.min/max stay the
// declared range (wraps, the Slider limit, the reshape); Param::slider_span() is the user's span.

// effect_graph_def.rs, on ParamSpecDef (all #[serde(default, skip_serializing_if = …)]):
pub limit: HardLimit,                    // saved only on Authored specs; Slider by default
pub unit: Option<Unit>,                  // saved only on Authored specs
pub scene: Option<SceneParamDesc>,       // saved only on Authored specs
// name, min, max, default_value gain #[serde(default)] so an id-only stub parses.

// instance_serde.rs ParamEntryWire: + rename, + section, + pin (skip if None);
// calibration keeps its key and shape {min, max, curve, invert} ↔ choices.slider/curve/invert.

// effects/instance.rs, on PresetInstance (#[serde(default, skip_serializing_if = "Vec::is_empty")]):
pub folded_sections: Vec<String>,        // card section titles the user folded
pub unfolded_sections: Vec<String>,      // titles the user unfolded; only a default-folded section (D11) needs it
```

On save, `ManifestSer` and `GraphWithDerivedParams` write `{"id": …}` for `Lookup` entries, the full spec for `Authored`, and the saved entry word for word for `Unresolved`. `BindingDef.label` is written only for `Composite` targets. `RowSpec` (`param_surface.rs:140`) gains `unit: Option<Unit>` and `limit: HardLimit`; `format_param_value` (`param_slider_shared/state.rs:462`) appends `unit.suffix()`. Material roles keep their own field and become a lookup output too.

### 3.5 Scene items — `manifold-nodes-scene/src/node_graph/scene_items.rs` (new)

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

### 3.6 Card projection — `manifold-app/src/ui_bridge/projection/scene_card.rs` (new; replaces `scene_performance.rs`)

```rust
pub(super) fn scene_card_rows(rows: &mut Vec<ParamRow>, inst: &PresetInstance, def: &EffectGraphDef) -> bool;
```

Called at the same site (`cards.rs:540-542`), after modulation is attached. Per row:
- Read `inst.params[id].spec.scene` and `choices.pin`.
- A row is on the card if `pin == Shown` (Show on Card, or an attach (D6)), or (`role == Performance` and `pin != Removed`).
- Drop unpinned rows whose `Gate::Fold` feature switch has base value off (D12); a pinned one stays, dimmed.
- Set `inactive_reason` for `Gate::Dim` and for a force with no physics targets.
- Set `spec.name` from `card_label` (unless `choices.rename` is set).
- Set `spec.section` to the band's section title (D4).
- Sort by (band, item index, card_order, manifest order).

User-added rows with no `scene` description are on the card unless `Removed`. Attached rows with role Setup or Hidden join their item's section in their band (the Scene band when the item has none).

### 3.7 Scene tab — `scene_setup_panel/outliner.rs` and `scene_setup_panel/item_header.rs` (new modules; `scene_setup_panel.rs` only shrinks)

The panel's body is hosted by the inspector column as `InspectorTab::Scene` (D16); the dock region, `layout.scene_setup_width`, `toggle_scene_dock` and the header's scene dock toggle are deleted. The tree reads `SceneSetupVm.items` (D8). Icons are new `Icon` atlas entries (`icons.rs`): SceneCamera, SceneLight, SceneMesh, ScenePlane, SceneWater, SceneModel, SceneForce, ScenePhysics, SceneWorld, bitmaps added where `native_text::generate_atlas_icons` builds the others. Properties for the selected item: rows owned by the item, minus Hidden (unless attached), grouped by `ControlGroup`, each group a fold section titled by the group's display name. The material inspector keeps its own layout inside the Material group.

### 3.8 Readers of saved labels, ranges and roles — what each needs (D2)

Every reader below either already reads the joined manifest (no change) or reads a saved copy around it (moved in P2). Re-derive at execution: `rg -n 'spec\.(name|min|max|section|card_visible|material_role)\b|binding\.label|b\.label\b' crates/ -g '*.rs'`.

| Reader | Where | Reads today | Change |
|---|---|---|---|
| Card projection | `projection/cards.rs:432-495` | `Param.spec` name, range, section, card_visible, material role | none (the join) |
| Scene panel | `projection/scene.rs:211` | `spec.section` | none; ownership moves to items (P4) |
| Automation lane names | `projection/timeline.rs:363`, `:386` | `spec.name` | none |
| Automation playback | `manifold-playback/src/automation.rs:267`, `:361`, `:454` | `spec.min/max` clamp | slider range, else declared; wraps on declared (P8) |
| Modulation | `manifold-playback/src/modulation/composition.rs:214` | `spec.min/max` clamp | slider range, else declared; depth over slider range (P8) |
| OSC router | `osc_param_router.rs:137`, `:187`, `:213` (ranges); `:91`, `:150`, `:258` (routing by id) | `spec.min/max` | 0–1 over slider range, clamp to hard limit (P8) |
| OSC addresses | `preset_definition_registry.rs:401-422` | type `osc_prefix`, layer id, param id | none — no label in any address |
| Value formatting | `preset_definition_registry.rs:377` `format_value` | template spec | none (already the template) |
| Content thread | `content_thread.rs:262-263`, `:999-1017` | `Param.spec` range and name | none |
| Content thread routings | `content_thread.rs:1632`, `:1669`; `ui_snapshot/mod.rs:745`; `node-engine load/loaded_preset_view.rs:220` | `BindingDef.label` | manifest spec name by binding id (P2) |
| Runtime reshape | `node-engine runtime/build.rs:386-395` (standalone base from `meta.params`), `:405-411` (manifest overlay), `:517` (binding label) | saved copies | declared slider range from `describe`, curve and invert from choices; label from the manifest (P2) |
| Group expansion | `node-engine load/expand/bindings.rs:189`, `:261` | copies `BindingDef.label` into expanded bindings | stop copying (P2) |
| Instance fallbacks | `effects/instance.rs:741`, `:933`, `:952` | `BindingDef.label` | lookup (P2) |
| Manifest build | `instance_serde.rs:272-300` `gather_known_params`, `:211` `spec_from_binding` | `meta.params` copies, `b.label` | the join (P1) |
| Save | `instance_serde.rs:135-145`, `:155-196` | writes spec copies | id stubs for `Lookup` (P1) |
| Load refresh | core `scene_exposure.rs:529-563` (pass 2), `:598-687` (pass 4) | rewrites copies | deleted (P2) |
| Import ranges | `gltf_import/scene_scale.rs:170-196` | writes scaled ranges into copies | deleted; `describe` applies them from `scene_bounds` (P2) |
| Mapping drawer | `editing commands/effects.rs:1146-1185`, `:1399`; called from `editor_bridge.rs:39`, `:74`, `scene_modifier_journey.rs:584` | writes `spec` fields, sets `calibrated` | writes `ParamChoices` (P2) |
| Row widget | `param_slider_shared/row_host.rs:342`, `:373`, `:391`, `:499`, `:512`, `:597`, `:686`; `builders.rs` | `spec.min/max` as span; drag clamps to track | span = slider range; drag relative, value clamps to `limit` (P8) |
| io migrations | `solve_level_card_v1180.rs:56,70`, `pressure_cap_card_v1190.rs:66,80`, `scene_cinematic_tail_v1130.rs:316` | write authored labels into copies | unchanged; their label and range writes become inert |
| Scene legacy migrations | `scene_modifier_legacy_migration/loop_upgrade.rs:242-246` (writes), `photoscan.rs:771`, `sources.rs:258` (read for matching) | `binding.label` | writes inert; ⚠ VERIFY-AT-IMPL (P2) that the two reads match old files only, before the join |
| `project_tool` | `manifold-app/src/bin/project_tool.rs` (raw JSON; `load_project_from_json` at `:178`; writes `genParams.params` entries near `:683`) | no labels; its typed check drops unresolved params | moves to `manifold-app` with the registry installed (P1) |

### 3.9 Commands and actions

| Gesture | Action (manifold-ui `actions.rs`) | Command / write | Precedent |
|---|---|---|---|
| Show on Card / Remove from Card | `ParamsAction::SetCardPin(GraphParamTarget, ParamId, Option<CardPin>)` | `SetCardPinCommand { target, param_id, pin, previous }` in `manifold-editing/src/commands/graph/card_pin.rs`; writes `Param.choices.pin`; undoable | `ToggleNodeParamExposeCommand` (`graph/expose.rs:41`) for target resolution |
| Set slider range… / Reset slider range | `ParamsAction::SetSliderRange(GraphParamTarget, ParamId, Option<(f32, f32)>)` | `SetSliderRangeCommand` writes `Param.choices.slider`, re-joins the spec; undoable | `EditParamMappingCommand` (`effects.rs:1263`) |
| Show in Scene tab | `ParamsAction::RevealInSceneTab(GraphParamTarget, ParamId)` | UI-only: switch the inspector to the Scene tab, `reveal_param` | `reveal_clip_response` (`parameter_navigation.rs:56`) |
| Section title click | `ParamsAction::RevealSectionInSceneTab(GraphParamTarget, Vec<ParamId>)` (the section's rows) | same, through `reveal_section` (D10 row choice) | same |
| Section fold | existing fold toggle, plus `MutateProject` write of `folded_sections` | non-undoable | `EffectCollapseToggle` (`dispatch/params.rs:337`) |
| Use this view | `SceneSetupAction::CopyViewportCamera(LayerId)` | `CopyViewportCameraCommand` — one undo unit setting the D14 params | `SetGraphNodeParamCommand::with_previous` batching |

`reveal_param(target, param) -> bool` reveals by id with no clip-trigger requirement and no section-string matching: ownership is the Scene tab's `items`. `CardPin` lives in foundation, so the UI action carries it directly. The slider-range popover is the mapping drawer's min/max editor, reused through the shared row recipe. ⚠ VERIFY-AT-IMPL (P8): which shared popover the drawer's range fields use (`rg -n 'fn .*range' crates/manifold-ui/src/panels/drawer.rs`).

### 3.10 Seam brief — what deletes (old → new)

| Old | New | Re-derive with |
|---|---|---|
| `meta.params` copies as generator authority; `inline_graph_spec` | the join; id stubs | `rg -n 'inline_graph_spec|fn gather_known_params' crates/manifold-core` |
| Passes 2 and 4 of `migrate_scene_exposures`; `scene_scale` range writes | `describe` | `rg -n 'Repair pass|apply_scene_scale' crates/` |
| `Param.calibrated`, `BindingMappingEdit::apply_to_manifest_spec` writes | `ParamChoices` | `rg -n 'calibrated|apply_to_manifest_spec' crates/ -g '*.rs'` |
| `SceneExposureMetadataProvider` passed as an argument | `param_lookup::stamped_node_params` over the registered `NodeParamSource` | `rg -n 'SceneExposureMetadataProvider|NodeParamProvider' crates/` |
| `card_visible_for(type, param)` (core `scene_exposure.rs:66`), 3 non-test call sites: core `:289`, `:555`, `scene_performance.rs:416` | `spec.scene` from the join | `rg -n 'card_visible_for\(' crates/ -g '*.rs'` |
| Stamp filter chain (`nodes-scene scene_exposure.rs:115-130`) | params whose role is Hidden are not stamped | read the function |
| `look_metadata` relabel (`:184-189`) | `ParamDef.label` edit | `rg -n 'look_metadata' crates/` |
| `curate_scene_rows`, `StandardTarget`, `synthetic_id`, `is_retained_extra`, `best_candidate` | `scene_card_rows` | `rg -n 'scene_performance' crates/` |
| `SceneVm.camera_controls`, `world_controls`, `object_controls` | `SceneItem.owned_nodes` | `rg -n 'camera_controls|world_controls|object_controls' crates/` |
| `property_selection_for_row` section matching, `reveal_clip_response` | `reveal_param` by item ownership | `rg -n 'reveal_clip_response|property_selection_for_row' crates/` |
| `SceneSelection::Rendering`, `OutlinerFold("Motion & Physics")`, `"Lighting & Environment"` | D8 groups | `rg -n 'Rendering|Motion & Physics|Lighting & Environment' crates/manifold-ui/src/panels/scene_setup_panel*` |
| `constrain_to_range(v, spec.min, spec.max, wraps)` | `param.driven_value(v)` (slider range, else declared; never past a declared limit; wraps over the declared range) | `rg -n 'constrain_to_range\(' crates/ -g '*.rs'` (4 non-test sites) |

Compiler-driven: delete the old symbol first; the errors are the checklist. Re-run each command at execution time; if the count differs from what the phase brief recorded, stop and list the new sites first.

### 3.11 Performance

Off the per-frame path. The lookup runs once per control when a manifest is built (load, registry fill, structural edit): one hash lookup per binding. `scene_items` and `scene_card_rows` run once per data change, on the UI thread, over at most a few hundred rows. The clamp change swaps one function for another of the same cost. No per-frame allocation is added; the per-frame value channel is unchanged.

**The plausible-wrong architecture, forbidden by name.** You will want to fix the card by adding cases to the projection — a `match scene_kind` in `scene_card.rs` choosing rows for "physics scenes" and "water scenes". No: every per-scene and per-type fact is a row in the scene table or an authored `scene` field; `scene_card.rs` contains no type ids and no param names (negative gate in P4). You will want to call the lookup from a projection because it is right there. No: the join in core is the only caller; projections read the manifest. You will want to keep writing names into saved files "just in case". No: an unresolved control keeps its old entry word for word, and that is the only case a copy is saved. You will want a label map in the projection so the card can say "Field of View" without touching the primitive. No: labels change only where they are declared (D9). And you will want to give the panel its own row builder for the new header, the slider range or the light swatch. No: WIDGET_TREE section 5b (agent contract and enforcement); `no_bespoke_row_infra` fails it.

---

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| INV-S1 Every param of every vocabulary type is classified exactly once | `scene_surface::tests::every_vocabulary_param_is_classified` — iterates `SCENE_VOCABULARY` × `PrimitiveRegistry` `parameters()`, fails on `None` (P3) |
| INV-S2 Only the join calls the lookup; scene surfaces read the manifest | negative gate: `rg -n 'param_lookup::describe|scene_param_facts' crates/ -g '!crates/manifold-core/src/effects/instance_serde.rs' -g '!crates/manifold-core/src/param_lookup.rs' -g '!crates/manifold-nodes-scene/src/node_graph/scene_*'` → 0 (P1 onward) |
| INV-S3 One curation decider | negative gate: `rg -n 'card_visible_for|StandardTarget|scene_performance' crates/` → 0 (P4) |
| INV-S4 Card and panel partition rows identically | `scene_card::tests::card_and_panel_agree_on_owners` (ui-snapshot feature, it reads the headless fixtures) over gltfscene, physicsscene, waterdambreak, waterdambreakmatter, oceanscene fixtures (P4) |
| INV-S5 Attached rows on the card, and kept there | `scene_card::tests::attached_rows_stay_on_card` — the pin, not the live attachment, decides, on a Setup and a Hidden row; `inspector::tests::modulating_roll_keeps_it_on_the_card` — arm, disable, delete, undo through dispatch |
| INV-S6 The join never writes user choices | `param_lookup::tests::join_preserves_choices_and_user_bindings` plus `migrate_twice_is_noop` (P1) |
| INV-S7 Row set follows base values | `scene_card::tests::look_rows_follow_base_not_effective` (P4) |
| INV-S8 Same label on both surfaces | `scene_card::tests::card_and_panel_labels_match` — every row on both surfaces has equal `spec.name` except `CardLabel::ItemName` rows, which read the item name then the panel label in lower case ("Sun intensity"), and `GroupName` rows, which read the group title (P4) |
| INV-S9 No units in scene labels | `scene_surface::tests::no_unit_in_scene_labels` — vocabulary `ParamDef` labels, recipe and bundled scene preset names: no `(` and no unit suffix tokens (P5) |
| INV-S10 Navigation by id only | negative gate: `rg -n 'spec\.section' crates/manifold-ui/src/panels/scene_setup_panel/parameter_navigation.rs` → 0 (P7) |
| INV-S11 No private id schemes on scene surfaces | negative gate: `rg -n 'scene_performance\.' crates/` → 0 (P4) |
| INV-S12 Godfiles do not regrow | `godfile_regrowth::no_register_listed_file_regrows` with ceilings added at current size for `scene_vm.rs`, core `scene_exposure.rs`, `projection/cards.rs`, `scene_setup_panel/material_inspector.rs` (P3), and `scene_setup_panel.rs` lowered to its post-P6b size (P6b) |
| INV-S13 Card header title never overlaps its controls | `param_card` geometry test `generator_title_clears_header_controls` (P4) |
| INV-S14 Saved files carry no copies of what the lookup describes | `instance_serde::tests::saved_scene_has_id_only_lookup_entries` — save gltfscene and physicsscene; every `Lookup` entry in `meta.params` has only `id`; no `label` on non-composite bindings (P1) |
| INV-S15 Unresolved controls survive | `instance_serde::tests::unresolved_control_round_trips_verbatim` — lookup missing a type: load, save, bytes of that entry unchanged; install it, reload, the control resolves with its value and choices (P1) |
| INV-S16 No reader goes around the join | negative gate: `rg -n 'binding\.label|b\.label\b' crates/manifold-node-engine/src crates/manifold-app/src/content_thread.rs crates/manifold-app/src/ui_snapshot crates/manifold-core/src/effects/instance.rs` → 0 outside `Composite` arms (P2) |
| INV-S17 A rename never moves an OSC address | `osc_param_router::tests::rename_keeps_address` — rename a control via `choices.rename` and via a `ParamDef.label` change; address and routing unchanged (P2) |
| INV-S18 Values pass the slider end, never the hard limit | `params::tests::{typed_value_clamps_to_limit_not_slider, drag_past_slider_end_reaches_limit, automation_respects_the_slider_range, wraps_use_declared_range}`; `lfo_stays_inside_the_user_slider_range`; `reshape_ignores_user_slider_range` (P8) |

---

## 5. Phasing

Phased by layer. P1 is the saved-control format and the lookup, P2 moves every reader onto it, and P3 adds the scene table. P4 is the card projection and the first visible phase, running model → manifest → projection → card pixels end to end. P0 also ships D17 (one scene per layer). P5 is the words and P6 the Scene tab: P6a moves the panel into the inspector as a tab, P6b builds the tree and item header. P7 adds the gestures between the two surfaces.

Sequencing with UI_INVALIDATION_DESIGN.md: P7 and P9 wait for its P4b (the planner core) and P8 waits for its P4c (live push), so every new dispatch arm is written once, against the new contracts. P8 is slider range against hard limit, through the shared row widget. P9 and P10 are two small new affordances.

Every child of BUG-pmbd3 (3D scene inspector panel upgrades) lands in a phase:

| Child | Phase |
|---|---|
| .1 scene section, forces and camera layout | P4 (layout), P5 (labels) |
| .2 one intensity row per light | P4 |
| .3 hide controls the scene lacks | P4 |
| .4 right-click show on / remove from card | P7 |
| .5 section name opens the panel | P7 |
| .6 units in the value readout | P3 (field), P4 (readout), P5 (labels) |
| .7 card hides mapped rows (bug) | P0 stopgap, P4 class fix |
| .8 camera rows follow camera type | P4 |
| .9 rows keep their section | P4 |
| .10 light colour swatch | P9 |
| .11 right-click Show in Scene tab | P7 |
| .12 dim controls that do nothing | P4 |
| .13 remember card fold state | P1 (field), P7 (behaviour) |
| .14 Use this view | P10 |
| .15 scene panel tree | P6b |
| .16 card header name/Opacity collision (bug) | P4 (INV-S13) |
| .17 naming pass | P5 |
| .18 one live lookup | P1, P2 |
| .19 slider range vs hard limit | P8 (P1 carries the `limit` field and the choice) |
| D17 one scene per layer (no child bead yet; the lead files one) | P0 |
| .21 Scene tab in the inspector (D16) | P6a (host), P7 (navigation) |

Common to every phase. **Entry:** the status line reads APPROVED; `git -C "/Users/peterkiemann/MANIFOLD - Rust" log --oneline -1 origin/main`; re-run the section 1 anchors the phase names. **Forbidden in every phase:** type ids or param names in `scene_card.rs` or the panel tree; a label map anywhere outside the declarations; a lookup call outside the join; saving a name, range or role for a control the lookup describes; bespoke rows, sliders or swatches (WIDGET_TREE section 5b (agent contract and enforcement)); new `Arc<Mutex>`; per-frame work; silently dropping an unresolved control on load or save; widening scope into material inspector internals. **GPU:** none in this design; land with `--skip-gpu`. **Reports** carry `Shortcuts taken:` and `Demo artifact:`.

### P0 — Baseline and the live bug
- **Entry:** `ls scripts/ui-flows | rg -c scene` = 22 (the set `run_ui_flows.py scene` selects) (stop if not; list the new ones).
- **Read-back:** this doc's sections 1, D6; `scene_performance.rs` whole; `scripts/run_ui_flows.py` docstring. Restate D6 and the forbidden list.
- **Deliverables:** (1) run every scene flow (`.claude/scripts/with-build-lock.sh scripts/run_ui_flows.py scene`) and file one typed bead per red, with the flow name and failing step. (2) `scripts/ui-flows/scene-surfaces-clickthrough.json` (gltfscene) and `-physics.json` (physicsscene): select every outliner item in turn and assert its properties header text and at least one `param_row.*` under it; every failing step is a bead, not a fix. (3) **Named stopgap** for BUG-pmbd3.7 (card hides mapped rows): `is_retained_extra` keeps any row whose modulation, audio, mapping or automation facts are set. Deleted with the function in P4. (4) `attached_rows_stay_on_card` test (INV-S5), shaped to survive P4. (5) D17 (one scene per layer): the `validate_def` check; the add, paste, duplicate and import refusal with "This layer already has a scene", before undo; the lowest-id pick plus the one-line notice for old two-scene projects (shown in the dock until P6a moves it into the Scene tab).
- **Gate:** positive — `cargo nextest run -p manifold-app scene_performance` green; INV-S5 test fails on the pre-fix code (show it red first) and passes after; D17 tests `validate_rejects_two_render_scenes`, `every_bundled_preset_has_at_most_one_scene`, `add_paste_duplicate_import_second_scene_refused_graph_and_undo_unchanged`, `two_scene_project_loads_unchanged_and_shows_notice`; flow counts reported as `N pass / M fail / 22 total + 2 new`. Negative — none.
- **Demo:** L3 — the two click-through flows; their failures are the bug list. Performer gesture: map a light's position to an LFO, check it stays on the card.
- **Forbidden:** fixing anything the click-through finds (beads only); inventing bugs not observed in a flow run; a D17 migration that deletes or rewires nodes.
- **Test scope:** `-p manifold-app -p manifold-node-engine -p manifold-editing`, plus the flow suite filtered to `scene`.

### P1 — The live lookup and the saved-control format (core, io, registration; no visible change)
P1 and P2 land together as one batch: P1 does not land until P2 moves the drawer (`EditParamMappingCommand`) onto `ParamChoices`, so no build drops a drawer rename on reload.
- **Entry:** P0 landed. Anchors: `instance_serde.rs:34`, `:47-63`, `:135-145`, `:155-196`, `:272-300`, `:356`; `params.rs:62-94`, `:167`; core `scene_exposure.rs:22-48`, `:381`; `preset_definition_registry.rs:340`; `effects/instance.rs:375`, `:413`; `manifold-app/src/bin/project_tool.rs`.
- **Read-back:** D2, D7, D13, sections 3.2, 3.4, 3.8; docs/archive/PARAM_STORAGE_DESIGN.md D6 (calibration). Restate where the join runs, what is saved per control, and the three old-project rules.
- **Deliverables:**
  - foundation `param_range.rs` and the `CardPin` enum (3.1).
  - core `param_lookup.rs` (3.2): `NodeParamSource`, registered by `manifold-nodes-scene` (D18), read through `node_params` and `stamped_node_params`.
  - The join in `gather_known_params` / `build_param_manifest`.
  - `ParamChoices`, `SpecSource`, `ParamSpecDef.limit` / `unit` / `scene` fields, and stub-parsable defaults (3.4). `Param.calibrated` is replaced by `choices`, and the calibration wire maps to the slider range with its key and shape unchanged.
  - `ParamEntryWire` gains `rename`, `section`, `pin`; `PresetInstance.folded_sections`.
  - Save writes id stubs for `Lookup` entries and no label on non-composite bindings.
  - The user-added conversion rule, and unresolved controls written back word for word.
  - Project format version bump.
  - `project_tool` moves to `crates/manifold-app/src/bin/project_tool.rs` with the registry installed. `manifold-io/tests/project_tool_cli.rs` moves with it, and its `scripts/dev.py` verb and `scripts/TOOLS.md` line are updated.
  - The light-title rung (D11), and the importer writes the GLB light name into `title` (`gltf_import/scene.rs:406`).
  - Tests: INV-S6, INV-S14, INV-S15, `every_bundled_and_fixture_param_resolves`, `old_scene_copies_are_ignored_then_dropped`, `user_exposure_label_becomes_rename_once`, `calibration_wire_round_trips_as_slider_choice`.
- **Gate:** positive:
  - The tests above.
  - `load_liveschool_live_show_v6` and `liveschool_roundtrip_preserves_addressing_sites` green, and a Liveschool save is byte-identical.
  - A dump of card row ids, names and ranges for physicsscene and gltfscene before and after P1. Every differing line is named in the report as a stale copy the join corrected (expected: curated and named exposures only, because pass 4 already corrected exact auto-stamps).
  - `project_tool info` on a scene project prints names.

  Negative — INV-S2.
- **Round trip:** save gltfscene and physicsscene, reload through `project_io`; card rows equal, and a driver armed before the save still modulates its row.
- **Demo:** none — L1 (deliberately no visible change).
- **Forbidden:** keeping pass 4 "as a safety net"; dropping a control because the lookup failed; a lookup call outside the join; changing any `ParamDef.label` (that is P5).
- **Test scope:** `-p manifold-foundation -p manifold-core -p manifold-io -p manifold-nodes -p manifold-nodes-scene -p manifold-app` (the saved format and the install cross these six) plus `-p manifold-io --test load_project`.

### P2 — Every reader on the lookup
- **Entry:** P1 landed; `rg -n 'pub fn describe' crates/manifold-core/src/param_lookup.rs` → 1. Re-run the 3.8 re-derivation command and record counts.
- **Read-back:** D2, D15 (the reshape rule only), section 3.8. Restate which readers already read the join and which go around it.
- **Deliverables:**
  - Every "P2" row of 3.8: routings, runtime reshape, group expansion and instance fallbacks read the manifest or `describe`.
  - Passes 2 and 4 and the `scene_scale` range writes are deleted.
  - `EditParamMappingCommand` writes `ParamChoices`; its undo snapshot becomes the previous choices.
  - The one sentence added to WIDGET_TREE_DESIGN.md INV-1 (D2).
  - Tests: INV-S16, INV-S17, `drawer_rename_is_a_choice_and_survives_reload`, `standalone_build_reshape_matches_manifest_build`.
- **Gate:** positive — the tests above; full `-p manifold-node-engine` and `-p manifold-editing` nextest green; graph-tool `validate` on every bundled scene preset unchanged. Negative — INV-S16; `rg -n 'Repair pass 4|fn apply_scene_scale' crates/` → 0.
- **Round trip:** rename a light's intensity row in the graph editor drawer and set its range, save, reload: name and range hold, its OSC address is unchanged, its automation lane shows the new name.
- **Demo:** L2 — automation lane header for a scene param shows the lookup name after a `ParamDef.label` edit on a scratch branch (revert after). Performer gesture: send OSC to a scene param, rename it, send again — same address moves it.
- **Forbidden:** a second resolver in node-engine; filling `BindingDef.label` in memory as a cache.
- **Test scope:** `-p manifold-core -p manifold-node-engine -p manifold-editing -p manifold-playback -p manifold-nodes-scene -p manifold-app`.

### P3 — Scene descriptors in the lookup (data, no visible change)
- **Entry:** P2 landed. Anchors: core `scene_exposure.rs:66`, `:432`; nodes-scene `scene_exposure.rs:37`, `:115-130`, `:184-189`, `:421`.
- **Read-back:** D3, D5, D13, sections 3.1, 3.3; docs/MATERIAL_INSPECTOR_DESIGN.md section 4.1 (manifest descriptors). Restate why the table carries no label.
- **Deliverables:**
  - foundation `scene_surface.rs` (3.1).
  - nodes-scene `scene_surface.rs`, covering every vocabulary param with role, group, band, unit and limit. Limits are declared but not yet enforced: `HardLimit` is read only in P8.
  - The provider fills `scene`, `unit` and `limit`, and drops Hidden params from stamping.
  - `is_retained_extra` reads `spec.scene` where it called `card_visible_for` (the function itself goes in P4).
  - Delete `card_visible_for`, the filter chain and `look_metadata`'s relabel.
  - Godfile ceilings (INV-S12).
  - Tests: INV-S1, `every_bundled_scene_row_has_one_owner_and_one_role` (using today's `owned_specs` rule).
- **Gate:** positive — the tests above; card row dump identical to P2's (zero lines). Negative — INV-S2; `rg -n 'card_visible_for|fn look_metadata' crates/` → 0.
- **Demo:** none — L1.
- **Forbidden:** a label field in `SceneParamDesc`; reading the table from `manifold-app`.
- **Test scope:** `-p manifold-foundation -p manifold-core -p manifold-nodes-scene -p manifold-nodes -p manifold-app`.

### P4 — Card on items and descriptors (the vertical slice)
- **Entry:** P3 landed; `rg -n 'fn scene_param_facts' crates/manifold-nodes-scene/src` → 1.
- **Read-back:** D3–D6, D11, D12, sections 3.5–3.6, 3.10; WIDGET_TREE section 5b (agent contract and enforcement). Restate the forbidden projection cases.
- **Deliverables:**
  - `scene_items.rs` (3.5) with `owner_of`; `SceneVm.camera_controls` / `world_controls` / `object_controls` replaced.
  - `scene_card.rs` (3.6) replacing `scene_performance.rs`, which is deleted along with the P0 stopgap.
  - `RowSpec.unit` and the value-readout suffix.
  - Force rows from recipe `scene` fields (Kick, Kick Size); light rows named per D11; camera rows per camera type; Look switches; no-physics dim.
  - The card header title truncates with an ellipsis before the Opacity control, fixing BUG-pmbd3.16 (card header name/Opacity collision) (INV-S13).
  - The water preset builder (`manifold-nodes-water/src/presets/gpu_flip.rs`) gives its mesher controls an authored Setup `scene` field, and the water presets are regenerated.
  - Matter's stiffness, cohesion and liveliness become Performance in the scene table (asserted by `matter_card_bands`).
  - Bokeh `max_radius` gets a role so D4's depth of field Max Blur row builds (the table marks it Hidden, after the stamp filter).
  - Open question, decided before rows are built: the GPU FLIP card controls the table marks Hidden (`solve_level`, `max_iterations`, `sheet_fill_rate`, `particle_capacity`). Under D5 they would also build no row in the Scene tab.
  - Ruling: those four are Setup, not Hidden. They show in the Scene tab and stay off the card.
  - The physics-world item owns its copy body and that body's transform (PhysicsBoxes), and Matter's `initial_column` transform gets an owner, emptying `OWNERLESS_UNTIL_P4` in `projection/scene.rs`.
  - `oceanscene` fixture (`physics_scene("Ocean", "Ocean")` in `fixtures.rs`).
- **Gate:** positive — INV-S4, S5, S7, S8, S13 tests; `scene_card::tests::{orbit_card_order, free_camera_card_order, physics_card_bands, water_card_bands, matter_card_bands, three_lights_three_named_rows, absent_dof_builds_no_row}`. Negative — INV-S3, INV-S11; `rg -n '"node\.|"orbit"|"intensity"' crates/manifold-app/src/ui_bridge/projection/scene_card.rs` → 0.
- **Demo:** L3 — `scripts/ui-flows/scene-card-bands.json` on physicsscene (assert section titles Camera, Physics, Lighting, Look in order, a "Count" row, no "Main Light"), `scene-card-free-camera.json` on oceanscene (Pan, Tilt present; no Distance), `scene-card-dof-switch.json` on gltfscene (click Depth of Field on, assert F-Stop appears; off, assert gone). Performer gesture: drag Orbit and Field of View, then fire a force's Kick.
- **Forbidden:** per-scene-type layout code; keeping `scene_performance.rs` beside the new module; reading `spec.section` for ownership.
- **Test scope:** `-p manifold-nodes-scene -p manifold-app -p manifold-ui`; flows `scene-card`, `scene-setup`.

### P5 — The naming pass (labels and units)
- **Entry:** P4 landed. Re-derive each D9 row with its command and record the hit count.
- **Read-back:** D1, D9; INV-S8, INV-S9. Restate: labels change where declared, nowhere else, and no rung is needed.
- **Deliverables:** every D9 label edit in `ParamDef`s, recipe JSON and bundled scene presets; units moved to `unit` (in the table for primitives, in the authored spec for JSON); "Max Iterations" kept; INV-S9 test; regenerate water presets (no water rungs, D13); White Furnace (`bake_environment` `uniform`) and RT Denoise Feed become Hidden in the scene table (their D9 role). For BUG-pmbd3.17 (naming pass).
- **Gate:** positive — INV-S8, INV-S9; Liveschool tests unchanged. Negative — `rg -n '"(FOV Y|Look Y|Exposure \(EV\)|Light Size|Copy Count|Simulation Speed|Initial Fill Height|Particle Capacity|RT Enabled|Appearance Gain|Azimuth Variation)"' crates/ -g '*.rs' -g '*.json'` → 0 outside `migrations/` history rungs.
- **Round trip:** a gltfscene project saved before P5 loads with new labels on card, panel and automation lane, with no migration; its OSC address and Ableton mapping still resolve.
- **Demo:** L3 — `scene-naming.json` on gltfscene: assert "Field of View", "Target Height", "Exposure" and value text ending " EV". Performer gesture: read the card at arm's length — no truncated label on a default-width card.
- **Forbidden:** a display-name map; renaming any `ParamDef.name` (the wire key); a label rung.
- **Test scope:** `-p manifold-nodes-scene -p manifold-water-gpu-flip -p manifold-water-gpu-mpm -p manifold-water-rigid -p manifold-nodes-image -p manifold-nodes -p manifold-app` (labels are declared across these crates).

### P6a — Scene tab host (shipped)
- **Scope:** the host move only; the panel content is unchanged. This is BUG-pmbd3.21 (Scene tab in the inspector replaces the Scene Setup dock).
- **Shipped:**
  - `InspectorTab::Scene`, placed after Layer, present only when the D16 layer is a generator whose graph holds a `render_scene` (`projection/inspector.rs` `inspector_tabs`); otherwise the inspector falls back to Layer. Once showing, the tab stays when the selection moves to another scene layer, as the dock did.
  - The panel builds into the inspector column below the tab strip (`InspectorCompositePanel::scene_content_rect`), inside the Inspector region. Its scroll clip keeps the name `scene_setup` for `under_name` selectors.
  - Deleted: the dock region, `layout.scene_setup_width` and its tween, the dock resize handle, `toggle_scene_dock`, the audio/scene dock exclusion, and the `DEFAULT/MIN/MAX_SCENE_SETUP_WIDTH` constants.
  - The Scene menu item, the header Scene button, the panel's close button and Escape toggle between the Scene and Layer tabs. A trigger response that lives in the scene panel and the `edit_selection.rs` light and object selection writes open the Scene tab.
  - The D17 notice reads "This layer has more than one scene; showing the first".
  - Every flow that opened the dock clicks the Scene tab (a "Scene" button under the `inspector` region). `scene-setup-empty-states` now asserts that non-scene layers have no Scene tab; the "New 3D Scene" empty state is unreachable from the tab.
- **Gate:** `scene_tab_present_only_for_scene_layer`, `scene_tab_falls_back_to_layer`, `two_scene_layer_shows_one_line_notice`; scene flows equal to the P0 reds. Negative — `rg -n 'toggle_scene_dock|scene_setup_width' crates/` → 0.

### P6b — Scene tab: tree, item header, groups
- **Entry:** P4 and P6a landed (items exist). `wc -l crates/manifold-ui/src/panels/scene_setup_panel.rs` recorded.
- **Read-back:** D8, D16, D17, section 3.7; SCENE_SETUP_PANEL_DESIGN.md section 7 (Decided — do not reopen) for outliner verbs still standing; P0's bead list for this panel.
- **Deliverables:**
  - `outliner.rs`, `item_header.rs`; icons; per-group add buttons with the Objects menu.
  - Physics group; World with Ray Tracing. `SceneSelection::Rendering` deleted; stats line deleted.
  - Item header with rename, kind, and copy and delete per D8. The force item header owns the name; the force card's own header keeps only enable, targets and trigger chips.
  - Properties grouped by `ControlGroup`; Hidden rows omitted unless attached (D6).
  - The panel's own "Scene Setup" title row and close button, and the header's Scene button, are removed or kept by decision here (section 3.7 deletes the header toggle).
  - `scene_setup_panel.rs` ceiling lowered.
  - Every scene flow that asserted the old tree is rewritten.

  This is BUG-pmbd3.15 (scene panel tree).
- **Gate:** positive — `scene_setup_panel::tests::tree_groups_in_fixed_order`, `add_buttons_one_per_group`, `header_names_item_once`; the flows the phase adds or edits, run by name, green. Negative — `rg -n 'Motion & Physics|Lighting & Environment|SceneSelection::Rendering|\\u\{1F4F7\}|\\u\{25A0\}' crates/manifold-ui/src` → 0.
- **Demo:** L3 — `scene-panel-tree.json` on gltfscene and physicsscene: select the layer, open the Scene tab, assert group order, select a light, assert header text equals its readable name once. Performer gesture: add a light from the Lights "+", rename it, see the new name on its card row.
- **Forbidden:** a new row or header widget outside the shared builders; keeping the old add row "for now".
- **Test scope:** `-p manifold-ui -p manifold-app`; only the flows the phase adds or edits, by name.

### P7 — Cross-surface gestures and fold persistence
- **Entry:** P4 and P6b landed, and UI_INVALIDATION P4b landed.
- **Read-back:** D6, D7, D10, section 3.9; `dropdowns.rs:1666-1731`; `parameter_navigation.rs` whole.
- **Deliverables:** scene panel rows emit `ParamLabelRightClick`; menu items Show on Card, Remove from Card (disabled on attached rows), Show in Scene tab; `SetCardPinCommand` with undo; section title click and Show in Scene tab switch the inspector to the Scene tab and reveal the owning item; the brief card-row highlight after Show on Card; `reveal_param`; `folded_sections` persistence and the D11 default fold; delete `reveal_clip_response`, `property_selection_for_row`, the `SceneSetupExposeParam` action and arm.
- **Gate:** positive — `card_pin::tests::{pin_unpin_undo_redo, remove_disabled_on_mapped_row}`, `reveal_param_selects_owning_item_for_light_object_force_world`. Negative — INV-S10; `rg -n 'SceneSetupExposeParam|reveal_clip_response' crates/` → 0.
- **Round trip:** pin a material colour row and unpin Orbit, fold Lighting, save, reload: pins and fold hold, a mapping on the pinned row still drives it.
- **Demo:** L3 — `scene-pin-roundtrip.json` (right-click Roughness in the panel → Show on Card → assert a card row; right-click it on the card → Remove from Card → assert gone), `scene-card-to-tab.json` (click the Lighting title → assert the inspector shows the Scene tab with a light selected). Performer gesture: mid-set, pin an object's Y position to the card and drive it.
- **Forbidden:** a second context menu; navigation by section string; pins stored anywhere but `ParamChoices`.
- **Test scope:** `-p manifold-editing -p manifold-ui -p manifold-app`.

### P8 — Slider range vs hard limit
- **Entry:** P7 landed (both surfaces emit the shared right-click), and UI_INVALIDATION P4c landed. `rg -n 'constrain_to_range\(' crates/ -g '*.rs'` → 4 non-test sites recorded.
- **Read-back:** D15, sections 3.1 (`HardLimit`), 3.8 (range rows), 3.9; docs/WIDGET_TREE_DESIGN.md section 5b (agent contract and enforcement). Restate the five behaviours (typing, drag, automation and modulation, OSC and MIDI, wraps and reshape).
- **Deliverables:**
  - `ParamSpecDef::clamp_value` replaces `constrain_to_range` at every site.
  - The OSC router maps 0–1 over the slider range and clamps to the limit.
  - The row widget's drag becomes relative to the press, with value clamped to `limit`, thumb pinned at the slider end, and an overflow mark. Typed entry clamps to the limit.
  - Automation lanes draw values past the span at the edge, with the mark.
  - "Set slider range…" and "Reset slider range" in the shared menu, on card and panel rows, with `SetSliderRangeCommand`; the popover reuses the drawer's range editor.
  - Scene-table limits take effect.
  - Tests: INV-S18, `set_slider_range_undo_redo`, `osc_full_sweep_covers_user_slider_range`.

  This is BUG-pmbd3.19 (slider range vs hard limit).
- **Gate:** positive — the tests above; `no_bespoke_row_infra` green. Negative — `rg -n 'constrain_to_range' crates/ -g '*.rs'` → 0; `rg -n '\.clamp\(0\.0, 1\.0\)' crates/manifold-ui/src/panels/param_slider_shared/row_host.rs` reduced to track-position math only (list each survivor with its reason).
- **Round trip:** set a slider range on a card row and on a panel row, save, reload: both hold; the old calibration of a Liveschool param loads as its slider range.
- **Demo:** L3 — `scene-slider-range.json` on gltfscene: right-click an object's Position X → Set slider range… → −1 to 1; drag past the right end; assert the value readout exceeds 1 and the row is unclamped. Performer gesture: mid-set, drag an object's position past the slider end and keep going; the value never clamps at the slider's end.
- **Forbidden:** auto-widening the slider; a card-only or panel-only range editor; the user's slider range reaching the reshape or the wrap period.
- **Test scope:** `-p manifold-foundation -p manifold-core -p manifold-playback -p manifold-editing -p manifold-ui -p manifold-app`.

### P9 — Light colour swatch on the card
- **Entry:** P7 landed. `rg -n 'ColourSwatch' crates/manifold-ui/src` recorded.
- **Read-back:** WIDGET_TREE_DESIGN.md section 5b (agent contract and enforcement) recipe; `material_inspector.rs:853-920`.
- **Deliverables:** the swatch moves into the shared row host (recipe steps 1–5); `RowRole::ColourSwatch` payload widens from `MaterialColour` to a `ColourGroup` enum adding `Light`; the light intensity row carries `rgb_members` of the light's colour params; the material inspector uses the shared swatch.
- **Gate:** positive — dispatch test: swatch gesture on a light row emits one undoable RGB write to the three colour ids. Negative — `rg -n 'fn .*swatch' crates/manifold-ui/src/panels/scene_setup_panel/material_inspector.rs` → 0; `no_bespoke_row_infra` green.
- **Demo:** L3 — `scene-light-swatch.json` on gltfscene: pick a colour on a light row, assert the row's colour value changed. Performer gesture: recolour the key light from the card.
- **Forbidden:** a card-only swatch copy.
- **Test scope:** `-p manifold-ui -p manifold-app`.

### P10 — Use this view
- **Entry:** P6b landed. ⚠ VERIFY-AT-IMPL: where the app's live viewport camera pose is readable from the UI thread (`rg -n 'SceneViewportPass' crates/manifold-app/src`); `viewport_session.rs:226` is the headless twin.
- **Read-back:** D14; `viewport_session.rs:1-27`.
- **Deliverables:** "Use this view" button in the Camera item header; `CopyViewportCameraCommand`; the orbit mapping as a pure function `orbit_from_view(eye, forward, current_look_y) -> OrbitParams` with tests.
- **Gate:** positive — `orbit_from_view` round trip: orbit params → eye/forward → back, equal within 1e-4; free-camera copy exact; one undo restores all params. Negative — none.
- **Demo:** L3 — `scene-use-this-view.json` on gltfscene: orbit the viewport, click the button, assert Orbit changed. Performer gesture: fly the viewport to a framing, click, then perform from that framing on the card.
- **Forbidden:** writing the viewport camera into the project on every move.
- **Test scope:** `-p manifold-nodes-scene -p manifold-editing -p manifold-app`.

---

## 6. Decided — do not reopen

1. Card = performance, Scene tab = setup; same items, same names (D1).
2. One live lookup read only by the manifest join. A saved project keeps per control only its id, value and user choices, and every reader uses the lookup. Unresolved controls are kept word for word, never dropped (D2).
3. OSC addresses are id-based; renames never move them (D2).
4. One scene item list; ownership by primary binding; both surfaces use it (D3).
5. Card bands Camera, Scene, Forces, Lighting, Look; no "Main Light"; no placeholders (D4).
6. Hide absent, dim inactive (D5). Attached rows always shown (D6).
7. User choices live only in `ParamChoices` and `folded_sections`; the join never writes them (D7).
8. Panel groups Camera, Lights, Objects, Forces, Physics, World; one add button per group (D8).
9. The D9 word list, including Brightness Variation, Break Apart, Coarse Solve, Max Iterations, FLIP Ratio, Level. Labels change where declared, with no rungs.
10. One right-click menu; navigation by param id (D10).
11. Every light gets a row; Lighting folds by default past eight (D11).
12. Row sets follow base values (D12).
13. Migration keeps structure only; water gets no rungs (D13).
14. Slider range and hard limit are separate. Values pass the slider end up to the limit. The user's slider range is a per-control choice set from the shared menu, and never reaches the reshape or the wrap period (D15).
15. The scene is a tab in the inspector, present only for a layer with a scene; no separate dock (D16).
16. One scene per layer: the validator rejects two, edits that would add a second refuse before undo, old two-scene projects load unchanged with a notice (D17).

## 7. Deferred

- **Master force strength** — Peter: "sounds like a lot of new work? Let's focus on what we have already." Revive when Peter asks for one fader over all forces.
- **Orbit camera lateral pan (target X/Z)** — new primitive params. Revive when Peter asks for pan on an orbit camera live.
- **Light duplicate** — no command exists. Revive when Peter asks to copy a light.
- **More performance rows for look-at and loop cameras** — revive when a show uses one of them.
- **Card pins on non-scene generator and effect cards** — the field is general after P1; only the menu items are scene-gated. Revive when Peter asks for pins outside scenes.
- **Hard limits for non-scene primitive params** — the mechanism is general after P8; only scene params and template JSON declare limits. Revive when Peter hits a capped effect param.
- **Compact three-value rows (position X/Y/Z on one line)** — predecessor section 5 (Decisions — resolved by Peter) item 4 parked it; still parked.
