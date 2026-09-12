# Mathematical fields — waves, choreography and moving boundaries

<!-- index: Shared spatial fields and object/instance responses for travelling waves, interference, formation transitions and selective transformation. -->

**Status:** PROPOSED · 2026-09-12 · Codex lead · not implemented.
**Prerequisites:** Foundation F8 (F1–F8 remains unbuilt; that foundation migrates Elastic Sculpture, Surface Peel, Vortex Fragments and Loop/Fog); the landed photoscan slice is the current mesh-stage precedent. W1–W4 are later field compositions, not prerequisites for the foundation migration.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8. Conformance treatment: the shipped atoms and their semantics are fixed; re-verify channel projections at implementation and extend exact equivalents instead of duplicating them. Foundation F7 is a file-only authorship proof using those atoms, not another travelling-wave implementation.

The performer controls a relationship: how a wave travels, how objects fall into phase, or how a formation opens. The camera can remain still. The graph exposes the mathematics as reusable fields and responses, while the card exposes a small useful set of controls. The current photoscan slice demonstrates this contract with ordinary saved groups: Elastic Sculpture is continuous deformation, while Surface Peel and Vortex Fragments are rigid fixed-cell patch motion. Their controls and names remain part of the migration surface.

Shared gates and resource limits: [Validation V9](SCENE_MODIFIER_VALIDATION_PLAN.md#4-shared-gates). Creative admission follows the [September 12 direction](SCENE_MODIFIER_PROGRAMME.md#2c-september-12-direction-and-creative-admission).

## 1. Audit — original source audit 2026-09-10; amendment 2026-09-12

| Piece | Source | Reuse / boundary |
|---|---|---|
| Scalar/array math | `primitives/array_math.rs:96`, `generate_range.rs:37`, `pack_vec4.rs:28` under `manifold-renderer/src/node_graph` | Reuse for sampling, sin/cos and channel construction; names can differ from filenames |
| Copy arrangements | `primitives/generate_instance_transforms.rs:49`, `cylinder_wrap_field.rs`, `torus_wrap_field.rs` | Existing layouts (`node.arrange_copies` for generate_instance_transforms); semantic keys are not declared outputs here. W1 qualifies fixed-index identity or adds keys. Wrap fields need a fresh fusion audit |
| Copy blend | `primitives/lerp_instance_fields.rs:39` (`node.blend_copies`) | Elementwise position/scale/Euler blend; not quaternion interpolation or ID correspondence |
| Jitter | `primitives/instance_rotation_jitter.rs:47` | Existing hash/instance response; preserve special instance markers |
| Object transform | `primitives/transform_3d.rs:27`; `node_graph/transform.rs` | TRS source; no general world-matrix/pivot field API |
| Mesh spatial mask | `primitives/mesh_ramp.rs:51` | Existing mesh-specific sweep weights; not a generic texture mask |
| Object and copy records | `node_graph/scene_object.rs:25`; `generators/mesh_common.rs:96` | Object carries resources/TRS; copies carry position/uniform scale/Euler plus marker/padding |
| Shipped mesh responses | `primitives/wave_shear_mesh.rs`, `transform_mesh_patches.rs` | Elastic Sculpture's analytic shear and Surface Peel/Vortex Fragments' fixed spatial-cell rigid response; exact bypass and GPU proofs are landed |
| Shipped instance field path | `primitives/copy_positions.rs`, `wave_field_3d.rs`, `displace_copies.rs` | Wave pilot's position → field → displacement composition; preserve its unnormalised direction, phase wrapping, capacity and inactive-hole semantics |
| Photoscan attachment precedent | `node_graph/scene_modifier.rs`, `scene_modifier_mesh.rs`; [landing report](landings/2026-09-11-photoscan-modifiers.md) | Current v2 JSON groups use `current`/`reference` → `vertices`; the future recipe migration reuses these graphs and control IDs |

Re-run `rg -n 'type_id:|purpose:' crates/manifold-renderer/src/node_graph/primitives/` and inspect the nearest complete preset before implementing any operation. Survey channel selection/packing macros as well as files; absence of a guessed filename is not an absence proof. F7 must author a file using the shipped atoms above; it must not add a second travelling-wave implementation. Every genuinely new atom still needs at least two concrete consumers listed below. *(The amendment records the shipped atom reuse and schema wording; the source inventory above remains anchored to the original audit.)*

## 2. Decisions

- **D1:** Stable source coordinates and stable element identities are different inputs. Never hash a transient resident buffer index. Loop's semantic cell key survives window shifts; target object seed derives from its stable path. Hash algorithm/version is frozen with fixture values.
- **D2:** Time is beat-primary. Convert typed `Beats` at the control boundary into bounded phase; no accumulating `dt` for these looks. Existing system inputs remain authority. Same beat/source/seed/controls means same output after seek.
- **D3:** Compute fields separately from applying them. A wave can drive instances and meshes; a plane/sphere mask can gate wave or assembly. No `MathematicalScene` giant primitive. The foundation's F7 proof is file-only composition of existing atoms.
- **D4:** v1 responses are position offset, local-axis rotation and uniform positive scale. Preserve existing object nonuniform scale; do not claim arbitrary matrix composition or introduce shear. World-space instance position is evaluated according to the actual render transform order, which W1 verifies numerically.
- **D5:** Formation endpoints pair by stable identity and count. Do not treat two arrays of equal length as automatically corresponding. Adding objects is a structural edit that can change formation spacing but never surviving object IDs/mappings.
- **D6:** Reference=pre-stack source at current beat; Current=preceding stage output. Mesh rest/bind pose is separate. Mask space is authored in the graph; no hidden feedback from moving geometry into its own reference field.

## 3. Shared data and operation seams

Use existing Channels signatures. Field positions are `Channels[POSITION: Vec3F]`; weights are existing `Array<f32>`; semantic keys are `Array<u32>`. Positions/weights/keys must have equal active counts and declared capacities; incompatible sizes are an install error. Do not use min(countA,countB) as silent correspondence repair. Elementwise outputs inherit input capacity; inactive copy slots preserve their inactive marker. In the recipe schema, mesh work is `SceneStageScope::EachObject` with `SceneEndpoint::Vertices`; `EachMesh` is descriptive shorthand only and must not become a second scope enum.

Proposed operation contracts (⚠ VERIFY-AT-IMPL exact equivalent primitive/channel projections):

| Operation | Typed boundary | Consumers |
|---|---|---|
| Position extraction | MeshVertex or InstanceTransform → POSITION channel, separate explicit adapters | Wave, spatial mask, assembly |
| Wave sample | positions + direction ScalarV3 + wavelength ScalarF32 + phase ScalarF32 → Array<f32> | Travelling wave, interference |
| Spatial falloff | positions + centre ScalarV3 + size ScalarF32 + feather ScalarF32 → Array<f32> | Moving boundary, assembly ordering |
| Copy displacement | InstanceTransform + weights + direction ScalarV3 + amount ScalarF32 → InstanceTransform | Wave elevation, radial release |
| Object displacement | Transform + offset ScalarV3 + amount ScalarF32 → Transform | Object wave, formation transition |
| Copy rotation/scale response | InstanceTransform + weights + amount → InstanceTransform | Choreography, pulse |

Keep plane and sphere falloff separate composable operations if one enum would make controls dead in one mode. Use existing scalar math/packing for object-size collections where appropriate; avoid CPU readback of GPU-produced arrays. Object transforms are CPU value wires today; evaluate their scalar fields from CPU-owned reference TRS, while array responses remain GPU work. They share the same formula and numeric oracle, not necessarily the same dispatch node.

Copy response modifies only its declared attributes. Preserve `rot_pad.w` reflection metadata, scale sign/inactive semantics and any future layout fields. Do not blindly lerp the full 32-byte record to achieve rotation. Rotation response is explicitly an offset to one XYZ Euler component in the existing convention; shortest-path quaternion pose blending is a separate operation if later needed.

## 4. Formula and controls

Let p be the chosen reference position, o the authored origin, d a unit direction, λ a positive wavelength and φ the bounded beat phase in cycles:

`w(p, φ) = sin(2π * (dot(p - o, d) / λ - φ))`.

λ must be finite and positive. UI range and validator enforce this; an invalid file fails, not a silent epsilon replacement. Direction is normalised at preparation/control update; zero direction rejects. Compute φ from existing beat-ramp/musical controls. Integer cycle multipliers produce exact advertised loop periods; arbitrary frequency drift is labelled free motion. Sample φ modulo 1 before f32 GPU conversion to avoid large-timeline precision loss.

Instance elevation is `currentPosition + amount * w * responseDirection`. Zero Amount/Enabled takes the exact input path. Position field and response direction are independent: a wave may travel horizontally while objects move vertically.

**Travelling Wave card (future field family):** Amount, Period (musical), Wavelength, Direction and Phase. Response direction and origin can stay graph defaults for the stock elevation preset. Do not expose a selector whose current mode makes another row inert. It is not an additional F7 implementation; F7 proves file-only reuse of the landed atoms and preserves the existing photoscan controls.

**Interference:** two wave branches with independently authored directions and phase offsets, then weighted sum using array math. Divide by the sum of absolute branch gains when using a normalised response; the zero-gain case explicitly returns zero. Preserve an unnormalised graph variation for deliberate amplitude reinforcement. Card: Amount, Period, Crossing Angle, Phase Difference.

**Choreography:** reference object/copy identity selects a parameter u in [0,1). Sample a ring `r*(cos 2πu, 0, sin 2πu)` or a helix with authored height/winding. Transition from reference anchor a to formation anchor b using `a + ease(progress)*(b-a)`; apply as an offset to Current so earlier stages survive. The graph chooses easing from reusable math. Phase spread offsets each element's progress; endpoints force exact reference/formation arrival independently of delay. Card: Progress, Spread, Radius, Winding (helix preset only).

**Moving boundary:** plane distance or sphere distance → smooth weight → multiply another response's weight. Both sides and feather have explicit scene-unit meanings. No whole-image `masked_mix` masquerading as a geometry mask. Material/light changes can consume the same weights after those endpoints receive their own extension; v1 demonstrates position/scale responses.

## 5. Identity and capacity

ObjectOrdinal is a stable-path-sorted index rebuilt on membership edits; ObjectSeed is stable across those edits. Use ordinal for deliberately evenly spaced formations, seed for persistent randomness. Explain the different result when a member is added. For copies, generator supplies semantic key and active count; Loop key is its corridor cell identity with repeat pattern where intentional. Fixed grid layouts may use index as identity because their source contract makes index stable. Unknown moving-window arrays without a key contract are not admitted to seeded choreography.

M1 supports waves without changing instance count. M2 formation generation owns count at preparation and active count at performance within capacity. Neither an LFO nor an exposed count can allocate a larger buffer live. Reordering noncommuting responses is a required test: rotate-then-translate differs from translate-then-rotate under their declared frame conventions.

### Programme acceptance boundary

| Family | Behaviour and gesture | Contrast and validation |
|---|---|---|
| Elastic Sculpture | Continuous deformation; scrub Phase, Bend and Cross Bend on the imported scan | Distinct from rigid patch motion; landed controls and static raster response are qualified by the photoscan plan/GPU proofs |
| Surface Peel | Rigid fixed-cell patch motion; sweep Lift, Curl and Phase | Distinct from continuous shear; saved name and controls remain stable, and disconnected faces may share a cell |
| Vortex Fragments | Rigid patch motion with orbital/rise response; sweep Orbit, Rise, Separation and Phase | A spatial response variation, not a new universal mode; raster frames show retained texture and visible Phase response |

The landing report records L3 controls and L2 raster visuals. Peter's report that LFOs seemed to work is useful direction, but does not establish project reopen, audio modulation, or sustained performance. Pattern, noise or seed changes are preset variations unless their behaviour changes.

## 6. Invariants & enforcement

| ID | Invariant | New check |
|---|---|---|
| W1 | Formula matches independent reference | `scene_modifier_math_wave` |
| W2 | Identity survives corridor window motion | `scene_modifier_wave_cell_identity` |
| W3 | Source animation survives Current+reference offset | `scene_modifier_reference_current` |
| W4 | Counts/keys do not silently truncate | `scene_modifier_field_cardinality` |
| W5 | Bypass and reconstruction endpoints exact | `scene_modifier_field_endpoints` |
| W6 | Fusion and parameter modulation preserve field | `scene_modifier_fusion_wave` |

Use validation V3/V4/V5 tolerances and V8 resource limits. Dense copies require raster and RT evidence before those modes are claimed supported.

## 7. Phasing

Each phase starts with read-back of D1–D6, the current primitive/preset inventory and the forbidden moves below. Commands run with the leased slot's absolute manifest path; all named tests are new deliverables.

**W1 — coordinate and identity adapters.** Entry: F8; read mesh_common, scene_array, shader transform order and channel macros. Deliver only missing extraction/admission adapters and semantic-key support, plus W2/W4 tests. Gate: `cargo test -p manifold-renderer scene_modifier_field_cardinality`; GPU proof filter `scene_modifier_coordinates`; focused renderer clippy. Demo: numeric readback — L1. Forbidden: slot-as-identity on a scrolling source, changing the instance ABI casually, general matrix rearchitecture.

**W2 — instance field composition.** Entry: W1. Reuse `wave_field_3d`, `copy_positions` and `displace_copies`; add only missing composition or attachment wiring, plus W1/W5/W6 tests. A future Travelling Wave file may be authored here, but F7's foundation proof remains file-only and is not repeated. Gate: GPU filter `scene_modifier_wave`; focused preset validation. Demo/gesture: Amount and phase flow, parked camera plus corridor crossing — L3 target. Forbidden: a second wave atom, same-frame GPU readback or a new universal mode switch.

**W3 — object motion and formations.** Entry: F8 plus the W1/W2 reusable field contracts; F8 does not imply that W1/W2 or this formation phase has shipped. Re-derive Transform consumers before adding scalar offset/rotation operations. Deliver candidate CPU scalar responses, stable-path ordering and exact endpoint tests; Ring/Helix remain candidate variants subject to V9. Gate: CPU `scene_modifier_object_motion`; GPU `scene_modifier_formation`; app `scene_modifier_binding`; focused touched-crate clippy. Demo/gesture: three independent imported objects spread into a ring and return after reload — L3 target. Forbidden: treating one fused scan as separate semantic objects, dropping nonuniform scale.

**W4 — interference and boundaries.** Entry: W1/W2 plus the specific falloff seam; object formations W3 are not a prerequisite. Deliver candidate JSON variants reusing the wave/weight operations; only missing falloff operations may add Rust, and promote a stock look only after V9. Gate: GPU `scene_modifier_interference` with cancellation/reinforcement oracles and `scene_modifier_boundary`; check-presets. Demo/gesture: move boundary through stationary objects while a wave remains active outside/inside per authored mask — L3 target. Forbidden: a named-look shader combining mask, wave and response in one authored atom.

## 8. Decided — do not reopen

Shared fields, typed data responses, reference/current distinction, stable semantic identity, phase-derived motion, exact zero/endpoints and honest capacity admission. High visual density begins with simple meshes; it does not waive render costs.

## 9. Deferred

Vector-flow integration, flocking, object collisions, general shear/world-matrix operators, material/light attachment endpoints and arbitrary user-defined field kernels. Revival requires the relevant simulation/material contract plus a concrete second consumer and compiler proofs. Existing irreducible WGSL escape-hatch policy remains available; it is not a shortcut around reusable atom decomposition.
