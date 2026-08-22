# Scene FX — performable deformers, scene mods, and layer skins for 3D scenes

**Status:** IN PROGRESS — P0–P4a SHIPPED 2026-08-22 (eight glitch deformers, passthrough check, transform_shake + five presets, decode_cache hermeticity fix, layer-skin registry + node.layer_source with live-path wiring). P4b (panel Skin row + L3 flow) not built. Known gap: cameras have no Transform wire — Camera-wire shake tracked in BUG-j42e (camera-wire shake atom). · APPROVED 2026-08-21 · k3 (lead), design session with Peter
**Prerequisites:** none hard — every consumed substrate is shipped in-tree (audit below).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

Peter, 2026-08-21, the ask: mesh modifiers and scene mods on glb imports that are
performable, plus *"integrating 2D generator outputs from other layers into the
scene."* UX directive, verbatim: *"it should be something like drag and drop glb in
and then you have the option to play with these scene effects"* — and *"all of the
UI that's simple and easy for the user."* Taste directive, verbatim: *"interesting,
visually stunning, easy to play with, digital, glitchy, beautiful."* On the
layer-skin source picker: *"kinda like a sidechain."*

The governing insight: **most of this shipped in July 2026 and the session that
asked for it didn't know.** Bend/twist/taper/morph deformers, the scene panel's
per-object modifier stack (the rack Peter described), drag-drop glb import, and
texture-driven vertex displacement all exist on main. What is genuinely new: a
glitch-flavored deformer family, the parked camera-shake atom, scene-mod presets on
existing port-shadowed params, and one real new seam — a layer's rendered output
bound into another layer's scene graph as a texture ("Skin Video").

On stage: drop the apricot lamp glb onto a layer, open the scene panel, add Boil —
the lamp's skin breathes. Add Voxelize, ride cell size down on the drop — the model
pixel-crushes into voxels. Camera shake gives the kick physical weight. Skin Video
puts the plasma layer onto the model's emissive map, where it lights the scene.

**Binding constraints** (DESIGN_AUTHORING section 1): hot path — all deformation is
GPU dispatch, zero CPU per-vertex work, zero per-frame allocation; thread residency —
the layer-skin registry lives entirely inside `manifold-renderer` on the content
thread, no new cross-thread state; persistence — atoms serialize as ordinary graph
nodes, `node.layer_source` stores a layer id as a param (round-trip gate in P4);
performance surface — every numeric param port-shadowed, rack slots are manifest
params, MIDI for free.

Companion docs: [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md) (`primitive!` + scope
test mechanics), [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) (atom
doctrine), [SCENE_SETUP_PANEL_DESIGN.md](SCENE_SETUP_PANEL_DESIGN.md) +
[SCENE_OBJECT_AND_PANEL_V2_DESIGN.md](SCENE_OBJECT_AND_PANEL_V2_DESIGN.md) (the panel
and modifier stack this extends), archive:MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md
(the shipped deformer family), [REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md) (the
scene itself).

---

## 1. Audit — what exists (verified 2026-08-21)

Instruction to every phase: **extend, don't redesign.**

| Piece | Where | State |
|---|---|---|
| Deformer family: `bend_mesh`, `twist_mesh`, `taper_mesh`, `morph_mesh`, `mesh_ramp`, `push_along_normals`, `facet_normals` | `crates/manifold-renderer/src/node_graph/primitives/` | SHIPPED 2026-07-11 (archive:MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md), all on the freeze codegen path. **Twist and bend exist — do not rebuild them.** |
| Texture-driven displacement | `push_along_normals.rs:1-13` — optional `field: Texture2D` sampled bilinear at vertex UV, `(sample.r - field_bias)` | exists; "2D texture pushes geometry" needs no new atom, only a texture to wire |
| Material maps as wires | `scene_object.rs:43-59` — 17 optional `Texture2D` map ports incl. `emissive_map`, `base_color_map` | exists; a 2D texture wires into a map port today |
| The rack | `scene_vm.rs:66-75` `MODIFIER_TYPE_IDS` (curated 7-atom vocabulary), `modifier_chain` VM `:154`, "Add modifier" affordance gated by `modifier_chain_parseable` `:160-163` | SHIPPED (SCENE_SETUP_PANEL); **the rack exists — new atoms registered in `MODIFIER_TYPE_IDS` appear in the panel's add-modifier menu with zero new UI** |
| Modifier insertion commands | `crates/manifold-editing/src/commands/graph/modifiers.rs` | exists; the panel edits the graph through commands, undoable |
| glb drag-drop import | `crates/manifold-app/src/app.rs:2824` `DroppedFile` → `process_dropped_files` | exists; verified end-to-end on the azalea import (SCENE_BUILD wave) |
| glTF animation | `gltf_animation_source.rs:32-38` — `progress` (0..1) port-shadowed, never clamps; morph weights `gltf_morph_weights.rs`, skeleton `gltf_skeleton_pose.rs` | exists; **time scrub/reverse/stutter is a modulation preset, not a feature** |
| Transform port type | `ports.rs` (`PortType::Transform`); `transform_3d.rs` produces, `scene_object` consumes | exists; **no Transform→Transform atom exists** (`rg "Transform" primitives/` — producers/consumers only) — camera shake is the first |
| In-kernel 3D simplex | `instance_position_jitter.rs:55` — 3-axis simplex noise in WGSL | the precedent for noise inside a deformer kernel |
| Clip/layer output textures | `generator_renderer.rs:847` `get_clip_texture(clip_id)`; per-layer composited buffers in `layer_compositor.rs` (`layer_buf.source_texture()` `:1696-1710`) | exists; **nothing binds a sibling layer's texture into a graph — the one genuinely new seam** |
| Panel sections for lights/camera/atmosphere | `scene_vm.rs` `SceneVm` (lights, camera, environment, atmosphere rows) | exists; fog/light/strobe scene mods are params on shipped rows |
| Scope test | docs/ADDING_PRIMITIVES.md:59 — codegen-path scope test, every barrier-free atom ships fusable | the enforcement host P1 extends |

---

## 2. Decisions

**D1 — The rack is the shipped scene-panel modifier stack.** New deformers register
in `MODIFIER_TYPE_IDS` and appear in the panel; no new UI system is built. Peter's
drag-drop flow is: drop glb → scene panel → Add modifier. Rejected: a separate
"Scene FX rack" panel — it would duplicate the exposure/command machinery the panel
already converged on (the BUG-237 (scene-setup-camera-param-scrub-dead) cluster's
lesson: one addressing system).

**D2 — Every new deformer is a stateless `primitive!` atom on the freeze codegen
path.** `wgsl_body` + `fusion_kind`/`input_access`, pipeline from
`standalone_for_spec::<Self>()`, value-level `gpu_tests` proof against CPU-computed
expected. No exceptions, no fused monolith "scene fx" node (the no-monolith rule).
Rejected: CPU-side deformation — hot-path violation, and the July family proved GPU
atoms cover the space.

**D3 — "Off is free" becomes a machine check.** The scope test gains a passthrough
requirement: at default params (amount 0 / identity), the atom's GPU output is
byte-identical to its input. A rack can then ship ten stacked modifiers and only
perform-active ones cost a dispatch. Enforcement lives in the test, not in habit.

**D4 — Camera shake is a Transform-wire atom, `node.transform_shake`.** Peter's
parked direction (2026-07-10, verbatim from the design memory): *"a shake/jitter atom
on the Transform wire, not a camera-node param"* — it composes onto meshes, lights,
groups, and cameras alike. Stateless smooth per-axis noise of `time × freq`,
**rotational noise dominant with positional at a fixed 0.25 ratio, amount² response
curve** (the envelope already decays; the node stays stateless). Params: `amount`,
`frequency`, plus a port-shadowed `time`. First Transform→Transform atom in the
catalog — the port type exists, so this is an atom, not plumbing.

**D5 — Layer skins read the *previous* frame.** `node.layer_source` emits a sibling
layer's composited output texture from the previous frame. No render-ordering
constraint, no feedback-loop hazard (a loop is a well-defined one-frame feedback —
documented, allowed), one frame of latency (invisible for skin content). Rejected:
same-frame binding with ordering constraints — it couples layer render order to
graph correctness and makes loops a crash class instead of a look.

**D6 — Skin source is the layer, not the clip.** The dropdown lists layers by tree
name ("Layer 3 — Plasma"); the bound texture is that layer's composited output
(post layer-effects — what the performer sees on that layer). Rejected: per-clip
binding (`get_clip_texture`) — clips fire and end under the performer; the layer is
the stable performable identity.

**D7 — The source picker is a manifest param, not bespoke UI.** `node.layer_source`
carries a `layer` param (layer id, string) + the panel adds one "Skin" row per
scene object: source dropdown + target-map dropdown (Emissive default, Base Color).
The row splices `layer_source → <map>` through an editing command (precedent:
`commands/graph/modifiers.rs`). WIDGET_TREE_DESIGN section 5b: no bespoke row
infrastructure.

**D8 — Deleted/missing source layer degrades loud, never silent.** Unresolvable
layer id → transparent-black fallback texture + the panel Skin row shows a
"missing layer" chip; the stored id is kept inert-but-present (load-path rule).
Rejected: clearing the param — silent data loss on load is the forbidden move.

**D9 — Registry lives inside `manifold-renderer`, content thread only.** The
compositor publishes previous-frame layer textures into a renderer-owned map at end
of frame; graph execution reads it next frame. No new `Arc<Mutex>`, no crate
crossing, no new channel. This is the design's one new seam and its committed shape
is in section 3.3.

---

## 3. Design body

### 3.1 The glitch deformer family (P1/P2)

All atoms: `Array(MeshVertex) in → Array(MeshVertex) out`, optional `weights:
Array(f32)` (degrade-to-1.0 per the `push_along_normals` convention), all scalars
port-shadowed, display labels per the table. Each is one GPU dispatch; each fuses.

| Node | Label | Math (per vertex) | Params |
|---|---|---|---|
| `node.voxelize_mesh` | Voxelize | `mix(pos, round(pos/cell)*cell, amount)` | `amount` (default 0), `cell_size` |
| `node.noise_displace` | Boil | `pos += normal * amount * simplex3(pos*freq + t*speed)` — simplex precedent `instance_position_jitter.rs` | `amount` (0), `frequency`, `speed`, port-shadowed `time` |
| `node.glitch_jitter` | Glitch Jitter | stepped hash: `step=floor(t*rate)`; `pos += (hash3(vert_id ⊕ step·K)−½) * amount` — hash precedent `instance_rotation_jitter.rs` | `amount` (0), `rate`, `seed`, port-shadowed `time` |
| `node.shatter_mesh` | Shatter | per-triangle (flat-list convention): face normal `n`, `pos += n * amount * hash(tri_id)`; output normals = face normals | `amount` (0), `seed` |
| `node.slice_mesh` | Slice | verts past the cut plane along `axis` clamp onto the plane — geometry wipes to a flat cut face | `axis` enum, `cut` (port-shadowed position) |
| `node.ripple_mesh` | Ripple | `pos += normal * amp * sin(dot(pos,dir)·freq − t·speed)` | `amplitude` (0), `frequency`, `speed`, `axis` enum, port-shadowed `time` |
| `node.fold_mesh` | Fold | mirror across plane through origin: `mix(pos, reflect(pos), amount)`; kaleido = chain multiple Folds | `amount` (0), `axis` enum |
| `node.melt_mesh` | Melt | `pos.y −= amount * (simplex(pos.xz·freq + seed)·½+½)` | `amount` (0), `frequency`, `seed` |

Normals policy follows the shipped family: pass-through except where wrong enough to
matter (Shatter sets face normals; heavy Boil users wire `facet_normals`
downstream — stated in each atom's `composition_notes`).

**Consequences, stated honestly:** deforming a heavy photoscan re-runs everything
downstream that keys on geometry (shadow passes, scatter) every frame — linear
per-draw cost, same as morph targets today. Slice's clamp-to-plane gives a flat cut
face, not a hollow solid — the right look for a wipe, wrong if Peter expects
CSG-style capping (he has not asked for capping).

### 3.2 `node.transform_shake` (P3)

`Transform in → Transform out`. Rotational offset `rot.xyz += (smooth_noise1(t·f +
axis_phase)) · amount²`, positional `pos.xyz += same · amount² · 0.25`. Smooth noise
= summed sines with irrational frequency ratios (the stateless standard — no stored
phase, no RNG state). Composes onto any Transform wire: camera (the kick weight),
lights (orbital shimmer), objects.

**Implementation shape (amended 2026-08-22, P3 escalation):** Transform is a CPU
struct wire — the freeze codegen has no element domain to dispatch over, so the
atom is a **CPU-only NonGpu primitive, precedent `camera_lens`/`transform_3d`** (no
`wgsl_body`, no fusion). D2's codegen rule covers per-element GPU atoms; a
single-struct CPU op (a few sin() calls per frame) is not a hot-path concern. The
D3 passthrough invariant still binds and is enforced as a **CPU unit test**:
amount=0 → output struct == input struct, bit-exact. Panel: no new row — it appears
in the modifier menu wherever a Transform chain is walkable; the panel's modifier
walk is mesh-only today, so P3 adds the Transform-chain walk to `scene_vm.rs` (one
seam, named here so nobody invents a second).

### 3.3 Layer skins (P4) — the one new seam

**`node.layer_source`** — texture producer. Params: `layer` (layer id string,
picker-rendered), `target_map` is NOT on the node — the wire target decides (D7's
row picks which map port the splice connects).

**Registry (committed shape):**

```rust
// crates/manifold-renderer/src/layer_skin.rs (new)
pub struct LayerSkinRegistry {
    /// Previous-frame composited output per layer, written by the compositor
    /// at end of frame, read by graph execution next frame.
    textures: HashMap<LayerId, GpuTexture>,   // content thread only
    fallback: GpuTexture,                     // 1×1 transparent black
}
```

Owned by the renderer alongside the compositor; the graph-execution context (the
executor reads `execution.rs` and picks the existing context carrier — that's
interior) exposes `layer_skin(&LayerId) -> &GpuTexture`. Missing id → `fallback` +
the row chip (D8). **Feedback rule:** writes happen only after all layer renders
complete; reads during graph execution always see the prior frame. Layer→layer
loops are one-frame-delay feedback — allowed, documented in the node's
`composition_notes`.

**Consequences, stated honestly:** one frame of latency on skin content; a
self-referencing layer is legal and produces feedback smear (a look, not a bug);
the registry holds references to textures that already exist — no new allocation.
This is the first feature that couples layers together; the coupling is
unidirectional and delayed, which is what keeps it cheap.

### 3.4 The user-facing surface (the "simple and easy" contract)

The whole UX in five sentences, each one gate-able:

1. **Drag a glb onto a layer** — it imports and renders (shipped). The scene panel
   opens on that layer's scene.
2. **Add a modifier** — the object's modifier stack shows an "Add modifier" menu
   listing the deformer vocabulary by friendly label (Voxelize, Boil, Shatter…).
   Adding one drops it in with safe defaults (all amounts 0 — nothing explodes on
   insert) and its knobs render immediately as manifest rows.
3. **Play** — every knob is MIDI/LFO-bindable through the standard param surface.
   No graph wiring is ever required for the default flow.
4. **Skin a model** — the object's "Skin" row: a source dropdown (layer names as
   they appear in the tree, "None" = the model's own texture) and a target
   dropdown (Emissive / Base Color). Two clicks, no wires.
5. **When something's missing** — a deleted source layer shows a "missing layer"
   chip on the Skin row; an unparsable custom chain shows "custom chain — edit in
   graph" (shipped behavior). Loud, never silent.

Affordance rule for every phase touching the panel (DESIGN_DOC_STANDARD section 5):
dropdowns and the add-modifier button must read as clickable in the demo artifact —
chrome, not bare text.

### 3.5 Scene-mod presets (P3)

Bundled JSON presets on shipped port-shadowed params, zero new code paths: **Fog
Blast** (atmosphere density envelope), **Light Orbit** (point-light position on LFO),
**Strobe** (light intensity gated), **Time Scrub** (`gltf_animation_source.progress`
on a performable ramp), plus one Skin preset per P0's verdict (in-graph generator →
`emissive_map`). Pre-warmed pipelines per the COMPILE_CONTRACT — these are graph
presets, so warmup rides the existing preset-prewarm path.

---

## 4. Invariants & enforcement

| Invariant | Enforcement |
|---|---|
| Every new deformer on the freeze codegen path | existing codegen-path scope test (docs/ADDING_PRIMITIVES.md:59) — P1 adds each atom to its coverage |
| Amount-at-zero = byte-identical passthrough ("off is free") | NEW check in the scope test: value-level GPU proof — with every amount-like scalar forced to 0 (NOT the atom's defaults: the July family ships non-zero defaults like `push_along_normals.amount=0.2`), output == input. New atoms ship 0 defaults so "default" and "zero" coincide — P1 deliverable, named `default_passthrough` per atom |
| No CPU per-vertex work, no per-frame alloc | hot-path discipline; `MANIFOLD_RENDER_TRACE=1` gate per phase (frame >20ms fails) |
| `layer_source` never blocks render | unit test: unknown/missing/deleted layer id → fallback texture, no panic |
| Feedback is always one-frame | structural: registry write site is after the last layer render (compositor end-of-frame); test: two-layer mutual-skin scene renders 300 frames without hang/panic |
| Layer id survives save/reload | round-trip test: bind → save → reload → skin still bound and rendering |
| No new shared state | negative `rg` gate: `Arc<Mutex\|Arc<RwLock` in `layer_skin.rs` + `layer_source.rs` → zero hits |

---

## 5. Phasing

### P0 — spike: how much of Skin Video is already free (half session)

- **Entry state:** main builds; `tests/fixtures/rt/apricot_tl05.glb` present.
- **Read-back:** this doc sections 1–2; `scene_object.rs:43-59`; one shipped
  texture-producing atom's descriptor.
- **Deliverables:** a hand-authored graph JSON: gltf mesh source → scene_object with
  a texture-producing atom wired into `emissive_map` → render_scene. Rendered PNG.
  Written verdict in the session record: in-graph skins work / what exactly blocks.
- **Gate:** `cargo run -p manifold-renderer --bin graph-tool -- render <graph.json>`
  exits 0; PNG region-mean probe at the model's screen position is non-zero and
  differs from the unwired baseline by a stated threshold (computed, not eyeballed).
- **Demo:** the two PNGs, L2 — Peter looks.
- **Forbidden moves:** no feature-code changes in this phase; no "it probably works" verdict
  without the probe numbers. (Lead amendment 2026-08-22: oracle-tooling fixes ARE in
  scope when the named oracle is unfit — `graph-tool render` didn't wait on
  `io_pending`, so GLB captures landed black; the lane fixed the tool, not the feature.)
- **Test scope:** none beyond the spike commands.

### P1 — host + first glitch atoms (one session)

- **Entry state:** P0 verdict written; anchors `scene_vm.rs:66-75` and
  `push_along_normals.rs` re-verified (`rg -n "MODIFIER_TYPE_IDS" scene_vm.rs`).
- **Read-back:** docs/ADDING_PRIMITIVES.md whole; `push_along_normals.rs` whole (the
  template); D1/D2/D3 restated; the forbidden-moves list below.
- **Deliverables:** `voxelize_mesh`, `noise_displace`, `glitch_jitter` atoms per
  section 3.1, each with `gpu_tests` value proof; `default_passthrough` check added
  to the scope test (runs against these three AND the shipped July family — the July
  atoms get the check retroactively); the three type ids appended to
  `MODIFIER_TYPE_IDS`; `docs/NODE_CATALOG.md` + `node_catalog.json` regenerated.
- **Gate:** `scripts/gpu_proofs_gate.py` green; scope test green incl.
  `default_passthrough`; `cargo nextest run -p manifold-renderer` green; negative:
  `rg "create_compute_pipeline\(include_str" primitives/{voxelize_mesh,noise_displace,glitch_jitter}.rs` → zero hits.
- **Demo:** headless renders of the apricot glb, one per atom at a mid amount, PNGs
  + region-mean probes vs baseline, L2.
- **Performer gesture:** cell_size on a knob swept to zero mid-render — geometry
  visibly crushes; exercised via param set in the demo render.
- **Forbidden moves:** hand-WGSL runtime kernels; per-atom bespoke uniform structs
  that drift from codegen layout (the BUG-253 (blinn/tonemap uniform-layout drift)
  class); widening MODIFIER_TYPE_IDS beyond these three; "improving" the July atoms.
- **Test scope:** `-p manifold-renderer` + gpu-proofs; clippy `-p manifold-renderer`.

### P2 — flow the family through (one session)

- **Entry state:** P1 landed; the `default_passthrough` check exists and is the
  template.
- **Read-back:** section 3.1 table; one P1 atom whole; D2/D3.
- **Deliverables:** `shatter_mesh`, `slice_mesh`, `ripple_mesh`, `fold_mesh`,
  `melt_mesh` + registration + catalog, same shape as P1.
- **Gate:** identical to P1, plus flat-list triangle-convention test for
  `shatter_mesh` (CPU-computed expected face normals).
- **Demo:** five PNGs on the apricot, probes computed, L2.
- **Performer gesture:** Slice's `cut` swept across the model — the wipe reads from
  the back of the room.
- **Forbidden moves:** index-buffer assumptions (flat triangle list only); CSG
  capping ambitions; per-family phase splitting.
- **Test scope:** as P1.

### P3 — shake atom + scene-mod presets (one session)

- **Entry state:** P1 landed (passthrough check exists); KICK_SWEEP_EVENT on main.
- **Read-back:** D4 + the parked craft notes (rotational > positional, amount²,
  stateless); `transform_3d.rs` + `ports.rs` Transform handling; whether
  `scene_vm.rs` walks Transform chains — if not, the section-3.2 walk addition is
  in scope and named here.
- **Deliverables:** `node.transform_shake` atom; the five presets of section 3.5
  (validation via `graph-tool validate --kind generator`); Transform-chain
  modifier walk in `scene_vm.rs` if the entry check found it missing.
- **Gate:** CPU unit tests green for the atom (value proof vs hand-computed noise;
  amount=0 passthrough bit-exact); presets validate; the statelessness negative rg
  (`struct.*Shake.*State\|static.*SHAKE` in transform_shake.rs → zero hits).
- **Demo:** three-frame PNG sequence of a camera-shaken scene at rising amounts —
  probe = inter-frame pixel delta grows with amount (computed), L2. Presets: L2
  renders.
- **Performer gesture:** kick → shake envelope on `amount`; the gate render drives
  `amount` by an envelope table, not a constant.
- **Forbidden moves:** stored noise phase/RNG state (statelessness is the design);
  a camera-node param instead of the atom (D4); presets hand-authored without
  `validate`.
- **Test scope:** `-p manifold-renderer` + gpu-proofs; presets: graph-tool validate.

### P4 — layer skins (the risk phase; split P4a/P4b 2026-08-22, lead call)

Split at design time: P4a is renderer-only, P4b is the UI surface. Combined they
were too wide for one lane session.

**P4a — registry + node + execution seam (one session)**

- **Entry state:** anchors `generator_renderer.rs:847` and
  `layer_compositor.rs:1696-1710` re-verified; D5–D9 read.
- **Read-back:** section 3.3 whole; the two-thread model in CLAUDE.md; D8's
  load-path rule.
- **Deliverables:** `layer_skin.rs` registry (section 3.3 shape); `node.layer_source`
  texture producer; context-carrier exposure; compositor end-of-frame publish;
  tests per section 4 (fallback, one-frame feedback, round-trip of the `layer`
  param).
- **Gate:** `cargo nextest run -p manifold-renderer` green; the three named tests
  green; negative rg gates per section 4; `MANIFOLD_RENDER_TRACE=1` two-layer skin
  scene: no frame >20ms.
- **Demo:** headless two-layer render where layer B's scene object wears layer A's
  output — probe: the skin region's mean matches layer A's known content within a
  stated tolerance (computed), L2.
- **Performer gesture:** source layer's clip ends mid-render — skin goes dark, no
  hitch; the feedback test covers it.
- **Forbidden moves:** `Arc<Mutex>` anywhere; same-frame binding; clearing the
  stored layer id on missing; per-clip binding (D6).
- **Test scope:** `-p manifold-renderer`; gpu-proofs if shared WGSL touched
  (expected: no).

**P4b — panel Skin row + L3 flow (one session)**

- **Entry state:** P4a landed; `commands/graph/modifiers.rs` read (splice
  precedent); WIDGET_TREE_DESIGN section 5b read.
- **Read-back:** section 3.4 whole; D7/D8.
- **Deliverables:** Skin row per scene object (source dropdown of layer names from
  the project snapshot + target-map dropdown, manifest params only, visibly
  clickable chrome); insert command routing through EditingService; missing-layer
  chip; `scripts/ui-flows/scene-skin.json`.
- **Gate:** `cargo nextest run -p manifold-renderer -p manifold-ui -p manifold-editing`
  green; L3 flow green end-to-end INCLUDING save → reload → still bound.
- **Demo:** the L3 flow IS the demo (L3); final step kills the source clip and
  asserts the frame still renders (fallback).
- **Performer gesture:** two clicks — pick source, pick map — and the model wears
  the layer; the flow performs exactly that.
- **Forbidden moves:** bespoke dropdown widgets outside the manifest surface;
  direct model writes (EditingService only); silent drop of an unresolvable id.
- **Test scope:** renderer + ui + editing crates.

---

## 6. Decided — do not reopen

1. The rack is the shipped scene-panel modifier stack; new atoms register in
   `MODIFIER_TYPE_IDS` (D1). Twist/bend/taper/morph already exist.
2. All deformers are stateless freeze-path `primitive!` atoms (D2).
3. "Off is free" is a scope-test check, not a habit (D3).
4. Shake is a Transform-wire atom: rotational-dominant, amount², stateless (D4).
5. Layer skins read the previous frame; loops are one-frame feedback, allowed (D5).
6. Skin source = layer (composited output), not clip (D6).
7. Source picker = manifest param + one panel row; no bespoke UI (D7).
8. Missing source layer → fallback + chip + id kept inert-but-present (D8).
9. Registry inside `manifold-renderer`, content thread, no new shared state (D9).

## 7. Deferred

- **CSG-style solid capping for Slice** — revive if Peter asks for hollow-solid cuts.
- **Hierarchy-aware scene-wide deform** (bend the whole scene as one) — needs the
  scene-graph hierarchy v2 deferred by REALTIME_3D; revive with it.
- **Morph-weight performance presets** (audio → morph channels) — plumbing shipped;
  preset authoring only; revive when a morph-target glb enters the show corpus.
- **HDRI/environment from a live 2D layer** — equirect mismatch makes this a
  projection design, not a wire; revive if stage looks want it.
- **Skin source = media clip directly** (bypassing the layer) — the layer
  indirection covers the stated ask; revive if routing videos without a layer
  becomes a real workflow.
- **Weight painting UI** for the optional `weights` inputs — `mesh_ramp` and field
  textures cover localization; revive if hand-painted masks are asked for.
