# Mesh Deform & Curve Geometry — organic motion for imported and procedural meshes

**Status:** APPROVED design, not built · 2026-07-10 · Fable (with Peter in the room)
**Prerequisites:** none hard — `node.render_scene` (REALTIME_3D P1) and SCENE_BUILD P1–P3 are shipped; every anchor below re-verifies at phase entry.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, 2026-07-10, the ask this design serves: *"using a 3D scanned set of flowers
(CC0) and animating them growing or unfolding or morphing into particles and digital
visuals."* Today MANIFOLD can import that scan (`node.gltf_mesh_source`), light it
(`node.render_scene`), move it rigidly (`node.transform_3d`), and dissolve it into
particles (`node.spawn_from_mesh`) — but it cannot deform it. The mesh vocabulary is
heightmap-shaped (`node.push_mesh` displaces Y on grids); there is no per-vertex
organic motion, no growth, no morph, and the 2D curve family dead-ends in
`node.draw_lines` with no path to 3D geometry. This design adds two atom families that
close both gaps:

- **Deform family** — per-vertex GPU atoms over `Array<MeshVertex>`: a weights
  producer (`mesh_ramp`) and five deformers (`push_along_normals`, `bend_mesh`,
  `twist_mesh`, `taper_mesh`, `morph_mesh`) plus one normals reset (`facet_normals`).
- **Curve→mesh family** — three grid-emitting builders (`revolve_curve`,
  `extrude_curve`, `tube_from_path`) that turn the existing 2D curve vocabulary into
  3D geometry, plus `scatter_on_mesh` (instances on a surface) and a `fit` extension
  on `gltf_mesh_source`.

**On stage:** a scanned flower breathes with the low band (`push_along_normals` amount
wired to audio), grows stem-upward on a ramp sweep (`mesh_ramp` phase on a beat ramp),
unfolds on a bend, morphs into a polytope on the drop (`morph_mesh` t), and dissolves
into a murmuration (`spawn_from_mesh`, unchanged, reading the *deformed* vertices).
Procedural vines and lathed forms come from the same curve atoms that already draw
Lissajous figures. Every scalar named in this paragraph is port-shadowed and therefore
bindable — this is performance surface, not authoring convenience.

**Binding constraints** (per DESIGN_AUTHORING §1): hot path — every atom here runs
per-frame on the content thread's graph walk, so all deformation is GPU dispatches,
zero CPU per-vertex loops, zero per-frame allocation; no new state — every atom is
stateless (scratch lives in `extra_fields`, reset-free); no persistence — atoms
serialize as ordinary graph nodes, nothing new; performance surface — every numeric
scalar param ships port-shadowed (DECOMPOSING §6.2 authoring rule).

Companion docs: [DECOMPOSING_GENERATORS.md](DECOMPOSING_GENERATORS.md) (the governing
working guide — §2.5 audit, §6.6 naming, §7 invariants), [ADDING_PRIMITIVES.md](ADDING_PRIMITIVES.md)
(the `primitive!` + `gpu_tests` mechanics every phase follows),
[NODE_CATALOG.md](NODE_CATALOG.md) (registry truth), [REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md)
+ [SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md](SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md)
(the scene these meshes render into).

---

## 1. Audit — what exists (verified 2026-07-10)

Instruction to every phase: **extend, don't redesign.** The wire types, the render
path, and the particle bridge all exist; this design adds producers and transforms on
existing wires. No new port types, no new channel families, no renderer changes.

| Piece | Where | State |
|---|---|---|
| `MeshVertex` = position/normal/uv, 48 B std430, `KnownItem` | `crates/manifold-renderer/src/generators/mesh_common.rs:34` (SPECS stride-asserted `:366`) | the wire every atom here reads/writes; **no joints/weights attributes exist** |
| Flat triangle-list convention (no index buffer): triangle `t` = verts `[3t..3t+3]`, trailing partial ignored | `spawn_from_mesh.rs` module doc + its `composition_notes` | the layout `facet_normals` and `scatter_on_mesh` rely on |
| glTF import | `node.gltf_mesh_source` (`primitives/gltf_mesh_source.rs`) — background-thread parse, `cached_verts` resident, re-upload only on change; `mesh_index=-1` world-combines the default scene (`gltf_load.rs:131` walks nodes, world-transforms positions + normals via `transpose(inverse(upper3x3))` `:153`) | ships; **parses static geometry only — `rg -n 'animation\|skin\|joint\|morph\|weight' gltf_load.rs gltf_import.rs` → zero hits (run 2026-07-10)**; no fit/normalize |
| Y-only heightmap displace | `node.push_mesh` (`primitives/displace_mesh.rs`) — `displaced_y = src.y + (h − bias) × amount`, normals passed through, grid-topology only (cols/rows UV) | the *only* existing deformer; along-normals, weights, bend/twist/taper/morph **do not exist** (`rg -l 'recompute_normals\|normals_from_positions\|compute_normals' src/` → only triangulate_grid's grid kernel) |
| Grid → triangles + normals | `node.triangulate_grid` (`primitives/triangulate_grid.rs`) — (N−1)×(M−1)×6 triangle list, finite-difference normals from grid neighbours; hand-kernel parity test in-module | **the topology/normals consumer every curve→mesh builder reuses (D5)** |
| Mesh sources | `node.grid_mesh` (`generate_grid_mesh.rs`, positions+uv, XZ plane), `node.cube_mesh` (36-vert list), `polytope_vertices`/`polytope_edges`, grid-uv parametric family (`generate_grid_uv` + `array_math` + `pack_vec4` + `edges_from_grid_uv`, Duocylinder reference) | rich; all emit `Array<MeshVertex>` or grid-uv precursors |
| Rigid motion | `node.transform_3d` (`primitives/transform_3d.rs:28`) → `PortType::Transform` into `render_scene`'s per-object `transform_n` port (SCENE_BUILD P2, migration v1.12.0) | whole-object TRS is solved; **this design is per-vertex, deliberately below TRS** |
| Scene renderer | `node.render_scene` (`primitives/render_scene.rs`) — dynamic object ports (`mesh_i`/`material_i`/`base_color_map_i`/`transform_i`), uncapped objects/lights, shared MSAA depth | consumes whatever mesh wire it's given — deformed meshes need **zero renderer changes** |
| Mesh → particles | `node.spawn_from_mesh` (`primitives/spawn_from_mesh.rs`) — vertices + area-weighted surface modes, 3-pass (area/scan/place), `reset_trigger` gate | shipped; **the 3-pass area-sampling shape is the committed precedent for `scatter_on_mesh`** |
| Instanced rendering | `InstanceTransform` = pos+uniform-scale / euler+pad, 32 B (`mesh_common.rs:93`); `node.generate_instance_transforms` (4 procedural layouts); `node.render_instanced_3d_mesh` | instances exist; **no mesh-surface scatter producer** |
| 2D curve family | `CurvePoint` = origin-centred pre-aspect xy, 8 B (`mesh_common.rs:136`); producers `pack_curve_xy`, `generate_lissajous`, `project_3d`/`project_4d`; consumer `node.draw_lines` (+`EdgePair` topology, `mesh_common.rs:168`) | alive but **dead-ends in 2D line drawing — no curve→mesh bridge exists** |
| Parametric scalar plumbing | `generate_range` (linspace), `array_math` (elementwise op enum), `pack_curve_xy` | the producers that feed `tube_from_path`'s path + lift inputs (D6) |
| Per-vertex scalar weights wire | `Channels<f32>` (`KnownItem for f32`, `ports.rs:150`) | exists — `mesh_ramp` emits it, every deformer consumes it (D2) |
| Same-frame GPU→CPU readback | forbidden — DECOMPOSING §7 "Shared MTLBuffer" bullet | why `fit` is a parse-time extension, not a GPU atom (D7) |

Nearest reference presets (per §2.5 step b, read end-to-end 2026-07-10):
**WireframeZoo** (`rotate_3d → project_3d → draw_lines` — the transform-stage shape
deformers slot into), **Duocylinder** (grid-uv parametric surface — the grid-emitting
shape the curve builders follow), **MetallicGlass-shaped graphs** (`grid_mesh →
push_mesh → triangulate_grid → render` — the committed precedent D5 generalizes).

Classification: **exists** — wire types, renderer, particle bridge, grid
triangulation, rigid TRS, curve producers. **One wire away** — breathing terrain
(`push_mesh` + noise texture); whole-object growth (`transform_3d.scale` on a ramp).
**Genuinely new** — per-vertex weights, along-normal displacement, bend/twist/taper,
morph, facet normals, curve→mesh builders, surface scatter, glTF fit. That list is
this design.

## 2. Decisions

- **D1 — Deformers are single-dispatch GPU atoms over `Array<MeshVertex>`, count- and
  layout-preserving.** One thread per vertex; output vertex `i` corresponds to input
  vertex `i`; uv always passes through untouched; capacity inherits from input
  (`array_output_capacity` transform-primitive override, like `push_mesh`).
  Rejected: *deform-at-render* (vertex-shader deform modes inside `render_scene`, the
  TouchDesigner-vertex-shader instinct) — because the deformed geometry must exist ON
  THE WIRE: `spawn_from_mesh`, `scatter_on_mesh`, `facet_normals`, and any future
  consumer read vertices, and a renderer-internal deform would make the particle
  dissolve read the *undeformed* flower. Rejected: CPU deformation loops — hot-path
  violation at scan vertex counts (a phone-scanned flower bouquet is 10⁵–10⁶ verts).
- **D2 — Weights are a composable wire, not per-deformer params.** `node.mesh_ramp`
  emits `Channels<f32>` (one weight per vertex); every deformer takes an optional
  `weights` input, default 1.0 when unwired. **This is the growth mechanism:** sweep
  `mesh_ramp.phase` 0→1 and any deformer it gates walks up the mesh. Rejected: baking
  axis/origin/feather into each deformer — five copies of the same param surface, and
  it forecloses non-ramp weight sources (a future texture-sampled or
  audio-band-per-region weights producer plugs into the same port).
- **D3 — Single-purpose atoms, not a `deform_mesh` mode-enum family.** Bend, twist,
  taper, and push have disjoint param surfaces; a mode enum would leave params
  wired-but-inert per mode — precisely DECOMPOSING §7's dead-state-param violation.
  §6.3's family test fails: nobody thinks "bend/twist/taper" is one knob.
- **D4 — Normal policy, stated honestly.** `bend_mesh` and `twist_mesh` rotate
  normals by the same local rotation they apply to positions — exact. `taper_mesh`
  applies the inverse-transpose scale in the taper plane and renormalizes —
  exact for its transform. `push_along_normals` and `morph_mesh` leave normals
  approximate (unchanged / lerp-renormalized): correct-looking for the moderate
  amounts organic motion uses, visibly wrong at extremes. **The exact reset is
  `node.facet_normals`** — per-triangle flat normals, one thread per triangle,
  trivially correct on the flat triangle-list layout (each vertex belongs to exactly
  one triangle; no gather, no race). Composition rule, which every deformer's
  `composition_notes` must state: *heavy push/morph → wire `facet_normals` after, and
  accept the faceted look — or keep amounts moderate and keep the scan's smooth
  normals.* Rejected for v1: smooth-normal recompute on arbitrary triangle lists —
  needs shared-vertex discovery (position hashing + atomics or sort), real
  infrastructure with its own design; Deferred #2.
- **D5 — Curve→mesh builders emit positions+uv GRIDS and reuse
  `node.triangulate_grid` for topology and normals.** A revolve of a P-point profile
  at S segments is naturally a P×(S+1) grid (seam column duplicated, uv wraps);
  extrude and tube are likewise grids. Emitting grids means the three builders share
  one triangulation/normals implementation that already ships with a parity test,
  and the MetallicGlass-shaped graph precedent holds. Cost, stated honestly: one
  extra node per graph and one extra dispatch — accepted; the alternative (each
  builder emitting final triangle lists with its own analytic normals) writes the
  triangulation three more times and re-fights triangulate_grid's tested edge cases.
- **D6 — Tube paths speak the existing curve vocabulary.** `tube_from_path` takes
  `Array<CurvePoint>` as the path (interpreted in the XZ plane) plus an optional
  `lift: Channels<f32>` per-point Y — both producible today (`pack_curve_xy`,
  `generate_range → array_math`). A spiral vine is a circle path + linear lift; no
  new 3D-path pack atom, no new channel family. Rejected: a `pack_vec3` + 3D-path
  ecosystem — nothing else consumes it yet; revive only if a second 3D-path consumer
  appears (Deferred #4).
- **D7 — `fit` is a parse-time extension of `gltf_mesh_source`, not a GPU atom.**
  New params `fit` (enum: `none` / `unit_box`, default `none`) and `recenter` (bool,
  default true under `unit_box`): computed on the existing background parse thread
  over `cached_verts`, cost zero per-frame. Every scan arrives at arbitrary scale
  and origin; `unit_box` makes deformer defaults and `mesh_ramp` bounds (0..1)
  meaningful. Rejected: a `normalize_mesh` GPU atom — bounds reduction is a
  GPU-write→CPU-read in the same frame, forbidden by the shared-buffer rule
  (DECOMPOSING §7); a two-frame-latency normalize would breathe on the first frames.
  This is §6.2 extend-before-build, verbatim.
- **D8 — `scatter_on_mesh` emits `Array<InstanceTransform>`, deterministic by seed.**
  Same 3-pass area-weighted sampling as `spawn_from_mesh` surface mode (the committed
  precedent), but the place pass writes instance transforms: position on surface,
  uniform scale in `[scale_min, scale_max]` by hash, yaw random, and
  `align_to_normal` (bool) derives pitch/roll euler from the sampled triangle's
  normal. Consumed by `node.render_instanced_3d_mesh` today. A field of scanned
  flowers on a terrain is: terrain mesh → `scatter_on_mesh` → instanced flowers.
- **D9 — glTF animation is OUT of this design.** TRS track playback, skeletal
  skinning (which needs joints/weights vertex attributes `MeshVertex` does not
  carry), and morph-target playback are one coherent future design with its own
  audit (vertex-stream extension vs sidecar buffers, beat-domain animation clock,
  Mixamo-class fixtures). Deferred #1 carries the scope sketch and trigger.
  `morph_mesh` here is the static two-mesh lerp only — do not grow it toward
  glTF morph targets mid-phase.

## 3. Atom specifications (committed)

Common to all: authored via `primitive!` per ADDING_PRIMITIVES; every numeric scalar
param is port-shadowed via `EffectNodeContext::scalar_or_param` (DECOMPOSING §7);
enum/bool params are not. `composition_notes` on every atom states when an agent
reaches for it and its normal-policy caveat (D4). Names follow §6.6 — plain words,
implementation detail stays in the source. **Angle/rotation params are UNBOUNDED
(range `None`)** — a saw LFO doing full revolutions is the first thing a performer
tries (the BUG-039 lesson); clamping them is a forbidden move.

Deform family — all `in: Array(MeshVertex) required`, `weights: Array(F32) optional`,
`out: Array(MeshVertex)`, capacity inherited, one dispatch, one thread/vertex.
Weight read: `w = i < weights_len ? weights[i] : 1.0` (a short buffer degrades to
1.0, never to silent zero — a wired input that blanked the mesh would be the
dead-state bug in its worst costume):

| Atom | Extra ports/params | Committed math (local space) |
|---|---|---|
| `node.mesh_ramp` — label "Mesh Ramp" (aliases: growth mask, gradient weights). **Source of weights, not a deformer**: `in: Array(MeshVertex) required` → `weights: Array(F32)` | `axis` enum (X/Y/Z/Radial XZ/Distance); `origin` x/y/z (f32×3); `phase`, `feather`, `bound_min`, `bound_max` (f32); `invert` (bool) | `m = measure(pos − origin)` per axis mode; `t = clamp((m − bound_min)/(bound_max − bound_min), 0, 1)`; `w = 1 − smoothstep(phase, phase + feather, t)`; `invert → 1 − w`. Defaults `bound_min 0, bound_max 1` — sane after D7 `unit_box` fit. Phase 0 → nothing grown… phase 1+feather → fully grown |
| `node.push_along_normals` — label "Push Along Normals" (aliases: inflate, breathe) | `field: Texture2D optional` (sampled bilinear at vertex uv, `.r`); `amount` (f32, world units); `field_bias` (f32, default 0.5) | `pos += normal × amount × w × f`, where `f = field wired ? (sample.r − field_bias) : 1.0` (bias semantics match `push_mesh`). Normals unchanged (D4 approximate) |
| `node.bend_mesh` — label "Bend Mesh" | `axis` enum (X/Y/Z = bend direction), `angle` (f32 rad, UNBOUNDED), `center` (f32) | classic bend: rotation about the axis-orthogonal line through `center`, angle proportional to coordinate along the bend axis, scaled by `w`. Positions and normals rotated by the same local rotation (D4 exact) |
| `node.twist_mesh` — label "Twist Mesh" | `axis` enum, `angle` (f32 rad/unit length, UNBOUNDED), `center` (f32) | `θ(v) = angle × (coord(v) − center) × w`; rotate position and normal about axis by θ (D4 exact) |
| `node.taper_mesh` — label "Taper Mesh" | `axis` enum, `taper` (f32, 1 = none, 0 = point), `center` (f32), `length` (f32) | `s(v) = mix(1, taper, clamp((coord − center)/length, 0, 1) × w)`; scale the two off-axis components by `s`; normals: off-axis components ÷ s, renormalize (D4 exact-for-transform) |
| `node.morph_mesh` — label "Morph Mesh" | second input `b: Array(MeshVertex) required`; `t` (f32 0..1 soft range) | `n = min(count_a, count_b)`; `pos = mix(a, b, t×w)`, `normal = normalize(mix(...))`, uv from `a`. Correspondence is by index — meaningful between variants of one mesh or as a deliberate scramble-morph between unrelated ones; both are stage-valid, notes say so |
| `node.facet_normals` — label "Facet Normals" | none | one thread per **triangle**: `n = normalize(cross(v1−v0, v2−v0))` written to all three verts; partial trailing triangle passes through. Exact on the flat-list layout (D4) |

Curve→mesh family — all emit positions+uv `Array<MeshVertex>` **grids** (normals
zero; wire `triangulate_grid` downstream with matching cols/rows — the builder's
`composition_notes` states the exact cols/rows values as formulas of its params, and
the demo presets are the worked examples). Capacity: computed override
(rows × cols), like `triangulate_grid`'s:

| Atom | Ports/params | Committed shape |
|---|---|---|
| `node.revolve_curve` — label "Revolve Curve" (aliases: lathe, spin) | `profile: Array(CurvePoint) required` (x = radius, y = height); `segments` (int, default 48); `sweep` (f32 rad, default 2π, UNBOUNDED) | grid rows = profile count P, cols = segments+1 (seam duplicated); `pos(i,j) = (x_i·cos φ_j, y_i, x_i·sin φ_j)`, `φ_j = sweep × j/segments`; `uv = (j/segments, i/(P−1))` |
| `node.extrude_curve` — label "Extrude Curve" | `outline: Array(CurvePoint) required`; `depth` (f32); `steps` (int, default 1); `close` (bool — duplicate first point as last column) | grid rows = steps+1, cols = P (+1 closed); `pos(i,j) = (x_j, y_j, depth × i/steps)`; uv = (j-frac, i-frac). No end caps in v1 (Deferred #3) |
| `node.tube_from_path` — label "Tube From Path" (aliases: vine, ribbon, sweep) | `path: Array(CurvePoint) required` (XZ plane); `lift: Array(F32) optional` (per-point +Y); `radius_scale: Array(F32) optional` (per-point, composable with a ramp for tapered vines); `radius` (f32); `sides` (int, default 8) | centerline `c_k = (x_k, lift_k or 0, y_k)`; frame per point: tangent from neighbours, reference-up = +Y (**degenerate when tangent ∥ Y — documented limit, Deferred #4 carries parallel transport**); ring of `sides+1` verts (seam dup) per path point; uv = (around, along) |
| `node.scatter_on_mesh` — label "Scatter On Mesh" | `in: Array(MeshVertex) required`; `count`, `seed` (int, port-shadowed); `scale_min`, `scale_max` (f32); `align_to_normal` (bool); `reset_trigger: ScalarF32 optional` (recompute gate, same contract as `spawn_from_mesh`) → `instances: Array(InstanceTransform)` | 3-pass area/scan/place per the `spawn_from_mesh` precedent; place writes `pos_scale = (surface point, scale by hash)`, `rot_pad` euler = random yaw (+ pitch/roll from triangle normal when aligned). Deterministic for fixed (seed, mesh) |

Extension (§6.2, not a new atom): `node.gltf_mesh_source` grows `fit` (enum
`none`/`unit_box`, default `none`) + `recenter` (bool, default true) — applied in the
background parse before caching (D7). Old presets unaffected (`none` default).

## 4. Invariants & enforcement

| Invariant | Enforcement (machine check, named) |
|---|---|
| Deformers preserve count, order, and uv | per-atom `gpu_tests`: assert output count == input count and uv bytes identical on a fixture grid |
| Short/absent weights degrade to 1.0, never 0 | `gpu_tests` case: weights buffer of length 2 against a 12-vert mesh → verts 2..12 deform at full weight |
| bend/twist rotate normals exactly | `gpu_tests`: analytic expected normals compared element-wise (1e-5) |
| Grid builders emit triangulate_grid-compatible grids | chain `gpu_tests` (per DECOMPOSING §9 chain rule): revolve → triangulate_grid on a 3-point profile, assert triangle count and a hand-computed vertex |
| Every new Array producer declares capacity | existing CI sweep `every_array_output_declares_a_valid_capacity_source` (effect_node.rs:442) — runs in the default suite |
| Angle params unbounded | `gpu_tests` param-decl assert: `range == None` for `angle`/`sweep` (the BUG-039 regression pin) |
| Demo presets compile | `cargo run -p manifold-renderer --bin check-presets` in every phase gate |
| No per-frame allocation in `run()` | review rule (existing discipline); scatter/spawn scratch lives in `extra_fields`, reallocs only on capacity change |

## 5. Phasing

Common: Git Mode B (ONE warm worktree for the whole workstream via
`agent-worktree.py acquire`; orchestrator lands, batched per 2–3 phases —
GIT_TREE_DISCIPLINE §2c); test scope per phase is
focused — `cargo test -p manifold-renderer --lib <module>::gpu_tests --features
gpu-proofs` for new kernels (deliberate GPU runs — these are shader atoms),
`check-presets` after any JSON, crate-scoped clippy; the single workspace sweep runs
at the END of P4 only. Demo renders use the `render-generator-preset` harness
(the generator look-dev bin) and the orchestrator READS the PNGs. No phase touches
`render_scene`, the graph runtime, or any existing primitive except where named.

- **P1 — Growth core: `mesh_ramp` + `push_along_normals` + `facet_normals`.**
  *Entry:* repo tip; anchors `mesh_common.rs:34`, `displace_mesh.rs`,
  `spawn_from_mesh.rs` re-verified. *Read-back:* this doc §2–§4, DECOMPOSING §6.2/§7,
  ADDING_PRIMITIVES whole. *Deliverables:* three atoms + `gpu_tests` per the §4
  table; demo generator preset `Breathe.json` (bundled): `cube_mesh` (or
  `grid_mesh→…` if cube reads too rigid) → `mesh_ramp` → `push_along_normals`
  (LFO on amount) → `facet_normals` → `render_scene`, outer cards for
  amount/phase/feather. *Gate (positive):* named `gpu_tests` green under
  `--features gpu-proofs`; `check-presets` green; headless PNG pair at
  `phase=0.2` vs `phase=0.9` shows the deformation band visibly walking up the
  mesh — orchestrator reads both. *Gate (negative):* `rg 'Vec::new\(\)|to_string\(' `
  in the three new `run()` bodies → zero hits; `rg 'range: Some' ` on the new
  angle-free atoms' amount params only where the table says so. *Demo:* the PNG
  pair — L2. *Performer gesture:* LFO → push amount = the mesh breathes on the low
  band; gate exercises it by rendering two LFO phases. *Forbidden:* CPU vertex
  loops; normals "recomputed" by copying triangulate_grid's grid kernel onto
  non-grid meshes (it's grid-only — that's why `facet_normals` exists); starting
  bend/twist early.
- **P2 — Shape deformers: `bend_mesh` + `twist_mesh` + `taper_mesh` + `morph_mesh`.**
  *Entry:* P1 landed (weights-read pattern exists to copy). *Read-back:* §2 D3/D4,
  §3 table rows, P1's landed weight-read code. *Deliverables:* four atoms +
  `gpu_tests` incl. the exact-normals and unbounded-angle asserts; demo preset
  `TwistColumn.json`: `grid_mesh` → `triangulate_grid` → `twist_mesh` ← `mesh_ramp`,
  saw LFO on angle. *Gate:* named tests green (gpu-proofs); PNG triptych at three
  LFO phases shows continuous rotation past 2π (the gesture: **saw LFO does full
  revolutions, no wrap seam**); `check-presets` green. *Demo:* triptych — L2.
  *Forbidden:* clamping angle ranges; a shared "deform_common" mega-module that
  fuses the four kernels into one dispatch with a mode switch (D3 forbids the enum
  even disguised as an implementation detail); touching P1 atoms except the
  weight-read helper if extracted.
- **P3 — Curve→mesh: `revolve_curve` + `extrude_curve` + `tube_from_path`.**
  *Entry:* P1–P2 landed; anchors `mesh_common.rs:136` (CurvePoint convention),
  `triangulate_grid.rs` re-verified. *Read-back:* §2 D5/D6, the Duocylinder preset
  end-to-end (the grid-family precedent), `pack_curve_xy.rs`. *Deliverables:* three
  atoms + `gpu_tests` incl. the chain test (§4); demo preset `Vine.json`:
  `generate_range` + `array_math` circle/spiral path + linear lift →
  `tube_from_path` (`radius_scale` ← `mesh_ramp`-shaped taper via `array_math`) →
  `triangulate_grid` → `twist_mesh` → `render_scene`; beat ramp on ramp phase =
  the vine grows. *Gate:* chain test green; `check-presets`; PNG pair
  (grow phase 0.3/1.0) read by orchestrator; seam check — the revolve demo PNG at
  `sweep=2π` shows no lighting crack at the seam column (duplicated verts share
  position+uv, FD normals agree). *Demo:* PNGs — L2. *Performer gesture:* beat-ramp
  → vine grows over 4 bars. *Forbidden:* emitting triangle lists directly from
  builders (D5); parallel-transport frames (Deferred #4 — v1 is Y-reference,
  documented); end caps (Deferred #3).
- **P4 — Placement + import polish: `scatter_on_mesh` + `gltf_mesh_source` fit.**
  *Entry:* P1–P3 landed; `spawn_from_mesh.rs` 3-pass shape + `gltf_mesh_source.rs`
  param block re-verified. *Read-back:* §2 D7/D8, spawn_from_mesh's scan/place
  kernels, DESIGN_DOC_STANDARD §5 round-trip gate. *Deliverables:* `scatter_on_mesh`
  + `gpu_tests` (determinism: same seed+mesh → identical buffer, two runs);
  `fit`/`recenter` params on `gltf_mesh_source` (parse-thread, old-preset default
  `none`); demo preset `Garden.json`: `grid_mesh` terrain → `push_mesh` (noise) →
  `triangulate_grid` → `render_scene` object 0, plus `scatter_on_mesh` →
  `render_instanced_3d_mesh` of a `revolve_curve` flower-form → object 1. Bundled
  preset uses procedural stand-ins; Peter's scanned-flowers `.glb` swaps in via the
  `gltf_mesh_source` path param at the rig — the doc does not bundle third-party
  scans. *Gate:* determinism test green; **round trip** — save a project using
  `Garden` with edited outer params → reload → params intact and modulation still
  moves them (BUG-036 rule); `check-presets`; Garden PNG read (instances visibly
  ON the terrain surface, aligned when `align_to_normal`); canonical fixture
  (`Liveschool Live Show V6 LEDS.manifold`) loads clean; **full workspace sweep +
  workspace clippy (the design's single sweep)**. *Demo:* Garden PNG — L2; ≤2-min
  click-script for Peter: drop his flowers `.glb` into `gltf_mesh_source`, set
  `fit=unit_box`, wire through P1's Breathe chain. *Forbidden:* skinning/animation
  scope creep (D9); a scatter that reads GPU-computed areas back to CPU same-frame
  (§7 shared-buffer rule — the scan stays on-GPU like the precedent).

Phasing-completeness check (2026-07-10): every §2/§3 commitment maps — ramp/push/
facet → P1; bend/twist/taper/morph → P2; revolve/extrude/tube → P3; scatter/fit +
round-trip + sweep → P4; animation/smooth-normals/caps/3D-paths → Deferred #1–#4.
Every §0 stage claim (breathe, grow, unfold, morph, dissolve, vines, field of
flowers) is exercised by a phase demo except "dissolve" (ships today via
`spawn_from_mesh` — no phase needed).

## 6. Decided — do not reopen

1. Deformation happens on the wire, not in the renderer (D1).
2. Weights are a wire; no per-deformer ramp params (D2).
3. Single-purpose deformers; no mode-enum deform family (D3).
4. v1 normal policy: exact where analytic, `facet_normals` as the reset, smooth
   recompute deferred (D4).
5. Curve builders emit grids; `triangulate_grid` is the one topology path (D5).
6. Tube paths are `CurvePoint` + lift; no 3D-path pack atom in v1 (D6).
7. Fit/normalize lives in `gltf_mesh_source` at parse time (D7).
8. glTF animation is a separate future design (D9) — nothing here grows toward it.
9. Angle/sweep params ship unbounded (BUG-039 class).

## 7. Deferred

1. **glTF animation playback** — node TRS tracks, skeletal skinning, morph-target
   playback; imported characters (Mixamo-class) and Blender-baked growth become
   playable, with playback position/rate as beat-domain performance params.
   Requires its own design: joints/weights vertex data (extend `MeshVertex` vs
   sidecar `Channels` buffers — audit first), a matrix-palette skinning dispatch,
   an animation-clock atom, and fixtures. *Trigger:* Peter schedules
   character/animated-asset work, or the flowers workflow hits the limits of
   procedural growth (he asks for Blender-baked animation import).
2. **Smooth-normal recompute on arbitrary triangle lists** — needs shared-vertex
   discovery (spatial hash / sort + atomics). *Trigger:* `facet_normals` after heavy
   push/morph reads as objectionably faceted on real content at the rig.
3. **End caps for `extrude_curve`** (polygon triangulation, ear-clip, CPU at build
   rate). *Trigger:* first preset that needs a closed extruded solid on camera.
4. **Parallel-transport tube frames + a 3D path vocabulary** (`pack_vec3`, path
   sources, vertical path support). *Trigger:* a look needs paths that go vertical
   (current Y-reference frame degenerates) or a second 3D-path consumer appears.
5. **Non-ramp weights producers** (texture-sampled weights, per-region audio bands).
   The `weights` port is already the seam. *Trigger:* first ask for "only the petals
   react".
