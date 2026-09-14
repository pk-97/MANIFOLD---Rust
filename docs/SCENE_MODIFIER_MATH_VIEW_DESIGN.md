# Scene modifier Math View — perform the structure behind the scene

**Status:** IN PROGRESS · 2026-09-13 · Codex. Native Vortex Fragments slice implemented; broader mathematical presentation tracked in BUG-fgfk.
**Prerequisites:** unified scene modifier recipes, parameter surface, native Metal.
**Execution contract:** DESIGN_DOC_STANDARD.md sections 5–6; current AGENTS.md controls validation and delivery.

Peter wants to show “the pure math, the graphs, lines, behaviours” behind a modifier, with a native section on its card and cuts between mathematics and the scene. The generated mockup establishes a visual direction, not the shader's trajectories. Content owns settings; normal parameter commands mutate them; the UI projects snapshots. Presentation controls are ordinary animatable generator macros. Rendering must remain bounded and must not duplicate deformation mathematics.

## 1. Audit — verified 2026-09-13

| Piece | Existing seam | Use |
|---|---|---|
| Authored mathematics | `primitives/transform_mesh_patches.rs`, its WGSL body, `mesh_spatial_mask`, `morph_mesh`; bundled VortexFragments recipe | Reuse the complete authored graph, including edits and bindings. |
| Structural expansion and binding provenance | `node_graph/scene_modifier_expand/compiler.rs` (`prepare_scene_modifiers`), `bindings.rs`, `routes.rs` | Same preparation for sparse and full geometry; keep ordinary per-object route cardinality. |
| Isolated GPU evaluation | `node_graph/viewport_render.rs` (`override_camera_def`), `preset_runtime/build.rs` | Persistent independent runtime built from a derived definition. |
| Native line presentation | `primitives/render_lines.rs`, `project_3d.rs`, `camera.rs` | Reuse camera and capsule/MSAA conventions for a mesh diagram renderer. |
| Card sections and parameter edits | `manifold-ui/src/param_surface.rs` (`RowSpec.section`), `ui_bridge/projection/cards.rs` (`modifier_surfaces`) | Existing collapsible section; no custom slider or addressing system. |
| Saved instance reconciliation | `manifold-core/src/scene_modifier_edit.rs` (`reconcile_scene_modifier_parameters`), `manifold-app/src/project_io.rs` | Add controls without replacing authored stage graphs or old values. |

Paths in the renderer rows are relative to `crates/manifold-renderer/src`.

## 2. Decisions

**D1. Evaluate the authored graph on sparse triangles.** Add a bounded triangle-grid source and a diagram rasterizer. Neither contains modifier mathematics. The sparse build uses the existing compiler, fusion, value routes and native backend. Rejected: a CPU or shader copy of Vortex's formula; it would drift from edits and composed mask/blend stages.

**D2. Keep the live scene graph intact.** A persistent derived runtime substitutes sparse initial vertex endpoints and renders the chosen modifier's output. Original source nodes remain available to binding validation but are dead in the sparse execution plan. Rejected: pinning additional full-mesh intermediates in the live graph; that could defeat fusion and increase the memory pressure that prompted this work.

**D3. Native controls use existing data.** `scene_modifier_math_view` in core enriches eligible Vortex recipes with shared root `node.value` nodes (`__math_view_<suffix>`), local macro IDs (`math_view_<suffix>`) and `section: "Math View"`. Existing host reconciliation supplies stable addresses, undo and modulation. No new serialized fields or UI addressing protocol. Enrichment is additive and idempotent; reserved-ID collisions fail before mutation.

**D4. Paths in this slice are motion trails.** They record actual evaluated sample positions over frame time, with bounded history and reset on discontinuity. They are labelled Motion trails. A parameter sweep is a different visualization and is deferred explicitly; a trail must never claim to be an Orbit sweep.

**D5. Scope is explicit.** This modifier starts the selected vertex stage from its original sparse reference; Within chain includes preceding vertex modifiers. Both use the scene camera and object transforms. Sample source positions subtract the saved source offset so the modifier uses the same calibrated coordinate frame. All selected objects are represented.

**Cost:** sparse evaluations add shader dispatches, bounded geometry/history storage and output textures. They do not remove the main scene's prepared allocations. Scene mode dispatches no diagram work. This is not a fix for the project's existing memory usage. Measure the native slice; no frame-rate claim follows from compilation alone.

## 3. Seams

Core exports `enrich_math_view_controls(&mut EffectGraphDef) -> Result<bool, String>` and `has_math_view_controls(&EffectGraphDef) -> bool`, plus the shared control vocabulary. New controls: Mode (Scene/Math/Overlay), Grid, Fragments, Ghosts, Vectors, Motion trails, Density (2–8), Line Width, Geometry Hue, Path Hue, Scope (This modifier/Within chain).

The compiler exports `MathViewScope { ThisModifier, WithinChain }` and `prepare_scene_modifier_math_view(owner: &EffectGraphDef, registry: &PrimitiveRegistry, modifier_id: &NodeId, scope: MathViewScope) -> Result<PreparedSceneModifierGraph, SceneModifierExpandError>`. It shares normal preparation and preserves its route/binding-source contracts. Canonical data is never modified by rendering.

`node.sample_triangle_grid` outputs at most 1536 `MeshVertex` items. Radius and source offsets preserve the captured scene frame; density changes active geometry without reallocating.

`node.render_mesh_diagram` consumes original reference, incoming and current vertices, Camera and optional Transform. It draws fragments, incoming ghosts, one displacement arrow per triangle centroid, grid/axes and bounded temporal trails. Default density is 3 (27 triangles per object), adjustable from 2 to 8. Fragment axes follow the actual evaluated triangle. Projection uses the scene's final Camera wire, including any lens processor, and the existing scene model-matrix function. The grid is one shared world XZ plane at Y=0, reconstructed from that camera's inverse view-projection. It has no finite square boundary and does not inherit object rotation, scale, translation or sample radius. World-unit lines at 1, 10 and 100 units fade before aliasing. Each graduation fades as a whole using the largest projected footprint, preventing a surviving longitudinal fan after transverse lines become unresolved; distance and grazing-angle fades soften the horizon. The grid remains an orientation reference, not a depth-occluding scene floor. Only the first deterministic object diagram receives the shared Grid control. Every other diagram has an unwired typed Bool(false); toggle resolution honors the scalar wire first, then Bool/Float parameters, preventing duplicate grids and making Grid Off effective across all objects. Pipelines prewarm through the existing startup cache and runtime installation. GPU-written buffers are never read immediately by the CPU.

The parent `PresetRuntime` owns derived runtimes and output targets, forwards value/reshape and lifecycle updates, and composites enabled views into the ordinary generator output. Thus export follows the same render path. Multiple enabled cards follow stack order: Math replaces the accumulated image; Overlay adds its diagram. This rule is deterministic and no card silently wins selection.

## 4. Invariants and enforcement

- Scene preparation/output is unchanged with all modes Scene: structural comparison and GPU pixel parity.
- Sparse graph uses authored stage nodes and bindings: compiler graph tests; live-plan check excludes GLTF mesh/texture loads and `render_scene`.
- Geometry cost is bounded independently of imported vertex counts: array-allocation plan test and sample-source capacity test.
- New controls preserve authored data and values: collision/idempotence tests, host reconciliation and save/reload tests.
- Parameter edits affect the real output: focused native runtime proof with nonempty/Orbit-change assertions, Scene pixel parity, and a two-object all-marks-off → Grid-on → Grid-off sequence in fused and standalone plans. Blank assertions examine RGB because Math output alpha is opaque. The native world-grid proof covers perspective, orthographic and a shallow-camera horizon; preview artifacts were observed by the lead. Camera routing is structurally preserved. The new application ContentCommand/undo/save-reload journey remains unverified under BUG-ywdj after fixture setup failures; runtime proofs do not establish that additional path.
- History clears across discontinuities: renderer history-reset proof and parent lifecycle propagation.
- No new locks or per-frame geometry allocation: fixed sample capacity, reusable history and MSAA resources after first use, focused code review and clippy. No frame-rate claim is made.

## 5. Execution

Three Luna lanes share one isolated slot and disjoint files. Controls owns enrichment, load reconciliation and bundled metadata. Mathematics owns sparse compiler preparation and structural proofs. GPU visuals owns the two primitives and shader/projection proofs. Lead owns design, runtime integration, review, validation and landing. Workers do not commit or land.

Entry: base `36c83a5eb7450646eaac77ab9fa0b908174e8e82`, verified slot-2. Briefs are prepared with `scripts/codex_prepare.py`; exact scopes and required checks travel with each brief. Use `scripts/codex_checks.py` for changed scopes. Final checks: focused clippy/tests, `scripts/gpu_proofs_gate.py`, then `scripts/land_branch.py` and its landing gate. No broad optional sweeps.

Acceptance gesture: change Orbit while Math is selected, observe sample fragments moving; switch to Overlay then Scene at the same controls. Test scope change on a two-modifier fixture; save/reload and move Orbit again. Verification must distinguish computed assertions from observed native output. Corrosion is a held-out project; its source file is never edited by development probes.

## 6. Decided — do not reopen

1. Same authored mathematics, independent sparse geometry.
2. Normal parameter surface and commands.
3. Native GPU output, including exports.
4. Trails are temporal history, not parameter sweeps.
5. No modification of the user's project file or GPU memory cap.

## 7. Deferred

Exact parameter sweeps, equation typography/highlighting, annotated spatial-mask graphs and additional modifier families follow the first native slice and its measured behavior. They are part of the broader mathematical presentation direction, not capabilities of this slice. Ordered Recon is the next recipe to qualify after Vortex demonstrates the shared path.

The qualified Vortex graph is stateless. Derived views start their own temporal state on activation; reproducing the inactive history of user-added simulation or trigger-latch nodes requires separate qualification.
