# Mathematical scene modifiers — programme and document map

<!-- index: Programme scope, milestones and ownership for file-defined mathematical scene modifiers across objects, instances, meshes and splats. -->

**Status:** IN PROGRESS · 2026-09-12 · Codex lead. Photoscan modifiers shipped; Peter reports successful visual/LFO play. Unified F1–F8 design approved for the next work, not implemented. Later creative families remain proposed.
**Document type:** Working guide. The contracts below own implementation decisions and phase gates.

Peter's direction: "displaying the 'beauty' of mathematics" and "presets just being jsonl files". The intended result is a live instrument: add a preset, combine it with another, perform its controls, open its graph, and save a variation. The existing file convention is a JSON document (`.json` or `.manifoldpreset`); this programme extends that convention rather than introducing JSONL records.

## 1. Read this first

| Document | Owns |
|---|---|
| [Preset architecture](SCENE_MODIFIER_PRESET_ARCHITECTURE.md) | Canonical data, stable addressing, attachment semantics, expansion, compatibility and performance integration |
| [Foundation implementation plan](SCENE_MODIFIER_FOUNDATION_PLAN.md) | Executable foundation phases, migration seams and the first end-to-end release |
| [Mathematical fields](SCENE_MATHEMATICAL_FIELDS_DESIGN.md) | Waves, interference, choreography, spatial masks and formation transitions |
| [Mesh assembly](SCENE_MESH_ASSEMBLY_DESIGN.md) | Vertex deformation, face motion, progressive reconstruction and matched-topology morphs |
| [Echoes and dynamics](SCENE_ECHO_DYNAMICS_DESIGN.md) | Analytic echoes, bounded recorded history and the simulation boundary |
| [Splat integration](SCENE_SPLAT_MODIFIERS_DESIGN.md) | Applying the common field vocabulary to the existing proposed Gaussian splat pipeline |
| [Validation contract](SCENE_MODIFIER_VALIDATION_PLAN.md) | Shared fixtures, numeric oracles, rendering checks, resource budgets and release criteria |

Read architecture and validation once, then the current milestone's contract and phase. Foundation receives full treatment; later contracts use DESIGN_DOC_STANDARD section 9 conformance treatment and explicitly require fresh source verification before implementation. Proposed signatures are normative design choices, not claims that APIs already exist.

## 2. Scope and value

| Milestone | Performer outcome | Included | Explicit boundary |
|---|---|---|---|
| M1: unified foundation | Keep current photoscan looks, duplicate/reorder them, edit/save/share presets and reopen mapped projects | Migrate Elastic Sculpture, Surface Peel, Vortex Fragments and Loop/Fog; repeated chainable instances; typed targets; file-only authoring proof | Static mesh/raster qualification first; preserve legacy source restrictions; no new simulation or universal renderer support |
| M2: mathematical motion | Dense polyhedral fields and separate scene objects move through coherent formations | Object and instance waves, interference, rings/helices, formation blending, moving masks | Rigid motion; no claim of high counts independent of mesh/material/RT cost |
| M3: mesh assembly | A scan ripples, breaks into triangular faces and reconstructs | Vertex weights, rigid triangle transforms, progressive return, compatible morphs | No automatic semantic segmentation, watertight fracture or unrelated-mesh correspondence |
| M4: echoes | Motion becomes spatial sculpture | Analytic phase echoes first; bounded recorded transforms second | Recorded deformed geometry and simulation history are separate costs |
| M5: splats | A splat scan opens into a cloud and returns | Existing splat importer/renderer plus common masks, displacement and assembly | Does not pretend the Gaussian splat renderer is already shipped |

Order: reconcile shipped examples → F1–F8 foundation → distinct photoscan behaviours. M2's reusable field/mask pieces are dependencies where needed, not a requirement to ship another cube collection before mesh work. Assembly, coherent slicing and analytic echoes are the next candidate behaviours; choose one bounded family at a time under section 2c. M5 still depends on the Gaussian source/renderer contract. Recorded history follows analytic echoes; stateful simulation is outside M1.

## 2a. Playable pilot before the foundation migration

**Scope amendment, 2026-09-11:** Peter wants useful examples before committing to the full migration, after the water-simulation work proved too complex to tune quickly. Build ordinary v2 generator presets first; this does not mark F1–F8 implemented. The pilot uses existing renderer and parameter machinery, not a second modifier framework.

The pilot has three wrappers (grid, ring and spiral copies) and one identical **Wave Motion** group. Its public boundary is `instances: Array<InstanceTransform>`, `reference_instances: Array<InstanceTransform>` and `phase: ScalarF32` → `instances: Array<InstanceTransform>`. Motion controls use stable preset bindings; camera, source layout, mesh, material and rendering stay outside the group. The current/reference split is mandatory even when both initially point at the same layout source.

Only missing reusable GPU operations may be added: copy-position extraction, a spatial sine field over points, and weighted copy displacement. The field is separate from its response. No hardcoded preset IDs in runtime/UI, custom shader strings inside the pilot presets, object-state writes, physics or v3 serialization changes. Source counts are fixed within prepared capacity; the first response changes position only. Rotational/scale responses follow only after the first looks are useful.

Upgrade path: wrap the same motion group as an EachObject/Instances stage in the future recipe, connect its current/reference/phase inputs through the attachment resolver, and carry its node IDs and outer parameter IDs through migration. The demonstration wrappers can remain valid generator presets. Tests pin the group boundary and equal group contents across wrappers, so the migration reuses the motion graph instead of extracting behaviour from a monolith. The future recipe schema may evolve after play-testing; reusable atom contracts and saved v2 presets must remain compatible.

Pilot acceptance: graph compilation, binding and save/reload checks; independent GPU numeric and standalone/fused parity; exact Amount=0 and inactive-copy preservation; no same-frame GPU-to-CPU field readback; bounded rendered previews of the three wrappers. Existing required landing checks still apply. Full modifier stacking, universal scan/object targets and RT performance qualification are not implied by this pilot.

Astra owns interfaces/review/landing. This pilot uses Luna workers at **high** effort at Peter's request, overriding the general low-effort worker default. Prepare briefs through `scripts/codex_prepare.py`; select checks through `scripts/codex_checks.py`, with the lead narrowing worker filters and preserving the landing gate.

**Pilot implementation:** `WaveGrid`, `WaveRing` and `WaveSpiral` are factory generators in `crates/manifold-renderer/assets/generator-presets/`, using the existing `.json` v2 format. They contain 1,000, 256 and 512 cube instances respectively. Play transport for beat-driven motion; Amplitude controls vertical displacement, Rate is cycles per beat, Frequency sets spatial repetition, Phase offsets the cycle, and Size changes copy scale. Camera Distance and Camera Tilt adjust the parked view. They use ordinary preset bindings, so existing modulation and persistence infrastructure applies. The outer `amplitude` ID denotes displacement in scene units; the reserved `amount`/`mix` IDs denote normalised wet/dry controls and are not used for it.

The atoms are `node.copy_positions` (XYZ extraction, homogeneous W=1), `node.wave_field_3d` (`sin(TAU * (dot(reference.xyz, direction) * frequency - fract(phase)))`, unnormalised direction coefficients), and `node.displace_copies` (weighted displacement added to current XYZ). Copy displacement rejects unequal input capacities during preparation and preserves scale, rotation and reflection markers; Amount=0 and inactive zero-scale records pass through exactly. Array values remain on the GPU. Group bindings address stable inner node IDs (`wave_field`, `wave_displace`); handle paths are presentation names, not node IDs.

Verification lives in the atoms' `wave_pilot` tests and `tests/wave_pilot_presets.rs`: GPU numeric reference, standalone/fused parity, identity preservation, group equality, capacity and binding contracts, graph compilation and JSON roundtrip. The factory validator passed all 78 presets. Bounded 1280×720 Metal previews were observed for each layout and corrected once for lighting/framing. These are still-image checks; interactive playback, full application project save/reopen and sustained performance are not yet visually qualified. The existing validator also reports the unrelated ApricotWeather card/default warning (BUG-1l7f).

The generator pilot uses the existing [static thumbnail contract](STATIC_THUMBNAILS_DESIGN.md) for factory previews. Scene modifier cards use the current text picker; this slice does not add a separate preview renderer.

## 2b. Photoscan modifier slice

**Scope amendment, 2026-09-11:** Peter rejected the cube demonstrations as insufficiently useful and requested modifiers on an **already loaded GLB scene**, using its textured surfaces and the full 3D space. Deliver Elastic Sculpture, Surface Peel and Vortex Fragments first. Spatial Slices and Surface Echoes remain deferred until these have been played and directed. No baked animation or simulation is included. A held control holds the geometry; Phase can be driven by the existing driver, LFO or audio machinery.

The existing descriptor registry, scene panel, `EditingService`, generic apply/remove commands and parameter surface remain authoritative. Each look is an ordinary v2 JSON graph in `assets/scene-modifier-presets/`, containing a group with `current` and `reference` MeshVertex inputs and a `vertices` output. A shared attachment builder clones that group before every connected `scene_object.vertices` in the selected scene, preserving source, material, textures, transform and instance connections. Stable nested node IDs identify attachment scopes; numeric document IDs only address wires inside a scope.

The core plan gains `MeshStageSplice { scope_path, target_node_id, stage, reference_source }` and `SceneModifierPlan.mesh_stages`. Editing preflights the entire batch before any graph or metadata mutation. Removal reconnects the stage's current input to its consumers, so removing the middle of a stack preserves later stages. Undo restores the graph and metadata snapshot. Applied groups and bindings serialize with the ordinary project graph; no new runtime or project version is introduced. One instance of each descriptor kind follows the current registry's singleton policy; arbitrary repeated instances belong to F1–F8.

Recipe parameters supply names, ranges and defaults. Top-level `node.value` controllers expose those controls; existing shared parameter bindings fan each value out to the cloned inner nodes at runtime. Every numeric primitive parameter has its scalar shadow input. This uses stable binding targets, not UI-only coupled writes or per-frame topology edits. The new `EnableDecl::Value` uses an ordinary Enabled row. Disabled atoms return current input attributes exactly, preserving prior modifiers.

| Recipe | Reusable operation | Primary gesture |
|---|---|---|
| Elastic Sculpture | Two perpendicular `node.wave_shear_mesh` stages | Sweep Phase while adjusting Bend and Cross Bend; surface and lighting normals follow the warp |
| Surface Peel | `node.transform_mesh_patches` | Lift and curl textured patches away, then return them by reducing displacement and rotation |
| Vortex Fragments | The same patch operation with orbital rotation and axial spread | Sweep Orbit and Rise through a spatial spiral, with Phase moving the response across the scan |

The new atoms implement a differentiable sine shear and a rigid transform of fixed spatial patches. The audit found existing bend/twist kernels leave tangents unchanged and do not apply the full normal Jacobian; existing shatter replaces authored normals even at zero. Extending those behaviours would change saved looks, so they remain unchanged. The shear transforms normals by the inverse-transpose Jacobian and tangents by the forward Jacobian. Patch motion rotates both. UVs and tangent handedness remain attached to the original faces. Both use generated GPU kernels; shear supports fusion, while reference triangle gathering has an explicit gather boundary and capacity checks.

Reference is the original pre-stack mesh producer; current is the actual upstream stage. Static GLB import expands triangles, bakes source transforms, recentres each material mesh and restores its position through `transform_3d`. Attachment captures that source offset and the imported bounds radius so all material groups share a scan reference frame. It does not infer scene scale from camera distance. Later object transforms remain outside the deformation. This qualification covers static photoscans; skinned/morphing sources and arbitrarily rewired transform graphs need separate reference-frame qualification.

Patch membership is the reference triangle centroid quantized into fixed cubes of 0.15 scene radii. Faces in one cell share a rigid pivot. Cell size is authored preparation data, not a performance control. This is spatial partitioning, not connected-surface segmentation: disconnected faces may share a cell, cuts have no caps, and there is no watertight fracture. Capacity never grows. GPU buffers are reused; there is no per-frame CPU mesh readback. Heavy scans still pay deformation bandwidth and ordinary raster cost.

**Rendering boundary:** `render_scene` currently defers ray-tracing acceleration rebuilds until vertex generations settle (`render_scene.rs`, content-key handling). Continuous deformation therefore does not have correct dynamic ray-traced geometry. Qualify this slice with raster rendering; do not silently switch users' rendering settings or claim dynamic RT support. Dynamic BLAS/refit work is a separate tracked task.

**Migration:** the saved groups are the reusable implementation. The descriptor attachment builder is an intentional interim adapter, retired when the shared EachObject/Vertices resolver lands (earlier “EachMesh” wording meant this mesh attachment). Migration adopts the same current/reference boundary, graph content and control identities. It must recognise saved adapter groups and lift losslessly representable instances without changing output; custom/ambiguous stacks remain ordinary graphs with diagnostics. F1–F8's canonical stack and expansion schema are not implemented by this slice.

Acceptance uses production import → production plan builder → real editing command on the mushroom photoscan, with a botanical held-out asset when practical. Tests cover nested multi-material targeting, atomic refusal, stacking and middle removal, undo/redo, serialization and binding cleanup; GPU tests cover independent numeric oracles, normal derivatives, exact bypass, phase periodicity, rigid patches and fusion classification/parity. Bounded renders must show retained photographic texture and visible response to Phase. The existing UI path must expose actionable controls. Required landing checks remain in force; visual and interactive qualification levels are reported separately.

**Observed qualification (2026-09-11):** Nine Metal GPU proofs passed, including a two-stage fused shear comparison. Five plan tests passed, including production mushroom import, applied-graph compilation and serialization, nested multi-material stacking and inverse removal. Four editing roundtrip tests passed. The 34-step GLB UI flow passed add/remove, Phase scrubbing and undo on the botanical fixture (L3 controls). Production raster renders of the mushroom retained photo textures and changed at Phase 0, 0.25 and 0.5 for all three modifiers (L2 visuals); Vortex Rise defaults to 0.16 after a framing correction. The UI fixture has no playing clip, so its black viewport is not a visual rendering proof. Full project reopen with live driver/LFO/audio and sustained timing/memory remain BUG-e3p6.5; dynamic RT remains BUG-e3p6.4. See the [landing report](landings/2026-09-11-photoscan-modifiers.md).

## 2c. September 12 direction and creative admission

Peter: "These need to look relatively unique in their style and behaviour." Also: "it looked like the LFOs worked when I played with it". Record that as user-observed LFO play alongside his positive visual review. It does not establish audio modulation, saved mapping recovery or measured performance. The next bounded qualification closes those gaps rather than repeating the basic slider demonstration.

The existing three looks are reference cases for the unified migration. Preserve their names, defaults, control identities, photographic surfaces and saved modulation. Surface Peel and Vortex Fragments share one patch-motion operation; retain both during migration. A later curation decision may present them as presets of one behaviour family, but must not rename saved instances, merge their mappings or remove access to either look. No serialized family hierarchy or universal mode switch is required for M1.

**A new preset is a variation; a new prominent modifier needs a distinct behaviour.** Reusing GPU atoms is encouraged. A changed seed, waveform, mask pattern or colour does not by itself justify another tool. Before authoring a new family, its brief names the surface action, signature live gesture, neutral/return endpoint, nearest existing look, visible difference, supported source/render modes and capacity cost. Compare on the same photoscan, lighting and camera using one bounded gesture sequence. Mathematical correctness is automatic; artistic distinction is observed and ultimately judged by Peter. An unconvincing result stays an experimental preset rather than multiplying stock cards.

| Behaviour family | Recognisable action | Boundary from current looks |
|---|---|---|
| Continuous deformation (shipped) | Connected textured surface bends without breaking into patches | Elastic Sculpture is the reference |
| Rigid patch motion (shipped) | Textured pieces lift, curl and orbit | Peel/Vortex are related presets, not proof of two unrelated algorithms |
| Progressive assembly (candidate) | Ordered pieces converge and lock into the original scan as Progress advances | Staged arrival/reconstruction, not just reversing a global separation amount |
| Coherent slicing (candidate) | Stable sections translate or fan as recognisable layers | Coherent sections, not fixed spatial patch noise; caps/booleans remain separate |
| Analytic echoes (candidate) | Simultaneous phase samples build a spatial sculpture | Multiple visible poses and bounded copies; no claim to record deformed history |

M1 proves file-only authorship using existing operations. That proof may remain a private conformance preset; it need not become another near-duplicate factory card. Later family work adds missing reusable operations only after the primitive audit. Shared fields/weights and representation-specific responses are the direction; do not rewrite the working photoscan atoms just to force a universal kernel or alter old appearances.

## 3. What gets shared

The reusable composition is **reference coordinates → field → weight → response**. A wave may drive object rotation, instance elevation, vertex displacement or splat opacity. The mathematics is shared; data-specific readers and writers preserve each representation's rules. A mesh face is a triangle group, not an independent mesh vertex; a splat also has orientation, anisotropic extent and opacity.

Source geometry remains available as a reference. Current upstream data supplies animation and previous modifier results. The distinction enables reversible gestures while preserving ordinary scene animation. Static photoscans require no special motion engine. A scan imported as one object cannot acquire meaningful chair/wall/body-part identities without segmentation.

## 4. Ownership and boundaries

The lead owns architecture, diagnosis, proof review and landing. Use Luna **high effort** for bounded implementation lanes, following Peter's usage preference for this work. Prepare briefs with repository tooling; each names owned paths, fixed interfaces, reuse targets, acceptance and exact checks. Workers neither delegate nor land. Schema, expansion and binding dependencies run in order; parallelise independent fixture/catalog work only once interfaces are pinned. Avoid two lanes editing the same seam, repeated broad checks or speculative design. Land each coherent phase before widening scope.

The documents alone do not authorise the entire programme. Peter has approved the unified direction and requested these documents as the foundation for the next bulk of work. This turn changes documentation only. The next implementation follows F0/F1–F8; later geometry families remain separately bounded. Use the slot ring, focused checks and `scripts/land_branch.py` rather than creating another planning document.

Work is measured in bounded work packages, not promised calendar dates. The foundation has an F0 baseline package followed by eight implementation phases F1–F8; F4–F6 activate together. Fields have four later phases, mesh assembly four, echoes three and splat integration three, in addition to the existing splat renderer's phases. Split an oversized package at its named seam before dispatch. The critical path is composition, migration and binding correctness, followed by GPU validation.

## 5. Relationship to existing contracts

- [Scene Modifier Framework](SCENE_MODIFIER_FRAMEWORK_DESIGN.md) governs the currently shipped descriptor system. The new architecture proposes replacing its kind registry and derived singleton list, not the underlying graph renderer. It remains authoritative until the migration lands.
- [Scene Loop](SCENE_LOOP_DESIGN.md) and [Endless Corridor](SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md) retain their geometry, timing, capacity and seam invariants.
- [Scene Mirror](SCENE_MIRROR_DESIGN.md) has a historical status/source discrepancy: its header says shipped, while the September 10 registry audit found Loop/Fog only; the photoscan slice has since added three kinds. `node.reflect_array` exists. F4 source verification resolves Mirror registration/history before migration; never infer another active kind from its document alone.
- [Merge](MERGE_MODIFIER_DESIGN.md) owns surface merging. This programme does not substitute ordinary deformation for boolean or field-based surface merging.
- [Mesh Deform](archive/MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md) owns existing atom behaviour. Extend useful atoms; do not replace them with named-look monoliths.
- [Gaussian Splats](GAUSSIAN_SPLATS_DESIGN.md) retains source/render/sort ownership. The companion here owns modifier integration only.

Tracking: existing bead `BUG-e3p6` owns the user-authored modifier programme. This proposal replaces its earlier suggestion of permanent Rust plan-builder escape hatches only when accepted and migrated; the old implementation remains current until then.

## 6. Completion

M1 is complete only when the photoscan journey passes the foundation gates, all five shipped modifier kinds preserve saved behaviour and mappings, repeat/reorder/retarget work, and an independently authored preset loads through the real picker without new Rust registration. F6 deletes the old runtime mechanism. Numeric checks establish correctness; observed gestures and Peter's review establish creative value. Dynamic RT and later assembly/echo/splat work do not silently become M1 exit requirements.

Later directions remain visible in their contracts and beads. There is no requirement to build the full vocabulary before releasing M1, and no licence to call the whole programme shipped when M1 lands.
