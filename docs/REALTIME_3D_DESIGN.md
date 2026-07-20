# Realtime 3D — Scenes, Lighting, Viewport

**Status: IN PROGRESS (D3/D8 AMENDED 2026-07-06 by `SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` — read its §8 before P6; D3/D4/§3/§6/§7.3 AMENDED 2026-07-10 (F2 coherence audit) — shadow-caster cap `MAX_SHADOW_CASTING_LIGHTS = 4` replaces the dead "8 objects, 4 lights" budget, read D4 before P2).** Shipped: P0 (MATERIAL M1–M6, all verified in-tree), P1 `node.render_scene` @ `8daa89fc`, P4 camera atoms (both `node.free_camera` + `node.look_at_camera` in-tree), §9 `node.spawn_from_mesh`, **P2 shadow maps + P3 atmosphere/fog @ `feat/realtime3d-p2p3` 2026-07-11** (gpu-proofs `render_scene_shadows` + `render_scene_fog`, PNG-verified; lights also moved to a ring-buffered storage buffer), **P8 scene instancing @ `feat/realtime3d-p8-instancing` 2026-07-11** (§10 D11 — each object group grows an optional `instances_n: Array(InstanceTransform)` port; wired draws `instance_count = buffer_size / 32` copies with `model_n · T_instance` in both the main pass and every caster's shadow pass; unwired binds a cached 1-entry identity stub, byte-identical to pre-P8 output; gpu-proofs `render_scene_instances` 4/4 green — identity parity, occlusion, instanced-shadow, instanced-fog; `Garden.json` re-wired single-pass, the `node.mix` Max-blend composite deleted), **P9 PCSS contact-hardening penumbra @ `feat/pcss-penumbra` 2026-07-12** (§11 D12 — `ShadowSoftness::Contact { light_size }`, 16-tap golden-angle blocker search + standard-PCSS penumbra estimate feeding the existing PCF loop with a dynamic half-width; `light_size`'s world-units→UV-space conversion derived per-fragment from the caster's own `vp` matrix, not a new caster-table field — zero layout growth as D12 required; gpu-proofs `render_scene_pcss` 3/3 green — contact-hardening gradient-width ratio, `light_size=0` byte-matches Hard tier, existing tiers unperturbed; `render_scene_shadows` proof unmodified and green). **The P1 "transforms not port-shadowed" deviation is retired by amendment, not by shadows: per-object transforms move to `node.transform_3d` atoms feeding `transform_n: Transform` ports** (SCENE_BUILD P2). **P5 viewport (navigate) COMPLETE 2026-07-17 on `lane/realtime3d-viewport`, pending landing — P5a core + P5b persistent session + P5c live panel wiring, see §5** — `node_graph::viewport_camera::ViewportCamera` (pure orbit/pan/dolly + trackpad-gesture nav, mirrors `node.free_camera`'s yaw/pitch convention exactly), `node_graph::viewport_render::override_camera_def` + `render_viewport_frame` (clones the generator's `EffectGraphDef`, splices a synthetic `node.free_camera` into the target `render_scene` node's `camera` port, renders through a BRAND NEW throwaway `PresetRuntime::from_def_with_device` — never the compositor's live runtime, which is what makes D9 mechanical rather than assumed), `node_graph::viewport_overlay` (grid/camera-frustum/light-billboard world lines, projected via the existing `Camera::project_to_pixel` oracle and composited as a plain CPU line draw directly onto the readback pixels — deliberately no new GPU pipeline, since overlay chrome never needs to enter a GPU pass). Gate: gpu-proofs `scene_viewport_navigate::viewport_render_is_isolated_and_produces_overlay_png` — headless PNG with grid+frustum+light overlays from a different camera angle than the wired show camera, AND the D9 proof (render show → open viewport → render show again on the SAME runtime, byte-exact readback comparison, zero diff). **P5b (persistent session + input mapping) BUILT 2026-07-17 on `lane/realtime3d-viewport`, pending landing** — `node_graph::viewport_session::ViewportSession` keeps one `PresetRuntime` alive across a navigation session (built/rebuilt only on open or on a real def change via `sync_def`'s content-hash compare, never per camera move); a camera move is a `Graph::set_param` on the spliced `node.free_camera` instance (`Graph::instance_by_node_id`, resolved once) — compare-on-write, `param_epoch`-bumping, the SAME live-param path the show already uses every frame, so orbit/pan/dolly cost one param write + one render, never a graph rebuild. `render_if_dirty` debounces to "camera or def actually changed" and caches the composited (render + D7 overlay) RGBA8 between calls. `manifold-app::viewport_input` is the winit-facing classification layer (LMB-drag→orbit, shift/MMB-drag→pan, scroll/pinch→dolly, `ViewportInputSensitivity`). Gate: gpu-proofs `scene_viewport_session::{viewport_session_navigates_and_debounces, viewport_session_rebuilds_on_def_change}` — camera moves change pixels and clear dirty; a no-op call is a byte-identical cache hit; `sync_def` on an unchanged def is a no-op that doesn't reset the camera; a real def change rebuilds and re-renders.

**P5c (live panel wiring) BUILT 2026-07-17 on `lane/realtime3d-viewport`, pending landing** — closes the scope note above. `Workspace` (the graph-editor window's own struct, `crates/manifold-app/src/workspace.rs`) grows `viewport_session: Option<ViewportSession>` + `viewport_pane: Option<TexturePane>` + `viewport_open: bool` + `viewport_rect: Option<Rect>` + `viewport_drag: Option<(MouseButton, f32, f32)>`. The `v` editor shortcut (`window_input.rs::editor_keyboard_input`) toggles `viewport_open`, always tearing the session down immediately on close. `Application::present_graph_editor_window` (`app_render.rs`) gates the actual session on `viewport_open` AND the previewed node resolving to a top-level `node.render_scene` node (`find_snapshot_node` against `self.content_state.active_graph_snapshot`); the def source is `self.watched_def_cloned()` — the UI thread's OWN mirrored `Project` (`self.local_project`), never a content-thread read, resolved BEFORE the editor workspace is borrowed mutably (`watched_def_cloned` needs `&self` whole). The session opens/rebuilds/`sync_def`s into the SAME reserved rect the 2D node-output monitor pane already occupies (no new dock UI), renders via `render_if_dirty` (self-gating — a no-op cache hit unless a navigation input or a def change actually marked it dirty, never per display tick), and uploads into a UI-device-local `GpuTexture` wrapped in `TexturePane::local`, blitted through `blit_texture_pane` exactly like the audio spectrogram (`texture_pane.rs`) — no IOSurface bridge, since render and present both run on the editor UI thread. `window_input.rs`'s `editor_mouse_input`/`editor_cursor_moved`/`editor_mouse_wheel` route presses/drags/scroll over `viewport_rect` through `viewport_input::classify_mouse_drag`/`classify_scroll`/`classify_trackpad_pan`/`classify_trackpad_pinch_dolly` + `apply` (a Ctrl-held trackpad `PixelDelta` scroll is the pinch-dolly — the OS's own pinch→Ctrl-scroll translation absent a native magnify-gesture handler; a bare `PixelDelta` is trackpad pan) instead of canvas pan/zoom, at the same precedence tier as the existing dock-drag/mini-timeline blocks. Known P5 constraint carried forward, not new to P5c: `override_camera_def` only splices into a node found in the def's FLAT top-level `nodes` list, so a `render_scene` node nested inside a group fails to open (`RenderSceneNodeNotFound`) — the session simply doesn't open, no silent fallback. L3 (scripted UI-flow) evidence wasn't reachable: `ui_snapshot::script`'s flow driver only knows the main window's `UIRoot`, not the graph-editor window/`Application::window_event` routing at all — named as the gap for a future click-script. Best-available L2 evidence instead: `manifold-app/src/viewport_p5c_demo.rs` (`#![cfg(test)]`, `cargo test -p manifold-app viewport_p5c_demo`) drives the production `classify_mouse_drag`/`apply` call against a real `ViewportSession` and asserts the rendered pixels change after an orbit drag — `/tmp/viewport_p5c_before.png` / `/tmp/viewport_p5c_after.png`. **P7 (scene starter preset) ABSORBED 2026-07-16 by `SCENE_SETUP_PANEL_DESIGN.md` P1 (`Scene Starter.json` + the panel's New 3D Scene gesture); §8's "hierarchy panel — revisit only with strong user pull" fired — the pull came from Peter, that design is the answer.**

**P6 (Viewport Tier 2 — gizmos) BUILT 2026-07-17 on `lane/realtime3d-viewport`, pending landing.** `node_graph::viewport_gizmo` (pure CPU, no GPU): `pick_object` (object-center picking against `scene_vm::SceneVm`'s already-traced object list, projected through `Camera::project_to_pixel` — a documented deviation from D7's literal "ID-buffer pick pass" wording, same class of pragmatic pivot P5 took with its throwaway runtime; see the module's doc comment for the full reasoning and the P7+ upgrade trigger), `gizmo_target_for` (resolves the selected object's `node.transform_3d` atom via `SceneVm`'s existing wire trace — `None` transform = P6 entry state), `gizmo_lines` (move/rotate/scale handle geometry, per-axis colored, locked-gray for a `_driven` axis per D8), `pick_axis` (screen-space hit test, ring-aware for Rotate), and `move_drag_delta`/`scale_drag_delta`/`rotate_drag_delta` (the drag-to-world-units math, unit-tested). `manifold-editing::commands::graph::AddObjectTransformCommand` is the P6 entry-state auto-create command (spawns `node.transform_3d`, wires its `transform` output into the unwired `node.scene_object`'s `transform` input, one undo unit — `created_node_id()` lets the caller chain a same-gesture `SetGraphNodeParamCommand` without a round trip). `manifold-app::window_input.rs` wires it live: `editor_viewport_gizmo_press` (object pick / gizmo axis grab, tried before the P5c orbit-arm on a Left press inside the viewport — a hit consumes the press so orbit doesn't also fire) and `editor_viewport_gizmo_drag_move` (per-move `SetGraphNodeParamCommand` dispatch through the SAME local-execute + `ContentCommand::Execute` path `ui_bridge::project::dispatch_project`'s `SceneSetupParamChanged` arm uses — the precedent the phase brief named); W/E/R cycle `Workspace::viewport_gizmo_mode` (Move/Rotate/Scale) while the viewport is open. A pick also writes `ScenePanel::set_selection` (new method) so the Scene Setup panel's Properties follows a viewport click — one selection store, not two. `ViewportSession::render_if_dirty` gained a `gizmo_lines: &[WorldLine]` parameter and now returns an owned `Vec<u8>` composited fresh every call (grid/frustum/gizmo overlays are cheap CPU line draws over a clean-scene cache that's itself still debounced to real dirty — fixes a latent staleness gap where P5's overlay could only refresh alongside a camera move; gizmo state, e.g. hover, needs to refresh independent of the camera). Gates: undo/redo round-trip proven at the command level (`manifold-editing`'s `add_object_transform_then_gizmo_param_drag_round_trips_undo_redo` — create, drag, undo the drag, redo the drag, undo both, byte-exact restore of the original graph); wired-axis refusal proven both functionally (`drag_write` reports `driven`, `editor_viewport_gizmo_drag_move`/`_press` no-op on it) and visually (locked axis renders `LOCKED_COLOR` gray, not its normal per-axis color); PNG evidence — `manifold-app/src/viewport_p6_demo.rs` (`#![cfg(test)]`, `cargo test -p manifold-app viewport_p6_demo`) drives the real renderer functions against a `node.scene_object`-shaped scene and dumps `/tmp/viewport_p6_pick_highlight.png` (pick + Move gizmo on the picked cube), `/tmp/viewport_p6_gizmo_{move,rotate,scale,locked}.png` (each mode's distinct handle geometry, the locked fixture's gray X axis), `/tmp/viewport_p6_move_{before,after}.png` (a `pos_x` write visibly translates the rendered cube). Same L2-not-L3 gap as P5c (the flow driver has no graph-editor-window routing) — not re-litigated here. **Scope cut this session:** object-center picking (not per-pixel ID-buffer) per above; light/camera gizmos deferred (D8 names them as in-scope but the phase brief and orchestrator both scoped this session to object transforms — the same `gizmo_target_for`/`pick_axis`/drag-delta machinery generalizes to a light's pos/aim or a camera's pos whenever that's picked up, no redesign needed); no drag-in-progress visual feedback beyond the write itself (e.g. no numeric HUD readout during drag) — v1 per D8/§7's "gizmos are v1."

· designed 2026-07-03 · Fable
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
- **P2 — Shadow maps.** ✅ **SHIPPED 2026-07-11** (`feat/realtime3d-p2p3`). Depth-only
  pre-pass per caster (new `manifold-gpu` colourless-PSO `draw_instanced_depth_only_batch`)
  into private `Depth32Float` maps (`RENDER_TARGET | SHADER_READ`, AGX 0x78 guard), PCF via
  `textureSampleCompareLevel`. Caster matrices ride a fixed K-slot `@binding(9)` table; each
  light's caster slot rides the spare `.w` of its colour vec4 (split layout — shadows cost no
  light-budget). Lights also moved to a ring-buffered (`FRAMES_IN_FLIGHT=3`) `@binding(8)`
  storage buffer (killed the 4KB `setBytes` cap). Proof: `gpu_proofs::render_scene_shadows`
  (occluder darkens ground 3.5%, PNG-confirmed soft shadow; >K casters render finite+lit).
  Original plan:
  Depth pass per casting light (first `K = MAX_SHADOW_CASTING_LIGHTS`
  casters, D4) + PCF consumption per the Light struct. Read-back: `light.rs` whole (the
  math is already documented there), `feedback_effect_chain_state_caches`, **D4's caster-cap
  policy (do not price this against object/light slider counts — the cap is `K`).** Gate:
  gpu_test — known box-over-plane scene, assert shadowed vs lit pixel values; softness
  levels change tap counts (verify via perf counter, not eyeball); `cast_shadows=false`
  skips the pass (frame capture shows no depth pass); **a scene with more than `K`
  `cast_shadows` lights produces exactly `K` shadow maps and the extra casters still light
  the scene (assert both: shadow-map count == K, and pixels lit by caster K+1).**
- **P3 — Atmosphere.** ✅ **SHIPPED 2026-07-11** (`feat/realtime3d-p2p3`). New
  `PortType::Atmosphere` (CPU-struct wire, mirrors `Transform` across the plumbing) +
  `node.atmosphere` + per-fragment exp depth fog in all four `render_scene` material entry
  points. Fog lerps STRAIGHT (non-premultiplied) rgb toward `fog_color`, alpha untouched —
  composits OVER the alpha contract. Uniform 272→320B. Proof:
  `gpu_proofs::render_scene_fog` — density-0 is byte-identical to no-atmosphere (pixel-exact);
  blue fog turns a white-lit receding plane blue-dominant with a real near→far depth gradient
  (PNG-confirmed). Original plan: Port type + `node.atmosphere` + fog in `render_scene` resolve.
  Pattern-copy MATERIAL M1 plumbing. Gate: gpu_test — fog density curve at known
  depths; density 0 = byte-identical to no-atmosphere.
- **P4 — Camera atoms.** `node.free_camera`, `node.look_at_camera`, full descriptors,
  §2.5 audit first (expect: pure emitters, no new math beyond `look_at_rh`). Gate:
  unit tests on emitted Camera basis; a beat-ramp-driven dolly example preset.
- **P5 — Viewport Tier 1 (navigate).** ✅ **COMPLETE 2026-07-17** on `lane/realtime3d-viewport`
  (P5a core + P5b persistent session + P5c live panel wiring, pending landing). Editor camera override in the preview context,
  standard orbit/pan/dolly + trackpad, light/camera/grid overlays. Read-back: node
  preview infra, D9. Gate: headless PNG of viewport with overlays; content-thread
  output byte-identical with viewport open (the D9 proof — diff the show render).
  **As-built:** the read-back audit found the existing node preview infra
  (`execution.rs`'s `preview_target`/`set_preview_target`) captures an extra texture
  from the SAME per-frame execution the live show uses — splicing a camera override
  into that shared execution would corrupt the live `camera` port value, directly
  violating D9. So P5 does NOT ride that infra: it builds a brand-new, throwaway
  `PresetRuntime` (via the existing production constructor
  `PresetRuntime::from_def_with_device`) from a CLONED `EffectGraphDef` with the
  target `render_scene` node's `camera` input re-wired to a synthetic
  `node.free_camera` node — see `node_graph::viewport_render`. Two structurally
  separate runtimes on the same `GpuDevice` cannot share execution state, which is
  what makes the D9 guarantee mechanical (proven, not assumed) in the gpu-proofs
  test. Overlays (grid/camera-frustum/light-billboard) are drawn on the CPU straight
  onto the tonemapped readback pixels via `Camera::project_to_pixel` — no new GPU
  pipeline, since overlay chrome is editor-only 2D chrome, never scene geometry.
  Gate: `cargo test -p manifold-renderer --features gpu-proofs scene_viewport_navigate`
  — headless PNG (`/tmp/viewport_navigate_p5.png`) + byte-exact D9 diff. **Deferred
  this session (named trigger: live UI wiring is genuinely separate infra —
  winit mouse/trackpad events → `ViewportCamera` → re-render, docked into the graph
  editor):** the navigation math (`ViewportCamera::orbit/pan/dolly/trackpad_*`), the
  isolated render path, and the overlay system are complete, unit-tested, and
  gate-proven; wiring them to live input events is the remaining follow-up before
  the viewport is interactively usable in the running app.
  **P5b/P5c (2026-07-17, same branch):** the persistent-session architecture
  (`ViewportSession`, amortizing the rebuild to open/def-change only) and the
  live panel wiring (dock rect in the graph-editor sidebar, `v` toggle,
  `TexturePane`/`blit_texture_pane` present path, real winit mouse/scroll
  routing through `viewport_input`) are both BUILT — see the P5b/P5c
  paragraphs in the doc header and §5 intro above for the as-built detail.
  The viewport is now interactively usable end to end, pending landing.
- **P6 — Viewport Tier 2 (gizmos).** ✅ **BUILT 2026-07-17** on `lane/realtime3d-viewport`,
  pending landing — see the P6 paragraph in the doc header for the as-built detail
  (object-center picking rather than a literal ID-buffer pass, `viewport_gizmo`,
  `AddObjectTransformCommand`, `window_input.rs` wiring, gates). ID-buffer picking,
  move/rotate/scale gizmos → `EditingService` param commands, wired-param lock state.
  **Entry state (amended 2026-07-06): SCENE_BUILD_AND_GROUP_PARAMS P2 landed — gizmos
  write `node.transform_3d` params per the amended D8; brief the unwired-`transform_n`
  auto-create command here.** Gate: gizmo drag round-trips undo/redo; dragging a
  wire-driven axis is refused visibly; headless PNG of gizmo states.
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
- ~~**Instanced objects in scenes**~~ — **PROMOTED 2026-07-11 → §10 (D11, P8).** The
  trigger fired: MESH_DEFORM P4's Garden demo needs scattered instances and scene
  geometry to occlude each other (its Deferred #6), and Peter approved the design.
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

## 10. Addendum 2026-07-11 — D11 + P8: per-object instancing in `render_scene`

Promotes the §8 "Instanced objects in scenes" deferral: MESH_DEFORM P4's Garden demo
(its Deferred #6) fired the trigger. Garden today draws terrain (`render_scene`) and
scattered flowers (`render_copies`) as two passes composited by a `node.mix` Max
blend — no shared depth, so a flower fully behind a hill crest still draws. It reads
correct only because the flowers sit on the near face and are brighter. Correct
occlusion between scattered instances and scene geometry cannot be composited in
after the fact; it has to happen in the scene pass. Peter approved 2026-07-11.

**What it buys on stage:** scatter density (`scatter_on_mesh`'s port-shadowed
`count`) rides a fader while the camera dollies, and occlusion stays correct at
every value — instances vanish behind ridges, drop shadows on the terrain, sit in
the fog. This is also the "huge scenes" lever: 64 objects × hundreds of instances
each at a bounded draw-call count.

### Audit (verified 2026-07-11 @ `8bdc4a70`)

| Piece | Where | State |
|---|---|---|
| Main-pass draws are already instanced, count hardcoded 1 | `render_scene.rs:1089-1100` (`depth_msaa_draw(&pipeline, bindings, vertex_count, 1)`) | The `manifold-gpu` API needs zero changes |
| Shadow-pass draws likewise | `render_scene.rs:940-951` | Same — pass the real count |
| `InstanceTransform` | `generators/mesh_common.rs:93-98` — 32-byte `{pos_scale, rot_pad}`: pos.xyz + uniform scale in `.w`, Euler rot in `rot_pad.xyz`; std430 stride 32, stride-drift test exists | Reuse as-is; no new type |
| Instance vertex math precedent | `shaders/render_instanced_3d_mesh.wgsl:109-152` — `@builtin(instance_index)` → `instances[iid]`, `euler_xyz(rot_pad.xyz)`, rotates normals too | `render_scene.wgsl:55` already documents sharing this Euler convention |
| Producer | `scatter_on_mesh.rs:61-62` outputs `Array(InstanceTransform)`; `count` is port-shadowed (`:51`) | Density is a live control on the producer |
| Port rebuild | `render_scene.rs:253-342` — object-group ports are name-generated | Adding one optional port per group is mechanical; old projects load it unwired (no migration) |
| Always-bind ABI-stub pattern | `render_scene.rs:874` (`ensure_shadow_binding_stubs` — dummy depth + comparison sampler) | The identity-instance stub copies this |
| Garden | `crates/manifold-renderer/assets/generator-presets/Garden.json` + `manifold-app/tests/garden_preset_round_trip.rs` | The two-pass composite this kills; the acceptance demo |

### D11 — instancing is a port on the object group, in the scene pass

- **Each object group grows `instances_n: Array(InstanceTransform) optional`.**
  Wired → that object's draws (main pass AND every caster's shadow pass) run with
  `instance_count = buffer_size / 32` (0 → skip the object's draws); each instance's
  world transform is `model_n · T_instance` — **instance TRS applies first, the
  group's `transform_n` second**, copied verbatim from
  `render_instanced_3d_mesh.wgsl`'s TRS math. Instance transforms therefore live in
  the object's local space: wire the SAME `node.transform_3d` into the terrain
  object and the scattered object, and the instances stay glued to the terrain
  under any group move. Uniform scale only (`pos_scale.w`), same as `render_copies`.
- **One always-instanced pipeline per MaterialKind — no instanced/non-instanced
  variant matrix.** `render_scene.wgsl`'s single `vs_main` and `shadow_depth.wgsl`
  always declare the instance buffer + `@builtin(instance_index)`; an unwired object
  binds a cached 1-entry identity stub (`pos_scale [0,0,0,1]`, `rot_pad` zeros) and
  draws with count 1 — the same always-bind ABI-stub pattern this node already uses
  for shadow bindings. *Consequences, stated honestly:* every non-instanced vertex
  now reads 32 identity bytes from a storage buffer — the same read `render_copies`
  does, L1-resident; the identity-parity gate below proves output identity. If it
  ever measures, split vertex entry points are the escape hatch — do not start there.
- **No per-object `instance_count` param** (amended D3: render_scene carries no
  per-object params). Count = the wired array's capacity; density control belongs
  to the producer. Per-instance material/color variation is NOT in scope —
  instances share the group's `material_n`/`base_color_map_n` (Deferred, below).
- **Rejected: a depth-aware two-pass compositor** (depth outputs + depth-tested
  mix). The scene's depth buffer is memoryless MSAA tile memory
  (`render_scene.rs:64-68,195-198`) — exposing it costs a stored resolve on every
  scene render, needs a depth-port convention across all 3D renderers, and
  composited passes still can't exchange shadows or fog. If splats or particles
  ever need scene occlusion, that's a new design with its own trigger; it does not
  reopen this one.
- **Rejected: routing instanced objects through `render_instanced_3d_mesh`'s
  pipeline inside the scene.** This is the plausible-wrong turn, named: reusing the
  existing instanced shader puts a second lighting model in the same image —
  instances would ignore scene lights, shadows, and fog. Extend `render_scene.wgsl`
  so instances share the exact lit path.

### P8 — phase brief (one session)

- **Entry state:** P2+P3 in-tree (`gpu_proofs::render_scene_shadows` /
  `render_scene_fog` pass); re-verify every anchor in the §10 audit table (rg/read;
  a moved anchor is an escalation); Garden.json still composites two passes via a
  Max-blend `node.mix`.
- **Read-back:** this §10 whole; D3/D4; `render_scene.rs` rebuild + both draw
  loops; `render_instanced_3d_mesh.wgsl` TRS + normal math; standard §5–§6.
- **Deliverables:** `instances_n` port in `rebuild()` · identity-stub buffer ensure
  fn · instance fetch in `render_scene.wgsl` `vs_main` + `shadow_depth.wgsl` ·
  real instance counts in both draw loops · updated `description()` purpose +
  composition_notes · gpu test module `render_scene_instances` · Garden.json
  re-wired single-pass (flowers become an object group with `instances_n` wired;
  the `node.mix` composite deleted) with `garden_preset_round_trip` still green.
- **Gate (positive, gpu-proofs — deliberate `cargo test -p manifold-renderer
  --features gpu-proofs render_scene`):** occlusion — an instance placed fully
  behind an occluder object contributes no pixels (value-level, shape it like
  `render_scene_shadows`); **identity parity — a wired 1-entry identity instance
  buffer renders byte-identical to the same scene unwired** (this is the invariant
  check: unwired objects cost nothing observable); shadow — an instance between a
  sun caster and the ground darkens the ground (shadowed < lit); fog — a far
  instance shifts toward `fog_color`. Bundled 3D preset PNGs unchanged except
  Garden.
- **Gate (negative):** `rg 'Arc<Mutex' ` on touched files → zero; Garden.json →
  zero hits for the Max-blend composite node; existing `render_scene_*` proofs
  untouched and green.
- **Acceptance demo (L2):** headless PNG of Garden from a camera angle that puts
  flowers behind the ridge — flowers hidden by terrain, shadows and fog consistent;
  read by the landing session.
- **Performer gesture:** sweep scatter `count` on a fader during a camera dolly —
  occlusion correct at every density, nothing to babysit.
- **Test scope:** focused `-p manifold-renderer --lib` for rebuild/port tests; the
  gpu-proofs run above for the render path; full workspace sweep at landing per
  protocol.
- **Forbidden moves:** reusing `render_instanced_3d_mesh`'s shader/pipeline for
  scene instances · exposing a depth output to composite instead · a per-object
  `instance_count` param · a CPU loop issuing one draw per instance · adding
  non-uniform instance scale "while in there" (scope fence).

**Deferred (new, with triggers):** per-instance color/material variation — ride a
`Channels` color lane when the first look needs it. Depth-aware compositing for
non-mesh passes (splats/particles vs scene) — its own design when such a preset
appears.

## 11. Addendum 2026-07-12 — P9: PCSS contact-hardening penumbra (soft shadows v2)

**Status: SHIPPED @ `feat/pcss-penumbra` 2026-07-12 · Fable 5 design, Sonnet 5
build.** Graduates RENDERING_INFRA_V2 §8. P2's PCF shipped fixed-width softness
(`ShadowSoftness` kernel half-width, `light.rs:69` — this addendum's own audit
below misnamed the variants "Off/Soft/Softer/Softest"; the shipped enum is
`Hard`/`Soft`/`VerySoft`, doc-drift corrected at build time, no functional
consequence); real shadows harden at contact and soften with occluder
distance. Peter's acceptance frame, 2026-07-12, choosing self-shadowing as
the target for his black-background hero scenes: *"a statue's arm onto its
torso, leaves onto stems is exactly what I want."*

**As-built deviations from the plan below (both interior-mechanism choices,
not architecture changes — see landing report `docs/landings/2026-07-12-pcss-penumbra.md`
for the full reasoning):**
1. **`light_size`'s world→UV conversion is derived per-fragment from the
   caster's own `vp` matrix** (project `world_pos` offset by `light_size`
   along world X and Z, take the larger UV displacement), not read as a raw
   UV-space number. The first implementation read `light_size` directly as
   a UV offset, which is wrong by 2-3 orders of magnitude at any
   performer-sensible value (a `light_size` of 1.5 world units produced UV
   offsets of 1.5 — off the [0,1] texture entirely) and silently produced
   "always fully lit" (zero blockers found, every tap clamped to the
   texture edge) instead of a visible error. The VP-transform fix needs no
   new caster-table field (D12's "zero layout growth" holds) and makes
   `light_size` behave as the literal world-units diameter the outer-card
   fader implies.
2. **`Contact` with `light_size = 0` reuses `pcf_average(khw=1)` directly**
   (Hard tier's exact kernel) rather than running the blocker-search loop
   with a zero radius, so gate (b) holds at 0px difference, not just within
   the required 1px.

**Audit (2026-07-12, tip `9e537b16`):** PCF sampling `render_scene.wgsl:172-196`
(`sample_shadow` switch + (2·khw+1)² loop); caster table `@binding(9)`, 5
vec4/slot, **`[4].w` spare = 0** (`render_scene.rs:129-134,799-801`); `Light`
carries `shadow_softness/bias/resolution` (`light.rs:95-123`); comparison
sampler is `compare: Less` linear (`render_scene.rs:450`). PCSS blocker-search
needs a PLAIN depth sample — `textureSampleCompareLevel` returns a comparison,
not depth. ⚠ VERIFY-AT-IMPL: whether shadow maps can additionally bind for
plain `textureSampleLevel`/`textureLoad` under the current binding layout
(read `create_render_pipeline_depth_msaa` reflection handling in
`manifold-gpu`); expected answer yes (`SHADER_READ` usage is already set) —
if the sampler-type collision bites, bind the map twice (depth + non-compare
sampler slots 15/16), an ABI addition, not a redesign.

**D12 — PCSS as a fourth+ softness tier, not a replacement.** `ShadowSoftness`
gains `Contact { light_size: f32 }` (world-units light diameter; serialized
like every light param — old saves load unchanged, enum growth is additive).
Existing tiers keep their exact code path (negative gate: existing
`render_scene_shadows` proof green unmodified). Algorithm, committed
(standard PCSS): (1) blocker search — 16 golden-angle taps (CINEMATIC_POST D2
formula) in a `light_size`-scaled search radius via PLAIN depth samples, avg
blocker depth `z_b` over taps with `z_b < z_r − bias`; zero blockers → fully
lit, early out. (2) penumbra_px = `light_size · (z_r − z_b) / z_b`, mapped to
texels via the caster's `texel_size`, clamped [0, 24]. (3) the EXISTING PCF
loop with half-width = ceil(penumbra_px). `light_size` rides the spare
caster-table `[4].w` — zero layout growth.

**Gate (numeric, no image judgment):** gpu_proof `render_scene_pcss` — box
floating above a plane, sun caster: (a) shadow-edge gradient width (count of
readback pixels with 0.05 < shade < 0.95 along a committed scanline) at
contact (box touching plane) is ≥3× NARROWER than at height 2.0 — the
contact-hardening property as an integer comparison; (b) `Contact` with
`light_size = 0` matches `Off`-tier hard edge within 1 px of gradient width;
(c) existing softness tiers byte-identical to build-of-record. Focused test
scope + clippy; workspace sweep at landing.

**Performer gesture:** `light_size` port-shadowed on the light — a fader ride
turns noon (hard) into overcast (soft) on the hero mesh's self-shadowing.

**Forbidden moves:** replacing the PCF loop · a second shadow shader file ·
resolution-dependent constants hard-coded from the test scene · touching the
caster-cap policy (D4/F2) · "improving" bias handling while in there.

**Amendment 2026-07-12 (post-ship, perf):** step (3)'s dense-PCF reuse was the
shipped implementation's bottleneck — at the 24px clamp it is a 49×49 = 2,401
compare-tap kernel per fragment, and foliage scenes (occluders far above
their receivers) drive most fragments to that ceiling: 5FPS on a tree GLB
that runs 60FPS on Hard. Step (3) is now a fixed 16-tap golden-angle compare
disc (+1 center tap) over the penumbra radius, with per-pixel IGN rotation of
the disc (blocker search + filter, same rotation) to break shared-pattern
banding. penumbra_px ≤ 1 falls back to the shared 3×3 loop so near-contact
stays byte-identical to Hard; gate (b)'s light_size=0 early-out untouched.
Landed `e21008b2` + `e43dd2dd`; gates (a)/(c) not re-run at landing
(usage-constrained session) — Peter confirmed 60FPS restored and the
contact-hardening look on the reference GLBs. This supersedes "the EXISTING
PCF loop" in step (3) and narrows the "replacing the PCF loop" forbidden move
to the fixed tiers (Hard/Soft/VerySoft), which still run the shared loop
unmodified. The glTF importer card now exposes the tier as a Shadow Type
stepper bound EnumRound → `shadow_softness` (`gltf_import.rs`).
