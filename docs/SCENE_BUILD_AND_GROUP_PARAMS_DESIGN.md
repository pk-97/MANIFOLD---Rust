# Scene Build + Group Params — named objects, transform atoms, sectioned cards

**Status:** ✅ **DONE — WAVE COMPLETE 2026-07-10 (P1–P5 all shipped).** Design 2026-07-06 · Fable;
built by an Opus-orchestrated Sonnet wave. Scene-building is now named per-object card sections,
transforms as composable `Transform`-port atoms (beat-modulatable), group boxes that carry their
sub-node sliders, and one-click "+ Object"/"+ Light" that spawn wired/lit/visible — all verified on
the real azalea glTF import. Owed: Peter's L4 feel-pass on the P5 gestures (click-script in
`docs/landings/2026-07-10-scene-build-p5.md`). Per-phase detail below. · design 2026-07-06 · Fable. **P1 SHIPPED 2026-07-10** (main `3a6e30b7`):
`PortType::Transform` + `node.transform_3d` atom. **P2 SHIPPED 2026-07-10**: `render_scene` sheds
all per-object transform params for `transform_n` ports; v1.12.0 migration carries old saves across
(values + card bindings re-pointed); glTF importer emits the end-state shape. Migration parity vs
the real `meshImportTests` project is pixel-identical; a wired LFO→`rot_y` spins an imported object.
This realizes REALTIME_3D's amended D3 (object transforms are now a `transform_n: Transform` port).
**P3 SHIPPED 2026-07-10**: card sections from group names (`ParamSpecDef.section` seeded at expose +
by importer; collapsible headers with UI-local fold state; group-rename sweep; manifest-only mapping
write). A glTF-imported scene's card is now named foldable blocks (`QS1694-W02-1-1`, `Material.001`,
`Camera`/`Sun`/`Environment`), not a flat slider wall — verified via faithful-render capture + a
field-level fold proof. Also fixed a load-bearing registry gap (`ParamDef` lacked the `section`
mirror, silently dropping sections for glTF-imported generators).
**P4 SHIPPED 2026-07-10**: group boxes render their exposed param rows on the node face (same
`ChangeGraphParamCommand` path as the card — one value, three surfaces; collapsed → "N params" chip).
Verified on the REAL azalea import: the "QS1694-W02-1-1"/"Material.001" object boxes carry live
Metallic/Roughness sliders. Fixed the payoff-blocking BUG-103 (`outer_routings_from_view` never
recursed into group bodies, so in-group material bindings were dropped for exactly the imported
scenes the wave targets — 9/13 → 13/13 routings). Landings:
`docs/landings/2026-07-10-scene-build-p{1,2,3,4}.md`.
**P5 SHIPPED 2026-07-10** (`AddSceneObjectCommand` + `AddSceneLightCommand` with node-face "+ Object"/
"+ Light" buttons, D7a light defaults incl. `cast_shadows` ON; same-pair wire ribbons with `×N` badge).
Both gestures verified on the real azalea import (node/wire counts 8→9/12→15 add-object, 8→9/12→13
add-light). Landing: `docs/landings/2026-07-10-scene-build-p5.md`. **Wave complete.**
**Prerequisites:** PARAM_STORAGE_DESIGN P1–P5 (SHIPPED). PARAM_STORAGE_BOUNDARIES_DESIGN
P1–P2 must land **before this doc's P3 only** (the card phase reads specs straight off
the manifest; building it against the pre-boundaries dual-source card path would wire it
to code P2 deletes). P1/P2/P4/P5 here have no dependency on the boundaries wave.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting
any phase.

Peter's directives (2026-07-06, verbatim — these opened the design): `render_scene` is
"really horrible to use and clunky and does not let you build scenes easily," and "this
pairs with the group node parameters." Session decisions (Peter at the keyboard,
2026-07-06): transform atom + Transform port over params-on-the-renderer; atom named
`node.transform_3d`; card bundling = **sections from group names**, NOT a revival of
group interface params ("Phase D" stays dropped); group boxes additionally render their
exposed params as live rows ("node groups have sliders and checkboxes like standard
nodes so you can play with the sub-node values from the group node itself"); both editor
gestures (add-object, wire ribbons) ship in this wave.

**The governing insight:** per-object identity already exists in the graph — the glTF
importer builds one named, tinted group box per object ("Leaf", "Bark") — but the
per-object *parameters* live on the aggregate `render_scene` node, which is built from
counts alone and can never know those names. Every surface downstream is therefore
anonymous: 4 objects → 36 identical "Position X/Y/Z…" rows, 10 objects → 90. The root
fix is to move per-object state to where identity lives (a transform atom inside each
object's named group) and to let group names organize the card (sections). Everything
else in this design is that principle applied to each surface.

Binding constraints (DESIGN_AUTHORING §1): **performance surface** (transforms and
sections are live controls — card, MIDI/OSC, modulation are in scope from the start);
**persistence** (saved scenes carry render_scene transform params today → one-time
migration; sections serialize on the spec); hot path is touched only trivially (one
CPU struct read per object per frame replaces nine param lookups).

Companions: `REALTIME_3D_DESIGN.md` (this doc **amends** its D3/D8 and re-grounds P6 —
see §8; do not read the two docs' transform stories independently),
`PARAM_STORAGE_BOUNDARIES_DESIGN.md` (card single-source — prerequisite for P3),
`GROUPING_GRAPHS.md` + `NODE_GROUPS_DESIGN.md` (groups stay organisation-only;
flattener untouched), `GRAPH_EDITOR_REDESIGN.md` (the node-face row infrastructure P4
reuses), `IMPORT_DESIGN.md` (the importer this changes is its shipped mesh-level door).

---

## 1. Audit — what exists (verified 2026-07-06, this session)

Instruction to executors: **extend, don't redesign.** Re-verify anchors at phase entry;
a moved anchor is an escalation, not a guess.

| Piece | Where | State |
|---|---|---|
| `render_scene` per-object params | `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:223-296` (`rebuild`) | 9 TRS `ParamDef`s per object, labels identical across objects ("Position X" ×N). `Cow<'static, str>` names, generated from counts — the node cannot know object names |
| `render_scene` param consumption | `render_scene.rs:640-655` (`evaluate`) | Nine `ctx.params.get(format!("pos_x_{n}"))` reads → `model_matrix(pos, rot, scale)` at `:419` |
| CPU-struct input accessors | `render_scene.rs:552-566` — `ctx.inputs.camera(..)`, `.light(..)`, `.material(..)` at `:614` | The Camera/Light/Material port pattern is proven end-to-end; a Transform port is a fourth instance of it, not new plumbing |
| No Transform port | `crates/manifold-renderer/src/node_graph/ports.rs:17-53` (`PortType`) | Negative claim, search run: variants are Texture2D(+Typed), Texture3D, Scalar, Array, Camera, Light, Material. No Transform/Mat4 |
| No mesh-TRS atom | `rg -i "transform" primitives/` | `affine_transform` = 2D UV effect; `generate_instance_transforms` + `InstanceTransform` (`generators/mesh_common.rs:93`, GPU Pod `pos_scale`/`rot_pad`) = the *instancing* array path — different semantics (many anonymous copies, GPU data), stays separate |
| Expose flow label | `crates/manifold-editing/src/commands/graph.rs:1497-1568` (`mirror_effect_side`) | Card slider label = the bare inner `ParamDef` label — exposing `pos_x_2` yields an anonymous "Position X" |
| Expose command scope | `graph.rs:1080-1096` (`ToggleNodeParamExposeCommand`) | Carries `scope_path: Vec<u32>` — the enclosing-group chain is ALREADY known at expose time; the section seed reads the innermost group's name from it |
| Card spec fields | `crates/manifold-core/src/effect_graph_def.rs:450-491` (`ParamSpecDef`) | `is_angle` (`:489`) is the exact precedent for adding a serde-skipped display field |
| Card rendering | `crates/manifold-ui/src/panels/param_card.rs:2205` (`build_generator`) + `build_effect` | Flat slider list, manifest order. **No section concept exists anywhere in the card UI** (searched panels/) |
| Node-face rows | `GRAPH_EDITOR_REDESIGN.md` on-node phases 1–6 (all ✅ 2026-07-01); `graph_canvas/model.rs` (`NodeRow`, `compute_node_rows`) | Regular nodes render param rows with sliders/checkboxes/editors; the row substrate P4 reuses. Canvas already computes wire-driven/outer-driven state (`apply_driven_state`, `outer_routings`) |
| Group box rendering | `graph_canvas/model.rs:114-123` (`is_group`, `group_tint`) | Groups draw as tinted boxes with interface ports only — no param rows |
| Group exposure policy | `NODE_GROUPS_UI_DESIGN.md` status | Phase D (interface editing) **dropped** — Peter 2026-06-13: organisation-only, exposure direct-to-card. This design keeps that; **no live group-param runtime**. *(F14 clarification 2026-07-10: `COMPONENT_LIBRARY_DESIGN.md` §4/§4a is the sanctioned `GroupParamDef` consumer — but declaration-only: component macros are `GroupParamDef` entries that **lower onto ordinary card `BindingDef`s at expose** (COMPONENT §4b), so the thing this design kills — a live group-param interface runtime — stays dead. "`GroupParamDef` stays unused" was too strong; "no live group-param runtime" is the real invariant.)* |
| glTF importer | `crates/manifold-renderer/src/node_graph/gltf_import.rs:274-669` (`build_import_graph`) | Already builds one named+tinted group per material with stable inner `node_id`s; curates a 13-slider card (camera/sun/reflections + per-object metallic/roughness with " 2"-style suffixes); sets recenter via `pos_x_{k}` params ON the render node (`:510-518`); **no transform sliders on the card at all** |
| Stale importer cap | `gltf_import.rs:45` (`MAX_RENDER_SCENE_OBJECTS = 8`) | Comment says "mirrored from node.render_scene's own MAX_OBJECTS" — that constant was deleted 2026-07-05 (`render_scene.rs:64`, `OBJECT_SLIDER_MAX = 64`). Imports silently drop materials past 8 while the renderer is uncapped. Fixed in P2 |
| Migration chain | `crates/manifold-io/src/migrate.rs:5-84` | Version-gated `Value → Value` steps, top currently `1.11.0`; `migrations/param_storage_v14.rs` is the quarantined-module precedent |
| Fan-out bindings | `effect_graph_def.rs:404-413` (`BindingDef`, one id → many targets, per-target `scale`/`offset`) | Ableton-style macros are already representable — a load-bearing input to the Phase-D kill |
| As-built REALTIME_3D deviation | `render_scene.rs:26` header; BASELINE_REVIEW_2026_07 | Object transforms are NOT port-shadowed; P6 gizmos not built. This design supersedes the "additive follow-up" note — see §8 |
| `EffectGroup` | `crates/manifold-core/src/effects.rs:2457` | Rack-grouping of whole effect *cards* (BUG-019's subject) — a different layer; NOT this design's mechanism. Named here so no executor conflates them |

`⚠ VERIFY-AT-IMPL` (all phases): the boundaries wave and other landings will have moved
lines; re-run the anchors above before editing. Line drift alone is not an escalation —
a missing symbol is.

## 2. Decisions

**D1 — New CPU-struct port: `PortType::Transform`.** A plain CPU struct
`Transform { pos: [f32; 3], rot_euler: [f32; 3] /* radians */, scale: [f32; 3] }`,
flowing exactly like Camera/Light/Material (same producer/consumer accessor pattern,
same lifetime model, zero GPU resources in the wire — so zero interaction with texture
prebinding or pooling). Identity default (`pos 0, rot 0, scale 1`).
Rejected: **`SceneObject` bundle port** (mesh+material+texture+transform in one wire) —
REALTIME_3D D3 rejected it as "zero authoring win"; this session *upgraded* the
rejection: a texture or GPU-array reference inside a CPU struct wire evades the
prebind-all-textures walk (`feedback_node_graph_prebind_all_textures`) — a liveness
hazard, not a taste call. The port wall is instead mitigated visually (D8).
Rejected: reusing `Array(InstanceTransform)` for scene objects — that is the GPU
instancing path (many anonymous copies); scene objects are few, named, and
individually addressed by gizmos and cards. The two stay distinct on purpose.

**D2 — One new atom: `node.transform_3d`** (name = Peter's pick, disambiguating from
the 2D `affine_transform`). Nine params mirroring `render_scene`'s current defs
verbatim (same labels, ranges, `ParamType::Angle` radians for rotation — the `is_angle`
degree display then works for free at expose), **each port-shadowed by a same-named
scalar input port** per the control-wires convention
(`project_control_wires_port_shadows_param`: prefer `ctx.inputs.scalar(name)`, fall
back to the param). One output `transform: Transform`. This closes REALTIME_3D's
"transforms not beat-addressable" as-built gap in the right place: an LFO wired to
`rot_y` is a spinning object; a `beat_ramp` into `pos_z` is a drop hit.
Optional input `parent: Transform` is **Deferred** (§9) — it is the v2 hierarchy
mechanism (compose parent × local), designed-for but not built.

**D3 — `render_scene` sheds all per-object transform params; gains
`transform_n: Transform` optional ports.** `rebuild()` emits only `objects` + `lights`
params; each object's port group becomes `mesh_n` (required) + `material_n` (required)
+ `base_color_map_n` (optional) + `transform_n` (optional; unwired = identity).
`evaluate` reads `ctx.inputs.transform(&format!("transform_{n}"))` and feeds the
existing `model_matrix`. **This amends REALTIME_3D D3** (see §8). The node face drops
from 2+9N rows to 2.
**The plausible-wrong architecture, forbidden by name:** keeping the 9N params as an
unwired fallback ("port shadows the params, no migration needed"). No. That is the
parallel-old-path pattern — two substrates forever, the anonymous wall stays, and
every consumer (gizmos, cards, migration, tests) must handle both. The params are
deleted; the migration (D4) carries old saves across. Port-shadows-param is a
*per-scalar* convention on atoms; it does not license a struct port shadowing nine
params on an aggregate.

**D4 — One-time shape-triggered migration in the project chain.** New step
`migrate_v1110_to_v1120` (re-derive the actual top version at execution:
`rg -n "is_version_less_than" crates/manifold-io/src/migrate.rs | tail -1`), quarantined
module `crates/manifold-io/src/migrations/scene_transform_v1120.rs` shaped like
`param_storage_v14.rs`. For every `node.render_scene` node in every instance graph
(walk the same JSON homes v14 walks — layer/master/clip effects + generators +
`embeddedPresets` defs): for each object index `k` present, if any
`pos_*_{k}/rot_*_{k}/scale_*_{k}` param exists on the node → synthesize a
`node.transform_3d` node (fresh doc id, handle `transform_{k}`, params carrying the old
values), wire its `transform` output to `transform_{k}`, delete the old params from the
render node, and **re-point any `BindingDef`/`UserParamBinding` targeting the render
node's `pos_x_{k}`-family params** to the new node's `pos_x`-family (same values,
same ids — card sliders keep working across the migration). The synthesized node is
placed *inside* object `k`'s group when the render node's `mesh_{k}` input traces to a
group output (the importer shape), else at top level.
Failure story: a transform param with a malformed value → drop that param, warn, keep
the rest (v14's posture). The migration never aborts a load.

**D5 — Card sections: `ParamSpecDef.section: Option<String>`.**
`#[serde(default, skip_serializing_if = "Option::is_none")]` — every existing preset
stays byte-identical (the `is_angle` precedent at `effect_graph_def.rs:489`). Seeding:
(a) **at expose** — `ToggleNodeParamExposeCommand` already carries `scope_path`; the
mirror write resolves the innermost group's display name and stamps it on the new
spec; top-level nodes get `None`; (b) **by the importer** — each object's card knobs
get `section: <object name>`, the shared knobs get `"Camera"` / `"Sun"` /
`"Environment"` (and the importer's " 2"-style label suffixes are dropped — the
section now carries the identity); (c) **manifest-stored, never derived at display**
— the card keeps reading only the manifest (BOUNDARIES D4); deriving sections from
graph structure at render time would re-couple the card to graph reads, the exact
seam that wave closes. Group *rename* therefore also sweeps sections: the existing
rename command additionally rewrites `section` on specs whose value equals the old
name AND whose binding target resolves inside the renamed group — one undoable
command, both writes. A hand-edited section (different string) is untouched by the
sweep.
Card rendering: contiguous runs of the same `section` draw under one collapsible
header row (name + fold triangle + row count when folded); `None` runs render exactly
as today. Fold state is UI-local (workspace state, like node collapse), not
serialized — honest cost: folds reset on app restart; persistence is Deferred with a
trigger (§9).
Rejected: **reviving `GroupParamDef` as a live group-param runtime / NODE_GROUPS_UI
Phase D** — a second card-to-node resolution path beside the just-audited manifest/binding
system, its own authoring UI, and its one real superpower (one knob → many targets) already
exists as fan-out `BindingDef`s. Peter re-confirmed the kill 2026-07-06 after full pricing.
*(F14, 2026-07-10: this rejects a **live runtime**, not the `GroupParamDef` **type** —
`COMPONENT_LIBRARY` §4 reuses the type as a declaration schema that lowers to those same
fan-out `BindingDef`s at expose. Same kill target, different word: no live group-param
runtime.)*

**D6 — Group boxes render their exposed params as live rows.** (Peter, 2026-07-06:
groups get "sliders and checkboxes like standard nodes so you can play with the
sub-node values from the group node itself.") A group `NodeView` gains rows for every
card param whose binding target resolves to a node inside it (transitively, nested
groups included) — the same join the canvas already computes for the "↳ outer-driven"
hints (`outer_routings` / `apply_driven_state`). Each row reuses the existing
`NodeRow` render/hit/scrub machinery and **emits the identical command the perform
card's slider emits for that param** (parity invariant — one value, three surfaces:
card, group face, inner node face; `⚠ VERIFY-AT-IMPL`: the card slider's dispatch
site — `rg -n "SetCardParam\|card_param" crates/manifold-app/src/ui_bridge/inspector.rs`
— transcribe, don't invent). Rows are read-only under the same conditions the card
slider would be. This is display + dispatch only: no new state, no new command type,
no group machinery.

**D7 — One composite command: `AddSceneObjectCommand`.** The add-object gesture
(button on the `render_scene` node face, next to the `objects` param row): one
undoable command that (1) bumps `objects` by one, (2) creates a new group named
"Object N" containing a `node.generate_cube_mesh` (placeholder), a
`node.phong_material` with a distinct hue, and a `node.transform_3d`, (3) wires the
group's three outputs to the new `mesh_k` / `material_k` / `transform_k` ports.
**One click = a visible object appears**, in a named box, with a grabbable transform —
replace the cube by rewiring `mesh_k`'s producer inside the group. This exists because
the alternative (bump count, leave `mesh_k` unwired) magenta-errors the whole scene
under render_scene's no-silent-fallbacks contract — correct behavior, hostile
mid-gesture; the placeholder keeps the contract intact (nothing is ever silently
skipped) while making the gesture completable. Remove = the existing delete-group +
decrement, no new command.

**D7a — `AddSceneLightCommand`, the light twin (resolved 2026-07-10, Fable + Peter;
supersedes the OPEN GAP raised by Opus the same day).** D7 gives objects a one-click
add; lights get the symmetric gesture, in scope for this doc, shipping in P5 beside
`AddSceneObjectCommand`. One composite undoable command on a "+ Light" button on the
`render_scene` node face next to the `lights` row: (1) bump `lights` by one, (2) spawn
a `node.light` — **bare on the canvas, no group** (an object's group earns its box by
holding three nodes; a light is one node, and a one-node group taxes every future edit
for zero legibility) — named "Light N", placed adjacent to its port, (3) **auto-wire**
it into the new `light_k` port (Peter's ruling: add means added — both commands wire on
click, never leaving a count bumped with a dead port). Defaults: Sun, white, intensity
1.0, ~45° elevation (a straight-overhead sun flattens the scene; 45° is why Blender's
default reads as lit), **`cast_shadows` ON (Peter's call, 2026-07-10)**. The shadow flag
is inert until REALTIME_3D P2 ships; P2 inherits a requirement from this default,
**revised same day (Peter's ruling, supersedes the earlier "budget/priority" phrasing):
every caster renders accurate shadows, always — no silent caster budget, no
priority-based dropping (that would be the silent-fallback pattern this codebase bans;
a scene that quietly stops casting on light N looks subtly wrong with no cause).
Shadow cost is the user's to spend; the design owes them VISIBLE cost feedback
(perf-HUD weight per caster or equivalent) so adding lights is an informed gesture,
not a discovered stutter.** This lands in the coherence-audit F2 amendment to
REALTIME_3D P2 (not this doc's D4, which is the migration step). Remove = delete the
light node + the existing decrement, no new command. Context: `RENDER_SCENE_UNBOUNDED_LIGHTS` (landed
2026-07-10) lifted the light ceiling to 64/127 but kept the count-param + per-`light_N`-
port model (REALTIME_3D D3 rejected an `Array<Light>` fan-in port); D7a is the authoring
gesture over that model.

**D8 — Same-pair wire ribbons (canvas-wide).** When ≥2 wires connect the same
(source node, dest node) pair, the canvas draws them as ONE ribbon with an `×N` badge;
hover (or either endpoint selected) expands to the individual wires for picking. Pure
render + hit-test change in `graph_canvas` — no data change, no snapshot change. The
import graph's 12-wire wall becomes 4 ribbons. Benefits every multi-wire graph
(fluid sim's fan-outs included).

**D9 — Importer emits the end-state shape.** Per object group: a `node.transform_3d`
inside (seeded `pos = -center` — the recenter moves off the render node), a fourth
interface output `transform`, wired to `transform_k`. Card: per-object knobs sectioned
by object name; **no transform sliders on the card by default** (the card is the
performance surface, not the scene editor — transforms are performed via expose-what-
you-need, gizmos when P6 lands, or the group face). Cap fix: `MAX_RENDER_SCENE_OBJECTS`
dies; the truncation threshold becomes `OBJECT_SLIDER_MAX` (64), keeping
largest-first ordering and the loud warning above it.

## 3. Data model (committed — the executor transcribes)

```rust
// crates/manifold-renderer/src/node_graph/transform.rs — sibling of camera.rs/light.rs
/// Local TRS of one scene object. CPU-only wire value (PortType::Transform),
/// composed to a model matrix by the consuming renderer per frame.
/// Euler radians, XYZ application order — matching render_scene's existing
/// model_matrix (render_scene.rs:419), which is unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub pos: [f32; 3],
    pub rot_euler: [f32; 3], // radians
    pub scale: [f32; 3],
}
impl Default for Transform {
    fn default() -> Self { Self { pos: [0.0; 3], rot_euler: [0.0; 3], scale: [1.0; 3] } }
}
```

- `PortType::Transform` variant + snapshot/validator/editor-color plumbing:
  pattern-copy the `Material` port end-to-end (M1's file list is the checklist; the
  editor pin color is a new entry in the type-color table, executor's pick from the
  unused palette).
- Producer/consumer accessors: `NodeOutputs::set_transform(port, Transform)` /
  `ctx.inputs.transform(name) -> Option<Transform>` — shape identical to
  `set_light`/`light`.
- `node.transform_3d`: params exactly the nine defs currently in
  `render_scene.rs:223-296` (labels, ranges, types verbatim, minus the `_{i}`
  suffixes); nine same-named optional scalar input ports; output
  `transform: Transform`; full `PrimitiveDescription` (purpose + composition_notes
  per ADDING_PRIMITIVES).
- `ParamSpecDef` gains `section: Option<String>` (serde rules per D5). It rides the
  V1.4 wire automatically: inline for user-added specs, via `meta.params` templates
  for authored cards.
- `AddSceneObjectCommand` lives in `manifold-editing/src/commands/graph.rs`, shaped
  like the existing group-creation command + `SetGraphNodeParam` composed as one
  undo unit (`⚠ VERIFY-AT-IMPL`: the compound-command precedent —
  `rg -n "struct GroupNodesCommand|fn undo" crates/manifold-editing/src/commands/graph.rs`).

Freeze/fusion: CPU-struct wires don't participate in pointwise fusion, and
`render_scene` is a raster node (never fused). `⚠ VERIFY-AT-IMPL`: confirm no
`PortType` match in the freeze compiler needs the new arm —
`rg -n "PortType::" crates/manifold-renderer/src/node_graph/freeze/ | rg -v test`.

## 4. What it buys on stage

- "Which Position X is Leaf?" stops being a question: Leaf's transform lives in the
  Leaf box, its card sliders sit under a "Leaf" header, and the Leaf box itself has
  the sliders on its face.
- Per-object transforms become part of the instrument for the first time: wire an LFO
  to a hero object's `rot_y` and it spins on the beat; expose `pos_z` and ride it from
  a MIDI knob; when P6 lands, grab it in the viewport — all the same param.
- A 10-object scene's card is ten named foldable blocks, not 90 sliders.
- Adding an object is one click and something appears on screen — the Unity/Blender
  reflex, not count-bookkeeping plus hand-wiring into a port wall.

## 5. Phasing

Forbidden, all phases: keeping any per-object transform param on `render_scene`
(parallel old path) · deriving card sections from graph structure at display time ·
touching `GroupParamDef`/flattener param overrides · `Arc<Mutex>` anywhere · new
port types beyond `Transform` · widening into REALTIME_3D P5/P6 viewport work.

### P1 — Transform port + `node.transform_3d` (one session)

- **Entry state:** clean tree off current `origin/main`; `rg -n "Transform" crates/manifold-renderer/src/node_graph/ports.rs` → no variant; §1 anchors re-run.
- **Read-back:** this doc §2 D1/D2, §3; `ports.rs` whole; one Material-port plumbing
  commit (`git log --oneline -S "PortType::Material" -- crates/manifold-renderer` →
  read the M1 diff); `node.light`'s producer atom end-to-end;
  `project_control_wires_port_shadows_param` memory. Restate the §2.5 audit verdict:
  port = one-wire-from-existing (fourth CPU-struct port), atom = genuinely new
  (`affine_transform` is 2D UV; `InstanceTransform` is instancing data).
- **Deliverables:** `transform.rs`, `PortType::Transform` + full plumbing,
  `set_transform`/`inputs.transform`, `node.transform_3d` (registered, descriptor,
  §2.5-audited), unit tests: identity default; param→output; each scalar port
  overrides its same-named param (the port-shadows contract, one test per family).
- **Gate.** Positive: `cargo test -p manifold-renderer --lib transform` green;
  `check-presets` clean. Negative: `rg -n "Arc<Mutex|Arc<RwLock" crates/manifold-renderer/src/node_graph/transform.rs primitives/transform_3d.rs` → 0.
  **Demo:** none — L1 (nothing consumes the port yet; P2 is the vertical slice).
- **Forbidden:** consuming the port in any renderer this phase; inventing a matrix
  type on the wire (the wire carries TRS; matrices are composed by consumers).
- **Test scope:** focused `-p manifold-renderer --lib`. No sweep, no gpu run.

### P2 — the swap: `render_scene` ports, migration, importer (one strong session)

- **Entry state:** P1 landed; `rg -n "pos_x_" crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` shows the param generation; the migration-chain top re-derived (D4 command); `~/Downloads/meshImportTests.manifold` present (ask Peter if moved).
- **Read-back:** §2 D3/D4/D9, §3, §8; `render_scene.rs` whole; `gltf_import.rs:274-669`;
  `migrations/param_storage_v14.rs` (the quarantine pattern); DESIGN_DOC_STANDARD §5
  round-trip gate. Restate: what is deleted, what replaces it, the forbidden fallback.
- **Seam brief.** Old → new: `rebuild()` loses the 9-param loop (`:223-296`), the port
  loop gains `transform_{i}: PortType::Transform, required: false`; `evaluate` replaces
  `:640-655` with one `ctx.inputs.transform(..).unwrap_or_default()` per object feeding
  the unchanged `model_matrix`. Compiler-driven where possible; the param *strings*
  aren't compile-checked — the two importer tests
  (`assembles_azalea_into_two_object_render_scene_graph`,
  `render_scene_with_three_objects_loads_per_object_transform_params` — the latter is
  **rewritten** to prove transform *nodes* load, its original subject no longer exists)
  and the migration fixtures are the checklist for the string layer. Call-site
  re-derivation at entry: `rg -n "pos_x_|rot_x_|scale_x_" crates/ --type rust` — every
  hit must be accounted for in the phase notes before editing (expect: render_scene,
  gltf_import, their tests, and nothing else; a surprise hit = stop and list).
- **Deliverables:** the `ParamSpecDef.section` field itself (D5 serde rules — added
  here, not P3, so the importer below can seed it; P3 owns everything that *reads*
  it); the render_scene swap; `scene_transform_v1120.rs` + chain step +
  frozen JSON fixtures (a 2-object project with edited transforms + an exposed
  `pos_x_1` binding — asserts values land on the synthesized nodes AND the binding
  re-points; a malformed-value case); importer per D9 (transform node in group, 4th
  interface port, cap fix, recenter moved); both importer tests updated; the
  group-placement rule for synthesized nodes.
- **Gate.** Positive: `cargo test -p manifold-renderer --lib` + `-p manifold-io --lib
  migrations::` + the importer tests green; **round-trip gate**:
  `meshImportTests.manifold` loads through the migration → all params resolve, cam
  orbit driver still runs, save → reload → transforms intact (throwaway scratch test,
  deleted after, per the BUG-036 session pattern); **held-out input**: one of the
  three CC0 scans in `tests/fixtures/gltf/` (VD-003 fixtures) imports and renders —
  not the azalea the code was developed against; focused GPU run:
  `cargo test -p manifold-renderer --features gpu-proofs render_scene` (the gpu tests
  now wire transform nodes; the shader and uniforms are untouched — if any .wgsl diff
  appears in this phase, stop, something went wrong). Full workspace sweep + clippy
  (port type + migration = infra). Negative:
  `rg -n "pos_x_|rot_y_|scale_z_" crates/manifold-renderer/src/node_graph/primitives/render_scene.rs` → **0 hits**;
  `rg -n "MAX_RENDER_SCENE_OBJECTS" crates/` → 0 hits.
- **Demo (L2):** headless render of the migrated meshImportTests project — PNG diff
  against a pre-migration render of the same project on the parent commit; identical
  pixels (the migration preserves values exactly). Attach both PNGs.
- **Performer gesture:** wire `node.lfo`→`rot_y` on an imported object's transform
  node; the object spins in the preview (eyeball the headless frame pair at two beat
  phases — two PNGs, different rotation).
- **Forbidden:** the D3 fallback (params kept "just in case") · migrating inside typed
  serde · consulting the live registry from the migration (quarantine rule) ·
  regenerating v14's baked tables · touching `render_mesh`/`render_copies`.
- **Test scope:** full sweep + focused gpu-proofs (stated above).

### P3 — card sections (one session; **entry-gated on BOUNDARIES P2**)

- **Entry state:** BOUNDARIES P1–P2 landed (`rg -n "reconcile_param_manifests" crates/manifold-io` hits; state_sync card rows read `inst.params` — spot-check the fn);
  P2 of THIS doc landed (sections get seeded with real content by the importer).
- **Read-back:** §2 D5; `ParamSpecDef` (`effect_graph_def.rs:450-491`);
  `mirror_effect_side` + `ToggleNodeParamExposeCommand.scope_path`;
  `param_card.rs` `build_generator`/`build_effect`; the group-rename command
  (`rg -n "RenameGroup" crates/manifold-editing`).
- **Deliverables:** expose-time seeding from `scope_path`'s
  innermost group name (both effect and generator mirror arms — the field itself
  landed in P2); rename-sweep in the rename command + inverse restore on undo; card header row +
  fold (contiguous same-section runs; workspace-local fold state); section shown in
  the param mapping editor as an editable text field (the calibration popover —
  `EditParamMappingCommand` gains the write, dual-write rules per BOUNDARIES D4:
  manifest spec only).
- **Gate.** Positive: focused `-p manifold-core --lib`, `-p manifold-editing --lib`,
  `-p manifold-ui --lib`; round-trip test: expose-inside-group → section on spec →
  save → reload → card row still sectioned; rename-group test: section follows,
  hand-edited section survives; serde guard: a preset with no sections re-serializes
  byte-identical. Negative: `rg -n "section" crates/manifold-app/src/ui_bridge/state_sync.rs`
  shows reads from manifest spec only (no graph-def reads).
- **Demo (L3):** a `scripts/ui-flows/` flow (copy `select-and-inspect.json`): open the
  imported scene's card, assert a section header's text equals the glTF object name,
  click it, assert the child rows collapse.
- **Performer gesture:** fold "Bark", leave "Leaf" open, drag Leaf's Metallic — the
  card stays one screen tall on a 4-object scene.
- **Forbidden:** deriving section at display · a second fold-state home in the model ·
  reordering manifest entries to force contiguity (display grouping only groups
  *contiguous runs*; the importer/expose already emit contiguously — a
  non-contiguous duplicate section name renders as two headers, accepted).
- **Test scope:** focused crates above. No sweep.

### P4 — group-face param rows (one session)

- **Entry state:** P3 landed (sections exist — the join key); the on-node row infra
  anchors (`compute_node_rows`, `apply_driven_state`) re-verified.
- **Read-back:** §2 D6; `graph_canvas/model.rs` rows + driven-state; the perform-card
  slider dispatch site (D6's VERIFY marker — resolve it FIRST and restate the command).
- **Deliverables:** group `NodeView` rows for card params targeting inner nodes
  (transitive); render + hit + scrub emitting the card param's exact command;
  read-only states mirrored; a collapsed group shows a compact "N params" chip, an
  expanded group shows the rows — no size threshold, collapse state is the switch.
- **Gate.** Positive: `-p manifold-ui --lib` canvas tests: rows appear for
  inner-targeted card params; scrub emits the same command struct the card emits
  (assert equality against the card path's constructor); nested groups: rows render
  on the group box visible at the **currently-viewed level** only — never on two
  boxes at once; entering a group re-homes its inner params' rows one level down
  (test at root and one level deep).
- **Demo (L2):** headless `ui-snap editor` on the imported scene — the "Leaf" box
  visibly carries slider rows; PNG read by the landing session.
- **Forbidden:** a new command type · rows for non-exposed inner params (the face
  shows the *card surface*, not an authoring picker — that picker was deleted
  2026-07-01, do not resurrect it on the group box).
- **Test scope:** focused `-p manifold-ui --lib`. No sweep.

### P5 — gestures: add-object + wire ribbons (one session, closes the wave)

- **Entry state:** P2 landed (transform atoms exist for the composite to spawn).
- **Read-back:** §2 D7/D7a/D8; the compound-command precedent (D7 VERIFY marker);
  `graph_canvas` wire render (`render.rs` wire pass) + hover-highlight code (the
  "must preserve" list in GRAPH_EDITOR_REDESIGN §7).
- **Deliverables:** `AddSceneObjectCommand` + node-face "+ Object" button on
  `render_scene` (a `NodeRow` action row, same geometry-source discipline);
  `AddSceneLightCommand` + "+ Light" button per D7a (bare `node.light`, auto-wired,
  D7a defaults incl. `cast_shadows` ON);
  same-pair ribbon render + `×N` badge + hover/selection expansion (keep wire
  hover-dim, arc routing, feedback-wire styling intact).
- **Gate.** Positive: command tests — add object → objects bumped, group + 3 wires
  exist, undo restores exactly (inverse-pair test); add light → lights bumped, bare
  `node.light` wired to `light_k` with D7a defaults, undo restores exactly; canvas
  tests — ≥2 same-pair wires produce one ribbon view, hover expands, single wires
  unaffected. **Full workspace sweep + clippy** (wave-closing phase). Negative:
  `rg -n "virtual.*socket|auto_grow" crates/manifold-ui` → 0 (the deferred
  auto-grow didn't sneak in).
- **Demo (L2 + gesture):** headless editor PNG before/after one Add Object — a new
  tinted "Object 3" box, a cube visible in the preview pane, ribbons where the wall
  was; and one Add Light — the preview visibly re-lit, a wired "Light N" node beside
  the render node. **Performer gesture:** click "+ Object" or "+ Light" once;
  something appears on screen without touching a port.
- **Test scope:** full sweep (final phase).

## 6. Performance (stated honestly)

Per frame, per object: nine `ParamValues` string-map lookups are replaced by one
CPU-struct input read (or an identity default) — strictly less work. `transform_3d`
itself is a trivial CPU node (nine reads, one struct write). Sections and group-face
rows are UI-thread display work, off the show path entirely. Ribbons *reduce* wire
draw calls. The migration is load-time-only, linear in nodes. Nothing here touches
the content-thread frame budget; no MANIFOLD_RENDER_TRACE gate is owed (no phase adds
content-thread work — if any phase finds itself doing so, that's a brief violation,
stop).

## 7. Decided — do not reopen

1. Per-object transforms are graph structure (`node.transform_3d` → Transform port),
   not renderer params. The renderer's params are deleted, not shadowed.
2. `node.transform_3d` / `PortType::Transform` naming (Peter, 2026-07-06).
3. Card bundling = `ParamSpecDef.section`, seeded from group names, stored on the
   manifest spec, never derived at display. Phase D (the live group-param runtime) stays
   dropped (re-confirmed 2026-07-06 after pricing). *(F14, 2026-07-10: the `GroupParamDef`
   **type** is not dropped — `COMPONENT_LIBRARY` §4 reuses it as a declaration schema that
   lowers to card `BindingDef`s; card sections here are orthogonal to component macros.)*
4. Group boxes render their exposed-param rows; same command path as the card —
   three surfaces, one value, zero new machinery.
5. Exposure stays direct-to-card by nodeId (2026-06-13 decision intact).
6. `SceneObject` bundle port stays rejected — upgraded to a liveness hazard
   (textures inside CPU struct wires evade prebinding).
7. ~~Scene-object list panel: killed. The graph's named groups + card sections + the
   P5 viewport are the list; a panel would be a second authoring model
   (REALTIME_3D decided-#1).~~ **SUPERSEDED 2026-07-16 by
   `SCENE_SETUP_PANEL_DESIGN.md` D1 (Peter's directive).** The panel that shipped
   that kill's fear — a second authoring *model* — is still dead: the Scene Setup
   panel is a pure view emitting the identical commands the card/group-face emit
   (this doc's own D6 "one value, three surfaces" defense, extended to a fourth
   surface). REALTIME_3D decided-#1 (no scene document) stands.
8. Add-object spawns a visible placeholder (cube + material + transform in a named
   group); empty-slot-then-magenta rejected as the gesture.
9. Migration is shape-preserving and binding-re-pointing; old saves render
   pixel-identical.
10. `main` merge-trunk discipline; each phase lands via fetch/merge/gate/push
    (`.claude/GIT_TREE_DISCIPLINE.md`).

## 8. Amendments to REALTIME_3D_DESIGN.md (applied in this session's landing)

- **D3 amended:** object port group = `mesh_n` + `material_n` + `base_color_map_n` +
  `transform_n: Transform` (was: "transform_n (TRS params, port-shadowed)"). The
  "object transforms NOT port-shadowed yet" as-built deviation is retired — the
  substrate moved rather than gaining shadows.
- **D8 clarified (semantics unchanged):** gizmos remain param editors via
  `EditingService`; the params they write are the object's `node.transform_3d`
  params. A wired *scalar port* on that atom locks that gizmo axis (per-axis lock,
  finer than the old whole-transform lock). Gizmo target resolution = follow the
  `transform_n` wire to its producing atom.
- **P6 entry state gains:** this doc's P2 landed; if an object's `transform_n` is
  unwired, the gizmo offers to create the atom (an `EditingService` command, P6's
  business — briefed there, not here).

## 9. Deferred (with revival triggers)

- **`parent: Transform` input on `node.transform_3d`** (hierarchy v2) — revive when
  glTF hierarchy import lands or totemed set pieces demand parenting. Compose
  parent × local; no type changes (REALTIME_3D D1's promise, kept).
- **Transform port on `render_mesh` / `render_copies` / `render_splats`** — additive;
  revive on first real ask. *(F4, 2026-07-10: `GAUSSIAN_SPLATS` D10 keeps port-shadowed
  TRS params for single-object placement and defers its optional `transform: Transform`
  override to **this** trigger — splat placement becomes P6-gizmo-addressable by the same
  mechanism and at the same time as meshes.)*
- **Card section fold persistence** — revive if Peter asks for folds surviving
  restart during set prep; home would be workspace serialization, never the spec.
- **Virtual auto-grow sockets** (wire into a ghost port → objects bumps) — revive if
  the add-object button proves insufficient; canvas-only change.
- **Group interface params (Phase D)** — revive only for swappable "rack" presets
  with stable knob surfaces (contents change, knobs don't). Sections don't block it.
  *(F14, 2026-07-10: that "rack presets" use case **is** `COMPONENT_LIBRARY` — it reuses
  `GroupParamDef` as a declaration schema lowered to card `BindingDef`s (its §4b), which is
  a component macro layer, not the live Phase-D runtime this doc kills. Named so the two
  don't read as contradicting.)*
- **Per-object normal/roughness/metallic map ports** — pre-existing render_scene
  deferral, unchanged by this design.
