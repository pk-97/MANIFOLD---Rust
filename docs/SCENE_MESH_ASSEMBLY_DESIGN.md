# Mesh assembly — deform, separate faces and return to form

<!-- index: Reference geometry, vertex and triangle-face targets, progressive assembly, normal/tangent handling and matched-mesh morph contracts. -->

**Status:** PROPOSED · 2026-09-10 · Codex lead · not implemented.
**Prerequisites:** Foundation F8 and fields W1–W3. Existing mesh deformers remain authoritative.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8; conformance treatment, with fresh source/schema verification before each phase.

Peter asks for vertices and faces to be "warped and distorted or split apart or morphed into their final mesh creatively". **The first assembly operation returns elements of one source mesh to their own reference geometry.** This gives reliable reconstruction without solving correspondence between unrelated models.

Shared gates and resource limits: [Validation](SCENE_MODIFIER_VALIDATION_PLAN.md).

## 1. Audit — verified 2026-09-10

| Piece | Source under `crates/manifold-renderer/src` | Finding |
|---|---|---|
| Mesh layout | `generators/mesh_common.rs:26` | Position/normal/UV/tangent, 64-byte vertex; preserve attributes |
| Deformer inventory | `node_graph/scene_vm.rs:68` | Bend/twist/taper/ripple/fold/shatter/slice/melt and others already curated |
| Shatter | `node_graph/primitives/shatter_mesh.rs:1` | Triangle-list face-normal displacement, hash per triangle; not rigid rotating solid fragments |
| Morph | `node_graph/primitives/morph_mesh.rs:1` | Index correspondence and approximate blended normals; not an unrelated-shape matcher |
| Growth mask | `node_graph/primitives/mesh_ramp.rs:51` | Vertex spatial weights already exist |
| Flat normals | `node_graph/primitives/facet_normals.rs:41` | Triangle-list normal recomputation; unsuitable as a hidden replacement for smooth scan normals |
| RT invalidation | `node_graph/primitives/render_scene.rs:1018`, `:1519` | Geometry/content/topology keys and refit eligibility already owned by renderer |

Read the actual shader and capacity method of every reused deformer. Some purpose/comments reflect older limitations; current code and focused proofs decide. No blanket normal-policy change to existing effects in this programme.

## 2. Decisions

- **D1 — Three distinct targets.** Vertex changes deform surfaces; face changes apply a common transform to each triangle's three corners; object changes move the whole object. Do not label per-vertex random displacement as face shattering.
- **D2 — Reference preserves every source attribute.** A cached immutable source/bind mesh is distinct from the pre-stack animated mesh. Default assembly destination is the current pre-stack mesh, so skinned/morph animation continues. A bind-pose destination is an explicit graph option, not silently captured from the first frame encountered.
- **D3 — Deterministic Progress.** In the explicit reconstruction preset, Progress 0 is the scattered arrangement and Progress 1 returns exact reference vertices, normals, tangents and UVs. Ordinary deformation presets return Current when their offset reaches zero. Amount/Enabled bypass always returns Current, preserving upstream modifiers. No integrated velocity required.
- **D4 — Face identity follows source topology.** Triangle ID is stable only while topology/order stays unchanged. Topology replacement is a structural rebuild that resets correspondence and temporal history explicitly.
- **D5 — Existing render paths own shading and acceleration.** Modified geometry must feed raster/depth/shadow/RT consistently. No raster-only vertex shader deformation hidden from RT. Nonlinear deformations update normals/tangents using an explicit policy.
- **D6 — Matched topology only for reliable morph.** New preset validation requires equal counts and declared source correspondence. Two unrelated scans with equal counts are not necessarily matched. General remeshing/correspondence and watertight fracture are separate projects.

## 3. Geometry contract

Use existing `Array<MeshVertex>` and weights. A face operation dispatches per triangle or gathers its three corners, but emits the same flat triangle-list layout. A source using indices must pass through the existing supported triangle-list conversion before this path; M3 does not change scene index-buffer ownership. Source triangle count and corner order are validated at preparation.

Proposed seams (⚠ VERIFY-AT-IMPL reuse existing equivalent operations first):

| Operation | Inputs → output | Exact meaning |
|---|---|---|
| Face centres | MeshVertex → POSITION per triangle | Arithmetic mean of three source positions |
| Face weight expansion | one weight per triangle → one per corner | Repeats a face weight exactly three times |
| Rigid face response | Current mesh + Reference mesh + per-face offset/rotation/weight → mesh | All corners share reference-centred rigid motion |
| Reference reconstruction | Current + Reference + weights → mesh | Blend defined fields, branch to Reference exactly at full reconstruction |

Prefer existing `shatter_mesh` for its actual face-normal explosion. Add a separate rigid-face operation only for rotation/centred motion; do not change existing Shatter's output normals or presets to make it fit. Face-centre sampling serves both assembly ordering and wave-driven face articulation, satisfying reuse.

For reference face centroid c and source corner v, scattered corner is `R_i*(v-c) + c + offset_i`. Interpolate rotation from identity to authored rotation using a normalised quaternion internally, even though object/copy storage may use Euler. Face index + stable object key + seed determines offset/axis. Apply a single progress value per face to maintain rigidity. Per-vertex progress belongs to the separate deforming variant.

Progress delay: for delay d_i in [0,spread], local progress is `clamp((progress-d_i)/(1-spread),0,1)`, with spread restricted to [0,0.95]. At global Progress=1 force exact reference for every element; at 0 use exact start. Ease the local value in the graph. This creates assembly that travels through the model rather than all fragments moving simultaneously.

In a stack, displacement is relative to Reference and added/applied to Current. The explicit "Reconstruct Source" preset intentionally blends all the way to Reference, replacing upstream geometric variation at Progress=1; its name and graph make that endpoint clear. A normal deformation preset does not silently erase preceding stages.

## 4. Shading, bounds and animation

Rigid face motion rotates source normals/tangents by the same rotation, preserving UVs and tangent handedness. Degenerate triangle: translation is allowed; orientation from its undefined normal is identity and is flagged by a counted fixture assertion. No NaNs. Zero Amount returns the unchanged source record including smooth normals; switching all normals to faceted at zero is not identity.

For nonlinear vertex deformation, choose either analytic Jacobian normal/tangent transport or an explicit normal-recompute stage appropriate to the source's smoothing policy. Nonuniform scale uses inverse-transpose rules; tangents are re-orthogonalised. A singular deformation is not advertised as physically faithful lighting. The first preset bounds Amount to avoid singularities and records that range in metadata.

Animation order: source decoding → skin/morph evaluation → existing object-local deform stack → scene-modifier vertex stages → scene_object binding → renderer. Verify the actual graph order against the current importer before implementation. A separate bind reference must use the import source output rather than sampling a GPU buffer on the CPU.

Geometry buffer generations, conservative changed bounds and topology keys must reach existing RT update policy. Fixed topology can use supported refit; topology changes follow existing rebuild rules. Motion-vector history receives the prior deformed geometry/transform via existing lifecycle. If the path lacks that information, record a renderer gap and withhold temporal-effect support for the preset until fixed; do not silently emit static velocities.

## 5. Cards and presets

First presets: **Face Bloom** (triangles rotate and spread), **Assemble** (scattered source returns), **Surface Wave** (vertex weights), **Folded Scan** (existing bend/fold composition). Typical card: Progress or Amount, Spread, Distance, Rotation and Seed; mathematical direction stays a graph control unless needed live. A source mesh can be a photoscan or procedural primitive. High-poly scans pay per-vertex work and dynamic RT costs; preparation reports counts.

Matched Morph is a separate two-input authoring recipe with a clear compatibility error. It can connect two variants generated from the same topology. It must not reuse `morph_mesh`'s min-count truncation as validation: validate correspondence/counts upstream and test the endpoint. Preserve the existing primitive's legacy behaviour for other graphs.

## 6. Invariants & enforcement

| ID | Invariant | Planned check |
|---|---|---|
| M1 | Exact original attributes at return endpoint | `scene_modifier_mesh_return_exact` |
| M2 | Rigid faces keep edge lengths | `scene_modifier_face_rigidity` |
| M3 | Degenerate faces finite; smooth normals survive bypass | `scene_modifier_face_degenerate` |
| M4 | No count/correspondence guess | `scene_modifier_morph_correspondence` |
| M5 | Animated source and RT see current deformation | `scene_modifier_mesh_render_paths` |
| M6 | Weights/delays reach all elements at Progress=1 | `scene_modifier_assembly_order` |

V3 numeric tolerance; face edge-length relative error ≤1e-5 outside exact zero-length edges; normals unit-length error ≤1e-5 for nondegenerate data. Return endpoint compares all vertex bytes. Tests include UV seam, negative scale, nonuniform object scale and two stacked deformers.

## 7. Phasing

All phases read back D1–D6 and refresh topology/animation/RT anchors first. They inherit the validation contract's bounded execution and absolute manifest-path convention.

**M1 — reference and weights.** Entry: W3 and exact current importer order. Deliver reference selection, field-to-vertex/face weights and topology admission; `scene_modifier_assembly_order`/correspondence tests. Gate: focused CPU mesh-admission tests and GPU `scene_modifier_mesh_weights`; renderer clippy. Demo: sampled positions/weights — L1. Forbidden: first-frame pose capture, guessed topology equivalence.

**M2 — rigid faces and reconstruction.** Entry: M1. Deliver centres/rigid response if absent, FaceBloom/Assemble JSON, exact return branch and M1–M3 invariants. Gate: GPU `scene_modifier_face` and `scene_modifier_mesh_return`; check-presets. Demo/gesture: sweep Progress 0→1 on a textured scan, save/reload, repeat — L3 target. Forbidden: random displacement per corner, replacing smooth source normals at bypass.

**M3 — deformation composition and matched morph.** Entry: M2. Deliver SurfaceWave/FoldedScan presets using existing atoms; validated paired source for MatchedMorph; selected normal/tangent transport proof. Gate: GPU `scene_modifier_mesh_deform` and `scene_modifier_morph_correspondence`; focused renderer clippy. Demo/gesture: animate a fold then release to exact source — L3 target. Forbidden: new generic remesher, collider fracture or arbitrary scan matching.

**M4 — render and performance qualification.** Entry: M3; current raster/RT/history keys verified. Deliver M5 proof, conservative bounds/update fixes only where this geometry requires them, held-out scan run and budget table. Gate: GPU `scene_modifier_mesh_render_paths`; V8 trace for 250k-triangle fixture and an independently reported RT configuration; required landing gate. Demo: raster/shadow/RT comparison artifacts and existing flow — L3 target. Forbidden: claiming RT support from raster images or disabling temporal paths without a visible supported-mode contract.

## 8. Decided — do not reopen

Source-preserving reconstruction, distinct vertex/face targets, immutable references, exact endpoints, existing renderer ownership and explicit correspondence.

## 9. Deferred

Watertight chunks/interior surfaces, semantic segmentation, topology-changing remesh, unrelated-shape correspondence and volume/boolean merge. Revival requires a separate contract defining geometry ownership, correspondence, capacity and renderer update cost. [Merge](MERGE_MODIFIER_DESIGN.md) remains the owner of surface merging; this design does not duplicate it.
