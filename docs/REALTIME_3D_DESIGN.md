# Realtime 3D — Scenes, Lighting, Viewport

**Status: IN PROGRESS (status corrected + baseline-reviewed 2026-07-05; D3/D8 AMENDED 2026-07-06 by `SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` — read its §8 before P6; D3/D4/§3/§6/§7.3 AMENDED 2026-07-10 (F2 coherence audit) — shadow-caster cap `MAX_SHADOW_CASTING_LIGHTS = 4` replaces the dead "8 objects, 4 lights" budget, read D4 before P2).** Shipped: P0 (MATERIAL M1–M6, all verified in-tree), P1 `node.render_scene` @ `8daa89fc`, P4 camera atoms (both `node.free_camera` + `node.look_at_camera` in-tree), §9 `node.spawn_from_mesh`. **The P1 "transforms not port-shadowed" deviation is retired by amendment, not by shadows: per-object transforms move to `node.transform_3d` atoms feeding `transform_n: Transform` ports** (SCENE_BUILD P2). Remaining: P2 shadows, P3 atmosphere, P5 viewport navigate, P6 gizmos, P7 scene starter preset. · designed 2026-07-03 · Fable
**Prerequisites: MATERIAL_SYSTEM_DESIGN M1–M5 (un-held by this doc — its contract is
unchanged; this design consumes its extension points). Vocab-audit apply should land
first (this doc uses post-rename ids: `node.render_mesh`, `node.render_copies`).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting
any phase.**

Peter's directives (2026-07-03): proper realtime 3D scenes with proper lighting — "I
think we have all of the basics but don't have it fleshed out yet so it's easy to use."
**Blender import = strategic yes**: "would be amazing and seriously open up Manifold as
a real contender in this space that beats TouchDesigner at this type of thing." **Fog =
v1**: "fog in general is just a nice look." Huge scenes = v2. **"Gizmos are
important."** Viewport controls: "industry standard is best," refine later. Queued
after this: realtime simulations (Houdini/Nuke tier — separate discussion; the
vertex-cache import path in §8 is the cheap bridge).

Companions: `MATERIAL_SYSTEM_DESIGN.md` (surface shading — required reading),
`CAPABILITY_ROADMAP.md` §4 (the original gap analysis), `GRAPH_EDITOR_REDESIGN.md`
(the editor the viewport lives in).

---

## 1. Audit — what exists (verified 2026-07-03)

| Piece | Where | State |
|---|---|---|
| `Light` struct — Sun/Point, PCF softness (3×3/5×5/7×7), `cast_shadows`, `shadow_bias`, `shadow_resolution`, `shadow_view()` | `node_graph/light.rs` | **Shadow-ready.** Only renderer-side consumption (depth pass + PCF) is missing |
| `Camera` struct — pos/basis/view, Perspective/Ortho, orbit builder | `node_graph/camera.rs` | Solid; doc comment already anticipates "free-look / Euler variants" |
| Material port + 4 atoms (unlit/phong/pbr/cel), per-kind pipelines | `MATERIAL_SYSTEM_DESIGN.md` | Designed, now un-held. Multi-light + shadows are its §7 extension points — consumed here, not reopened |
| Single-object renderers | `primitives/render_3d_mesh.rs`, `render_instanced_3d_mesh.rs` (→ `node.render_mesh` / `node.render_copies` post-vocab) | Each renders ONE object into its OWN depth buffer. Two objects cannot occlude each other today |
| Dynamic ports | `EffectNode::reconfigure` hook; first adopter `node.mux_texture` | Proven. Dynamic nodes are hand-written `EffectNode` impls (no `primitive!` macro) |
| Procedural meshes | cube/grid/platonic/hypercube atoms, `arrange_copies`, `push_mesh`, `make_triangles` | The v1 content source. No splines/extrude/import |
| Node preview infra | graph editor preview panes, filmstrip LOD | The substrate the viewport promotes to interactive |
| `InstanceTransform` arrays | `render_copies` input | Transform-as-data precedent |

`⚠ VERIFY-AT-IMPL`: re-verify all anchors; the material tranches will have landed
between this doc and execution.

## 2. Decisions

- **D1 — The scene model is import-shaped.** Peter's call: Blender/glTF import is the
  strategic future. A `.glb` is a flat-or-hierarchical list of (mesh, transform,
  material) nodes + cameras + lights — so MANIFOLD's scene is **a list of objects with
  transforms, rendered into one shared depth buffer**. v1 is a flat list; the
  transform field is hierarchy-ready (v2 parenting composes matrices, changes no
  types). Import itself is a later phase with its own design pass (§8) — this design
  makes it land without rework.
- **D2 — One new renderer: `node.render_scene`. The graph stays the scene assembler.**
  No scene editor document format, no second authoring model. A hand-written dynamic
  node (mux_texture precedent): an `objects` count param reconfigures N object port
  groups. `node.render_mesh` / `node.render_copies` stay for the single-object fast
  path; existing presets untouched.
- **D3 — Object port group = `mesh_n` (Array(MeshVertex)) + `transform_n` (TRS params,
  port-shadowed) + `material_n` (Material).**
  **[AMENDED 2026-07-06 — SCENE_BUILD_AND_GROUP_PARAMS_DESIGN §8: the transform member
  is `transform_n: Transform` (CPU-struct port fed by `node.transform_3d`, whose own
  scalar params are port-shadowed); render_scene carries no per-object params.]**
  **[AMENDED 2026-07-10 (F2) — the "8 objects, 4 lights" caps in the original draft
  are dead. Shipped `render_scene` is `OBJECT_SLIDER_MAX = 64` objects; light count is
  owned by `RENDER_SCENE_UNBOUNDED_LIGHTS_DESIGN.md` (up to 64 soft / 127 hard), not
  this doc. Object/light counts are UI sliders, not the shadow budget — the bounded-cost
  guarantee moved to the shadow-caster cap in D4.]**
  (`light_0..N` dynamic ports — Light is a CPU struct; no `Array<Light>` port type
  is invented). Rejected: a `SceneObject` bundle
  port carrying a GPU-buffer reference + CPU material in one wire — new port
  semantics for zero authoring win.
- **D4 — Shadows v1 = shadow maps for the first `MAX_SHADOW_CASTING_LIGHTS = 4`
  casters, consumed from the existing Light struct.**
  **[AMENDED 2026-07-10 (F2) — caster policy decided now that objects=64 and lights are
  unbounded.]** One depth pass over all objects per shadow-casting light, PCF per the
  struct's softness/bias/resolution. **The shadow-caster cap is a separate, tighter
  budget than the object/light sliders:** shadow maps are honored for the first `K` lights
  **in slot order** whose `cast_shadows == true`, where `K = MAX_SHADOW_CASTING_LIGHTS`
  (a named constant, `= 4`, bumpable like any cap — the perf HUD shows the cost). Lights
  beyond the first `K` casters **still illuminate the scene**; they simply cast no shadow.
  This decouples the shadow bill from the (now-large) object and light counts — Peter's
  call: cap = K. Worst case is `objects × K` depth-pass draws (64 × 4 = 256), not
  `objects × lights` (64 × 64 ≈ 4096); the shadow-map cache keyed `(node, light_slot)` is
  bounded to `K` entries for the same reason. Sun = ortho frustum, Point = single-face
  approximation (both already specified in `light.rs` doc comments).
- **D5 — Atmosphere is a port, like everything else.** New CPU struct + port type
  `Atmosphere` (same plumbing pattern as Material M1) + one atom `node.atmosphere`:
  fog color, density (exp depth fog), height falloff, ambient/sky tint. Wired into
  `render_scene`, applied in its resolve. Fog is v1 per Peter — it's what makes a
  scene read as deep. Unwired = no atmosphere (density 0), never an error.
- **D6 — Camera family grows two atoms:** `node.free_camera` (pos/euler/fov — the
  gizmo- and import-friendly one) and `node.look_at_camera` (pos + target). Both emit
  the existing `Camera` struct. Every spatial param is port-shadowed → **camera moves
  are beat-addressable** (a `beat_ramp` scrubbing a dolly is the sizzle nobody else
  has). `orbit_camera` stays.
- **D7 — The viewport is a promoted preview, not a new app mode.** An interactive
  panel in the graph editor showing `render_scene` output. Tier 1: navigate — an
  **editor camera** (orbit/pan/dolly, industry-standard bindings + trackpad
  gestures, refined later per Peter) that overrides the wired camera in the editor
  preview context only; light billboards, camera frustum lines, ground grid drawn as
  editor-only overlay. Tier 2 (v1, Peter: "gizmos are important"): click-select via
  ID-buffer pick pass; move/rotate/scale gizmos.
- **D8 — Gizmos are param editors.** A gizmo drag writes the object's `transform_n`
  params (or a light's pos/aim, a camera's pos) **through `EditingService` like any
  slider** — undoable, bindable, nothing new in the mutation model. **If the param is
  wired** (transform driven by upstream animation), the gizmo shows locked — the
  viewport never fights the graph.
  **[AMENDED 2026-07-06 — semantics unchanged, substrate moved: the params a gizmo
  writes are the object's `node.transform_3d` params (found by following the
  `transform_n` wire); a wired scalar port on that atom locks that gizmo AXIS.
  P6 entry state: SCENE_BUILD P2 landed; unwired `transform_n` → gizmo offers to
  create the atom (briefed in P6, not in SCENE_BUILD).]** Consequence of gizmos-as-params: anything you can
  grab in the viewport, you can perform from a knob.
- **D9 — Show path never pays for the viewport.** Editor camera, overlays, and pick
  passes run only in the editor preview context. The content-thread render is
  byte-identical with the viewport open or closed. (Watch item: UI-present/content-GPU
  contention — the viewport is one more editor consumer, not a new clock.)
- **D10 — Ease ships as content:** a bundled **Scene starter preset** (mesh + PBR
  material + sun & rim point light + atmosphere haze + orbit camera, good at
  defaults) plus component-library entries once components land. Three clicks: drop
  preset, swap mesh, grab a light.

## 3. Data model (committed)

```rust
// manifold-renderer/src/node_graph/atmosphere.rs — pattern-copy of material.rs (M1)
pub struct Atmosphere {
    pub fog_color: [f32; 4],
    pub fog_density: f32,      // exp depth fog; 0 = off
    pub height_falloff: f32,   // 0 = uniform; >0 = ground haze
    pub ambient_tint: [f32; 4],// scene-wide ambient/sky tint multiplier
}

// render_scene's per-object transform params (per group n, port-shadowed):
// pos_x/y/z, rot_x/y/z (degrees), scale_x/y/z — TRS, composed to a matrix per frame.
// Flat v1; hierarchy (v2) composes parent matrices without changing this surface.
```

`node.render_scene` (hand-written `EffectNode`, no macro): params `objects: Int
(1..=OBJECT_SLIDER_MAX)` (shipped `OBJECT_SLIDER_MAX = 64`), `lights: Int` (range owned
by UNBOUNDED_LIGHTS); ports rebuilt in `reconfigure` —
`camera: Camera required`, `atmosphere: Atmosphere optional`,
`light_0..N: Light optional`, per object `mesh_n / transform params / material_n` +
the material-kind texture inputs shared per the MATERIAL doc. Outputs: `color`,
plus lazy `depth` / `world_normal` (G-buffer outputs, same lazy rule as
`render_mesh`). Internally: per-kind pipelines from the material tranches, one
shared depth texture, shadow-map cache keyed `(node, light_slot)` (two-cache rule
applies).

## 4. What it buys on stage

- Two totems show one scene from two cameras? No — one scene, one camera, but a
  camera whose dolly rides a `beat_ramp`: the drop hits, the camera punches in.
- A hero mesh lit by a slow-orbiting point light = one knob (light orbit bound to a
  macro). Fog density on a fader = instant depth mood.
- Author by grabbing things — Unity-style — then bind the same transforms to MIDI.
  The gizmo and the knob edit the same param.

## 5. Phasing (Sonnet-executable)

Forbidden, all phases: touching `render_mesh`/`render_copies` behavior (additive
design) · `Arc<Mutex>` anywhere · viewport/gizmo state in `manifold-core` (it's
editor state) · gizmo writes bypassing `EditingService` · shadow/fog work when the
feature is unwired (unwired = zero cost, checked, not assumed).

- **P0 — MATERIAL_SYSTEM M1–M5** ✅ SHIPPED (verified in-repo 2026-07-04; as-built
  record in MATERIAL §11.1 — note the single-wgsl entry-point layout, which
  `render_scene`'s per-kind pipelines should reuse, not re-file). **MATERIAL M6**
  (albedo/metallic maps + alpha cutout + back-face fix, MATERIAL §11) slots before
  or parallel to P1 — P1 does not depend on it, IMPORT P1 does.
- **P1 — `render_scene` + multi-light (no shadows).** The dynamic node, object
  groups, shared depth, `light_0..3` accumulation in the lit material shaders
  (MATERIAL §7 multi-light extension point). Read-back: this doc whole, MATERIAL §5/§7,
  `reconfigure` contract (memory: dynamic-ports-infra), `render_3d_mesh.rs` end-to-end.
  Gate: gpu_test — two overlapping meshes occlude correctly from a known camera
  (value-level depth check); two-light accumulation matches single-light × 2 within
  tolerance; existing preset PNG parity untouched (negative gate: zero diffs on
  bundled 3D presets).
- **P2 — Shadow maps.** Depth pass per casting light (first `K = MAX_SHADOW_CASTING_LIGHTS`
  casters, D4) + PCF consumption per the Light struct. Read-back: `light.rs` whole (the
  math is already documented there), `feedback_effect_chain_state_caches`, **D4's caster-cap
  policy (do not price this against object/light slider counts — the cap is `K`).** Gate:
  gpu_test — known box-over-plane scene, assert shadowed vs lit pixel values; softness
  levels change tap counts (verify via perf counter, not eyeball); `cast_shadows=false`
  skips the pass (frame capture shows no depth pass); **a scene with more than `K`
  `cast_shadows` lights produces exactly `K` shadow maps and the extra casters still light
  the scene (assert both: shadow-map count == K, and pixels lit by caster K+1).**
- **P3 — Atmosphere.** Port type + `node.atmosphere` + fog in `render_scene` resolve.
  Pattern-copy MATERIAL M1 plumbing. Gate: gpu_test — fog density curve at known
  depths; density 0 = byte-identical to no-atmosphere.
- **P4 — Camera atoms.** `node.free_camera`, `node.look_at_camera`, full descriptors,
  §2.5 audit first (expect: pure emitters, no new math beyond `look_at_rh`). Gate:
  unit tests on emitted Camera basis; a beat-ramp-driven dolly example preset.
- **P5 — Viewport Tier 1 (navigate).** Editor camera override in the preview context,
  standard orbit/pan/dolly + trackpad, light/camera/grid overlays. Read-back: node
  preview infra, D9. Gate: headless PNG of viewport with overlays; content-thread
  output byte-identical with viewport open (the D9 proof — diff the show render).
- **P6 — Viewport Tier 2 (gizmos).** ID-buffer picking, move/rotate/scale gizmos →
  `EditingService` param commands, wired-param lock state. **Entry state (amended
  2026-07-06): SCENE_BUILD_AND_GROUP_PARAMS P2 landed — gizmos write `node.transform_3d`
  params per the amended D8; brief the unwired-`transform_n` auto-create command
  here.** Gate: gizmo drag round-trips undo/redo; dragging a wire-driven axis is
  refused visibly; headless PNG of gizmo states.
- **P7 — Scene starter preset + polish.** Bundled preset (D10), perf HUD shadow-pass
  cost line, component entries when the component library lands.

Full workspace sweep gates P1 and P2 (graph runtime + new port type = infra); P3–P7
use focused tests per the scope rule.

## 6. Performance (stated honestly)

- Scene cost ≈ sum of object draws + one geometry pass per shadow-casting light
  (capped at `K = MAX_SHADOW_CASTING_LIGHTS = 4`, D4) + fog resolve (pointwise, cheap).
  Worst case = `objects × K` shadow-depth draws + `objects` main draws = 64 × 4 + 64 = 320
  geometry passes — **the shadow-caster cap `K`, not the object/light sliders, is what
  keeps the worst case bounded** now that objects=64 and lights are unbounded; the HUD makes
  the cost visible. Baseline content render is 4.5–5.5ms; the 4K-margin campaign owns the
  budget.
- Shadow maps are pooled textures keyed (node, light_slot, resolution) — no per-frame
  allocation. Pipeline cache per MaterialKind × shadow on/off.
- Viewport: editor-context only (D9). Pick pass on click, not per frame.

## 7. Decided — do not reopen

1. Scene = flat object list in ONE `render_scene` node, shared depth; graph remains
   the assembler. No scene-document format, no second authoring model.
2. Import-shaped from day one (D1); glTF import itself is a later phase + own design.
3. **[AMENDED 2026-07-10 (F2)]** Object count = `OBJECT_SLIDER_MAX` (shipped 64); light
   count is owned by UNBOUNDED_LIGHTS, not this doc. Shadows are capped separately: the
   first `MAX_SHADOW_CASTING_LIGHTS = 4` `cast_shadows` lights (slot order) get shadow
   maps; the rest still illuminate. That caster cap `K` — not the object/light sliders —
   is the bounded-cost guarantee. All three are constants, not architecture. (The original
   "8 objects, 4 lights; any light may cast" is dead — see D4.)
4. Fog/atmosphere is v1, as a port type (D5). Huge scenes are v2.
5. Gizmos are v1 and are `EditingService` param edits; wired params lock (D8).
6. Viewport = promoted graph-editor preview; show path never pays (D7/D9).
7. Industry-standard viewport bindings, trackpad-first; refinement later (Peter).
8. `render_mesh`/`render_copies` stay; `render_scene` is additive.
9. MATERIAL_SYSTEM contract unchanged; its §7 extension points (multi-light, shadow
   consumption) are exercised, not redesigned.

## 8. Deferred (with triggers)

- **glTF/.glb import** — **designed: `docs/IMPORT_DESIGN.md`** (scenes,
  Principled→pbr_material, rigid animation with the beat-retimed playhead,
  MDD/PC2 vertex caches, texture-set drops, TD + Resolume migration funnels);
  skeletal stays deferred there.
- **Hierarchy/parenting** — when import lands or when totemed set pieces demand it;
  composes transforms, no type changes (D1).
- **Instanced objects in scenes** (`instances_n` optional input per object group) —
  the v2 huge-scenes lever, with instancing tricks + fog doing the "massive" look.
- **Volumetric light shafts** — post-v1 atmosphere upgrade (real volumetrics, not
  depth fog).
- **Spot lights / cubemap point shadows** — LightMode extension, v2.
- **Hierarchy panel / in-viewport mesh editing** — Blender territory; MANIFOLD is the
  stage, not the modeler. Revisit only with strong user pull.

## 9. Addendum 2026-07-04 — `node.spawn_from_mesh` (mesh-explode vocabulary)

Added for the glTF wave: seed particles from a mesh's geometry so an imported model
can dissolve/explode into the existing 3D particle stack
(`node.spawn_from_image` → this, but sourced from `Array(MeshVertex)` instead of a
texture). **No dependency on `render_scene` or on import** — `Array(MeshVertex)` and
`Array(Particle)` both exist today; this atom can land in any session, including
before P1.

- **Precedent (shape this like):** `seed_particles_from_texture.rs`
  (`node.spawn_from_image`, `seed_particles_from_texture.rs:55`) — same
  `max_capacity` param, same optional `active_count` / `frame_seed`, same
  recompute-on-integer-edge gate input, same `particles: Array(Particle)` output
  (the shared type the 3D steppers consume — `euler_step_particles_3d.rs:37`).
- **Inputs:** `vertices: Array(MeshVertex) required` + the precedent's optional
  scalars. **Params:** `max_capacity` (same range as precedent); `mode` enum —
  `vertices` (one particle per vertex, exact silhouette) | `surface`
  (area-weighted random triangle sampling, uniform density regardless of
  triangulation). **Output:** `particles: Array(Particle)`, positions in the mesh's
  local space (transform upstream of the renderer applies, same as the mesh itself).
- **The stage composition it exists for:** `mesh → spawn_from_mesh →
  apply_radial_burst_3d_to_particles → euler_step_particles_3d → render` alongside
  the intact mesh — crossfade mesh-out/particles-in on the drop.
- **§2.5 audit at impl** (expect: genuinely new; `scatter_particles_3d` scatters in
  a volume, `spawn_from_image` samples a texture — neither reads geometry).
- **Gate (positive):** unit/gpu test — a known single triangle in `surface` mode
  yields particles whose positions all satisfy the triangle's plane equation +
  barycentric bounds (value-level); `vertices` mode on a cube yields exactly 8
  distinct positions (dedup-free count ≤ capacity). **Gate (negative):**
  `rg 'Arc<Mutex' ` on the new file → zero. **Test scope:** focused
  (`-p manifold-renderer --lib`).
- **Forbidden:** CPU-side per-frame reseeding (respect the recompute gate — seeding
  is per-trigger, not per-frame) · inventing a new particle struct.
