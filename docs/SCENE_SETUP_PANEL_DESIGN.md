# Scene Setup Panel — compose, light, and dress a 3D scene without opening the graph

**Status: IN PROGRESS — P1 (column + discovery + Environment/Fog) + P2 (Objects section) SHIPPED 2026-07-17; P3–P5 not implemented. Sonnet-executable, orchestrated overnight. BUG-193 (no object/light remove command) and BUG-194 (vertex count not computable from def) opened as honest escalations, not blocking. · 2026-07-16 · Fable 5 (design session with Peter)**
**Prerequisites:** none — every substrate this design consumes is SHIPPED and verified in-tree
(SCENE_BUILD_AND_GROUP_PARAMS P1–P5, REALTIME_3D P1–P4/P8/P9, MATERIAL M1–M6, IMPORT_FIDELITY
F-P1–F-P7, GLB_CONFORMANCE, the audio-dock `ScreenLayout` column, `ChangeGraphParamCommand`).
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any
phase. Executor: Sonnet, orchestrated overnight. The orchestrator may consult ONE Fable
advisor agent, only on a critical blocking problem the doc does not foresee.

Peter's directives (2026-07-16, verbatim — these opened and decided the design):

- "It would be nice if the 3D pipeline had a sort of 'add effects' style system that we have
  for the 2D texture effects … you drag and drop a mesh or scene in and then you can click
  the add effect button and add different modifiers, different lighting options, different
  scenes, different modifiers and behaviours … higher level cards for these types of things
  rather than node granularity that most users will never want to get into."
- "A slide out column, using the same slide out and column infra as the audio settings panel,
  and that's your 'scene setup panel'. It's high level, simple, super intuitive, user
  friendly, and gives you all the tools … that a basic user needs to create strong, well
  designed, well composed, light, cinematic scenes."
- "Per layer. A scene can only live on a layer, each layer is only one scene. Keeps it simple
  and intuitive."
- "Yes, it's a view. I only want a single source of truth … No rotting, no staleness … No
  hacks, no bandaids, no stop gaps."
- "A scene should also let me drop multiple GLBs into a single scene so I can create custom
  environments etc. We will likely need sensible hierarchies, groupings, etc."
- "The most important thing from all of this is it needs to stay user friendly and not become
  overly complex like the graph editor backend layers. Users need to just be able to
  understand and use this system to stay creative and in flow state."

**The governing insight: the scene model this panel needs already shipped — what is missing
is the surface.** A 3D scene in MANIFOLD is a generator-layer's `PresetInstance` graph
containing `node.render_scene`; the generator instance (graph included) lives ON the `Layer`
(`layer.rs:131`, `gen_params`), so "one scene per layer" is the shipped ownership model, not
a new one. Per-object identity lives in named node groups (glTF importer + SCENE_BUILD);
per-object transforms are `node.transform_3d` atoms; lights, camera, atmosphere, environment
are all nodes with params; every mutation already routes through undoable `EditingService`
graph commands addressed by `(scope_path, node doc id)`. This design adds **zero new model
state and zero new mutation paths**: the panel is a fourth surface over the same params
(card, node face, group face — SCENE_BUILD D6's "one value, three surfaces" — and now the
dock), plus three genuinely new pieces: the dock column itself, a merge-import path
("drop a second GLB into this scene"), and the per-object **modifier stack** gesture.

Binding constraints (DESIGN_AUTHORING §1): **performance surface** (every panel control is a
live, bindable graph param — MIDI/OSC/audio-mod reach them through the existing expose
machinery, nothing new owed); **persistence** (all edits land in the graph def / params that
already serialize — one round-trip gate per phase, no migration needed anywhere);
**hot path**: untouched — the panel is UI-thread display + command dispatch; the content
thread render is byte-identical with the panel open or closed. Thread residency: house model,
unchanged (`state_sync` view-models in, commands out).

**Supersessions (applied to the other docs in this landing):**
- SCENE_BUILD_AND_GROUP_PARAMS_DESIGN §7 item 7 ("Scene-object list panel: killed … a panel
  would be a second authoring model") is **superseded** by Peter's 2026-07-16 directive. The
  ground has shifted since that kill: the panel is NOT a second authoring model — it emits
  the identical commands the card/group-face emit (the same defense that doc's own D6 used
  to add the *third* surface). REALTIME_3D decided-#1 ("no scene-document format, no second
  authoring model") is **upheld**, not reopened: there is still no scene document; the graph
  stays the only model.
- REALTIME_3D_DESIGN §8's "Hierarchy panel … revisit only with strong user pull" — the pull
  arrived, from Peter. Its **P7 (scene starter preset)** is absorbed here as P1's
  `Scene Starter.json` deliverable; REALTIME_3D's status line gains a pointer at landing.

Companions: `SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` (the substrate: Transform port, named
groups, add-object/add-light commands, card sections), `REALTIME_3D_DESIGN.md` (render_scene,
shadows, fog, instancing, PCSS; its P5 viewport / P6 gizmos are SIBLING work — not touched
here), `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` (the dock column precedent this
copies), `IMPORT_FIDELITY_DESIGN.md` + `GLB_CONFORMANCE_DESIGN.md` (the import look and the
importer this extends), `MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md` (the modifier atoms),
`GLTF_ANIMATION_DESIGN.md` (animation nodes appear in scenes; panel shows them as custom
rows in v1 — §9 Deferred).

---

## 1. Audit — what exists (verified 2026-07-16, this session, tip `6fb1714d`)

Instruction to executors: **extend, don't redesign.** Re-verify anchors at phase entry; a
moved line is fine, a missing symbol is an escalation.

| Piece | Where | State |
|---|---|---|
| Scene ownership is per-layer already | `crates/manifold-core/src/layer.rs:131` — `gen_params: Option<PresetInstance>` (graph override on the instance, accessor `Layer::generator_graph`) | SHIPPED. "One scene per layer" needs no model change |
| Clips are thin (timing + source refs), carry no param state | `crates/manifold-core/src/clip.rs:10-90` | Confirms: clips gate WHEN the layer's generator shows; the scene itself is layer-owned |
| Dock column pattern | `crates/manifold-ui/src/layout.rs:26` (`audio_setup_width`), `:110` (`content_area` subtraction), `:187` (`audio_setup()` rect), `AnimF32` tween `:51`, `DEFAULT_AUDIO_SETUP_WIDTH` in `color` | SHIPPED (audio dock P1). The exact precedent — copy the field/rect/handle/snap-back/Escape wiring |
| `state_sync` as the panel's sole data boundary | audio dock design §5 item 8; `manifold-app/src/ui_bridge/state_sync.rs` (AudioSendRow builder precedent) | SHIPPED rule. The scene panel gets `SceneVm` rows the same way |
| Graph param edit command, shared by card + group face | `ChangeGraphParamCommand` path (SCENE_BUILD P3/P4 — "one value, three surfaces"); command family carries `.with_scope(scope_path)` (`manifold-editing/src/commands/graph.rs:145-176` et al.) | SHIPPED. The panel is the fourth surface, same command |
| Per-object identity: named/tinted groups; transform atom inside; stable inner `node_id`s | `gltf_import.rs:487-501` (`build_import_graph` doc), SCENE_BUILD D9; flatten at load (`manifold_core::flatten::flatten_groups`) | SHIPPED |
| One-click add gestures | `AddSceneObjectCommand` + `AddSceneLightCommand` (+ node-face buttons), SCENE_BUILD P5 | SHIPPED — the panel's "+ Object"/"+ Light" buttons dispatch THESE, not new commands |
| Group rename sweeps card sections | SCENE_BUILD P3 rename-sweep command | SHIPPED — panel rename dispatches it |
| `node.render_scene` | `primitives/render_scene.rs` — ports `camera` (required), `atmosphere`, `envmap` (via importer), `light_k`, per object `mesh_k`/`material_k`/`base_color_map_k`/`transform_k`/`instances_k`; params `objects`/`lights` only | SHIPPED (P1–P3, P8, P9) |
| Light atom params | `primitives/light.rs:143-199` — `intensity`, `cast_shadows`, `shadow_softness` (enum incl. Contact/PCSS), `light_size`, pos/aim/color/mode | SHIPPED |
| Camera atoms | `node.orbit_camera` (`orbit`/`tilt`/`distance`/`fov_y`/`near` — importer stamps them, `gltf_import.rs:673-682`), `node.free_camera`, `node.look_at_camera` | SHIPPED |
| Atmosphere | `PortType::Atmosphere` + `node.atmosphere` (fog color/density/height falloff/ambient tint) | SHIPPED (REALTIME_3D P3) |
| Environment chain (importer shape) | `gltf_import.rs:624-671` — `node.bake_environment` (`intensity`/`mode`/`fill`/`emitter_intensity`) + `node.hdri_source` (`hdri_file` string binding) + `node.exposure` gain + `node.switch_texture` selector → render_scene `envmap` | SHIPPED (F-P4/F-P7, GLB_CONFORMANCE D6) |
| Mesh-modifier atoms (mesh → mesh) | `node.bend_mesh`, `node.twist_mesh`, `node.taper_mesh`, `node.push_along_normals`, `node.push_mesh` (texture displacement — type_id is `node.push_mesh`, file `displace_mesh.rs`), `node.morph_mesh`, `node.rotate_3d` | SHIPPED — the modifier-stack vocabulary. Re-derive at P5 entry: `rg -l "Array<MeshVertex>" crates/manifold-renderer/src/node_graph/primitives/` and keep only single-mesh-in/mesh-out atoms |
| Import assembler is a pure function | `gltf_import.rs:482` `assemble_import_graph(path) -> (EffectGraphDef, ImportReport)`; split `build_import_graph(&summary, path)` testable on synthetic summaries; node ids allocated by a local `fresh_id` counter from 0 | SHIPPED. The merge path (D5) reuses `gltf_load::gltf_import_summary` + a new merge assembler; id-offsetting is required |
| App-side import entry | `Application::import_model_file` (manifold-app file-drop handler; `gltf_import.rs:26` doc) | SHIPPED — P4 adds a second entry that targets an existing scene |
| Mesh sources reference the .glb by path param | `gltf_import.rs:57-63` — `model_file` card binding → source node `path` string; `hdri_file` likewise | SHIPPED. Assets do NOT embed in the project file (§9 Deferred — real, known cost) |
| Held-out fixtures on disk | `tests/fixtures/gltf/abandoned_warehouse_-_interior_scene.glb`, `skull_salazar_downloadable.glb`, `the_rosetta_stone.glb` (untracked, gitignored-class fixtures) | Present in the main checkout — P4's held-out merge gate uses two of them |
| Selected-layer scoping for inspector surfaces | inspector panels scope to the selected layer via `state_sync` today (AUDIO TRIGGERS section precedent, audio dock P3b) | SHIPPED pattern. ⚠ VERIFY-AT-IMPL (P1): the exact selection accessor `state_sync` uses — `rg -n "selected_layer" crates/manifold-app/src/ui_bridge/state_sync.rs` — transcribe, don't invent |
| Generator assignment to a layer (for "New 3D Scene") | the generator picker's command path | ⚠ VERIFY-AT-IMPL (P1): `rg -n "SetGenerator\|AssignGenerator\|gen_params" crates/manifold-editing/src/commands/ -l` then read the command the picker dispatches — the empty-state button dispatches THAT command with the starter preset's type id |
| Bundled preset loader + `graph_tool` | `crates/manifold-renderer/assets/generator-presets/`; `graph_tool validate --kind generator` / `fusion` | SHIPPED — the starter preset ships and validates through these |

Negative claims, searches run 2026-07-16: no scene/dock panel exists (`rg -l "scene_setup" crates/` → 0);
no per-object visibility mechanism exists on `render_scene` (no `visible_k` port/param — §9);
no insert-node-into-wire composite command exists in `manifold-editing` (P5 builds one);
no per-scene frame-cost attribution exists (§9).

## 2. Decisions

**D1 — A scene IS the generator-layer's preset graph; the panel is a view, never a model.**
The panel's subject is the selected layer's `gen_params` instance graph, specifically the
subgraph reachable around its `node.render_scene`. There is no scene document, no panel-owned
state beyond UI chrome (fold/scroll), no apply step — every control reads a view-model built
from the same `Arc<Project>` snapshot everything else reads, and writes by dispatching the
same `EditingService` command the perform card emits for that param. Peter, verbatim: "it's a
view. I only want a single source of truth." Rejected: a `Scene` struct on `Layer` mirroring
the graph (two homes for one fact — the exact rot Peter banned); panel-local staging with an
apply button (staleness by construction). This supersedes SCENE_BUILD §7 item 7 (see header)
and upholds REALTIME_3D decided-#1.

**D2 — The panel is a `ScreenLayout` column, cloned from the audio dock.** New field
`scene_setup_width: f32` (0.0 = closed; default `DEFAULT_SCENE_SETUP_WIDTH = 400.0`, a
sibling constant of `DEFAULT_AUDIO_SETUP_WIDTH`), computed `scene_setup()` rect, subtraction
in `content_area()`, resize handle + double-click snap-back + `AnimF32` tween + Escape-close,
all pattern-copied from `audio_setup_width` (`layout.rs:26/:110/:187`). Toggled by a header
"Scene" button beside the Audio button. **The two utility columns are mutually exclusive:**
opening one animates the other closed (a plain either/or toggle, not conditional visibility —
both buttons always exist). *Consequences, stated honestly:* you cannot calibrate audio
triggers and dress a scene in the same glance; these are different activities (calibration
loop vs set dressing), and two open columns beside a 500px inspector would crush the preview
below usefulness on a laptop. Rejected: both-open-at-once (unusable content area, and it
re-raises the width-fallback question D1 of the audio dock already killed).

**D3 — Discovery: the view-model is a pure function of the graph def, with committed trace
rules and graceful degradation.** `SceneVm::from_def(&EffectGraphDef) -> Option<SceneVm>`
(new module, lives beside the other view-model builders feeding `state_sync`; pure,
unit-testable on synthetic defs — no GPU, no registry lookups beyond type_ids):

- **Scene root** = the first `node.render_scene` in the def whose output reaches the graph
  output (walk wires backward from the output node; the liveness notion the canvas already
  computes). More than one live `render_scene` → target the first by doc id and show a
  static header chip "2 scenes in this graph — showing the first" (no picker in v1, §9).
  None → no scene (empty state per D7).
- **Objects**: for `k in 0..objects`, trace `mesh_k`'s wire to its producer. Producer is a
  group output → the object row is that group: name/tint from the group, mesh source = the
  inner producer chain's head, transform = the `node.transform_3d` feeding `transform_k`
  (traced independently; SCENE_BUILD D9 places it in the same group but the trace must not
  assume it), material = `material_k`'s producer when it is one of the four material atoms,
  modifier stack = the chain of single-mesh-input/mesh-output nodes between the mesh source
  and the group output (in wire order). Producer is NOT a group output → the row renders as
  **"Object k — custom (edit in graph)"** with whatever members DID resolve (transform row if
  `transform_k` traces to a `transform_3d`), the rest read-only labels. Nothing is ever
  hidden and nothing errors: a hand-built scene degrades to labeled rows, never to a blank
  or a lie.
- **Lights**: for each wired `light_k`, producer `node.light` → editable row (mode, color,
  intensity, pos/aim, `cast_shadows`, `shadow_softness`, `light_size`); anything else →
  custom row.
- **Camera**: `camera` port producer — `node.orbit_camera` → orbit/tilt/distance/fov rows;
  `node.free_camera` / `node.look_at_camera` → their params; a lens/other node in between
  (the importer inserts a physical-lens stage) → trace THROUGH single-camera-in/camera-out
  nodes to the emitting atom and show the lens node's own row beneath (fov/f-stop as
  labeled sliders when the atom matches, custom row otherwise).
- **Environment**: `envmap`-port chain. Importer shape (`switch_texture` selecting
  `bake_environment` vs `hdri_source`→`exposure`) → Mode enum + Intensity + Fill +
  Strips + HDRI file row (Browse dispatches the existing `hdri_file` string-binding write).
  Bare `bake_environment` → Intensity/Fill. Otherwise custom row. Unwired → "None" +
  an "Add environment" action (spawns `node.bake_environment` wired to `envmap`, one
  composite command shaped like `AddSceneLightCommand`).
- **Atmosphere**: `atmosphere` port → `node.atmosphere` params (density, color, height
  falloff, ambient tint); unwired → "Add fog" action (same composite shape).
- Every editable row carries its write address: `(scope_path, node_doc_id, param_id)` —
  the exact addressing the graph command family takes (`graph.rs .with_scope`). The Vm is
  data; the panel renders it and dispatches.

**The plausible-wrong architecture, forbidden by name:** you will want the panel to keep its
own copy of scene values and reconcile ("cheaper than re-walking the def") — no. The Vm is
rebuilt from the snapshot exactly like every other `state_sync` row set; staleness is
impossible by construction, which is the entire point (Peter: "no rotting, no staleness").
You will also want to make discovery registry-driven-generic ("any node with params shows
rows") — no: that rebuilds the graph editor in a column, the exact complexity Peter banned.
The panel knows the curated vocabulary above and degrades to labeled custom rows for
everything else.

**D4 — The panel's sections and controls (committed inventory).** Top to bottom, all
collapsible, fold state UI-local (workspace state, like card sections — not serialized):

| Section | Rows (v1, committed) | Writes to |
|---|---|---|
| **Header** | Scene name (= generator display name, read-only in v1) · object/light/vertex counts + shadow-caster count (static, from the Vm — the honest cheap cost line) | — |
| **Objects** | Per object: name (editable → group rename command, which already sweeps card sections) · transform (pos/rot/scale, 3 compact triplets) · material quick knobs (base color, metallic, roughness when `pbr_material`; the atom's own params otherwise) · modifier stack (P5: list + add/remove/reorder) · remove (delete-group + decrement, the existing path). Buttons: **+ Object**, **+ Light** (existing commands), **Import Model…** (P4) | `ChangeGraphParamCommand` at each row's address; composite commands for add/remove |
| **Lights** | Per light row per D3 · **+ Light** | same |
| **Environment** | Mode (Softbox/HDRI) · Intensity · Fill · HDRI file Browse | same + the `hdri_file` string binding write |
| **Fog** | Density · Color · Height falloff · Ambient tint | same |
| **Camera** | Per D3 (orbit: Orbit/Tilt/Distance/FOV; free: pos/euler/fov; look-at: pos/target/fov) | same |

Sliders/steppers/color rows reuse the existing widget vocabulary (`param_slider_shared`
et al.) — **no new widget kinds** (the audio-dock audit rule carries over). Params whose
graph port is wired (driven by an LFO etc.) render read-only with the same driven styling
the group-face rows use (`apply_driven_state` precedent) — the panel never fights the graph.

**D5 — "Import Model…" merges a second (third, nth) GLB into the selected scene.** New pure
assembler in `gltf_import.rs`:
`merge_import_into_graph(def: &EffectGraphDef, summary: &GltfImportSummary, path: &Path) -> Result<MergePlan, String>`
where `MergePlan` lists: new nodes (the incoming asset's object groups ONLY — mesh source /
material / texture / transform_3d per material, exactly the shape `build_import_graph` emits
per object; **no camera, no envmap, no lights, no lens** — the target scene owns its chrome),
new wires, the new `objects` count, and the card-spec additions (per-object knobs sectioned
by object name, same as import). Node doc ids are allocated **above the def's current max
id** (the merge twin of `fresh_id`); group names collide → " 2"-suffix like the importer's
existing label convention. A composite `ImportModelIntoSceneCommand` (manifold-editing,
shaped like `AddSceneObjectCommand`: one undo unit) applies the plan. **Scale sanity,
decided:** the incoming asset keeps its native units; its seeded `transform_3d` gets
`pos = -center` (the importer's own recenter convention) — and **iff** the incoming bbox
radius differs from the scene's reference radius (the largest existing object's) by more
than 10× in either direction, the seeded `scale` is `ref_radius / incoming_radius` so the
asset arrives visible and grabbable instead of invisible or engulfing. The normalization is
an ordinary, visible, undoable value on the object's own transform — never hidden state.
Rejected: always-normalize (breaks deliberately-scaled kits); never-normalize (a
meters-authored prop dropped into a centimeters warehouse is a thousand-fold error the user
must debug blind — the exact BoomBox/near-plane class of failure, `gltf_import.rs:569-594`).
Rejected: merging by re-running `assemble_import_graph` and splicing defs wholesale
(duplicates chrome, collides ids — the hurried implementer's move, forbidden by name).

**D6 — The modifier stack is graph splicing inside the object's group, with a curated
vocabulary.** "Add modifier" on an object row opens a fixed list (display name → type_id):
Bend (`node.bend_mesh`), Twist (`node.twist_mesh`), Taper (`node.taper_mesh`), Inflate
(`node.push_along_normals`), Displace by Texture (`node.push_mesh`), Morph (`node.morph_mesh`),
Rotate (`node.rotate_3d`) — re-derived and confirmed at P5 entry (§1 command). Three new
composite commands in `manifold-editing/commands/graph.rs`, each one undo unit, shaped like
`AddSceneObjectCommand`:
- `InsertMeshModifierCommand { scene addr, object k, type_id, position }` — create the node
  inside the object's group and splice it into the mesh wire at `position` (0 = just after
  the source; default = end of stack, just before the group output).
- `RemoveMeshModifierCommand` — unsplice + delete, re-joining the wire.
- `MoveMeshModifierCommand` — reorder within the stack (unsplice + resplice).
The stack the panel shows IS the wire chain (D3's trace) — no stored list, so graph edits
made in the editor and stack edits made in the panel are the same facts. Modifier rows show
the atom's own params (amount/axis/center …) as ordinary rows. An object whose mesh chain
the trace couldn't parse shows "custom chain — edit in graph" and the add button disabled
for that object (never a blind splice into a topology we didn't understand).
**Instrument meaning:** this is Peter's opening ask made literal — the "add effect" button
for 3D. Wire an LFO to a Bend amount in the graph later, or expose it to the card: the
modifier params are ordinary node params from birth.

**D7 — Empty states are designed states, and "New 3D Scene" ships a starter preset.**
Selected layer is a generator layer with a live scene → the full panel. Generator layer, no
`render_scene` in the graph → "This generator has no 3D scene" + an **Open Graph Editor**
button (the existing open-editor action). Generator layer with NO generator, or an empty
slot → two buttons: **New 3D Scene** (assigns the bundled `Scene Starter` generator preset
via the existing generator-assignment command — §1 VERIFY marker) and **Import Model…** (the
existing file-drop import path, targeted at this layer). Video/audio/group layer or nothing
selected → one sentence naming what to select. The panel column itself never conditionally
vanishes while open (`feedback_no_conditionally_visible_ui`).
`Scene Starter.json` (new bundled generator preset, absorbs REALTIME_3D P7): grid floor +
one cube on it (`generate_grid_mesh` + `triangulate_grid`, `generate_cube_mesh`), each in a
named group with `transform_3d` + `phong_material`, sun light (SCENE_BUILD D7a defaults,
`cast_shadows` on) + soft fill point light, softbox environment at the import-fidelity
defaults, subtle fog, orbit camera framing the cube — good at defaults, validated by
`graph_tool validate --kind generator` and `graph_tool fusion` in the build.

**D8 — Scope fence (what this panel refuses, permanently).** No mesh editing, no UVs, no
shader or node authoring, no wire editing beyond the D6 splices, no viewport (REALTIME_3D
P5/P6 own navigate + gizmos — when they land, the panel and viewport are siblings over the
same params), no physics (BOX3D_PHYSICS owns it), no splat controls (GAUSSIAN_SPLATS owns
them), no per-instance scattering UI (producer params are card/graph territory), no
animation timeline (GLTF_ANIMATION owns playback; its nodes appear as custom rows in v1).
Peter: the panel must not "become overly complex like the graph editor backend layers" —
every future control proposal must name which existing control it replaces or why a basic
user needs it; otherwise it belongs in the graph editor.

## 3. What it buys on stage

Drop a skull GLB on a layer: it arrives lit and framed (import fidelity). Open Scene Setup:
rename it "Hero", nudge its scale, add a Twist modifier and leave the amount at zero, add a
rim light behind it, raise the fog. Drop the warehouse GLB into the SAME scene: it lands
recentered and sanely scaled; the skull now sits inside an environment with correct occlusion,
shared shadows, shared fog. Total graph editor visits: zero. Every value just touched is an
ordinary graph param — expose Twist amount to the card, bind it to a filter sweep, and the
statue wrings itself on the drop. The panel is set dressing; the card + MIDI map stay the
performance surface; the graph editor remains the deep end for the one user in fifty who
wants it.

## 4. Invariants & enforcement

| Invariant | Enforcement (machine check, named) |
|---|---|
| The panel introduces no new mutation path: every write is an existing command type or one of the five named composites (env/fog add, D5 import, D6 ×3) | Negative gate each phase: `rg -n "MutateProject\|Arc<Mutex\|Arc<RwLock" crates/manifold-ui/src/panels/scene_setup_panel.rs` → 0; command-equality unit test (below) |
| One value, four surfaces: a panel slider emits the identical command struct the perform card emits for the same param | Unit test asserting constructor equality against the card path (the SCENE_BUILD P4 gate, re-pointed) — `scene_panel_slider_emits_card_identical_command` |
| `SceneVm` is a pure function of the def (no staleness possible) | It takes `&EffectGraphDef` only; unit tests on synthetic defs (`scene_vm_*` in the Vm module); negative gate: `rg -n "Project\b" <vm module>` → 0 hits outside the builder's snapshot walk in state_sync |
| Merged graphs are flatten-clean and validator-clean | P4 gate runs `graph_tool validate` + `graph_tool fusion` on a merged def written to a temp file; flatten-equivalence test on a synthetic merge |
| Show path never pays: content render byte-identical with panel open | P1 gate diffs the headless show render with `scene_setup_width` 0 vs 400 (the REALTIME_3D D9 proof pattern) |
| No new port types, no model fields, no serialization change | Negative gates: `rg -n "PortType::" crates/manifold-renderer/src/node_graph/ports.rs` unchanged; `git diff --stat crates/manifold-core/src/layer.rs` empty across the wave (except none — layer.rs is not touched) |

## 5. Phasing (Sonnet, one session each; orchestrated overnight)

Forbidden, all phases: panel-owned scene state · direct `Project` reads from `manifold-ui` ·
new widget kinds · new port types · touching `render_scene`'s runtime/shader · touching
`layer.rs` · wiring the panel to the graph-editor canvas internals · widening into viewport,
gizmos, physics, splats, animation · keeping any "old path" alive when a phase replaces one
(none should exist — this design is additive) · `Arc<Mutex>` anywhere.

Landing: per `.claude/GIT_TREE_DISCIPLINE.md` §2 — one warm worktree for the workstream,
batch landings per 2–3 phases (P1+P2, P3+P4, P5), full workspace sweep + clippy at each
landing in the main checkout, landing reports per DESIGN_DOC_STANDARD §8.10.

### P1 — The column + discovery + Environment/Fog live (the vertical slice)

- **Entry state:** clean worktree off current `origin/main`; §1 anchors re-run (notably
  `layout.rs` audio-dock trio, `ChangeGraphParamCommand`, the two VERIFY markers: selection
  accessor + generator-assignment command — resolve BOTH and restate before code).
- **Read-back:** this doc §2 D1–D4/D7, §4; audio dock design D1 + §3.5 (the column recipe);
  SCENE_BUILD D5/D6 (sections, the card-identical-command rule); `layout.rs` whole;
  `gltf_import.rs:604-700` (the chrome shapes D3 traces).
- **Deliverables:** `scene_setup_width`/`scene_setup()`/`content_area()` subtraction/handle/
  snap-back/Escape/header Scene button + mutual exclusion with the audio column; layout unit
  tests (both columns; exclusion; zero-width byte-identity); `SceneVm` module + `from_def`
  with the FULL D3 trace (objects/lights/camera/environment/atmosphere/modifier chain) and
  unit tests on synthetic defs (importer-shaped, hand-built/custom, empty, two-render_scene);
  `scene_setup_panel.rs` rendering Header + Environment + Fog sections live (sliders dispatch
  through the fourth-surface path; driven params read-only) and the D7 empty states incl.
  **New 3D Scene** + `Scene Starter.json` (graph_tool-validated) + **Open Graph Editor**;
  the two "Add environment"/"Add fog" composites; `scene_panel_slider_emits_card_identical_command`.
- **Gate.** *Positive:* `-p manifold-ui --lib` + `-p manifold-app --lib` focused;
  `check-presets` clean (starter preset); L3 flow (`scripts/ui-flows/`): open panel via
  header button on a layer holding the imported-scene fixture, drag Fog density, assert the
  param value changed and undo restores it; round-trip: edit env intensity → save → reload →
  value persists AND the panel re-shows it (create+reload, per the standard). *Negative:*
  §4's rg gates; show-render byte-identity diff (panel open vs closed). **Demo (L3 + PNGs):**
  the flow above + headless PNGs of every D7 empty state and the full panel on a real
  imported scene — affordance check per standard §5 (buttons read as buttons).
- **Performer gesture:** select the layer mid-prep, ride Fog density with the mouse, watch
  the preview haze move — no graph editor open.
- **Forbidden:** starting Objects/Lights UI (P2/P3); a generic param-tree renderer (D3's
  named wrong turn).
- **Test scope:** focused crates above; sweep at the P1+P2 landing.

### P2 — Objects section (list, transforms, materials, rename, add/remove)

- **Entry state:** P1 landed; `AddSceneObjectCommand`/`AddSceneLightCommand` + group-rename
  command anchors re-verified.
- **Read-back:** D4's Objects row; SCENE_BUILD D6/D7/D7a + P3's rename-sweep; the group-face
  row dispatch site (transcribe the command construction — same one here).
- **Deliverables:** Objects section — per-object collapsible row: editable name → rename
  command; transform triplets; material quick knobs per D3; remove (delete-group + decrement
  composite, the existing path — ⚠ VERIFY-AT-IMPL: the exact existing removal command,
  `rg -n "delete.*group\|RemoveScene" crates/manifold-editing/src/commands/graph.rs`);
  "+ Object"/"+ Light" buttons dispatching the existing commands; custom-object degraded
  rows; header counts row (objects/lights/vertices/shadow casters from the Vm).
- **Gate.** *Positive:* Vm + panel unit tests (rows for the azalea-shaped synthetic def;
  rename emits the sweep command; add-object button emits `AddSceneObjectCommand`); L3 flow:
  click "+ Object", assert a new object row appears AND the preview shows the placeholder
  cube (PNG pair); undo restores both. *Negative:* §4 gates. **Demo (L2):** PNG of the panel
  on the real warehouse import — named object rows with transforms, read by the orchestrator.
- **Performer gesture:** rename "Material.001" to "Skull", drag its Y position — the object
  lifts in the preview and the card section follows the rename.
- **Test scope:** focused `-p manifold-ui -p manifold-app -p manifold-editing --lib`;
  sweep + clippy at the P1+P2 landing.

### P3 — Lights + Camera sections

- **Entry state:** P2 landed.
- **Read-back:** D3 lights/camera traces; `light.rs:143-199` param ids; the three camera
  atoms' param sets; REALTIME_3D D4 (caster cap `K=4`) + P9 (`light_size` only meaningful on
  Contact softness).
- **Deliverables:** Lights section (rows per D3/D4; `shadow_softness` as the same stepper the
  importer card uses — EnumRound binding precedent, `gltf_import.rs`; `light_size` row shown
  only as the sub-row of Contact softness — that's parameter dependency, not conditional UI:
  the stepper is always present); Camera section for all three atoms + lens pass-through
  trace; custom rows for anything else.
- **Gate.** *Positive:* Vm tests (each camera atom shape; light row param addresses); L3
  flow: drag a light's intensity, assert value + undo; PNG pair proving a `cast_shadows`
  toggle visibly changes the preview shadow. *Negative:* §4 gates; a scene with >4 casters
  still renders every light lit (no panel-side cap enforcement — the cap is the renderer's,
  the panel only reports the count).
- **Performer gesture:** two-finger the sun's elevation and watch the long shadows come up —
  the "golden hour" move, one slider.
- **Test scope:** focused; no sweep (mid-batch).

### P4 — Import Model into scene (merge) + normalize

- **Entry state:** P1 landed (panel exists to host the button); `assemble_import_graph` /
  `build_import_graph` anchors re-verified; the three held-out .glb fixtures present in the
  main checkout (orchestrator runs this phase's demo there).
- **Read-back:** D5 whole; `gltf_import.rs` end-to-end (the per-object group emission is the
  code being reused — read, don't reimplement); `Application::import_model_file`;
  GLB_CONFORMANCE D4 (1:1, `OBJECT_SAFETY_MAX`, loud errors).
- **Deliverables:** `merge_import_into_graph` + `MergePlan` (pure, unit-tested on synthetic
  summaries: id offsetting above max id, chrome skipped, name-collision suffixing, objects
  bump, card-spec sections extended, the 10× normalize rule incl. both boundaries);
  `ImportModelIntoSceneCommand` (one undo unit); the panel "Import Model…" button → file
  dialog → command (reusing the app's existing open-file plumbing); `OBJECT_SAFETY_MAX`
  enforced on the POST-merge total with the importer's loud-error posture.
- **Gate.** *Positive:* merge unit tests above; flatten-equivalence + `graph_tool validate`
  + `graph_tool fusion` on a real merged def (azalea + cube starter); **held-out gate:** merge
  `skull_salazar_downloadable.glb` INTO the imported `abandoned_warehouse_-_interior_scene.glb`
  scene — loads, renders, both visible in one frame (headless PNG), occlusion correct
  (skull placeable behind a warehouse column — second PNG from a second camera value);
  round-trip: save → reload → merged scene intact, undo removes the entire merge as one step.
  *Negative:* `rg -n "assemble_import_graph" <merge call path>` shows the merge does NOT call
  the whole-graph assembler; no camera/envmap/light node ids in any `MergePlan` fixture.
  **Demo (L2):** the two PNGs, read by the orchestrator.
- **Performer gesture:** drag a second .glb onto the button; the prop lands in the
  environment, correctly lit, correctly shadowed, one undo away from gone.
- **Forbidden:** re-running the full importer and splicing defs; silent truncation anywhere;
  normalizing inside mesh data (the transform param is the only home —
  `feedback_fix_asset_transforms_in_graph_not_mesh_files`).
- **Test scope:** focused `-p manifold-renderer --lib` (assembler) + `-p manifold-editing
  --lib` + `-p manifold-app --lib`; sweep + clippy at the P3+P4 landing.

### P5 — Modifier stack (closes the wave)

- **Entry state:** P2 landed; modifier vocabulary re-derived (§1 command) and reconciled
  against D6's list — a missing atom is a list edit, a signature surprise is an escalation.
- **Read-back:** D6 whole; `AddSceneObjectCommand`'s composite/undo shape; the wire-splice
  topology rules in `NODE_GROUPS_DESIGN.md` (inner wires + group boundary constraints);
  DECOMPOSING_GENERATORS §2.5 (confirm: zero new primitives here — the stack is pure reuse).
- **Deliverables:** the three splice commands (insert/remove/move, each one undo unit, each
  with inverse-pair unit tests incl. nested-group placement and first/last positions); panel
  modifier UI on each object row (stack list, per-modifier param rows, add popover with the
  curated list, remove, up/down); disabled add on unparseable chains.
- **Gate.** *Positive:* command inverse-pair tests; Vm chain-trace tests (stack order matches
  wire order after every operation); L3 flow: add Twist to the starter cube, drag amount,
  assert param + visible PNG change, reorder two modifiers, undo ×N restores the original
  def byte-identically (def-equality assert); `graph_tool validate` on the post-splice def.
  *Negative:* §4 gates; `rg -n "modifier" crates/manifold-core/` → no stored stack anywhere
  (the wire chain is the only home). **Demo (L3 + PNG):** the flow + a before/after PNG pair
  of Bend on the rosetta-stone fixture. **Full workspace sweep + `cargo clippy --workspace
  -- -D warnings` + `cargo deny check bans`** (wave close), plus the focused GPU run ONLY if
  any phase touched shader/runtime files (none should — if the diff says otherwise, that's a
  brief violation to escalate, not a test to run).
- **Performer gesture:** add Bend, expose its amount to the card, bind a MIDI knob — the
  statue bows on cue. The panel built a performable control in four clicks.
- **Test scope:** focused + the wave-close sweep above.

**Phasing-completeness walk:** column/toggle/exclusion (P1) · discovery Vm + degradation
(P1) · Environment/Fog rows + add-composites (P1) · empty states + starter preset + New
Scene + Open Graph (P1) · Objects rows/rename/transform/material/remove/counts (P2) ·
+ Object/+ Light buttons (P2) · Lights rows (P3) · Camera rows incl. lens trace (P3) ·
Import Model merge + normalize + safety bound (P4) · modifier stack UI + 3 commands (P5) ·
every other §2 commitment is in §9 Deferred with a trigger. The §6 skeleton's "scene name
editable", multi-scene picker, visibility toggles: §9, by name.

## 6. Performance (stated honestly)

The panel is UI-thread work: Vm rebuild on snapshot change (a linear walk of one generator's
def — hundreds of nodes at worst, microseconds against the UI frame), widget rows, command
dispatch. The content thread is untouched by construction and P1 proves it by byte-diff.
The merge (P4) and splices (P5) are edit-time operations through the normal command path —
load-cost only, no per-frame anything. No `MANIFOLD_RENDER_TRACE` gate is owed; if any phase
finds itself adding content-thread work, that is a brief violation — stop and escalate.

## 7. Decided — do not reopen

1. A scene is the layer's generator graph containing `node.render_scene`; the panel is a
   view. No scene document, no panel-owned values, no apply step (Peter, 2026-07-16).
2. One scene per layer = the shipped `gen_params` ownership; nothing moves to clips. Clips
   gate when the scene shows; scene-state-per-clip is Deferred, not v1.
3. `ScreenLayout` column cloned from the audio dock; the two utility columns are mutually
   exclusive; one layout rule at every width.
4. Every panel write is an existing command or one of the five named composites — the
   fourth-surface rule, enforced by the command-equality test.
5. Discovery is curated + tolerant: known atoms get rows, unknown shapes get honest labeled
   custom rows, nothing hides, nothing errors.
6. Merge-import skips chrome, offsets ids, bumps `objects`, and seeds a visible normalize
   scale only past 10× mismatch. Native units otherwise.
7. Modifier stack = the wire chain inside the object's group. No stored list anywhere.
8. The D8 fence stands; additions must displace an existing control or justify a basic
   user's need.
9. SCENE_BUILD §7 item 7 is superseded (this doc's header); REALTIME_3D decided-#1 stands.
10. Landing discipline per `.claude/GIT_TREE_DISCIPLINE.md` §2 — merge-trunk, batched
    landings, sweep at landing.

## 8. Execution notes for the orchestrator

- Fresh session per phase; phase brief + this doc are the context (standard §8).
- Gates are run by the orchestrating session, never solely the worker; PNGs are read, not
  assumed. Landing reports per §8.10 in `docs/landings/`, status line updated per §8.9.
- The single Fable advisor (Peter's allowance, 2026-07-16) is for CRITICAL blockers only —
  a moved anchor is not critical (re-derive); a missing symbol, a contradicted decision, or
  a design gap that forces an unlisted choice is. One advisor session, total, for the night;
  its verdict goes in the phase notes.
- Escalations that must PAUSE a phase rather than improvise: any need to touch `layer.rs`,
  `render_scene`'s runtime, serialization, or a new port type; any panel control with no
  existing command to dispatch; merge topology the D5 plan can't express.
- BUG_BACKLOG entries for anything found-not-fixed, before the session ends (house rule).

## 9. Deferred (explicitly not v1, each with its revival trigger)

- **Clips as scene states** (a clip = a param snapshot of the layer's scene; transitions =
  interpolation). Trigger: Peter asks for per-clip looks on a scene layer, or SESSION_MODE
  scene-launch wants scene morphs. Big, real, and additive — envelopes/automation cover
  time-varying looks meanwhile.
- **Per-object visibility/solo/mute.** Needs a renderer-side mechanism decision
  (render_scene carries no per-object params by SCENE_BUILD D3 — a `visible` param on
  `transform_3d`? scale-zero is a hack and banned). Trigger: first real set-dressing session
  reaches for it. Design the mechanism then; do not improvise it into v1.
- **Live per-scene frame-cost attribution** (ms, not counts). Trigger: profiler per-node
  attribution lands or a scene blows the budget in rehearsal. The counts row is v1's honest
  stand-in.
- **Asset embedding in the project container** (today: `model_file`/`hdri_file` are absolute
  path params — a project moved to another machine loses its meshes). Real gig-resilience
  cost, named. Trigger: PROJECT_FILE_INTEGRITY / media-relink wave, or the first show
  prepped on a second machine. Belongs to the IO design family, not this panel.
- **Asset file-watch hot reload** (re-export from Blender → scene updates in place, keeping
  transforms/modifiers/mappings — stable inner node_ids already make this possible).
  Trigger: real Blender round-trip friction during content prep; needs the app's first FS
  watcher, its own small design.
- **Multi-camera / camera switcher / per-output cameras.** Trigger: MULTI_DISPLAY P3+ lands
  or the two-tower rig needs distinct angles of one scene. Scene evaluation is already
  separate from camera choice (camera is just a port) — nothing in v1 forecloses this.
- **Multi-scene picker** for graphs holding two live `render_scene` nodes (v1 shows the
  first + a chip). Trigger: a real project hits it.
- **Animation section** (imported clips list, play/loop/speed in beats). Owner:
  GLTF_ANIMATION_DESIGN — SHIPPED 2026-07-17 (all phases A1–A4; A4 stamps Rate/Clip/Loop
  Mode/Retrigger card knobs per animated object directly via `gltf_import.rs`'s existing
  D5/D9 curated-card mechanism, not a dedicated panel section). Vocabulary is stable; this
  panel item is now a real "build the curated section" trigger rather than a "wait for the
  vocabulary" one — re-derive scope against A4's actual card knobs before picking it up.
- **Video/layer textures into scene materials** via a panel picker (`base_color_map_k`
  accepts any Texture2D wire in the graph today — the capability exists; the curated picker
  is what's deferred). Trigger: VIDEO_IO or the first "video on a screen mesh" ask.
- **Scene presets** (save a composed environment as a reusable named preset). The generator
  preset system already IS this at the whole-scene granularity (save the generator);
  sub-scene presets (a lighting rig alone) wait for COMPONENT_LIBRARY. Trigger: that design
  building its macro layer.
- **Splats/point-cloud rows.** Owner: GAUSSIAN_SPLATS_DESIGN; the Vm's custom-row
  degradation already displays them honestly meanwhile.
