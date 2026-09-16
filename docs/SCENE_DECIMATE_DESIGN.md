# Scene Decimate — mesh density as a scene/object modifier card

**Status:** PROPOSED — awaiting Peter · 2026-09-16 · k3 (lead)
**Prerequisites:** none — the file-authored scene-modifier regime and the
`Vertices` endpoint this design rides are on main (`SceneFog.json`,
`RenderMode.json`, Math View).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

Decimate — quadric mesh simplification (meshoptimizer) — as a **modifier
card**: applied to one object or a whole scene from the inspector, tuned by
a ratio slider, enabled/bypassed, MIDI-mapped, serialized with the project.
On stage this makes mesh density a performance axis and a look axis: drop a
1.4M-tri photoscan to 10% on a pad, stack it under the Render Mode card for
low-poly + wireframe/points looks, ride density off a stem.

Peter's directives, verbatim:

- "Is it possible to use an open source remesh or decimation algorithm …
  to add a new set of scene and or object modifiers that apply these mesh
  optimisations?" (2026-09-16) — the modifier-card shape is the product call.
- "I'm thinking this would be useful for making low poly styled versions and
  wireframes etc" (2026-09-16) — composition with Render Mode is a
  requirement, not a bonus.

Companions: `SCENE_MODIFIER_FRAMEWORK_DESIGN.md` (descriptor/card contract),
`SCENE_RENDER_MODE_DESIGN.md` (the recipe card this mirrors),
`MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md` (the `Array<MeshVertex>` atom
family), `ADDING_PRIMITIVES.md` (the one new atom),
`GLB_IMPORT_OPTIMIZATION_GUIDE.md` (the offline gltf-transform pipeline this
complements, not replaces).

---

## 1. Audit — what exists (verified 2026-09-16)

Verified by lead spot-read over a recon lane's survey; anchors re-verified
at execution per phase briefs.

| Piece | Where | State |
|---|---|---|
| File-authored modifier recipes, catalog-loaded, no Rust registration | `assets/scene-modifier-presets/*.json` (13); loader `node_graph/bundled_presets.rs:106` | Shipped. The authoring path. |
| Object-level targeting — `SceneTargetSelection::{AllObjects, Explicit}`, `SceneStageScope::{Scene, EachObject}` | `manifold-core/src/scene_modifier_preset.rs:49,201` | **Shipped. Scene-level and object-level come from the same recipe — no separate scene-level phase exists.** |
| `SceneEndpoint::Vertices` — per-target vertex replacement, spliced between source and `render_scene` | `scene_modifier_preset.rs:214`; `node_graph/scene_modifier_expand/compiler.rs:56` (`endpoint_port`), per-target attachments `:605,674,731` | Shipped; sole current user is Math View (vertex-grid replacement). **Decimate is the second user.** |
| RT incompatibility lock — vertices modifier ⇒ `rt_enabled` param-locked and expansion refused | `scene_modifier_preset.rs:97-113`; `compiler.rs:96-107` (`UnsupportedRenderMode`) | Shipped. Exists because vertex modifiers may animate per frame. P3 lifts it for declared-static recipes (D6). |
| Mesh representation — flat non-indexed `Vec<MeshVertex>`, 64B `#[repr(C)]` Pod (pos/normal/uv/tangent) | `generators/mesh_common.rs:35-47`; indices expanded at `gltf_load.rs:703` (`flatten_primitive`) | Settled shape. Decimate welds internally, outputs the same flat shape (D3). |
| Vertex source atom — `node.gltf_mesh_source`, output `Array(MeshVertex)` | `primitives/gltf_mesh_source.rs:115,120` | Shipped. |
| Async geometry precedent — background parse via `pending_load: mpsc::Receiver`, `cached_verts`, staging→dst blit, `published`/`copy_in_flight` gates, `content_version` | `gltf_mesh_source.rs:255-288` | **Shipped, battle-tested. The decimate atom mirrors this machinery exactly (D5).** |
| Mesh-transform atom family — `node.taper_mesh`, `node.shatter_mesh`, `node.facet_normals`, `node.rotate_3d`, `node.spawn_from_mesh`, all `Array(MeshVertex) → Array(MeshVertex)` | `primitives/taper_mesh.rs:1` et al. | Shipped, all GPU per-vertex/per-tri. Decimate joins the family as its first CPU atom (D2). |
| Flat shading (the low-poly faceted look) | `primitives/facet_normals.rs` | **Exists.** Compose after Decimate; no new work. |
| Graph array buffers are `StorageModePrivate` — no CPU access | `manifold-gpu/src/metal/device.rs:362` (`create_buffer`); `mapped_ptr` only on shared buffers (`metal/types.rs:229`) | **The binding constraint.** A CPU atom cannot read its array input directly; decimate needs a blit-readback on cache miss (D4). |
| Shared-buffer CPU access + readback machinery | `metal/types.rs:229` (`mapped_ptr`), `write`; `gpu_readback.rs:126` | Exists. |
| RT reads the same vertex buffer as raster — per-draw geometry at `render_scene.rs:5292` reads `d.vertices`; raster binds at `:4231` | `node_graph/primitives/render_scene.rs` | **One decimated buffer feeds both paths by construction — no RT/raster mismatch class.** |
| BLAS rebuild on topology/content change — `MeshTopologyHistory`, topology key `render_scene.rs:2850-2870`, content key (`vertices_generation`) `:2903-2924`; the stale-BLAS regression fix f0c61b10c | `render_scene.rs`, `scene_object.rs:121-141` | Shipped. A ratio change lands as a generation bump → automatic BLAS rebuild. |
| Skinned + morph paths — separate source atoms (`gltf_skinned_mesh_source`, `gltf_morph_deltas_source`, `gltf_morph_weights`) | `gltf_load.rs:2027` (skins), `:1385` (morphs) | Exist. Decimate refuses these targets (D7). |
| Disk decode cache — `cached_load_gltf_mesh`, sha256+selector LRU | `decode_cache.rs:317,43` | Exists upstream of the atom; untouched. |
| meshopt crate 0.6.2 — `simplify(indices, &VertexDataAdapter, target_count, target_error, SimplifyOptions, result_error) -> Vec<u32>` (indices into the ORIGINAL vertex buffer); `SimplifyOptions::{LockBorder, Sparse, ErrorAbsolute, Prune, Regularize, Permissive}`; `simplify_sloppy` family; `cc`-vendored C++ | docs.rs/meshopt | Verified 2026-09-16. New dependency — P1 carries it (pre-authorized by D2). |
| Points render mode — `depth_msaa_draw_points`, `render_scene.rs:4494` | just landed | Decimate output composes: fewer verts = sparser point cloud, zero extra work. |
| Vertices-stage splices demand equal capacity — content replacement only (`displace_copies` rejects unequal capacities, SCENE_MODIFIER_PROGRAMME.md section 2b (photoscan modifier slice)); wholesale different-sized replacement exists exactly once (Math View's `node.sample_triangle_grid`, per-target, calibrated by `mesh_frames`) | programme doc; `primitives/sample_triangle_grid.rs:28`; `scene_modifier_preset.rs:70-76` | Decimate keeps capacity = input capacity and carries the real count on a scalar wire (D9) — satisfies the splice constraint by construction. |
| Draw count is buffer-size-derived everywhere — `mesh_vertex_count` (`render_scene.rs:1702`), raster `:4145`, RT `triangle_count` `:5296`; no active-count channel into render_scene. The codebase's established answer to over-capacity buffers is a port-shadowed `active_count` scalar | `primitives/edges_from_mesh.rs:69-86` | **The seam D9 closes.** |

Classification: modifier machinery, targeting, endpoint, async precedent,
RT invalidation, flat-shading look — **exist**. The atom, its readback path,
the recipe, the static-vertices RT declaration — **genuinely new, small**.

## 2. Decisions

- **D1 — Decimate is one new CPU atom `node.mesh_decimate` plus a
  file-authored recipe `assets/scene-modifier-presets/Decimate.json` riding
  the `Vertices` endpoint.** Same machinery as Math View's vertex
  replacement; card rows, object/scene targeting, serialization, modulation
  addressing all come from the framework. No Rust descriptor registration.
  Rejected: import-time decimation (the gltf-transform pipeline already
  covers asset prep; it isn't performable, which is the point). Rejected:
  decimate params on `node.gltf_mesh_source` — wrong owner, kills
  composability (decimate-after-taper), breaks the modifier model.

- **D2 — CPU atom, `boundary_reason: NonGpu`.** Simplification is a global
  topology rewrite, not a barrier-free per-element op; the freeze-codegen
  fusable rule does not apply (same class as `node.atmosphere`). A GPU
  decimation kernel is not on the table. The `meshopt` crate dependency is
  pre-authorized by this decision (the section 4 escalation line would
  otherwise stop P1 cold).

- **D3 — Flat non-indexed in, flat non-indexed out.** Internally: weld
  (hash map over the 64-byte `MeshVertex` Pod bytes — bitwise, so UV/tangent
  seams never merge) → indexed positions into `meshopt::simplify` → expand
  result indices back to a flat triangle list, carrying attributes from the
  welded source verts. Output type is unchanged; nothing downstream knows.
  The weld is internal scratch — INDEXED_MESH_RENDERING stays closed; this
  does not revive R4. `target_error = 1.0` (effectively unlimited): ratio,
  not error, is the contract. Normals/UVs/tangents survive via the weld, so
  no tangent rebuild is needed.

- **D4 — Cache miss path: blit input → shared readback buffer, decimate off
  the content thread, blit result to the output, last-good output
  meanwhile.** Private graph buffers (audit) force one readback per miss;
  `gpu_readback.rs` and gltf_mesh_source's staging/`published` gates are the
  precedents. A miss spans frames: request → readback lands → worker job →
  publish. **No synchronous meshopt call on the content thread, ever** — a
  1M-tri simplify is 100–500ms; inline execution on a slider drag is a
  stage-visible hitch.

- **D5 — Async state machine mirrors `gltf_mesh_source` field-for-field:**
  `pending_job: Option<mpsc::Receiver<Result<Vec<MeshVertex>, String>>>`,
  `cached_out: Vec<MeshVertex>`, `staging`/`uploaded`/`published`/
  `copy_in_flight`, `content_version` bump on land. One job in flight per
  atom instance; a ratio change while a job runs queues nothing — the new
  ratio is picked up when the in-flight job lands (coalescing, not queuing).
  **Consequences, stated honestly:** fast ratio scrubbing resolves at job
  throughput (hundreds of ms per step on big scans), not per frame. That is
  the live-modulation answer — the "V3 async" we discussed collapses into
  this design from day one; there is no separate live phase.

- **D6 — Card params:** `enabled` (framework gate), `ratio` (f32 0.01–1.0,
  default 0.5 — fraction of triangles kept), `lock_border` (toggle, default
  on — maps to `SimplifyOptions::LockBorder`, keeps UV seams and open
  borders pinned). Cache key: `(input content_version, ratio quantized to
  1% steps, lock_border)`; single-entry last-good cache per instance.
  **Consequences, stated honestly:** one cached entry for a 1.4M-tri scan is
  ~270MB CPU (4.2M × 64B) plus the readback staging buffer — decimating
  several hero scans in one project is real memory, and the doc accepts it;
  multi-ratio LRU caches are Deferred (section 7).

- **D7 — Skinned and morph-driven targets are refused at attachment
  validation.** The validator identifies the target's vertex producer atom;
  `gltf_skinned_mesh_source` / morph-delta wiring ⇒ named refusal, same
  class as the existing `UnsupportedRenderMode` error. Decimation rewrites
  the vertex set, which invalidates joint weights and morph-delta
  correspondence; a remap-based treatment is Deferred (section 7).
  ⚠ VERIFY-AT-IMPL: how the compiler reaches a target's producer atom —
  read the `mesh_frames` / attachment path in `compiler.rs:595-740`.

- **D8 — RT support is a recipe declaration, `sceneModifier.staticVertices:
  true`, and it ships as its own phase (P3), not in P1.** Meaning: vertex
  output changes only on param edit, never per frame. The validator and the
  param lock (`scene_modifier_preset.rs:97-113`, `compiler.rs:96-107`) allow
  Vertices + `rt_enabled` only for declared recipes. Safety is structural:
  raster and RT read the same buffer (audit), and a landing decimate job
  bumps `content_version` → the accel content key (`render_scene.rs:2903`)
  → BLAS rebuild via the BUG-326 (rt-depth-snapshot-wrong-on-imported-glb-scenes) machinery. Undeclared vertices modifiers
  keep refusing RT.

- **D9 — The decimated vertex count travels on a scalar wire; buffers stay
  input-capacity.** render_scene derives draw counts from buffer byte size
  today (audit) and has no count channel, so the atom emits
  `active_count: ScalarF32` (the `edges_from_mesh.rs:69-86` precedent) and
  render_scene gains one optional `vertex_count` scalar input that, when
  wired, overrides the size-derived count in the raster draw (`:4145`),
  the RT `triangle_count` (`:5296`), and the topology/content key hashes
  (`:2622`, `:2859`). Unwired = today's behavior, byte-identical. The
  decimate output buffer keeps the input's capacity — the equal-capacity
  splice constraint (audit) is satisfied by construction, and a ratio scrub
  never triggers a graph re-plan. **Consequences, stated honestly:** the
  GPU buffer stays full-size regardless of ratio — the frame-time win
  (vertex processing, the measured geometry-bound bottleneck) is delivered;
  the GPU memory win is not, and exact-sized buffers are Deferred
  (section 7).

- **D10 — No voxel/point-sampling method param.** Quadric output serves
  Points mode fine (removals distribute over the surface); a spatially-even
  point-thinning mode is Deferred with its trigger.

## 3. Design body

### 3.1 The atom

```rust
// crates/manifold-renderer/src/node_graph/primitives/mesh_decimate.rs
crate::primitive! {
    name: MeshDecimate,
    type_id: "node.mesh_decimate",
    inputs: {
        in: Array(MeshVertex) required,
        ratio: ScalarF32 optional,        // port-shadowed, drives cache key
        lock_border: ScalarF32 optional,  // port-shadowed
    },
    outputs: {
        out: Array(MeshVertex),
        active_count: ScalarF32,  // real vertex count; wires to render_scene's `vertex_count` (D9)
    },
    params: [
        ratio: f32 in [0.01, 1.0], default 0.5,
        lock_border: Bool, default true,
    ],
    // boundary_reason: NonGpu (D2)
}
```

`run()`: resolve effective key; if key == cached key, return (INV-D3). On
miss with no job in flight: blit input → shared readback, spawn worker job
(readback bytes → weld → simplify → flatten → `Vec<MeshVertex>`), output
stays last-good. On job land: `cached_out` swap, staging blit, `published`
gate, `content_version += 1`. Output capacity equals the input capacity
(D9); the decimated mesh is written compacted in the prefix and
`active_count` carries the real count. render_scene's new optional
`vertex_count` input overrides the size-derived draw count where wired
(raster `:4145`, RT `:5296`, key hashes `:2622`/`:2859`); unwired is
byte-identical to today. ⚠ VERIFY-AT-IMPL: how the Vertices attachment
wires the stage's scalar outputs alongside `vertices` — read the
attachment path in `compiler.rs:595-740`; if a second endpoint is cleaner
than a paired wire, escalate with the two shapes rather than choosing
silently.

### 3.2 The recipe

`Decimate.json`, schema v3, shaped on `RenderMode.json`: `presetMetadata`
id `Decimate`, `sceneModifier` with `enabledParam: "enabled"`, one stage,
scope `EachObject`, targets `AllObjects` (the picker flips to `Explicit`
for object-level), stage output `{port: vertices, endpoint: Vertices}`.
Rows: `enabled`, `ratio`, `lock_border`. Gate wiring mirrors SceneFog
(`enabled` × pass-through), so bypass = byte-identical passthrough. The
stage also wires the atom's `active_count` output to render_scene's
`vertex_count` input alongside the `vertices` wire (D9).

### 3.3 The static-vertices declaration (P3)

`SceneModifierRecipe` gains `static_vertices: bool` (default false;
serialization default keeps old recipes valid). The param lock and the
expansion refusal both read it. Enforcement is the atom's behavior, not the
JSON — a declared recipe whose stage contains a per-frame vertex atom is a
bug the P3 gate test catches (RT + declared Decimate + ratio edit ⇒
observed BLAS rebuild, sane render).

## 4. Invariants & enforcement

- **INV-D1 — Bypassed or removed Decimate is byte-identical to no
  modifier.** Enforcement: headless parity test in the modifier test
  harness (same scene, modifier applied+bypassed vs absent, buffers
  compared) — the Render Mode INV-R1 test is the template.
- **INV-D2 — Output vertex count ≤ input count, monotonic non-increasing in
  ratio.** Enforcement: atom unit test over a synthetic flat mesh at ratios
  1.0/0.5/0.05.
- **INV-D3 — Unchanged inputs ⇒ no job, no readback, no allocation.**
  Enforcement: atom test running two frames with a fixed key, asserting
  zero spawns and zero blits (instrumented encoder mock).
- **INV-D4 — Skinned/morph targets refuse attachment with a named error.**
  Enforcement: validator test in `compiler.rs` tests (synthetic graph with a
  skinned source ⇒ `Err`, not silent pass).
- **INV-D5 — Weld never merges across attribute seams.** Enforcement: atom
  test — two verts sharing position but different UV stay distinct
  (synthetic bowtie mesh keeps both triangles).
- **INV-D6 — Undeclared vertices modifiers still lock RT.** Enforcement:
  the existing lock tests (`scene_modifier_preset.rs:1616` et al.) stay
  green, plus a new test: declared recipe passes, undeclared refuses.

## 5. Phasing

### P1 — The atom (vertical slice, headless)

The whole mechanism once, no card: weld → simplify → flatten, async job,
readback, cache.

- **Entry state:** `rg -n 'mesh_decimate' crates/` — zero hits. Re-verify
  anchors: `mesh_common.rs:35`, `gltf_mesh_source.rs:255-288`,
  `device.rs:362`, `gpu_readback.rs:126`.
- **Read-back:** this doc D1–D6; `gltf_mesh_source.rs` whole (the async
  machinery being mirrored); `taper_mesh.rs` whole (the atom family shape);
  ADDING_PRIMITIVES.md sections on CPU atoms.
- **Deliverables:** `primitives/mesh_decimate.rs`; `meshopt = "0.6"` in
  `manifold-renderer` (pre-authorized dependency, D2); render_scene's
  optional `vertex_count` input + the four override sites (D9); atom tests
  for INV-D2/D3/D5; a raw-executor headless test (mesh_snapshot.rs pattern)
  rendering a decimated cube → PNG.
- **Forbidden moves:** a synchronous simplify on the content thread ·
  changing the `MeshVertex` layout or adding an index buffer · a shared
  cross-instance cache (`Arc<Mutex>`) · resizing the output buffer below
  input capacity (D9 — capacity is compile-time, count is the wire) ·
  deriving draw count from anything but the wire when it is wired.
- **Gate:** `cargo nextest run -p manifold-renderer mesh_decimate` green;
  `MANIFOLD_RENDER_TRACE=1` ratio scrub on a 1M-tri mesh — no frame >20ms
  (the async claim, measured); INV gates green.
- **Round-trip gate:** none — no serialized surface in P1 (atom only).
- **Acceptance demo (L2):** headless PNG pair — scan at ratio 1.0 vs 0.05 —
  Peter looks; agent gate is the test exit codes.
- **Performer gesture:** ratio mapped to a fader, swept 1.0→0.05 while a
  clip plays (covered by the render-trace gate).
- **Test scope:** `-p manifold-renderer`.

### P2 — Recipe, card, targeting

- **Entry state:** P1 landed. Re-verify `RenderMode.json` shape and
  `compiler.rs:605`.
- **Read-back:** D1, D7; `RenderMode.json` whole; SCENE_RENDER_MODE_DESIGN.md
  section 3 (The recipe); the Vertices attachment path in
  `compiler.rs:595-740`.
- **Deliverables:** `assets/scene-modifier-presets/Decimate.json`; validator
  refusal for skinned/morph producers (INV-D4); modifier round-trip test.
- **Forbidden moves:** Rust descriptor registration · gating `ratio` through
  the enable math (bypass handles it) · per-object ratio overrides outside
  Explicit targeting.
- **Gate:** `cargo nextest run -p manifold-renderer scene_modifier` green;
  graph-tool `validate Decimate.json --kind generator` clean; round-trip
  gate — apply → save → reload → card present, ratio binding still
  modulates after reload.
- **Acceptance demo (L3):** `scripts/ui-flows/` flow — apply Decimate to a
  GLB scene, set ratio via the card row, assert the atom instance exists and
  the object redraws smaller (vertex-count query or region-mean probe);
  PNG for Peter.
- **Performer gesture:** apply mid-set from the picker on a playing scene.
- **Test scope:** `-p manifold-renderer`.

### P3 — RT compatibility (static-vertices declaration)

- **Entry state:** P2 landed. Re-verify `scene_modifier_preset.rs:97-113`,
  `compiler.rs:96-107`, `render_scene.rs:2903`.
- **Read-back:** D8; the f0c61b10c diff (the stale-BLAS regression fix);
  the `MeshTopologyHistory` path at `scene_object.rs:121-141`.
- **Deliverables:** `static_vertices` schema field (core) + validator/lock
  changes + recipe opt-in; INV-D6 tests; RT render test — Decimate applied
  under `rt_enabled`, ratio edit ⇒ accel content-key bump observed.
- **Forbidden moves:** lifting the lock for all vertices modifiers ·
  per-frame BLAS refits (topology rebuild on land only) · a runtime
  auto-detect of "static" (the declaration is the mechanism).
- **Gate:** `-p manifold-renderer` + `-p manifold-core` nextest green;
  `scripts/gpu_proofs_gate.py` green (RT path touched).
- **Acceptance demo (L2):** RT headless PNG of a decimated scan with
  correct shadows/reflections — Peter looks.
- **Performer gesture:** decimate an RT scan mid-set without dropping RT.
- **Test scope:** as gate.

## 6. Decided — do not reopen

1. Atom + file-authored recipe on the Vertices endpoint; no registration (D1).
2. CPU atom, flat-in flat-out, internal weld only (D2/D3).
3. Async job from day one; no synchronous content-thread simplify (D4/D5).
4. Ratio (not error) is the contract; `target_error = 1.0` (D3).
5. Skinned/morph refused at validation (D7).
6. RT via declared `staticVertices`, P3, undeclared modifiers stay locked (D8).
7. Count on a wire, capacity unchanged; render_scene's `vertex_count` input
   is the only count override (D9).

## 7. Deferred

- **Morph/skin decimation via weld-remap** — simplify's indices plus the
  weld map could resample morph deltas; genuinely fiddly, no show need
  named. Trigger: a skinned/morph scan Peter actually wants to decimate.
- **Multi-ratio result LRU** — single-entry cache today. Trigger: a set
  that toggles between two densities on one scan often enough that
  re-decimation latency is felt.
- **Voxel/spatial point-thinning method** — even-distribution thinning for
  pure point-cloud looks. Trigger: quadric removal distribution visibly
  bothers a Points-mode piece.
- **Absolute scene-wide triangle budget** (N tris shared across objects
  with an allocation policy) — ratio-per-object covers the need. Trigger:
  a show that must hold a hard frame budget across mixed scenes.
- **Exact-sized output buffers** (GPU memory scales with ratio, not just
  frame time) — requires runtime capacity changes the executor's
  compile-time plan doesn't have. Trigger: a project where decimated
  full-size buffers pin enough GPU memory to matter.
- **GPU-side progressive LOD** (meshlets, per-frame density) — a different
  subsystem (meshoptimizer meshlet path), priced only if live density
  becomes a headline effect.
