# Splat modifiers — mathematical motion for scanned clouds

<!-- index: Modifier integration for the approved Gaussian splat design: reference channels, masks, anisotropic transforms, assembly and qualification. -->

**Status:** PROPOSED · 2026-09-10 · Codex lead · not implemented.
**Prerequisites:** Foundation F8, fields W1–W4 and [GAUSSIAN_SPLATS_DESIGN](GAUSSIAN_SPLATS_DESIGN.md) source/renderer phases. This contract does not build a second splat renderer.
**Execution contract:** [DESIGN_DOC_STANDARD](DESIGN_DOC_STANDARD.md) sections 5–6 and 8. Conformance treatment; the old splat design's source/render/depth anchors must be refreshed before implementation.

Splats share the same coordinate/weight/response composition as other geometry. A scan can ripple, open into a spiral and reconstruct. Its anisotropic shape, orientation, opacity and view-dependent appearance remain part of the representation, not incidental point-cloud attributes.

All splat source/render symbols below are future dependencies until the existing Gaussian design lands.

Shared gates and resource limits: [Validation](SCENE_MODIFIER_VALIDATION_PLAN.md).

## 1. Audit — verified 2026-09-10

`rg -n -i 'gaussian.?splat|gaussian_splat|splatting' crates docs` finds the approved design and its references; no implemented Gaussian splat pipeline was found in crates. The Gaussian Splats header says approved/not built. Its D1 proposes `Channels[position: Vec3F, mask: F32, rotation: Vec4F, scale: Vec3F, opacity: F32, color: Vec4F]`; D5 requires rest-derived displacement; D6 owns projection/sort/draw; D10's placement/depth anchors are dated and need current-code reconciliation.

Existing channels, GPU dispatch and scene depth infrastructure are reusable. Particle scatter is not a Gaussian renderer and is not a compatibility fallback. The current modifier endpoint enum has no Splats target; adding that endpoint is S1's explicit schema extension.

## 2. Decisions

- **D1 — Existing Gaussian contract owns import, sort and rendering.** This contract adds graph attachment and mathematical responses only. Do not duplicate its sort or treat an unbuilt renderer as a minor adapter.
- **D2 — Add a typed Splats endpoint after its representation lands.** Extend `SceneEndpoint` with Splats and recipe schemaVersion=2. Add `SceneStageScope::EachSplatSource` and a default-empty `splat_sources: Vec<SceneNodeRef>` selection field on the instance. A splat preset requires an explicit nonempty source selection in v1; object AllObjects does not silently select cloud sources. The enclosing graph version advances as needed to reject older readers that do not know these enum variants.
- **D3 — Immutable source channels are the reconstruction reference.** Sample mathematical fields at source positions; apply offsets to Current. Progress=1 assembly returns exact source attributes, and ordinary Amount=0 bypass preserves Current.
- **D4 — Move splat shape coherently.** Rigid rotation updates both centre and quaternion. Scale changes anisotropic extent consistently and preserves nonnegative scales. Moving only the centre is an explicit displacement effect, not claimed as a faithful deformation of the scanned surface.
- **D5 — Masks are reusable weights.** Existing proposed splat colour/bounds masks feed shared field responses. Preserve mask/opacity distinction: selection weight does not permanently erase source opacity.
- **D6 — Geometry sorting is renderer-owned every required frame.** Modifier IDs follow source entries, never depth-sort rank. Sorting happens after transformed geometry and is not persisted as canonical identity.

## 3. Typed integration

S1 resolves the selected source's splat-array producer and the consuming render_splats input using stable paths. It inserts stages on that edge before renderer projection/sort. Selection across multiple splat renderers is admitted only if they share the selected scene camera/depth contract; otherwise reject explicitly. This is a recipe endpoint resolver extension, not arbitrary string-based rewiring.

Reuse POSITION extraction, wave/falloff sampling and scalar math from W1–W4. Splat adapters read/write the exact landed Splat channel signature. New operations must declare supported fields and preserve all others, including any higher-order appearance fields added upstream. Type validation refuses unsupported layouts instead of stripping channels.

For rigid transform A=R*s, update centre and shape using `Σ' = A Σ Aᵀ` (equivalently rotate quaternion and scale principal extents for uniform s). A nonlinear warp with local Jacobian J requires `Σ' = J Σ Jᵀ` and a stable factorisation to the renderer representation. **v1 ships centre displacement plus rigid/uniform-scale responses only.** A general covariance warp is deferred; adding it requires finite/positive-semidefinite and reconstruction proofs.

The original splat design proposes DC-only appearance. Modifier integration honours the landed appearance contract rather than revisiting importer scope. If higher spherical-harmonic bands land first, rigid orientation changes require an explicit appearance-frame decision and tests before S2; do not rotate shape and leave view-dependent appearance accidentally inconsistent.

## 4. First presets and controls

**Cloud Wave:** shared wave weights displace centres along a direction. **Spiral Assemble:** reference-relative radial/rotational positions return to the source with staggered Progress. **Breathing Scan:** rigidly scaled splat extents and/or source-relative centre spread, separate graph operations. **Reveal Sphere:** spatial mask modulates opacity from immutable source opacity.

Typical live controls are Amount/Progress, Spread, Period, Radius and Phase. Source import, history duration and capacity are authoring settings. New labels expose only controls active in the chosen preset; no large universal mode switch with dead rows.

## 5. Resource and rendering contract

Measure the actual landed Splat stride; the proposed 64-byte record implies ~64 MB per million entries per full buffer, before sorting, double buffering and renderer intermediates. That arithmetic is a lower-bound estimate, not an approved live count. S1 admission computes total additional buffers with checked arithmetic and validates available declared budget before installation. S3 chooses supported counts from measured evidence; first synthetic test counts are 16,384 and 131,072.

No live count growth or CPU sorting. Transform and sort buffers obey current GPU retirement. Keep valid inactive masks through filtering; no NaN/zero-denominator projection. Depth-composite against mesh geometry must use the same camera and current transformed splat centres/extents. A splat depth-test path does not imply splats cast ray-traced mesh shadows; capability labels must state the actually implemented composition.

## 6. Invariants & enforcement

| ID | Invariant | Planned check |
|---|---|---|
| S1 | Source channels return exactly | `scene_modifier_splat_return` |
| S2 | Rigid rotation updates anisotropic axes | `scene_modifier_splat_orientation` |
| S3 | Selection does not erase original opacity | `scene_modifier_splat_mask` |
| S4 | Sorting follows moved geometry, identity follows source | `scene_modifier_splat_sort_identity` |
| S5 | Mesh occlusion uses matching camera/depth | `scene_modifier_splat_scene_depth` |
| S6 | Unsupported channel/appearance layouts reject | `scene_modifier_splat_signature` |

V3 field tolerance; exact source endpoint bytes. Test one clearly elongated splat rotated 90 degrees, overlapping differently coloured splats changing depth order, and a mesh occluder crossing the cloud. Include a held-out supported splat file after importer development. These are new tests, not existing renderer capabilities.

## 7. Phasing

Read back D1–D6 and the landed source/renderer/channel contracts first. Current Gaussian design phases run under their own ownership/gates; no parallel modifications to render_scene or splat shaders by two leads.

**S1 — endpoint and adapters.** Entry: Gaussian source/render/depth integration shipped and F8/W4 available. Deliver recipe v2 extension, explicit cloud-source selection, POSITION/weight adapters, layout admission and resource accounting. Gate: core/IO `scene_modifier_splat_schema`, renderer `scene_modifier_splat_signature`, roundtrip after selection and focused clippy. Demo: typed source selection/diagnostic flow — L3 target. Forbidden: Particle fallback, duplicate importer, silent channel dropping.

**S2 — mathematical presets.** Entry: S1. Deliver CloudWave, SpiralAssemble and RevealSphere using shared fields; only missing splat response atoms; S1–S3 numerical proofs. Gate: GPU `scene_modifier_splat_return`/`scene_modifier_splat_orientation`/`scene_modifier_splat_mask`, modifier check-presets. Demo/gesture: displace a scan then return to exact source after reload — L3 target. Forbidden: arbitrary covariance warp or undocumented SH handling.

**S3 — sort/depth/budget qualification.** Entry: S2. Deliver S4/S5 tests, held-out file, measured buffer/timing table at declared counts and explicit supported rendering capabilities. Gate: GPU `scene_modifier_splat_sort_identity` and `scene_modifier_splat_scene_depth`; V8 trace and required landing gate. Demo: moving cloud through mesh occluder and camera reversal — L2 artifact plus L3 flow. Forbidden: assuming mesh RT shadow support from depth compositing or promising million-splat performance without measurements.

## 8. Decided — do not reopen

One splat renderer; source-channel preservation; shared fields; explicit new endpoint; orientation/extent-aware rigid operations; source identity before sort; measured counts.

## 9. Deferred

General nonlinear covariance transport, high-order appearance rotation if not provided upstream, recorded splat histories, physical splat simulation, splat-to-mesh conversion and splat participation in mesh RT acceleration. Revival requires a concrete visual use, representation/resource contract and independent renderer proofs. Existing Gaussian importer format/appearance deferrals remain owned by that design.
