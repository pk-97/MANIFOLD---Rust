# Merge — scene modifier that melts intersecting 3D objects into each other

**Status:** PROPOSED design, not built · 2026-09-06 · k3 (lead)
**Prerequisites:** none — the scene modifier framework ships (SCENE_MODIFIER_FRAMEWORK, `scene_loop` is kind #1).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md sections 5–6 (phase briefs, seam briefs) before starting any phase.

A scene modifier that, when two objects in a `render_scene` pass through each
other, replaces the hard clip with a smooth blended surface — the metaball-like
neck, the lumpy biological growth — re-shaded with an interpolated material.
Peter: *"is it possible to have an effect or 'scene modifier' that merges
objects together based on their mesh and materials if they pass through each
other? Like a 3D morph style thing… it looks like they distort and have this
weird biological almost looking merging and growth"*. On stage: you arm it, and
when the stone passes through the form mid-set, they melt.

Companion docs: SCENE_MODIFIER_FRAMEWORK (modifier shape — the card, the plan,
the toggle), DECOMPOSING_GENERATORS.md section 2.5 (audit by analogy) — the
audit this doc's section 1 completes, DEPTH_RELIGHT (nearest shipped
screen-space-depth feature), ADDING_PRIMITIVES.md (freeze-codegen path
requirement).

## 1. Audit — what exists (verified 2026-09-06)

Classification: exists / one wire away / genuinely new.

| Piece | Where | State |
|---|---|---|
| Modifier framework: descriptor = plan builder + trace + row whitelist + enable wiring; apply/remove are generic commands; modifier list never stored | `crates/manifold-renderer/src/node_graph/scene_modifier.rs:32`, module doc `:1` | exists — `merge` is kind #2, one descriptor file shaped like the loop |
| `render_scene` draws object groups into ONE shared depth; per-group depth-only passes already run for every shadow caster (main pass AND shadow pass, instanced) | `render_scene.rs:22`, `:58` | exists — per-group merge depths extend this machinery |
| `render_scene` `depth` output port, R32Float | `render_scene.rs:389`, `:3993`, `:4013` | exists |
| Screen-space depth-raymarch pattern precedent (consume depth + camera, emit a per-pixel term) | `heightfield_shadow.rs:38`, `ssao_gtao.rs:116` | exists — Merge's march is the same family, writing depth/normal/weights instead of a shadow term |
| `Texture3D` channel type + slice/gradient/blur atoms; u32 3D accumulator splat (`scatter_particles_3d`) | purposes surveyed in `crates/manifold-renderer/src/node_graph/primitives/` (3D blur, Texture3D slice, 6-tap 3D gradient) | exists — the SDF field texture is representable today |
| 3D simplex noise atoms (per-pixel z-slice; per-UV array sampler) | same survey | exists — the growth-noise vocabulary for a noisy blend radius |
| Material atoms (flat / phong / cook_torrance / cel) + `mix`/`mux` | NODE_CATALOG.md material family | exists — re-shade composes from shipped material vocabulary |
| glTF morph support | `gltf_morph_deltas_source.rs` | exists — the morph guard has real weights to read |
| smooth-min (smin) | — | 3 lines of math; lives **inline in the march atom**, not a primitive (DECOMPOSING_GENERATORS.md section 1.1 (no fused monoliths)) |
| Mesh → SDF bake | — | **genuinely new** (one atom, one dispatch) |
| Depth-seeded merge march atom | — | **genuinely new** (one atom, one dispatch, barrier-free per-pixel) |
| `merge` modifier kind descriptor | — | **genuinely new** (one Rust file, shaped exactly like `scene_modifier.rs`'s loop kind) |

Negative claims verified by survey this session: no smin/metaball/SDF-union
primitive exists; the RT path traces triangles through a per-object BLAS +
instance TLAS (`manifold-gpu/src/metal/raytrace.rs:67`), it does not raymarch
fields; the apricot and rosetta-stone fixtures carry no morph data (so V1's
rest-pose bake matches every asset Peter owns today).

## 2. Decisions

**D1 — Screen-space seeded SDF raymarch hybrid.** Render the scene normally
(full mesh fidelity where objects are separate); a merge pass finds pixels where
two groups' depths are within a threshold and marches `smin(sdf_A, sdf_B, k)`
seeded from that depth — a few steps from the surface, not a full-ray march.
Where the blended surface wins, replace depth/normal and re-shade. Cost is
bounded by contact: objects apart ≈ one depth comparison per pixel; objects
kissing = a handful of march steps in a small region.
Rejected: **full-scene SDF raymarch** — a parallel renderer next to the BVH
tracer, rounds off everything including the stone's sharp detail. **Pure
screen-space depth smooth-min** — cannot grow surface past either silhouette;
the outward neck is the entire point. **Vertex attraction** — no proximity
query vocabulary at dispatch granularity and no topology to form the neck.

**D2 — Scene modifier kind, not a layer effect.** The merge needs both meshes,
both materials, and scene depth *before* shading (goo pixels are re-shaded with
an interpolated material). A 2D layer effect receives finished RGBA — depth and
materials are gone. Peter: *"Just call it 'Merge' not goo merge."*

**D3 — Per-object SDF bake at rest pose, import-time, cached.** Compute
voxelization against the object's BVH at load; keyed by mesh content version;
128³ R16Float default (4 MB/object), resolution curated to the object's longest
axis. Rejected: **per-frame bake** (import-scale cost), **CPU bake**
(content-thread hitch; first-use must be prewarmed per the content-thread gate).

**D4 — Per-group depths via `render_scene` extension.** The modifier plan
stamps `merge_group_a` / `merge_group_b` (object-group indices) on the
`render_scene` node, which gains optional `merge_depth_a` / `merge_depth_b`
R32Float outputs — depth-only re-draws of just those groups, the same machinery
as the per-caster shadow passes (`render_scene.rs:58`). Rejected: a separate
depth prepass node drawing the groups again (duplicates transform/material
binding state, two sources of truth for object poses).

**D5 — March pass inside the `render_scene` graph, before final shading.**
New atom `sdf_merge_march`: inputs merge_depth_a/b, both SDF Texture3Ds, both
groups' transforms, camera, radius/noise/sharpness params; outputs blended
depth/normal + material blend weights. `render_scene` shades replaced pixels
with the interpolated material (extension of its shading path, D9). The atom is
barrier-free per-pixel → freeze codegen path mandatory, `wgsl_body` +
`fusion_kind`/`input_access`, pipeline from `standalone_for_spec::<Self>()`,
`gpu_tests` value parity vs a CPU smin reference.

**D6 — Noise rides the blend radius.** `radius` is modulated by 3D simplex
sampled in object-local space (existing noise atoms' vocabulary) — asymmetric
merge = growth character. One `noise_amount` / `noise_scale` pair on the card.

**D7 — Morph guard: toast and skip the pair.** Peter: *"let's not morph for
now, but if one is used that breaks raise a toast message for the user. Keep
the design and architecture open for it in the future and log a bead."* The
march atom checks morph weights on both merged groups; non-zero → user-visible
toast, merge passes through unmerged for that pair. The bake slot is
per-object and re-runnable so a later phase can re-bake without re-plumbing.
Tracked: BUG-nygh (morph-aware SDF re-bake).

**D8 — Modifier plan must be declarative-expressible.** Peter: *"it would be
really nice to have the option for users to create their own scene modifiers in
the future too… drag and drop json graphs between users."* Merge's plan is
static-splice-shaped (stamp two group params, add the march node, repoint
wires) — it must be buildable as a recipe, never assuming Rust-only kind
registration. Tracked: BUG-e3p6 (user-authored scene modifiers).

**D9 — Material crossfade via smin weights, not a post composite.** The smin
polynomial yields per-pixel blend weights for free; `render_scene` re-shades
goo pixels interpolating material params. Rejected: compositing the two
surfaces' shaded colors (double-shading, wrong occlusion, no single fused look).

## 3. Design body

**Data model.** All state lives in the existing graph runtime — no new shared
state, no new thread.

- `SdfBakeSlot` (per merged object group, renderer crate, owned by the
  render_scene node's plan resources): `{ group_index: u32, field: Texture3D
  (R16Float, N³), bounds: Aabb (object-local), mesh_version: u64 }`. Cache key
  = mesh content version; the slot is re-runnable (D7 door).
- New atoms (each one dispatch, `primitive!` + `composition_notes`):
  - `mesh_sdf_bake` — Array<MeshVertex> + transform → SdfBakeSlot's Texture3D.
    Voxelize (triangle splat into occupancy, the `scatter_particles_3d`
    pattern) then a jump-flood distance transform; ⚠ VERIFY-AT-IMPL: JFA needs
    rg32float position tags — if the ping-pong pass count/format breaks the
    barrier-free rule, the transform rides the WGSL escape hatch
    (DECOMPOSING_GENERATORS.md section 5 (the WGSL escape hatch)) with the
    coupling+formats justification written in `composition_notes`.
  - `sdf_merge_march` — the D5 pass. `wgsl_body` fragment, `input_access`
    per-texture, port-shadowed scalar params (radius, noise_amount,
    noise_scale, sharpness) per the authoring convention.
- `render_scene` extension: params `merge_group_a` / `merge_group_b` (int,
  default −1 = disabled); output ports `merge_depth_a` / `merge_depth_b`
  (R32Float, emitted only when the params select a valid group); shading path
  gains the material interpolation for pixels the march replaced.
- `merge` modifier descriptor (one file, `inventory::submit!`): `kind_id:
  "merge"`, display name "Merge", same slot group as Scene Loop. Row
  whitelist: Group A, Group B, Radius, Noise, Noise Scale, Sharpness, Material
  Crossfade. Enable wiring = the framework's D5 toggle (arm mid-set).

**Seams (committed).** Bake runs at gltf-load/schedule time on the content
thread (prewarmed — first march frame never waits on it). March runs per frame
as a graph node; hot-path discipline applies (scratch buffers as fields, no
per-frame allocs). Serialization: nothing new — the modifier is a graph delta
(D2 of the framework); the plan inverts for remove. UI: the card is free via
the whitelist, no bespoke rows.

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| `sdf_merge_march` is barrier-free per-element and freeze-fusable | `gpu_tests` value parity vs CPU-computed smin reference (ADDING_PRIMITIVES scope test); `graph_tool fusion` shows it folds |
| `mesh_sdf_bake` output matches a CPU distance reference on a held-out mesh | `gpu_tests` parity: sampled field vs CPU closest-point queries, bounded epsilon |
| Morph weights > 0 on a merged group never silently merge | march-atom unit test with a morph-weighted input → toast + pass-through asserted |
| No per-frame allocation in the march/bake steady state | existing hot-path audit / clippy; content-thread gate `MANIFOLD_RENDER_TRACE=1` any frame >20 ms fails |
| Modifier apply/remove round-trip leaves the graph byte-identical to pre-apply | round-trip gate on a fixture graph (save → apply → remove → compare) |
| Merge plan is declarative-expressible (D8) | plan builder contains no graph-state reads beyond the two stamped group indices — reviewed at landing; the declarative-kind host itself is BUG-e3p6's work, not this doc's |

## 5. Phasing

**P1 — vertical slice: one pair melts.** Bake atom + march atom + render_scene
per-group depths + `merge` kind + card, on a two-object scene (the
stone/apricot fixture). Gate: `gpu_tests` parity for both atoms; fusion shows
the march folding; headless render to PNG where the goo region visibly replaces
the hard clip vs a no-merge baseline (computed region-mean probe at named
coordinates — agents gate on the number, Peter looks at the PNG); `graph_tool
validate` + `fusion` clean. Demo: L2 (PNG Peter reads), flow driver targets L3
in the same phase if the modifier card is reachable. Held-out input: one GLB
not used during development.

**P2 — the growth character.** Noise-modulated radius (D6), material
crossfade (D9), Sharpness + Crossfade rows live. Gate: P1 gates re-run;
crossfade correctness via a two-material fixture asserting interpolated params
at named goo pixels; performer gesture — "sweep Radius with a fader while the
stone drifts through the form and watch the neck thicken."

**P3 — guard + hardening.** Morph toast (D7), disable-on-invalid-group
(apply-time applicability check greys the picker), bake cache eviction on mesh
version change. Gate: morph-guard test; round-trip gate; content-thread trace
on the canonical fixture.

Each phase: read-back first, forbidden moves per standard (no fuse-for-parity,
no silent pass-through, no new shared state), crate-scoped clippy +
nextest for `manifold-renderer`.

## 6. Decided — do not reopen

1. Hybrid screen-space seeded march (D1) — not full raymarch, not depth-blend-only.
2. Scene modifier kind named **Merge** (D2).
3. Rest-pose bake, 128³ R16Float default (D3) — morph re-bake is BUG-nygh's future.
4. Per-group depths as a render_scene extension (D4).
5. smin stays inline (audit finding).
6. Plan must be declarative-expressible (D8) — BUG-e3p6.

## 7. Deferred

- **Morph-driven merging** (re-bake on weight change) — trigger: a piece needs a
  morphed object in a Merge pair. BUG-nygh (morph-aware SDF re-bake).
- **User-authored modifier kinds / JSON drag-and-drop** — trigger: external
  authoring ecosystem decision. BUG-e3p6 (user-authored scene modifiers).
- **More than one merge pair per scene** (A–B and C–D) — march cost scales
  linearly; trigger: a show needs it.
- **Merging across separate `render_scene` nodes / layers** — needs composite
  depth before layer blend; trigger: a look that can't be built in one scene.
- **Deforming-mesh SDFs** (skinned objects in the pair) — same door as morphs.
