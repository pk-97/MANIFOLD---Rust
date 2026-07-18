# Indexed Mesh Rendering (R4) — kill the ~2.9× vertex amplification at the draw boundary

**Status:** APPROVED 2026-07-18 (Fable, with Peter in the room; K3 medium consulted on GPU
specifics). Execution authorized starting with P0. Supersedes the deferred R4 stub in
`RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md` §Deferred (which framed R4 as a graph-wide re-index;
this design rejects that framing — see D1). **P0 is a proof-of-concept gate that MUST pass (a real
multi-ms drop, measured back-to-back with the app closed) before P1+ proceed — if P0 fails, STOP
and surface, do not build the rest.**

**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before any phase.

## 1. Intake — what this does on stage

Peter's multi-scene photoscan shows (e.g. `MeshAudio.manifold`: three CC0 flower scans, 0.6–1.4M
tris each, composited live at 3840×2160@60) drop below 60fps and dip under 40. The measured
residual after AO-strip and the shipped shadow/IBL caching (BUG-189/197) is ~100% `render_scene`'s
main geometry pass, and it is **vertex-bound** on these assets (resolution-insensitive in
measurement: 4K and 1440p render the same). The instrument goal: a heavy imported scene holds 60fps
without the performer hand-decimating meshes before every gig — the geometry cost stops being the
thing that decides whether a look is usable live.

**Binding constraints (per DESIGN_AUTHORING §1):**
- **Hot path** — this is the per-frame render. Correctness of the shipped shadow/IBL caching and
  the freeze/executor invariants must not regress. No per-frame allocation.
- **GPU backend** — native Metal (`manifold-gpu`), hand-written; adds one draw variant.
- NOT persistence (no project-format change — see D1), NOT thread residency (mesh caching already
  lives where it lives), NOT time model, NOT a new performance-surface control.

## 2. Audit — what exists (verified 2026-07-18 against `cff8595e`)

| Piece | Where | State |
|---|---|---|
| De-indexing | `flatten_primitive`, `gltf_load.rs:552` | Reads indexed glTF, expands each triangle to 3 flat `MeshVertex`. Also **bakes the object world transform into positions** (`world_pos = M·pos`) and **computes a per-face normal when NORMAL is absent** (faceted). |
| Amplification | measured | 2.83× / 2.90× / 3.10× on the three flowers (unique verts vs flattened); ~3.84× on the AMG. |
| Draw path | `metal/encoder.rs:667,722` | `drawPrimitives(vertexStart, vertexCount, instanceCount)` — non-indexed only. Main pass (`draw_instanced_depth_msaa_batch`) and shadow pass (`draw_instanced_depth_only_batch`) both consume the flat buffers. |
| Mesh vertex | `mesh_common::MeshVertex` | Fat (~48–64B): position+pad, normal+pad, uv, … |
| Flat-list is load-bearing | `edges_from_mesh` (`(3t,3t+1,3t+2)` arithmetic), `facet_normals` (`base=3*(idx/3)`), `spawn_from_mesh` (one particle/vertex), `gltf_morph_deltas_source` (flat-aligned deltas) | 410 `MeshVertex` refs across primitives; many hardwired to the flat, non-indexed, 3-verts-per-triangle convention. |
| Mesh caching | `render_scene` draws persistent `ObjectDraw.vertices` buffers | Meshes are cached across frames, not rebuilt per frame — so an index can be derived once at cache time. |

**The audit's decisive finding:** the flat triangle-list layout is a **convention the whole mesh
primitive library depends on**, not merely render_scene's input. Any design that re-indexes the
graph representation must reconcile every one of those consumers — weeks of work, real regression
surface across effects that ship today.

## 3. Decisions

**D1 — Index at the render boundary; do NOT re-index the graph.** The graph's `Array<MeshVertex>`
flat convention stays exactly as-is. `render_scene` derives a **cached index buffer** from the flat
vertex buffer once, when the mesh is cached, and issues `drawIndexedPrimitives`. Every upstream
mesh primitive (edges_from_mesh, facet_normals, spawn_from_mesh, morph) is untouched; the change
localizes to `render_scene` + the Metal encoder.
*Rejected: the stub's graph-wide re-index (`Array<u32>` index port on every mesh node, or a
`Mesh{vertices, indices}` handle threaded through the graph).* Same perf win — the cost is at the
draw — for a fraction of the blast radius and none of the port-desync / codegen-compat risk. K3's
combined-handle recommendation (Q3) is correct *for the graph-wide framing*; D1 sidesteps the
question by not changing the graph.
*Named debt:* the importer still de-indexes and we rebuild the index it discarded — a one-time
de-dup per mesh, cached, not per frame. The "purer" fix (stop discarding at import) is the
graph-wide path we rejected; this is a deliberate, recorded tradeoff, not an accident.

**D2 — The de-dup key is the full `MeshVertex` (position + normal + uv), and that solves the
faceted-normal case for free.** Building the index = hash each flat vertex on all its attributes;
identical vertices collapse to one index, distinct ones don't. A faceted mesh (per-face normals,
NORMAL absent) has no two vertices identical in normal, so it simply doesn't collapse — it draws
with an identity-order index and full vertex count, shading unchanged. No "index-only-when-NORMAL"
branch is needed; K3's Q2 rule is enforced automatically by the key. Bit-exact rule: two vertices
merge iff every byte of their `MeshVertex` matches (bitwise, incl. pads zeroed), so no vertex the
shader could distinguish is ever merged.

**D3 — The index is built once at mesh-cache time and stored beside the vertex buffer.** Not per
frame (that would be a hot-path allocation and hash — forbidden). Cache key = the mesh's existing
identity/version. Camera animation doesn't invalidate it (the baked verts are stable; only the
camera uniform changes). A mesh whose baked vertices change (animated *object* transform, morph,
skin) invalidates the vertex cache already — the index rides the same invalidation.

**D4 — Keep transform baking where it is (in the cached vertices); do NOT move it to a shader
uniform in this design.** K3's Q1 recommendation (per-object uniform, immutable `.private` vertex
buffer) is sound and worth doing, but it is a *separate, larger* optimization that touches the
vertex shader and the object-transform path — out of scope for the localized perf win, and adopting
it here would reopen the blast radius D1 closed. Deferred with a named trigger: revisit if animated
object transforms (not camera) become common and the per-frame re-bake shows up in a profile.

**D5 — Add `drawIndexedPrimitives` alongside the non-indexed call; both passes use it; mixing is
fine.** Per K3's Q4: same encoder, same pipeline state, same depth attachment — indexed and
non-indexed draws coexist in one render pass, so the faceted fallback (identity index) and the
indexed meshes share a pass with no special handling. `indexBufferOffset` aligned to 16B (>4B API
minimum, some drivers faster). Index buffer is `.uint32`, passed per-draw (not a vertex slot).
**Both the main pass and the shadow pass must draw the same mesh with the same index** or silhouettes
diverge — the shadow pass gets the identical index buffer. Preserve glTF's original index order (no
reorder pass in v1; Apple's post-T&L cache handles decent order — photoscan order is fine).

## 4. Phases

**P0 — Proof-of-concept + measurement gate (STOP-if-fails).** Before any productionization: build
the index for the three flower meshes offline (or behind a dev flag), wire `drawIndexedPrimitives`
for the main pass only, and measure `render_scene` p50 with `cargo xtask perf-soak` **back-to-back,
app closed** (the session's contention lesson). Gate: a real multi-ms drop on the flowers at 4K
matching the ~2.9× vertex-share prediction. If the measured gain is not multi-ms, STOP and surface —
the vertex-bound inference was wrong and the full build isn't justified. This is the de-risking
Peter asked for; no estimate substitutes for it.

**P1 — Metal encoder: indexed draw variant.** Add `draw_indexed_*` alongside the existing calls in
`metal/encoder.rs`, with the alignment/ordering contract from D5. Unit-cover the encoder path.

**P2 — render_scene index cache.** Build + cache the index (D2/D3) beside each `ObjectDraw`'s vertex
buffer; issue indexed draws for both main and shadow passes (D5). Bit-exact de-dup (D2).

**P3 — Parity + perf gate.** GPU-readback parity: indexed vs non-indexed render must be
pixel-identical on a mesh with shared verts AND bit-identical on a faceted (no-NORMAL) mesh (proves
D2's fallback). Perf gate: the P0 number, now on the committed path, on all three flowers + the AMG.
Re-run the full `gpu-proofs` suite (touches the render path).

## 5. Alternatives considered and killed

- **Graph-wide re-index (the stub's framing).** Killed in D1 — blast radius across 410 mesh refs,
  port desync risk, no extra perf.
- **Shader-uniform transform (K3 Q1).** Deferred in D4 — larger scope, separate win.
- **Generate smooth normals so every mesh indexes.** Killed — silently changes faceted meshes'
  intended look (K3 Q2); D2 preserves them for free instead.
- **Reorder/tipsify index for post-T&L cache.** Deferred — glTF order is adequate on Apple GPUs;
  revisit only if P3 shows the cache underperforming.

## 6. Risks

- **P0 disproves the win.** Explicitly handled: P0 is a STOP gate, not a formality.
- **De-dup cost at cache time** — one-time per mesh; if it stalls a live import, it moves to the
  background mesh-load worker (where `gltf_mesh_source` already threads).
- **Shadow/main index divergence** — D5 mandates the same index for both; P3 parity catches it.
- **Debug-build validation** — Metal per-index validation can dominate debug frames on big meshes
  (K3 Q4); measurement is release-only, already our perf-soak default.
