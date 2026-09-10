# Mathematical scene modifiers — programme and document map

<!-- index: Programme scope, milestones and ownership for file-defined mathematical scene modifiers across objects, instances, meshes and splats. -->

**Status:** PROPOSED · 2026-09-10 · Codex lead. Documentation complete for review; this programme's implementation has not started.
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
| M1: foundation and first look | Load a scene, add Scene Loop and a travelling wave, perform, edit, save and reload | File-defined Loop/Fog; repeated chainable modifiers; target selection; bindings; editor; one instance wave | Loop remains a singleton scene/camera source; no general multi-scene graph targeting |
| M2: mathematical motion | Dense polyhedral fields and separate scene objects move through coherent formations | Object and instance waves, interference, rings/helices, formation blending, moving masks | Rigid motion; no claim of high counts independent of mesh/material/RT cost |
| M3: mesh assembly | A scan ripples, breaks into triangular faces and reconstructs | Vertex weights, rigid triangle transforms, progressive return, compatible morphs | No automatic semantic segmentation, watertight fracture or unrelated-mesh correspondence |
| M4: echoes | Motion becomes spatial sculpture | Analytic phase echoes first; bounded recorded transforms second | Recorded deformed geometry and simulation history are separate costs |
| M5: splats | A splat scan opens into a cloud and returns | Existing splat importer/renderer plus common masks, displacement and assembly | Does not pretend the Gaussian splat renderer is already shipped |

Order: architecture → F1–F8 foundation → M2 → M3. M4 analytic can follow M2 independently of M3. M5 depends on the existing Gaussian Splats design as well as the common field contract. Recorded history follows analytic echoes. Stateful dynamics has its own design-entry gate; it is not an implicit extra in M1.

## 3. What gets shared

The reusable composition is **reference coordinates → field → weight → response**. A wave may drive object rotation, instance elevation, vertex displacement or splat opacity. The mathematics is shared; data-specific readers and writers preserve each representation's rules. A mesh face is a triangle group, not an independent mesh vertex; a splat also has orientation, anisotropic extent and opacity.

Source geometry remains available as a reference. Current upstream data supplies animation and previous modifier results. The distinction enables reversible gestures while preserving ordinary scene animation. Static photoscans require no special motion engine. A scan imported as one object cannot acquire meaningful chair/wall/body-part identities without segmentation.

## 4. Ownership and boundaries

The lead owns architecture, diagnosis, proof review and landing. Astra is the intended lead for this programme. Luna low-effort lanes receive independent mechanical scopes only after the lead has pinned the seam and exact checks. Workers neither delegate nor land. The first wave is intentionally sequential around core schema, expansion and binding changes; parallelise fixtures and isolated atoms only when those interfaces are settled.

No application implementation is authorised merely by the presence of these proposed documents. The current task delivers documents. Future implementation follows the agreed milestone scope, the slot ring, focused checks and `scripts/land_branch.py`.

Work is measured in bounded phase sessions, not promised calendar dates. Foundation has eight phases, fields four, mesh assembly four, echoes three and splat integration three, in addition to the existing splat renderer's phases. These are work packages, not guarantees that every package completes in one attempt; split any phase at the named boundary before dispatch if fresh inventory exceeds one session. The critical path is composition, migration and binding correctness, followed by GPU validation.

## 5. Relationship to existing contracts

- [Scene Modifier Framework](SCENE_MODIFIER_FRAMEWORK_DESIGN.md) governs the currently shipped descriptor system. The new architecture proposes replacing its kind registry and derived singleton list, not the underlying graph renderer. It remains authoritative until the migration lands.
- [Scene Loop](SCENE_LOOP_DESIGN.md) and [Endless Corridor](SCENE_LOOP_ENDLESS_CORRIDOR_DESIGN.md) retain their geometry, timing, capacity and seam invariants.
- [Scene Mirror](SCENE_MIRROR_DESIGN.md) has a status/source discrepancy: its header says shipped, while the audited registry submits Loop and Fog only. `node.reflect_array` exists. F0 source verification resolves registration/history before migration; never silently infer a third active kind from the document.
- [Merge](MERGE_MODIFIER_DESIGN.md) owns surface merging. This programme does not substitute ordinary deformation for boolean or field-based surface merging.
- [Mesh Deform](archive/MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md) owns existing atom behaviour. Extend useful atoms; do not replace them with named-look monoliths.
- [Gaussian Splats](GAUSSIAN_SPLATS_DESIGN.md) retains source/render/sort ownership. The companion here owns modifier integration only.

Tracking: existing bead `BUG-e3p6` owns the user-authored modifier programme. This proposal replaces its earlier suggestion of permanent Rust plan-builder escape hatches only when accepted and migrated; the old implementation remains current until then.

## 6. Completion

M1 is complete only when the first journey passes the foundation gates, old saved loops preserve their appearance and modulation, and a new wave variation is authored entirely as JSON using the supported vocabulary. Numeric checks establish correctness; captured frames and Peter's review establish whether the look is worth performing. Record those levels separately.

Later directions remain visible in their contracts and beads. There is no requirement to build the full vocabulary before releasing M1, and no licence to call the whole programme shipped when M1 lands.
