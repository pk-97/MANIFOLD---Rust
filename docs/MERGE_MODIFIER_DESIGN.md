# Merge — scene modifier that melts intersecting 3D objects into each other

**Status:** PROPOSED design, not built · 2026-09-06 · k3 (lead) · revised 2026-09-06 after adversarial review (RT + lighting correctness in D5/D10) and again after Peter's edge-case rulings (pair-less all-instances semantics, D11–D14)
**Prerequisites:** none — the scene modifier framework ships (SCENE_MODIFIER_FRAMEWORK, `scene_loop` is kind #1).
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md sections 5–6 (phase briefs, seam briefs) before starting any phase.

A scene modifier that, whenever any two objects or instances in a `render_scene`
pass through each other, replaces the hard clip with a smooth blended surface —
the metaball-like neck, the lumpy biological growth — re-shaded with an
interpolated material, correctly lit including the RT path. Peter: *"is it
possible to have an effect or 'scene modifier' that merges objects together
based on their mesh and materials if they pass through each other? Like a 3D
morph style thing… it looks like they distort and have this weird biological
almost looking merging and growth"*. And: *"all of this must work with ray
tracing and correct lighting behaviour."* And: *"this is critical and the
standard use case. Must work for instances across any and all objects in the
scenes."* On stage: you arm it, and whatever passes through whatever melts.

Companion docs: SCENE_MODIFIER_FRAMEWORK (modifier shape — the card, the plan,
the toggle), DECOMPOSING_GENERATORS.md section 2.5 (audit by analogy) — the
audit this doc's section 1 completes, DEPTH_RELIGHT (nearest shipped
screen-space-depth feature), ADDING_PRIMITIVES.md (primitive authoring +
parity-test pattern), RAYTRACING_DESIGN.md (the RT pass chain the merge
orders itself into).

## 1. Audit — what exists (verified 2026-09-06)

Classification: exists / one wire away / genuinely new.

| Piece | Where | State |
|---|---|---|
| Modifier framework: descriptor = plan builder + trace + row whitelist + enable wiring; apply/remove are generic commands; modifier list never stored | `crates/manifold-renderer/src/node_graph/scene_modifier.rs:32`, module doc `:1` | exists — `merge` is kind #2, one descriptor file shaped like the loop |
| `render_scene` draws object groups into ONE shared depth; per-group depth-only passes already run for every shadow caster (main pass AND shadow pass, instanced) | `render_scene.rs:22`, `:58` | exists — the two-layer merge depth extends this machinery |
| `render_scene` `depth` output port, R32Float; single-sample depth snapshot (E2a) the RT shadow pass reconstructs origins from | `render_scene.rs:389`, `:991`, `:3993` | exists — the merge writes blended depth into this snapshot, making goo a correct shadow receiver |
| RT pass chain rides the raster G-buffer: `rt_enabled` forces `depth`+`velocity` into it | `render_scene.rs:3965` | exists — Merge orders itself before every RT consumer |
| RT shadow trace (half-res dispatch, TLAS, per-instance caster masks) | `manifold-gpu/src/metal/raytrace.rs:11`, `:275` | exists — its miss path is where goo casting hooks (D10); its per-instance masks are what transmissive goo excludes (D13) |
| Per-object BLAS (RT) — closest-point query target for the bake | `raytrace.rs:67` | exists — the bake walks it |
| Screen-space depth-raymarch pattern precedent (consume depth + camera, emit a per-pixel term) | `heightfield_shadow.rs:38`, `ssao_gtao.rs:116` | exists — Merge's march is the same family, writing G-buffer instead of a shadow term |
| `Texture3D` channel type + slice/gradient/blur atoms; 3D simplex noise atoms | purposes surveyed in `crates/manifold-renderer/src/node_graph/primitives/` | exists — SDF field texture + growth-noise vocabulary |
| Material atoms (flat / phong / cook_torrance / cel) + `mix`/`mux` | NODE_CATALOG.md material family | exists — re-shade composes from shipped material vocabulary |
| glTF morph support | `gltf_morph_deltas_source.rs` | exists — the morph guard has real weights to read |
| smooth-min (smin) | — | 3 lines of math; lives **inline in the march pass**, not a primitive (DECOMPOSING_GENERATORS.md section 1.1 (no fused monoliths)) |
| Mesh → SDF bake | — | **genuinely new** (one dispatch, BLAS closest-point walk) |
| Depth-seeded merge march pass | — | **genuinely new** (internal `render_scene` pass, one dispatch, barrier-free per-pixel) |
| Two-layer (front + second surface) depth with per-layer object/instance id | — | **genuinely new** (second depth-only geometry pass, existing draw machinery) |
| `merge` modifier kind descriptor | — | **genuinely new** (one Rust file, shaped exactly like `scene_modifier.rs`'s loop kind) |

Negative claims verified by survey this session: no smin/metaball/SDF-union
primitive exists; the RT path traces triangles through a per-object BLAS +
instance TLAS, it does not raymarch fields; the apricot and rosetta-stone
fixtures carry no morph data (so V1's rest-pose bake matches every asset
Peter owns today).

## 2. Decisions

**D1 — Screen-space seeded SDF raymarch hybrid, pair-less.** Render the scene
normally (full mesh fidelity where nothing merges). The merge pass then:
(1) reads the **two nearest surfaces** at each pixel from a two-layer depth
(`d1` front, `d2` second; each layer tagged with its object/instance id —
D11), unprojects both through the camera, and thresholds the **world-space**
3D gap — screen-space pixel gaps false-trigger at grazing angles and under
occlusion; (2) dilates the contact mask by the merge radius in pixels,
because the outward neck lives where only one layer has depth — the dilation
is what lets the goo grow past both silhouettes; (3) marches
`smin(sdf_i(d1), sdf_j(d2), k)` only inside the dilated mask, seeded on the
interval `[d_min − r, d_max]` — a few steps from the known surface, not a
full-ray march. Where the blended surface wins it replaces depth/normal in
the G-buffer and shading interpolates the material. Early-out when both
layers are the same instance (smin of an instance with itself is a no-op —
skip the march). Cost stays bounded by contact: nothing close ≈ the
unprojection comparison; contact = a handful of steps in a small screen
region.
Rejected: **full-scene SDF raymarch** — a parallel renderer next to the BVH
tracer, rounds off everything including the stone's sharp detail. **Pure
screen-space depth smooth-min** — cannot grow surface past either silhouette.
**Vertex attraction** — no proximity vocabulary at dispatch granularity and
no topology to form the neck. **Named-pair semantics** (v1 of this doc) —
cannot express instances of one object merging with each other; Peter's
standard case. Killed by D11.

**D2 — Scene modifier kind, not a layer effect.** The merge needs both
surfaces, both materials, and scene depth *before* shading. A 2D layer effect
receives finished RGBA — depth and materials are gone. Peter: *"Just call it
'Merge' not goo merge."*

**D3 — Per-object SDF bake: exact closest-point walk of the object's BLAS.** One
compute dispatch, one thread per voxel, closest-point-on-triangle against the
per-object BLAS the RT path already builds (`raytrace.rs:67`) — exact
distance, which is what lets the parity gate be a max epsilon. One field per
object **type** (instances share it; each instance contributes its own
transform at march time). Object-local bounds normalized to [0,1]³ so field
precision stays relative to the merge radius; 128³ R16Float default
(4 MB/object); keyed by mesh content version. When Merge is armed, every
scene object gets a field — bounded memory (~4 MB × object types), baked
prewarmed at schedule time so arming mid-set never hitches.
Rejected: **jump-flood distance transform** — approximate at concavities, and
photoscans are all concavity; it cannot pass this doc's max-epsilon gate.
**Per-frame bake** (import-scale cost) and **CPU bake** (content-thread
hitch).

**D4 — Two-layer merge depth as a `render_scene` extension.** With Merge
armed, `render_scene` runs a second depth-only geometry pass (same draw
machinery as the per-caster shadow passes, `render_scene.rs:58`) writing
`depth2` + per-layer object/instance ids. Rejected: per-group depth outputs
(pair semantics, D1's rejected path) and a separate prepass node (duplicates
transform/material binding state).

**D5 — The merge is an internal pass of `render_scene`, in a committed order.**
`render_scene` is one primitive whose internal pass chain is code — the march
is not a graph node. Order: raster G-buffer → two-layer depth (D4) → **merge
pass** (D1) writes blended depth/normal/material weights into the G-buffer in
place → **then every lighting consumer runs against the updated G-buffer**:
GTAO, the RT shadow trace (origin reconstructed from the E2a depth snapshot,
`render_scene.rs:991` — goo is a correct shadow *receiver*), RT reflections
and GI (rays originate from the blended position/normal — correct receiver
terms). Shading interpolates the material for replaced pixels and folds the
traced terms normally. Rejected: **a post-shading composite** — the goo would
glow unshadowed and un-occluded, the exact "lighting is broken" look.

**D6 — Noise rides the blend radius.** `radius` is modulated by 3D simplex
sampled in object-local space (existing noise atoms' vocabulary) — asymmetric
merge = growth character. One `noise_amount` / `noise_scale` pair on the
card.

**D7 — Morph guard: toast and skip.** Peter: *"let's not morph for now, but if
one is used that breaks raise a toast message for the user. Keep the design
and architecture open for it in the future and log a bead."* The merge pass
checks morph weights on every object with a baked field; non-zero →
user-visible toast **once per arming** (never per frame) and merge passes
through unmerged. The bake slot is per-object and re-runnable so a later
phase can re-bake without re-plumbing. ⚠ VERIFY-AT-IMPL: name the toast
channel at implementation — reuse the app's existing notification surface if
one exists; a new UI surface is a Peter call. Tracked: BUG-nygh (morph-aware
SDF re-bake).

**D8 — Modifier plan must be declarative-expressible.** Peter: *"it would be
really nice to have the option for users to create their own scene modifiers
in the future too… drag and drop json graphs between users."* Merge's plan is
static-splice-shaped (stamp the merge-enabled param + pass wiring, repoint
wires) — it must be buildable as a recipe, never assuming Rust-only kind
registration. Tracked: BUG-e3p6 (user-authored scene modifiers).

**D9 — Material crossfade via smin weights, not a post composite.** The smin
polynomial yields per-pixel blend weights for free; shading interpolates
material params for replaced pixels. Rejected: compositing the two surfaces'
shaded colors (double-shading, wrong occlusion, no single fused look).

**D10 — RT on is the shipped configuration; correct lighting on goo is
required, not approximate.** Peter: *"all of this must work with ray tracing
and correct lighting behaviour."* Two consequences, one named platform
boundary. (a) **Casting**: the goo neck must also cast shadows — the RT
shadow trace's miss path gains an optional SDF march over flagged merge
pixels (bounded: it runs only where the TLAS trace missed and the merge mask
is set). The raster PCF shadow path cannot do this (baked depth maps) —
documented v1 approximation: goo casts no shadow with RT off. (b) **Being
reflected**: goo appearing *inside other surfaces' reflections* requires an
implicit iso-surface in the TLAS; Metal exposes no custom intersection
programs (triangles only), so other objects' reflections keep showing the
original meshes inside the merge zone. Accepted boundary; revisit trigger in
Deferred.

**D11 — All-objects, all-instances semantics.** Peter: *"this is critical and
the standard use case. Must work for instances across any and all objects in
the scenes."* Merge has **no pair selection**: any two surfaces from any
objects or instances that come within `radius` of each other in world space
fuse. Instances of the same object merge with each other (smin of the same
field under two different instance transforms — the transforms are what make
it a real blend). This is what the two-layer depth (D4) exists for, and it
also deletes the v1 design's index-stamping problem (no group indices → no
drift). The card has no Group rows — arming Merge arms it for the scene.

**D12 — Camera-inside the goo is handled, not deferred.** When the flythrough
camera enters a merged volume, one depth layer is missing and seeded marching
would pop out of existence mid-crossing — Peter: *"yes important otherwise it
will look glitch and snap."* Guard: each frame, evaluate both objects' fields
at the **camera position** (two trilinear samples — trivial). If either is
within `radius` of the camera, the whole frame switches to a **union
march**: full `smin` march from the camera wherever the union surface is
nearer than `d1`, bounded to the same dilated-mask + radius logic. Costly
only during the crossing itself; seamless across it.

**D13 — Transmissive materials work, via trace-mask exclusion.** Peter: *"can
we get this working too?"* For a merged pair where the interpolated material
is transmissive, the refraction trace fires from the goo surface with the two
**source instances excluded from TLAS visibility** (the per-instance mask
machinery already exists, `raytrace.rs:275`) — light passes through the fused
neck as if the original walls weren't there, attenuated/tinted by the
interpolated material. Residual boundaries, same family as D10b: goo-goo
internal refraction and goo as a reflector stay out of scope (platform wall).
Cutout/alpha-test materials take the same path as opaque (the field is the
full mesh; cutout shaping inside the goo is accepted).

**D14 — No temporal smearing or ghosting on goo.** Peter: *"critical, no
smearing or ghosting please."* Goo pixels carry **zero velocity** and reset
temporal history (RT denoiser + MetalFX temporal both consume depth+velocity,
`render_scene.rs:3965`): the first goo frame renders clean rather than
accumulating from the pre-merge surface. Arming or disarming Merge mid-set
also resets temporal history once (same pop-avoidance as D12). Enforced by
the existing RT noise gate extended over goo pixels (frame-to-frame |delta|
ceiling on a paused static merged scene, `scripts/rt_noise_gate.py`).

## 3. Design body

**Data model.** All state lives in the existing graph runtime — no new shared
state, no new thread.

- `SdfBakeSlot` (per object type, renderer crate, owned by the render_scene
  node's plan resources): `{ object_type_id, field: Texture3D (R16Float, N³),
  bounds: Aabb (object-local, normalized to [0,1]³), mesh_version: u64 }`.
  Cache key = mesh content version; the slot is re-runnable (D7 door).
- `mesh_sdf_bake` — one compute dispatch (D3): Array<MeshVertex> →
  SdfBakeSlot's Texture3D, walking the object's BLAS. Ships with a `gpu_tests`
  max-epsilon parity check against CPU closest-point queries on a held-out
  mesh.
- `sdf_merge_march` — the D1/D5 pass as a **shared WGSL module invoked from
  `render_scene`'s internal pass chain** (not a registry atom; `render_scene`
  is in the render_* exempt class, ADDING_PRIMITIVES.md). One dispatch,
  barrier-free per-pixel; its body carries a GPU value-parity test against a
  CPU smin reference. Contains the camera-proximity union-march switch (D12).
- `render_scene` extension: param `merge_enabled` (bool, default false); the
  two-layer depth pass (D4); the merge pass writing blended depth/normal/
  material weights into the G-buffer in place (D5 order); material
  interpolation in shading; transmissive trace-mask exclusion when the
  interpolated material transmits (D13); zero-velocity + history reset for
  replaced pixels (D14).
- `ShadowRayTracer` extension (manifold-gpu, active when merge is on): miss
  path marches the flagged SDFs before declaring lit (D10a).
- `merge` modifier descriptor (one file, `inventory::submit!`): `kind_id:
  "merge"`, display name "Merge", same slot group as Scene Loop. Row
  whitelist: Radius, Noise, Noise Scale, Sharpness, Material Crossfade.
  Enable wiring = the framework's toggle (arm mid-set). The plan stamps
  `merge_enabled` + pass wiring — no object indices anywhere.

**Seams (committed).** Bake runs at gltf-load/schedule time on the content
thread (prewarmed — arming mid-set never hitches; export warmup rebuilds
fields before exported frame 1). The march pass and all lighting consumers
run per frame inside render_scene's existing pass chain; hot-path discipline
applies (scratch buffers as fields, no per-frame allocs). Serialization:
nothing new — the modifier is a graph delta (D2 of the framework); the plan
inverts for remove. UI: the card is free via the whitelist, no bespoke rows.

## 4. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| Rendered goo surface matches the analytic smin of two spheres | two-sphere fixture, rendered depth vs CPU-computed iso-surface, per-pixel bounded epsilon (the end-to-end gate — a region-mean probe alone only proves "something changed") |
| `mesh_sdf_bake` matches CPU closest-point on a held-out mesh | `gpu_tests` max-epsilon parity |
| `sdf_merge_march` math matches the CPU smin reference | `gpu_tests` value parity |
| Merge runs before all lighting consumers; goo receives correct shadows / GTAO / reflections / GI | headless two-object scene, sun behind the neck, RT on: shadow term at named goo pixels matches the CPU smin-geometry prediction (computed number, not a PNG read) |
| Goo casts correct shadows under RT | same fixture, light-side probe: pixels the neck must occlude read shadowed with RT on; with RT off the pass-through (non-casting) behaviour is asserted as the documented approximation |
| Instances of the same object merge with each other | loop fixture (K copies of one object, interpenetrating): rendered goo surface matches a CPU smin of the same field under two instance transforms |
| Camera crossing a merged volume does not pop | flythrough fixture: frames bracketing the crossing match the union-march reference within the temporal ceiling |
| No temporal smearing or ghosting on goo | `scripts/rt_noise_gate.py` extended over goo pixels: paused static merged scene, frame-to-frame delta within the committed ceiling |
| Morph weights > 0 never silently merge | march-pass unit test with a morph-weighted input → exactly one toast + pass-through per arming |
| No per-frame allocation in the march/bake steady state | content-thread gate `MANIFOLD_RENDER_TRACE=1`, any frame >20 ms fails |
| Modifier apply/remove round-trip leaves the graph byte-identical to pre-apply | round-trip gate on a fixture graph, including a scene with Scene Loop also applied (two modifiers, one render_scene) |
| Merge plan is declarative-expressible (D8) | plan builder contains no graph-state reads — reviewed at landing; the declarative-kind host itself is BUG-e3p6's work, not this doc's |

## 5. Phasing

**P1 — vertical slice: anything melts, correctly lit, RT on.** Bake + march +
two-layer depth + G-buffer merge ordering + RT shadow miss-path casting +
camera-crossing union march + temporal reset + `merge` kind + card, on a
fixture that is **Scene Loop + Merge on one render_scene** (K interpenetrating
copies — the standard case, D11), demonstrated with `rt_enabled: true`.
Gate: the seven computed-number gates from section 4 that P1 covers
(analytic spheres, bake parity, march parity, receiver + caster shadow terms,
same-instance loop merge, camera crossing, temporal ceiling); tooling clean;
headless PNG vs a no-merge baseline for the record (Peter reads it; agents
gate on the numbers). Demo: L2, flow driver targets L3 if the modifier card is
reachable. Held-out input: one GLB not used during development. Note: P1
touches `manifold-gpu` (shadow miss path) — landing gate covers both crates.

**P2 — the growth character + transmissive.** Noise-modulated radius (D6),
material crossfade (D9), Sharpness + Crossfade rows live; transmissive merge
(D13) with its trace-mask exclusion. Gate: P1 gates re-run; crossfade
correctness via a two-material fixture asserting interpolated params at named
goo pixels; transmissive fixture asserting the excluded-instance refraction
term at named pixels; performer gesture — "sweep Radius with a fader while
the stone drifts through the form and watch the neck thicken."

**P3 — guard + hardening.** Morph toast (D7), bake cache eviction on mesh
version change, RT-off documented-behaviour test, export-warmup bake rebuild.
Gate: morph-guard test; round-trip gate; content-thread trace on the canonical
fixture.

Each phase: read-back first, forbidden moves per standard (no fuse-for-parity,
no silent pass-through, no new shared state), crate-scoped clippy +
nextest for touched crates.

## 6. Decided — do not reopen

1. Hybrid screen-space seeded march (D1) — two nearest surfaces, world-space threshold, dilated mask, `[d_min − r, d_max]` seed interval.
2. Scene modifier kind named **Merge** (D2).
3. BLAS closest-point bake, 128³ R16Float (D3) — JFA rejected by name; morph re-bake is BUG-nygh's future.
4. Two-layer merge depth (D4); per-group depths rejected.
5. Merge is an internal render_scene pass before all lighting consumers (D5) — not a graph node, not post-shading.
6. Material crossfade via smin weights (D9).
7. RT required with correct lighting (D10): casting via the RT shadow miss-path march; raster-PCF non-casting and reflections-of-goo are the named boundaries.
8. Pair-less all-instances semantics (D11) — no group selection, no indices.
9. Camera-inside handled by the union-march switch (D12).
10. Transmissive via trace-mask exclusion (D13).
11. Zero velocity + history reset on goo; no smearing (D14).
12. Plan must be declarative-expressible (D8) — BUG-e3p6.

## 7. Deferred

- **Morph-driven merging** (re-bake on weight change) — trigger: a piece needs
  a morphed object in a Merge scene. BUG-nygh (morph-aware SDF re-bake).
- **User-authored modifier kinds / JSON drag-and-drop** — trigger: external
  authoring ecosystem decision. BUG-e3p6 (user-authored scene modifiers).
- **Goo inside other surfaces' reflections** (implicit iso-surface in the TLAS)
  — trigger: Metal gains custom intersection programs, or the Vulkan backend
  (docs/VULKAN_BACKEND_DESIGN.md) ships with intersection shader support.
- **Goo casting with RT off** (raster PCF shadow path) — trigger: a show runs
  merged scenes with RT off.
- **Goo-goo internal refraction** (transmissive neck refracting itself) —
  trigger: a look needs it; same platform wall as reflections-of-goo.
- **Cutout shaping inside goo** (alpha-test silhouette detail in the field) —
  accepted as full-mesh fields for now.
- **Deforming-mesh SDFs** (skinned objects) — same door as morphs.
