# Water Family — one water, separate looks

**Status:** PROPOSED · 2026-10-06 · Codex · not built; Peter's answers incorporated.
**Prerequisites:** whitewater fusion P4's builder end state landed.
**Execution contract:** read [DESIGN_DOC_STANDARD.md](DESIGN_DOC_STANDARD.md) sections 5, 6 and 8; re-derive P4-dependent seams before implementation.

<!-- index: Water parent, Foam/Spray/Bubbles looks, grouped shared Add Water/preset recipe, and undoable family lifecycle. -->

Adding Water produces a complete instrument in one gesture. Content owns
undoable edits; UI projects snapshots and uses existing exposure bindings and
`ParamSurface`. No new clock, thread, lock, identity system or per-frame family
discovery. WIDGET_TREE_DESIGN governs controls; GPU_WHITEWATER_DESIGN governs
simulation.

## 1. Audit — what exists (verified 2026-10-06)

Snapshot: local `origin/main` at `194e0106d4a6bb0102ff5a0adfae8ba644cb49c3`,
read with `git show`; nothing merged. Remote-tip verification failed on GitHub
DNS. All source anchors below refer to this SHA, not this worktree's HEAD.
**Extend, don't redesign.** Short paths inherit the preceding module directory.

| Piece | File:line evidence | State / consequence |
|---|---|---|
| Compound rows | `crates/manifold-renderer/src/node_graph/scene_vm.rs:967–1004`; `crates/manifold-ui/src/panels/scene_setup_panel.rs:1838–1841` | Exists: grouping, sort and expansion. Water needs its own physical row as parent, not the importer's synthetic parent. |
| Card ownership | `crates/manifold-app/src/ui_bridge/projection/scene.rs:102–123`; `scene_vm.rs:1350–1372` | Exists: owned node IDs select exposures; water owns its domain, whitewater and budget. Child mesh ownership is new. |
| Family recipe | `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_preset.rs:707–745`, `:764–854`, `:879–895` | BUG-ju9j: Add Fluid imports only water material/object from the seed; render builders separately import whitewater dressing. Shared solver is not shared family construction. |
| Flat builder assumptions | `gpu_flip_preset.rs:334`, `:586`, `:668`, `:939` | Exists: `feed_intervals`, `id_named`, `add_obstacle_render`, `add_dust_render` operate on top-level JSON. Grouping requires rewriting these seams. |
| Golden fixture | `crates/manifold-renderer/src/node_graph/primitives/whitewater_golden_tests.rs:64–95`, `:144–152` | Exists: `variant()`/`all_emitters()` edit top-level `whitewater`/`whitewater_budget`; fingerprints read step outputs and captures. Grouped lookup must not silently skip edits. |
| Template insertion | `crates/manifold-editing/src/commands/graph/scene/fluid/template.rs:15–60`; `scene/fluid.rs:66–105`, `:263–271` | One wire away: template, fresh IDs, metadata stamping and undo; extend single-output insertion and metadata coverage. |
| Visibility | `crates/manifold-renderer/src/node_graph/gltf_import/object_group/static_compound.rs:313–321`; `crates/manifold-app/src/ui_bridge/project.rs:1342–1365` | Exists: shared binding retargeted to `parent_visible`, resolved through ordinary parameter commands. |
| Delete / duplicate | `crates/manifold-editing/src/commands/graph/scene.rs:1137–1146`, `:1191–1197`; `scene/duplicate.rs:444–451` | Parent deletion is one wire away: group slots removed together, fluid roles detached. Duplicates deliberately carry no card exposes. |
| Sheet Fill Rate | `crates/manifold-renderer/src/node_graph/primitives/gpu_flip_step.rs:2232`; `scene_vm.rs:1350–1353` | Exists at 0..1. BUG-7zby1 (sheeting speed and controls) supplies its domain dial, wire and binding, following the pressure-cap pattern; domain ownership puts it on Water automatically. |

The committed `docs/WHITEWATER_STAGE_FUSION_DESIGN.md:3` records P0 and P1
landed, including the copies change. Its P4 contract (`:344–350`) supplies packed
`faces`, matching extent validation, removal of forced `with_faces()` in both
render builders, removal of whitewater face adapters, `step.faces → whitewater.faces`,
and regenerated presets. Explicit axis-face fixtures remain. Peter's answer
below also drops P4's own migration rung: regenerate presets instead. This
design depends on P4's **builder end state**, not any P4 migration. The cited
fusion document still contains that superseded rung at this snapshot.

## 2. Decisions

**D1 — Peter:** “Water is a parent row in the Scene Setup panel. Foam, Spray and
Bubbles are its child rows.” Water owns its physical look; no second Water child.
Rejected: a separate Whitewater parent, which splits one instrument.

**D2 — Peter:** “A child holds only its look: material, size and visible.” Amount
and emission settings belong on Water. Use manifest exposures and the shared
card host, with modulation and undo; no bespoke sliders or child simulation controls.

**D3 — Peter:** “Adding water from the scene panel builds the whole family in one
go.” Add Water and both shipped presets use one recipe. Particle View substitutes
only the water display. Insertion is one undo; redo restores identities.

**D4 — Peter:** “Let's not do dust yet”. No Dust child; remove the dust render
chain from regenerated presets. Preserve the simulation oracle's dust coverage.

**D5 — Technical:** the recipe lives in `gpu_flip_preset.rs` beside
`water_def`/`gpu_flip_liquid_body` (`:707`), shaped as BUG-ju9j's proposed
`WaterScene::with_whitewater`. One consumer needs no new core module or dust
option. The recipe constructs its family rather than reading it back from its
generated preset.

**D6 — Peter:** “whitewater to 1 please”. New water gets Amount **1.0**, accepting
the whitewater GPU work.

**D7 — Peter:** “we don't need to spend time upgrading old projects please. We can just load the preset (which should always be updated with our new features and fixes)”. Regenerate the presets; no upgrade work or version bump in this design.

**D8 — Peter:** “deleting yes”. A child's control reads **Hide**, not Remove,
and hides it. Deleting Water removes the whole family in one undo, using
`RemoveSceneObjectCommand` and its existing fluid-role detachment.

**D9 — Lead:** shipped presets are **grouped**. This gives Add Water and presets
the same family boundary, but costs real rewrites of the flat builders and golden
fixture lookups. Rejected: keeping a flat shipped preset with separate dressing.

**D10 — Technical:** family rows have no Duplicate verb. Precedent: lights
(`scene_setup_panel.rs:2468`). Existing duplication omits card exposes; exposing
it here would create an instrument without its controls.

## 3. Design body

### Recipe and insertion

One ordinary group owns domain/state/solver/surface, whitewater step/budget,
display interpolation, and water plus three child looks. Outputs are `object`
(Water), `object_1` (Foam), `object_2` (Spray), `object_3` (Bubbles), each feeding
a physical render slot. Preserve P4 packed faces, substep history and tick
captures; display consumes captured populations, never live outputs outside
the tick region. Preserve P1's alias and influence-swap behaviour.

The group gains an `obstacle_source` interface input. It is optional on the
step (`whitewater_step.rs:93`), fed in the preset and unfed by Add Water.
`gpu_flip_liquid_body() -> EffectGraphDef` keeps its signature and uses the
shared recipe. Rewrite `render_def`, `particle_view_def`, `add_obstacle_render`
and `feed_intervals` for the group boundary; remove the shipped `add_dust_render`
path. Their top-level `id_named`/JSON assumptions cannot survive as wrappers.

`LiquidTemplate` retains its fields and adds `pub object_outputs: Vec<String>`;
single-output literals get `vec!["object".into()]`, family literals all four.
Insertion reserves physical slots from the content-owned count atomically.
`AddSceneFluidCommand::new` (`fluid.rs:66–75`) stays. Add `ExposureSet::Whitewater`
and `::Look`, supplied by `with_whitewater_metadata` and `with_look_metadata`,
following `with_role_metadata` (`:97`). Extend the exhaustive `metadata_for`
match (`:263–271`); compiler errors enumerate missed cases.

`TemplateExposure` also needs a shared-binding form: one exposure ID and explicit
node/parameter targets for all four `parent_visible` values. Remap every target
through the same fresh-ID insertion map. Do not try to obtain this from per-object
metadata: `scene_exposure.rs:105` excludes `parent_visible`.

### Row identity, size and visibility

The Water parent **is the water object's own row**: `is_group=true`,
`object_node_id = water object's doc id`, `group_node_id=Some(group)`. Its
children have `parent_group_id=Some(water object doc id)`. Keep Water's material
and `fluid_controls`. **Clone-and-clear** (`scene_vm.rs:967–985`) is forbidden
for Water. The sort (`:995–1004`) keys parents by `object_node_id`, as does
`expanded_groups` (`scene_setup_panel.rs:1838–1841`); group-node identity here
would break those dependencies. No duplicate physical-water row is emitted.

Discover membership structurally on rebuild via group outputs and one
unambiguous simulation owner; reuse `SceneNodeRef`, not fixed IDs or handles.
Imported compound behaviour stays intact. Add one row field,
`look_mesh: Option<NodeId>`, populated only for family children. Include it in
`object_controls` (`projection/scene.rs:102–123`). Filter
`node.platonic_solid_mesh` metadata to `radius` in `scene_exposure.rs`'s filter
chain (precedent `:107`); label it **Size** through
`SceneParamMetadata.label` (`crates/manifold-core/src/scene_exposure.rs:24`).
There is no second stored size. Suppress child transform, physics, skin and
modifier affordances.

Children cannot leak fluid controls: `liquid_domain_of` starts only from
`vertices` (`crates/manifold-core/src/liquid_domain.rs:114`), and whitewater
objects take vertices from their platonic mesh, not the liquid surface.
`water_family_row_ownership` must assert that their fluid ownership is empty,
as well as checking parent identity, sort, expansion and mesh ownership.

Water's eye uses a **shared exposure binding**, following
`shared_visible_bindings` retargeted to `parent_visible` in
`gltf_import/object_group/static_compound.rs:313–321`. The path is
`eye → SceneSetupParamChanged → apply_scene_param_write → binding_id_for_node_param → ChangeGraphParamCommand`
(`ui_bridge/project.rs:1342–1365`). Child eyes write local `visible`; parent
hide/show preserves those choices and never pauses simulation.

For children, the panel withholds `submesh_remove_ids` and routes **Hide** to
the eye's action (`scene_setup_panel.rs:2765–2776`), writing 0 rather than toggling
an already hidden child on. Keyboard/context deletion follows the same policy.
Parent delete uses the water row's physical slot, reaching the existing group
removal. Preserve external source objects and existing atomic rejection rules.
Parent rename changes group/card sections, not child labels; child rename keeps
its role. No family Duplicate control or keyboard/context bypass.

### Change surface and re-derivation

F1a owns renderer builders, golden fixtures and both
`assets/generator-presets/WaterDamBreak{GpuFlip,Particles}.json`.
F1b/F3 own `scene_vm.rs`, `scene_exposure.rs`, editing `scene/fluid.rs` and
`fluid/template.rs`, scene lifecycle tests, app `ui_bridge/project.rs`,
`projection/scene.rs`, `projection/inspector.rs`, `edit_selection.rs`, UI
`scene_setup_actions.rs`, `scene_setup_panel.rs`, and mapped flows. Reuse
content routing and the parameter host. No shader, solver or backend rewrite.

⚠ VERIFY-AT-IMPL after P4:
`rg -n 'gpu_flip_liquid_body|LiquidTemplate|TemplateExposure|ExposureSet|AddSceneFluidCommand|render_def|particle_view_def|feed_intervals|add_dust_render|add_obstacle_render' crates`.
Record the fresh production/test inventory before code; changed seams require
revision. Add required fields/variants first and use compiler errors to enumerate
literals and matches; no compatibility adapters.

## 4. Invariants & enforcement

These are required implementation checks, not claims of passes here.

| Invariant | Enforcement / phase |
|---|---|
| One recipe, complete without seed dressing; grouped preset parity | `water_family_builder_parity`, both preset snapshots / F1a |
| Regrouping preserves simulation fingerprints and closed tick region | `whitewater_tick_state_matches_golden`, `whitewater_per_tick_preset_closes_the_liquid_region` / F1a |
| Real Water parent; children own material, Size and visibility, no fluid controls | `water_family_row_ownership`, `water-family-preset.json`, `no_bespoke_row_infra` / F1b |
| Atomic insertion, physical slots and stable undo/redo IDs | `water_family_add_undo_redo`, two families after an imported compound / F1b |
| Shared parent gate preserves child choices | `water_family_visibility_round_trip` / F1b |
| Hide never removes a child; parent deletion is one undo; no Duplicate; rename keeps roles | `water_family_delete_undo`, `water_family_visibility_rename_round_trip`, `water-family-controls.json` / F3 |
| No new locks or bespoke sliders | Added production lines scanned for `Arc<(Mutex|RwLock)|BitmapSlider::new` yield zero / all phases |

## 5. Phasing

Each phase rechecks its cited anchors and section 3's inventory. Use
`CARGO_BUILD_JOBS=4`, one cargo command at a time, focused check/clippy/module
tests and the standard landing gate. Compile outside the GPU lock; serialize
GPU execution through `scripts/gpu_queue.py` or the owning proof gate. No
whole-crate sweep. Every persistent phase verifies save/reload and bindings.

### F1a — Renderer recipe and grouped presets

- **Entry/read-back:** P4 builder end state landed; read its gate report and
  D3–D7/D9. Recheck packed faces, captures and the builder inventory.
- **Deliverables:** grouped family body, both regenerated presets, group-aware
  `variant()`/`all_emitters()` lookups for `whitewater`/`whitewater_budget`
  (`whitewater_golden_tests.rs:64–95`), and `water_family_builder_parity`.
  Assert lookups find their targets; no silent no-op edits.
- **Gate:** focused renderer check/clippy, `water_family_builder_parity`;
  regenerate Dam Break then
  Particle View with `UPDATE_GPU_FLIP_PRESET=1` snapshot tests, then both without
  it. Run `scripts/gpu_proofs_gate.py --filter whitewater_tick_state_matches_golden`
  and `--filter whitewater_per_tick_preset_closes_the_liquid_region`, each with
  nonzero count. Identical fingerprints prove regrouping changed no simulation
  values; do not refresh the golden to accept a mismatch.
- **VERIFY-AT-IMPL:** read `whitewater_golden_tests.rs:144–152` and its probe
  helpers: `all_emitters()` must fingerprint the step, not `dust_copies`.
  Removing dust rendering must retain all-emitter simulation coverage.
- **Demo/scope:** focused renderer snapshots and GPU proofs; L1 numeric parity.
  UI acceptance follows in F1b. Forbidden: restored adapters, copied seed recipe,
  hand-edited preset JSON, shader/solver changes.

### F1b — Add Water, exposures and rows

- **Entry/read-back:** F1a landed; re-read template/metadata and row seams, D1–D3,
  D6 and section 3. Recheck projection and exposure owners.
- **Deliverables:** template outputs, atomic insertion, ExposureSets and shared
  visibility, VM rows/Size ownership, **+Water** label retaining automation name
  `scene_setup.add_fluid` (`scene_setup_actions.rs:76`); all F1b checks above;
  `water-family-preset.json` flow and manifest mapping.
- **Gate:** focused editing/renderer/app/UI check/clippy and
  `water_family_add_undo_redo`, `water_family_row_ownership`,
  `water_family_visibility_round_trip`, `no_bespoke_row_infra`; negative scan.
  `scripts/run_ui_flows.py --touched` accounts for every selected flow.
- **Demo/gesture:** L3,
  `cargo xtask ui-snap scene-setup --script scripts/ui-flows/water-family-preset.json`:
  load Dam Break, Add Water, undo/redo, select Water then Foam, edit Size;
  assert ownership, bound values and parent/child visibility. Save/reload,
  modulate Amount and Size, and assert distinct targets. PNG for Peter.
- **Scope/forbidden:** focused insertion/projection/flow and mapped proofs.
  No clone-and-clear Water parent, logical row count for slot reservation,
  bespoke controls or UI model writes.

### F3 — Family lifecycle

- **Entry/read-back:** F1b landed; recheck deletion, duplicate and rename paths;
  read D8/D10 and the visibility path.
- **Deliverables:** child Hide, parent family deletion, no Duplicate verb,
  rename, `water_family_delete_undo`,
  `water_family_visibility_rename_round_trip`, `water-family-controls.json`
  and flow mapping.
- **Gate:** focused check/clippy and named tests; negative scan;
  `scripts/run_ui_flows.py --touched`. Check button, keyboard and context paths,
  fluid-role detachment, external source survival, atomic delete/undo/redo,
  absent Duplicate and rename/binding persistence after save/reload.
- **Demo/gesture:** L3,
  `cargo xtask ui-snap scene-setup --script scripts/ui-flows/water-family-controls.json`:
  Hide Spray, hide/show Water, rename both, undo, delete Water, undo;
  assert child choice and restored family IDs/bindings. PNG for Peter.
- **Scope/forbidden:** focused lifecycle/flow and mapped proofs. No child graph
  removal, duplicate bypass or visibility-driven simulation pause.

## 6. Decided — do not reopen

1. Water's own row is parent; Foam, Spray and Bubbles are look-only children.
2. Amount 1.0; no Dust child or shipped dust render chain.
3. One renderer recipe, grouped presets, atomic Add Water.
4. Regenerate presets; depend on P4's builder end state only.
5. Shared parent visibility, child Hide, whole-family delete/undo, no Duplicate.
6. Sheet Fill Rate arrives from BUG-7zby1 through domain ownership.

## 7. Deferred and verification limits

| Item | Revives when |
|---|---|
| Dust child and render chain | a preset ships dust on |
| Family Duplicate | duplication preserves independent card exposes and bindings, proven through save/reload and modulation |
| New physics, solver optimisation, general family tooling | a separate concrete request |

Docs-only static audit: no app, runtime tests or render run. P4's end state and
BUG-7zby1's control wiring remain to be re-read at implementation;
appearance and GPU cost are unverified. No fusion document changes were made.
