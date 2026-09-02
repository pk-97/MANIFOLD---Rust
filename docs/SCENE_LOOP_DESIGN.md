# Scene Loop — infinite looping flythroughs for imported GLB scenes

**Status:** IN PROGRESS — P1 on main 2026-09-02 (atoms, apply/remove commands, wrap-parity net); P2 (panel section) landed on lane 2026-09-02 (renderer-side plan builder, D10 camera-home addendum, flow + gates); P3 (fog polish) not started. · 2026-09-02 · k3 (lead)
**Prerequisites:** none (builds on REALTIME_3D P0–P6, on main).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

A classic VJ loop is a fixed 10-second render. This design makes an imported GLB
scene loop infinitely, beat-locked: the scene is instanced end-to-end along a
travel axis, and a camera flies exactly one copy-length per loop of N bars, so
the frame at loop end is identical to the frame at loop start. Infinite
flythrough from one finite asset — the feedback-tunnel trick done in world space
with real geometry (Peter: "almost like an instanced feedback effect") — true
parallax, correct shadows and fog, no accumulation drift.

Peter's directives that shaped this design, verbatim:

- "I'm talking about the 3D scenes with the glb files etc. I think they have the
  biggest potential at the moment as they're not as 'param driven' as our
  generative content." — the target is imported scenes, not generative graphs.
- "This stuff should be done at the 'scene panel' level. The graph editor is
  waaayy too granular for this type of modification." — the UX surface is a
  ScenePanel section; the graph is implementation, never required reading.
- "We often miss little things that take a few sessions to hunt down and tune
  properly." — hence the audit-first shape of this doc and the panel-trap
  forbidden moves in section 5 (Phasing).

On stage: a scanned corridor, forest, or tunnel flies past forever, in time with
the track. When it's broken it looks like: a one-frame jump at the wrap (wrap
purity violated), a visible joint where two copies meet (seam strategy wrong for
the scene), or frame drops at 4K (copy count beyond the perf gate).

Companion docs: `docs/REALTIME_3D_DESIGN.md` (the scene system this modifies),
`docs/GROUPING_GRAPHS.md` (group mechanics the import graphs use), and
WIDGET_TREE_DESIGN.md section 5b (manifest-backed param surfaces) — the panel
gets rows for free through it, never bespoke.

---

## 1. Audit — what exists (verified 2026-09-02)

Verified by three read-only audit lanes against the codebase and against
`tests/fixtures/rt/apricot_tl05.glb` through the production
`assemble_import_graph` path (17 nodes / 22 wires / 4 object groups). Extend,
don't redesign.

### Renderer — the instancing already exists

| Piece | Where | State |
|---|---|---|
| Per-object instancing | `render_scene.rs:56-66` | `scene_object.instances_n: Array<InstanceTransform>` port; renderer composes `model_n * T_instance` per instance. **The corridor mechanism — one wire away.** |
| InstanceTransform | `mesh_common.rs:96-101` | 32 bytes: pos vec3 + quat vec4 + scale vec3. |
| Render flattening | `render_scene.rs:4046` (`evaluate`), object loop `:4596`, batch submit `:7181` | Scene flattens to one draw per object per frame in ONE shared 4xMSAA pass; full inter-copy occlusion via shared depth. |
| Shadow passes instance-aware | `render_scene.rs:5381, 5448` | Instances render into shadow maps already. |
| Frustum culling | absent — only `visible == false` skip at `render_scene.rs:4612` | **Does not exist.** All N×M instances submit every frame, including behind-camera copies. |
| Scene bounds at import | `gltf_load.rs:1560-1624` (`GltfImportSummary.bbox_min/max`), stamped to `PresetMetadata.scene_bounds` (`effect_graph_def.rs:443`) at `gltf_import/scene.rs:884`; panel reads via `SceneVm.scene_bounds` (`scene_vm.rs:118`) | **Auto cell-size is one subtraction** along the travel axis. |
| Loop phase source | `primitives/beat_ramp.rs` | `out = clamp(fract(beats·rate)/attack, 0, 1)`; with `attack=1, rate=1/bars` this IS the loop phase. **Exists.** |
| Atmosphere/fog | `primitives/atmosphere.rs:28` | Every param port-shadowed by a same-named optional scalar input (`fog_density`, `height_falloff`, …) — fully wire-drivable. Importer emits NO atmosphere node (`gltf_import/scene.rs:562`); unwired = `Atmosphere::default` = no fog. |

### Graph shape and mutation path

| Piece | Where | State |
|---|---|---|
| Import graph spine | `gltf_import/scene.rs:693-694` | `camera (node.orbit_camera)` → `lens (node.camera_lens)` → `render.camera`; lens also feeds AO, DOF, motion_blur. **Re-pointing `lens.camera` re-points every consumer.** |
| Object groups | `gltf_import/scene.rs:59` (`build_import_graph`) | One group per material (`object_{k}`), inner `mesh_{k}`/`tex_{k}`/`mat_{k}`/`transform_{k}`/`object_{k}_bind (node.scene_object)`; group output → `render.object_{k}`. |
| Composite multi-node command template | `scene.rs:1411` (`ImportModelIntoSceneCommand`) + renderer-side plan builder (`assemble_merge_plan`) | The shape Scene Loop's apply-command copies: plan renderer-side, splice editing-side. |
| Single-node add-into-unwired-port template | `scene.rs:1148` (`AddSceneFogCommand`) | The fog half of the modifier. |
| Undo | `commands/graph/mod.rs:68` (`with_target_graph_mut`) | Whole-level `(nodes, wires)` + `preset_metadata` snapshot; structural composite, never hand-reversed. `refresh_target_manifest` after stamping (`modifiers.rs:341`). |
| nodeId stability | `core/flatten.rs:167-174`, test at `:790` | Grouping flattens and RENUMBERS numeric ids; stable `nodeId`s survive. Panel-facing identity is nodeId, never numeric id. |
| One-render_scene rule | `scene_vm.rs:106` (`multiple_scenes` chip) | The panel's structural trace assumes exactly one `node.render_scene`. The loop instances OBJECTS, never clones the renderer. |
| Panel node location | `scene_vm.rs:642` (`trace_objects`), `:1007` (`trace_camera`), `:1098` (`trace_atmosphere`) | Structural trace from render_scene, re-derived every sync — save/load-safe by construction. New node types join the curated type consts (e.g. `CAMERA_LENS_TYPE_ID` pattern). |
| Curated mesh-modifier lists | `scene_vm.rs:68` + `modifiers.rs:33` | DUPLICATED lists, must stay in sync (commented in code). Any non-curated node in a per-object chain breaks `walk_mesh_modifier_chain` → "custom chain" state. **Scene Loop stays scene-level and never touches per-object chains.** |

### Scene panel

| Piece | Where | State |
|---|---|---|
| Section rendering | `manifold-app/src/ui_bridge/projection/scene.rs:71` (`sections_for_doc_ids`) | A section renders only because exposure metadata carries `spec.section == Some(name)`; stamped at creation AND at load migration — two paths that must produce the same string. |
| Rows | `scene_setup_panel.rs:1700` (`build_filtered_properties`), full surface built with `SurfaceVisibility::All` at `inspector.rs:960` | Rows are free once the section string lands: filter by `spec.section`, manifest-backed, no bespoke row code. |
| Per-frame value sync | `scene_setup_panel.rs:1051` (`sync_properties_values`) | Id-joined via `row_id_index`; a row missing from the manifest trips the panel's INV-6 check (debug_assert dev / one-time warn + frozen row release). |
| Write targeting | `manifold-app/src/ui_bridge/project.rs:949` (`apply_scene_param_write`) | Rows must target `GraphParamTarget::GeneratorOf(vm.layer_id)`, never plain Generator — BUG-292 (scene-panel-wrong-layer-target). |
| Godfile ceiling | `scene_setup_panel.rs` at 4148/4150 lines | New panel code goes in a new `scene_setup_loop.rs` module (precedent: `scene_setup_actions.rs`, `004cf26ff`). |
| Headless verification | `cargo xtask ui-snap gltfscene` (`crates/manifold-app/src/ui_snapshot/`) | Real UI root, real GLB, panel open, wheel/click reachable through the real event surface; flow scripts under `scripts/ui-flows/` (L3). |

---

## 2. Decisions

- **D1 — Instance at the object level, through the existing `instances_n`
  port.** The apply-command wires one shared copy-transform array into every
  object group's `scene_object.instances_n` input. One render pass, one depth
  buffer, full inter-copy occlusion, zero new render machinery.
  Rejected: a scene-level instancing primitive with its own shader/shadow path
  — new machinery for the same GPU result.
  Rejected: duplicating the object groups N times in the graph at edit time —
  a 10-mesh scene ×5 copies is 50 groups, 50 mesh uploads, and a panel trace
  surface that explodes for nothing.
- **D2 — One new atom mints the copy array: `node.scene_array`.** Inputs:
  `count`, `axis` (enum: ±X/±Y/±Z), `cell_size`. Output:
  `Array<InstanceTransform>`, entry i = identity TRS translated `i * cell_size`
  along `axis`. Barrier-free per-element GPU atom on the freeze codegen path
  per CLAUDE.md (`wgsl_body` + `fusion_kind` + value-level gpu_tests proof).
  One instance of this node feeds ALL object groups — copy count changes are
  one param write, not N.
- **D3 — The loop camera is a new curated primitive: `node.loop_camera`.**
  Emits `Camera` from `phase` (0..1, wired from a `beat_ramp` at attack=1,
  rate=1/bars), `cell_size`, `axis`, `lateral`/`height` offsets within the
  cross-section, `fov`. Position advances `phase * cell_size` along `axis`;
  look direction is travel-aligned with a small loop-phased sway. Precedent:
  `node.orbit_camera`. Composition (phase → math atoms → free_camera position)
  was considered and rejected: a 4-node fan-out the user can silently break,
  and the panel's `trace_camera` needs a curated type either way
  (`scene_vm.rs` camera consts + `CameraVm` arm).
- **D4 — Cell size auto-derives from `PresetMetadata.scene_bounds`** (extent
  along the chosen axis) with a manual `trim` param (bbox lies when a stray
  rock inflates it). The SAME resolved cell_size value feeds both
  `node.scene_array` and `node.loop_camera` — the plan builder computes it
  once; camera travel per loop == instance spacing by construction, not by
  the user matching two sliders.
- **D5 — Apply/remove is ONE composite EditingService command** in the
  `ImportModelIntoSceneCommand` shape: renderer-side plan builder (mints
  `scene_array`, `loop_camera`, `beat_ramp`, optional `atmosphere`; computes
  cell_size; lists the per-group instance splices), editing-side command does
  the level-snapshot undo + splice + `refresh_target_manifest`. Undo of
  "apply Scene Loop" is one undo step. Removing the modifier is the symmetric
  command, not "undo and hope".
- **D6 — The panel surface is a "Scene Loop" fold section** in a new
  `crates/manifold-ui/src/panels/scene_setup_loop.rs` (godfile ceiling), shown
  for imported-scene selections. Section membership rides the existing
  `spec.section` exposure stamping — creation stamper AND load-migration
  stamper, same string. Rows are manifest-backed; all writes target
  `GeneratorOf(vm.layer_id)`. **Zero-new-systems rule (DESIGN_AUTHORING
  section 3, the zero-new-systems test): no synthesized param ids, no
  panel-side id map, no resolution funnel** — the reference failure is
  BUG-237 (scene-setup-camera-world-light-param-scrub) and its siblings.
- **D7 — Fog is the default seam strategy, auto-sized.** When the graph has
  no atmosphere node (import default), the apply-command adds one and sets
  fog far ≈ 1.5 × cell_size (overridable). Enclosed scenes hide their own
  ends; fog covers the rest. **Consequences, stated honestly:** open-landscape
  scans will show the joint no matter what — scene selection is part of the
  feature, and the doc's demo scene is an enclosed one.
- **D8 — Wrap purity is an invariant, not a hope.** Inside a looped scene,
  every time-varying input rides the loop phase wire. Camera shake, audio
  modulation, exposure pulses that aren't loop-phased produce a one-frame jump
  at the wrap — the exact "worked in the demo, jumps at the gig" bug.
  Enforcement: the phase-0 == phase-1 pixel-diff gate (section 4 INV-3).
- **D9 — v1 is raster `render_scene` only.** RT compatibility is unverified
  (the RT path's handling of `instances_n` is unknown) and is Deferred with a
  trigger, not promised.
- **D10 — Copy count default 3.** One copy behind (for the wrap), the cell
  you're in, one ahead; fog eats anything further. No standalone perf probe:
  static analysis predicts the shape (no frustum culling ⇒ vertex cost scales
  linearly with copies, fragment cost at 4K is flat for behind-camera copies —
  shared depth rejects them before rasterization), and the real number comes
  free as a byproduct of P1's demo render. If that number blows the 4K frame
  budget at ×3, escalate to Peter — far-copy degradation is a redesign, not a
  lane decision.
  **P2 addendum — camera home (lead ruling 2026-09-02):** the loop camera's
  phase-0 home is the CORRIDOR ENTRY, never the scene center: `home =
  -cell_size/2` along the travel axis (imports recenter the scene at the
  origin, so the near face is not 0), cross-section center + lateral/height
  offsets, looking along +axis. The view from the cell-0 near face down
  copies 0/+1/+2 is period-identical to a `-cell/0/+cell` framing, so cycles
  stay bit-identical (D4) — behind-camera geometry never enters the frame.
  `home` is a real `node.loop_camera` param the plan builder sets; copies
  stay at `i*cell_size` (D2's atom contract, not reopened).

---

## 3. Design body

### 3.1 What the apply-command inserts

Into the imported graph (one `render_scene`, N object groups), all with stable
nodeIds in the importer's convention:

```
loop_phase   node.beat_ramp      nodeId "loop_phase"   (attack=1, rate=1/bars)
scene_array  node.scene_array    nodeId "scene_array"  (count, axis, cell_size)
loop_camera  node.loop_camera    nodeId "loop_camera"  (phase←loop_phase, cell_size, axis, …)
atmosphere   node.atmosphere     nodeId "loop_fog"     (only if trace_atmosphere finds none)
```

Rewires:

- `loop_camera.out → lens.camera` (re-points render + AO + DOF + motion_blur in
  one hop; the old `camera` node stays in the graph, unwired — removing the
  modifier restores it).
- `scene_array.out → object_{k}/object_{k}_bind.instances` inside every object
  group, `scope_path = [group_node_id]` (the `InsertMeshModifierCommand`
  descent pattern at `modifiers.rs:211` — but targeting the scene_object's
  instances port, never the mesh chain).
- `loop_fog.out → render.atmosphere` when minted; `fog_density` and far-plane
  params set from cell_size.

Exposure stamping: the loop nodes' panel-visible params (bars on the beat_ramp,
count/axis/trim/camera offsets, fog) stamp into `preset_metadata` with
`spec.section = Some("Scene Loop")` via `stamp_scene_node_exposures_into`,
plus the load-migration stamper, plus the section id set alongside
`world_sections` (`inspector.rs:779-834`).

### 3.2 Data-model answers (the four questions)

Owner: the generator layer's `graph_def` on the content thread, like every
graph. Thread: mutated by the composite command through EditingService; UI sees
snapshots. Serialization: ordinary layer graph_def — no format change, no
migration (the modifier's absence in old projects is just "not applied").
Mutation: `with_target_graph_mut` level snapshot (D5).

### 3.3 Panel section

New file `scene_setup_loop.rs`, registered as a fold section. States:

- **Not applied** → one "Enable Scene Loop" affordance (dispatches the
  apply-command). Graph without the loop group = this state, always derived by
  structural trace, never by a flag.
- **Applied** → manifest-backed rows: bars, axis, count, trim, camera
  height/lateral, fog toggle + density, plus a wrap-debug toggle (parks the
  camera at phase 0 so the seam is inspectable).
- **Hand-edited graph** → the trace shows what it finds (the
  `CameraVm::Custom` precedent: honest display, no silent correction).

### 3.4 Seam honesty

Repeat tiling requires the scene's entry and exit cross-sections to roughly
match; real scans often don't. Mitigations, in order: enclosed scene selection
(D7), fog (D7), camera path placement away from the boundary. Mirror tiling
(alternate flipped copies — ends always match) is Deferred: instancing is
transform-only and mirroring needs negative scale, which flips triangle winding
and breaks culling/normals — unverified, not promised.

**Consequences, stated honestly (D1):** instances share materials, textures,
and light rig. Sun and ambient carry fine; a scene with baked-in practical
lights will not repeat those lights per cell — the corridor reads as one long
room lit from one end. Sunlit/enclosed scans are the good candidates.

---

## 4. Invariants & enforcement

| # | Invariant | Enforcement |
|---|---|---|
| INV-1 | Exactly one `node.render_scene` in a looped graph | The apply-command refuses (error, not silent skip) when the trace finds ≠1; test `scene_loop_apply_rejects_multi_scene`. |
| INV-2 | Every minted node carries a stable `nodeId` | Round-trip test: apply → save → load → structural trace re-finds all loop nodes (`scene_loop_roundtrip.rs`). |
| INV-3 | **Wrap purity:** frame at phase 0 == frame at phase 1 | Headless render at phase 0.0 vs phase 0.99999 via `render_viewport_frame` (`viewport_render.rs:148`); pixel diff must be zero (deterministic raster path). Test `scene_loop_wrap_parity.rs`. A red result = a non-loop-phased driver snuck in — the test IS the class fix. |
| INV-4 | Camera travel per loop == instance spacing | Both derive from the single cell_size the plan builder computes (D4); test asserts the two stamped params match the plan value. |
| INV-5 | Panel rows id-joined, writes target `GeneratorOf(layer_id)` | Existing panel row-value assert + ui-snap flow `scene-setup-loop.json` asserts a bars edit lands on the looped clip's beat_ramp (not the active layer's) — BUG-292 (scene-panel-wrong-layer-target) regression net. |
| INV-6 | No synthesized param ids / panel-side id maps | Review rule + negative gate: `rg 'scene_loop' crates/manifold-ui/src/panels/` shows no `format!`-built param id strings. |

## 5. Phasing

Entry state for every phase: re-verify the section 1 anchors it touches (audit
is a 2026-09-02 snapshot). Forbidden across all phases: duplicating object
groups per copy (D1) · a second render_scene (INV-1) · inserting into
per-object mesh modifier chains (the duplicated curated lists trap) · bespoke
panel rows or synthesized ids (D6) · a camera-travel param the user must match
to cell spacing by hand (D4) · gating on anyone (agent OR Peter) looking at a
PNG — every gate in this design is a computed number or exit code (Peter
2026-09-02, automated orchestration over stop-and-approve).

- **P1 — Atoms + composite command. ✅ LANDED 2026-09-02 (merge 44ec74f7f).** `node.scene_array` (D2, freeze-path
  atom + gpu_tests value proof), `node.loop_camera` (D3 + curated camera-type
  registration), apply/remove commands (D5), fog wiring (D7), exposure
  stamping both paths (D6). Read-back: sections 1–3 whole, `modifiers.rs:211`,
  `scene.rs:1148,1411`, `commands/graph/mod.rs:68`. Gate: INV-1..4 tests green;
  `scene_loop_wrap_parity` red on a deliberately non-phased driver (gate must
  see red before green); round-trip save/load re-traces the group.
  Test scope: `manifold-renderer` + `manifold-editing` + **`manifold-app`
  clippy/compile** (the CameraVm arm lands in renderer but its match-site
  consumers live in app — P1 execution missed this and the landing gate
  caught it; the scope line is the fix); gpu-proofs suite for the new atom. Acceptance demo — fully numeric, no human look
  (Peter 2026-09-02: "not a huge fan of these stop and approval things for
  single png checks"), all computed from `render_viewport_frame` buffers:
  (1) wrap parity — phase 0 vs 0.99999 max abs pixel diff == 0;
  (2) copies present — count=1 vs count=3 frame diff above threshold, and
  far-half region mean shifts toward fog color with copies on;
  (3) frame time at 1920 and 4K ×3 reported as a number; over 16.6 ms at 4K
  = escalate, don't degrade silently (D10).
  Performer gesture: change bars 8→16 mid-set — phase rescales, no
  position jump (asserted in test via phase continuity at the rate change).
- **P2 — Panel section.** `scene_setup_loop.rs` fold section in the three
  states of section 3.3, wrap-debug toggle, ui-snap flow
  `scripts/ui-flows/scene-setup-loop.json` (enable → bars row visible → edit
  bars → assert write landed on the looped layer). **Flow wiring, both
  mandatory in the same commit** (the manifest's own rule, and the
  green-landing/dead-flow escape class): (1) the flow name MUST match the
  `scene-setup-*` prefix — that prefix is what maps it to the `gltfscene`
  fixture in `scripts/ui-flows/manifest.json`; (2) add a `path_triggers` row
  for the NEW file `crates/manifold-ui/src/panels/scene_setup_loop.rs` →
  `scene-setup-loop`, or the landing flow gate runs nothing for it. Read-back: section 3.3,
  plus the historic panel bug classes from the audit — BUG-313 (positional-value-join),
  BUG-292 (scene-panel-wrong-layer-target), the rebuild-less-state class
  (internal state change with no structural action = dead UI), and the
  unstamped-section silent no-render trap.
  Gate: flow green (L3); INV-5/INV-6 nets green; round-trip gate — save,
  reload, section still finds and edits the loop nodes.
  Acceptance demo (L3): the ui-snap flow — scripted, numeric asserts, no
  human look. A PNG of the section is produced as a byproduct, never gated.
- **P3 — Atmosphere polish.** Loop-phased fog density/shaft drivers wired off
  `loop_phase`, sensible defaults per cell_size. Read-back: `atmosphere.rs`
  port list. Gate: wrap parity still green with drivers live, and far-region
  mean color differs between phase 0.25 and 0.75 by at least the driver's
  stated swing — numeric, no eyeballs. Demo: the computed numbers — L1.

## 6. Decided — do not reopen

1. Instance via the existing `instances_n` port; never clone groups, never a
   scene-level instancing renderer (D1).
2. One `node.scene_array` feeds all groups; one resolved cell_size feeds array
   and camera (D2, D4).
3. Loop camera is a curated primitive, not a composition of math atoms (D3).
4. Apply/remove is one undoable composite command each way (D5).
5. Panel section rides exposure stamping; zero new id systems (D6).
6. Raster-only v1 (D9); copy count 3 default, frame-cost measured on P1's
   demo render (D10).

## 7. Deferred

- **Mirror tiling** — trigger: negative-scale instance winding verified (or a
  winding-flip instance flag lands in the renderer).
- **RT compatibility** — trigger: RT path audit for `instances_n` handling;
  note the BUG-326 (rt-depth-snapshot-wrong-on-imported-glb-scenes) precedent
  that RT + imported scenes have their own traps.
- **Crossfade/whip wrap for mismatched ends** — trigger: a scene Peter wants
  that fog can't save.
- **Looping noise (torus-sampled) for particles/deform** — trigger: atmosphere
  drivers shipped (P3) and Peter wants moving geometry in the loop.
- **Nested loop periods per element** (camera 4 bars, lights 16) — trigger:
  v1 loops feel monotonous in the set.
- **Frustum culling for instances** — trigger: the P1 demo timing (D10)
  showing behind-camera vertex cost matters at real copy counts.
- **Per-cell material variation** — requires group duplication; only if
  identical cells read as repetitive on stage.
