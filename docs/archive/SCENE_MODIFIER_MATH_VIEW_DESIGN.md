# Scene modifier Math View — perform the structure behind the scene

**Status:** SHIPPED · 2026-09-18 · section 8 (standalone Math View) is the live design and complete on main: combined-chain capture including instance modifiers with per-copy motion trails, non-destructive per-carrier migration preserving each carrier's Connect to Mesh association under split capacity (16 stage-carrying + 16 views), historical partial-set detection. Owed: BUG-fgfk (broader mathematical presentation), BUG-jvn5 (post-view residuals, low). Sections 1–7 remain as the superseded record.
**Prerequisites:** unified scene modifier recipes, parameter surface, native Metal.
**Execution contract:** DESIGN_DOC_STANDARD.md sections 5–6; current AGENTS.md controls validation and delivery.

Peter wants to show “the pure math, the graphs, lines, behaviours” behind a modifier, with a native section on its card and cuts between mathematics and the scene. The generated mockup establishes a visual direction, not the shader's trajectories. Content owns settings; normal parameter commands mutate them; the UI projects snapshots. Presentation controls are ordinary animatable generator macros. Rendering must remain bounded and must not duplicate deformation mathematics.

## 8. Standalone Math View — one modifier owns the view — k3 (lead), 2026-09-17

Sections 1–7 embedded Math View controls in each qualified modifier's recipe. That direction is superseded: Math View is now one standalone scene modifier that visualises the combined deformation of all preceding modifiers in its scene. No target selector, no per-modifier sections.

Verified mechanics behind the design:

- `prepare_scene_modifier_math_view(owner, registry, modifier_id, scope)` (`scene_modifier_expand/compiler.rs:148`) is already parameterised by an arbitrary modifier id; chain state keys (`attachment_key`, compiler.rs:1440) are per scene/object/endpoint, not per modifier, so captures at the view's chain position read the accumulated output of every preceding modifier.
- `MathViewScope::WithinChain` seeds sampled faces at the first modifier of the scene (compiler.rs:281-303) and evaluates each preceding stage through the ordinary compiler, fusion, value routes and backend. The standalone view always uses these chain semantics; the two-variant runtime and the Scope control are dropped.
- A stage-less recipe is schema-valid (`SceneModifierRecipe.stages` defaults empty; `validate_recipe` requires only `enabledParam`, scene_modifier_preset.rs:831); `append_instance` clones non-stage nodes as shared and writes no endpoints, so the view modifier passes the chain through unchanged.
- Diagram semantics for the combined view: `incoming` wires from the reference samples (not the chain output), so ghosts show the undeformed mesh and arrows show the total reference→current displacement (`render_mesh_diagram.wgsl:124-173`).
- Connect to Mesh keeps its patch-family contract (`compiler/math_events.rs:100-117`): supported when exactly one preceding modifier in the scene carries one reference patch transform per selected object with its reference wired from the saved original mesh; a migrated legacy view records its carrier (`legacy_math_view_carrier`) and keeps that carrier's patch association when several patch carriers precede it. Unsupported chains force the control neutral and lock the card row with the reason; nothing partially connects.
- Compatibility: on load, legacy `math_view_*` controls are stripped from carrier recipes and their host binding values are moved onto one appended Math View instance per carrier (reusing a pristine existing view only when it immediately follows the carrier and samples the same objects). Embedded control values without host bindings carry onto the view. Legacy Scope values are dropped; an authored This-modifier scope with preceding modifiers is named in a load notice because the standalone view always renders the combined chain. Migrated views hold their own capacity budget (16) beside the 16 stage-carrying limit, so a valid 16-carrier project stays executable.

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

**D1. Evaluate the authored graph on sampled real faces.** The parent selects at most 512 complete original triangles per object, evenly distributed by face index. Both scopes borrow those samples and evaluate the authored graph through the existing compiler, fusion, value routes and native backend. The sampler and diagram share the same integer face-index mapping. Synthetic triangles cannot establish mesh correspondence and are no longer the Math View source. Rejected: copying Vortex's deformation formula into a separate CPU or shader implementation.

**D2. Share appearance weights with the live scene.** A persistent derived runtime substitutes sampled initial vertex endpoints and renders the chosen modifier's output. A connected event writes one float per original vertex, consumed by the scene object and borrowed by both diagram scopes. It never changes positions. Multiple connected cards multiply their masks; every connected diagram reads the final composed weights for its object. Original source nodes are dead in the derived execution plan. No additional full `MeshVertex` intermediate or deformation split is introduced.

**D3. Native controls use existing data.** `scene_modifier_math_view` in core enriches eligible Vortex recipes with shared root `node.value` nodes (`__math_view_<suffix>`), local macro IDs (`math_view_<suffix>`) and `section: "Math View"`. Existing host reconciliation supplies stable addresses, undo and modulation. No new serialized fields or UI addressing protocol. Enrichment is additive and idempotent; reserved-ID collisions fail before mutation.

**D4. Paths in this slice are motion trails.** They record actual evaluated sample positions over frame time, with bounded history and reset on discontinuity. They are labelled Motion trails. A parameter sweep is a different visualization and is deferred explicitly; a trail must never claim to be an Orbit sweep.

**D5. Scope is explicit.** This modifier starts the selected vertex stage from sampled original faces; Within chain includes preceding vertex modifiers. Both use the scene camera and object transforms. Sample positions retain the original attributes and coordinates; the authored patch transform applies the saved calibration. All selected objects are represented.

**D6. Events affect appearance only.** Each element has independent brightness/opacity, neutral at 1. A manual Pulse plus a triggered beat envelope scales brightness above 1 and opacity below 1; negative Pulse Strength can hide marks. Stroke width, mark dimensions, and positions never receive pulse values. Scan defaults to Highlight, with progressive Reveal available. Amount, Progress, Width, six axis directions, Target, and duration in beats are ordinary automatable controls. Pulse Trigger and Scan Trigger opt into the existing modifier-scoped event stream; events restart on clip edges. Both reuse `BeatEnvelopeState`, the implementation behind `node.envelope_beats`, including baseline suppression, live duration, seek cancellation and completed-event latching.

**D7. Connect to Mesh means reference patch groups.** The mask and Vortex use the same reference-centroid cell quantization, cell size, scene radius and source offset. A cell can group disconnected faces; it is not a topology-derived fracture. One reference patch transform per object is required for this qualified Vortex path. When connected, a mesh-targeted event affects the real patch group and all associated diagram marks together; Grid-only targets remain independent. The parent event clock runs in Scene, Math and Overlay, so changing mode or scope cannot restart it. Transport reset clears it. Graphics-only operation keeps the scene neutral. Raster color uses brightness/alpha coverage; zero weight also discards depth and shadow coverage. Partial shadow coverage remains binary. RT remains unsupported for the existing vertex-modifier path.

**D8. Occlusion is explicit.** Math View's Occlusion selector offers X-ray (0, the compatible default) and Depth (1). Depth draws the current sampled triangles into a shared R32Float surface-depth image across all selected objects, using the same camera, transforms and appearance masks as their diagram marks. Hidden fragments do not occlude. Colour draws test against that shared depth; Overlay also tests against the owning scene's existing depth output. Ghosts, arrows and trails remain translucent and do not act as solid occluders. The grid can be hidden by geometry but never acts as an occluding floor. Depth lines retain endpoint depth and clip at the near plane; X-ray retains its existing projection and composition. Surface edges use a bounded depth tolerance to avoid hiding their own strokes. This is opaque-surface occlusion, not sorted volumetric transparency.

The existing `node.render_mesh_diagram` supplies an optional `depth` output and optional `surface_depth`/`scene_depth` inputs. Depth-only instances accumulate nearest surface depth before any colour diagram executes; they allocate no motion history. This extends the raster boundary without introducing a new primitive or copying modifier math. A depth-only instance with Occlusion at X-ray clears/passes depth without drawing surfaces. The parent retains one scene-depth texture per scene and views borrow its handle; it is never returned to a view's texture pool. Scene depth resolution is requested even with Math View inactive. Additional diagram depth images are canvas-sized R32Float, one per selected object; no additional full mesh arrays are introduced.

**Cost:** each eligible modifier adds one 4-byte weight per original vertex, plus at most 1536 sampled vertices per object. The parent owns these buffers; both views retain the same native handles without copies. Local graphics-only scan masks, deformation intermediates and history stay bounded by the sample capacity. Shared producer readiness gates derived rendering during loading. Pure masks/samplers skip unchanged work. Scene mode dispatches no diagram work, but retains event evaluation and shared resource preparation. Existing scene allocations remain; no frame-rate claim is made.

## 3. Seams

_Sections 1–7 describe the embedded per-modifier design. Superseded by section 8 (standalone Math View): `enrich_math_view_controls`, `has_math_view_controls` and `MathViewScope` no longer exist, and the Scope control is retired. The paragraphs below remain as the record of the original slice._

Core exports `enrich_math_view_controls(&mut EffectGraphDef) -> Result<bool, String>` and `has_math_view_controls(&EffectGraphDef) -> bool`, plus the shared control vocabulary. Controls include Mode (Scene/Math/Overlay), Occlusion (X-ray/Depth), five element toggles and brightness levels, plus an Axes toggle, Pulse and Scan controls described in D6, Connect to Mesh, Density (2–8), Line Width, Geometry Hue, Path Hue, and Scope (This modifier/Within chain). The bundled recipe carries the same metadata; saved recipes receive additive enrichment on load.

The compiler exports `MathViewScope { ThisModifier, WithinChain }` and `prepare_scene_modifier_math_view(owner: &EffectGraphDef, registry: &PrimitiveRegistry, modifier_id: &NodeId, scope: MathViewScope) -> Result<PreparedSceneModifierGraph, SceneModifierExpandError>`. It shares normal preparation and preserves its route/binding-source contracts. Canonical data is never modified by rendering.

`node.sample_mesh_triangles` outputs at most 1536 `MeshVertex` items; inactive triangles are zero. Density changes active face selection without reallocating. `system.mesh_output` keeps parent exports live; `system.mesh_input` receives those retained buffers before allocation and executes without a copy. The older standalone `node.sample_triangle_grid` remains available outside this path.

`node.render_mesh_diagram` consumes original reference, incoming and current vertices, Camera and optional Transform. It draws fragments, incoming ghosts, one displacement arrow per triangle centroid, grid/axes and bounded temporal trails. Default density is 3 (27 triangles per object), adjustable from 2 to 8. Up to three fragment frames follow representative evaluated triangles, selected at the midpoints of equal ranges of the existing face sample. Each frame uses its own centroid, basis and appearance weights. The Axes toggle (on by default) hides both local RGB frames and the world-origin triad without hiding the grid or fragment outlines; the existing Grid/Fragments toggles still gate their respective frames. Marker style and size are unchanged. Face sampling remains index-based, so this is bounded representative coverage rather than spatial clustering. Projection uses the scene's final Camera wire, including any lens processor, and the existing scene model-matrix function. The grid is one shared world XZ plane at Y=0, reconstructed from that camera's inverse view-projection. It has no finite square boundary and does not inherit object rotation, scale, translation or sample radius. World-unit lines at 1, 10 and 100 units fade before aliasing. Each graduation fades as a whole using the largest projected footprint, preventing a surviving longitudinal fan after transverse lines become unresolved; distance and grazing-angle fades soften the horizon. The grid remains an orientation reference, not a depth-occluding scene floor. Only the first deterministic object diagram receives the shared Grid control. Every other diagram has an unwired typed Bool(false); toggle resolution honors the scalar wire first, then Bool/Float parameters, preventing duplicate grids and making Grid Off effective across all objects. Pipelines prewarm through the existing startup cache and runtime installation. GPU-written buffers are never read immediately by the CPU.

The parent `PresetRuntime` owns derived runtimes and output targets, forwards value/reshape and lifecycle updates, and composites enabled views into the ordinary generator output. Thus export follows the same render path. Multiple enabled cards follow stack order: Math replaces the accumulated image; Overlay adds its diagram. This rule is deterministic and no card silently wins selection.

## 4. Invariants and enforcement

- Neutral connected controls preserve scene output; prepared appearance masks add resources even in Scene mode. Non-neutral connected events intentionally affect Scene output.
- Sparse graph uses authored stage nodes and bindings: compiler graph tests; live-plan check excludes GLTF mesh/texture loads and `render_scene`.
- Derived geometry cost is bounded independently of imported vertex counts; the shared appearance mask costs 4 bytes per original vertex per modifier. Allocation tests must prove borrowed buffers keep parent storage and capacity.
- New controls preserve authored data and values: collision/idempotence tests, host reconciliation and save/reload tests.
- Parameter edits affect the real output: native runtime proof with nonempty/Orbit-change assertions, Scene pixel parity, shared buffer identity, connected pulse/reveal, partial face selection, clip retriggers, and mode/scope cuts. The two-object Grid Off sequence covers fused and standalone plans. Blank assertions examine RGB because Math output alpha is opaque. The world-grid proof covers perspective, orthographic and a shallow-camera horizon. The application `math_view_grid_app_control_journey` passes ContentCommand edits, undo/redo and save/reload; its Grid-on and reloaded Grid-off captures were observed by the lead, along with the native two-object Math preview. These are headless native application proofs, not a manual card-click session.
- History clears across discontinuities and before the first draw after Trails is re-enabled: renderer history-reset proof and parent lifecycle propagation.
- Axes polish: the native fixture uses two transformed objects with coincident first faces and verifies frames on three other representative faces per object in X-ray and Depth. Axes Off retains fragment outlines; the runtime proof covers both fused and standalone controls. The lead observed the marker-toggle comparison and integrated Math View captures. The trail proof checks the first re-enabled frame is empty and subsequent frames accumulate fresh history.
- Depth occlusion: native proofs cover nearest-surface accumulation, zero appearance, sloping line depth, Math/Overlay scene-depth selection, both scopes, fused/standalone parity, borrowed texture identity and resize. The lead observed the native Depth/X-ray line comparison and two-object Depth overlay captures. Existing X-ray output is checked before and after switching modes.
- No new locks or per-frame geometry allocation: fixed sample capacity, reusable history and MSAA resources after first use, focused code review and clippy. No frame-rate claim is made.

## 5. Execution

Three Luna lanes share one isolated slot and disjoint files. Controls owns enrichment, load reconciliation and bundled metadata. Mathematics owns sparse compiler preparation and structural proofs. GPU visuals owns the two primitives and shader/projection proofs. Lead owns design, runtime integration, review, validation and landing. Workers do not commit or land.

Entry: base `36c83a5eb7450646eaac77ab9fa0b908174e8e82`, verified slot-2. Briefs are prepared with `scripts/codex_prepare.py`; exact scopes and required checks travel with each brief. Use `scripts/codex_checks.py` for changed scopes. Final checks: focused clippy/tests, `scripts/gpu_proofs_gate.py`, then `scripts/land_branch.py` and its landing gate. No broad optional sweeps.

Acceptance gesture: change Orbit while Math is selected, observe sample fragments moving; switch to Overlay then Scene at the same controls. Test scope change on a two-modifier fixture; save/reload and move Orbit again. Verification must distinguish computed assertions from observed native output. Corrosion is a held-out project; its source file is never edited by development probes.

## 6. Decided — do not reopen

1. Same authored mathematics, real sampled faces and shared reference-group weights.
2. Normal parameter surface and commands.
3. Native GPU output, including exports.
4. Trails are temporal history, not parameter sweeps.
5. No modification of the user's project file or GPU memory cap.

## 7. Deferred

Exact parameter sweeps, equation typography/highlighting, annotated spatial-mask graphs and additional modifier families follow the first native slice and its measured behavior. They are part of the broader mathematical presentation direction, not capabilities of this slice. Ordered Recon is the next recipe to qualify after Vortex demonstrates the shared path.

The qualified Vortex deformation graph is stateless. Derived views start their own geometry history on activation; the parent graphics-event clock continues independently. Reproducing the inactive history of user-added simulation or trigger-latch nodes requires separate qualification.
