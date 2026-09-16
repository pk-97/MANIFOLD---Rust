# Scene modifier RT — source and migration inventory

<!-- index: Dated inventory of scene recipes, mesh producers, fusion/load call sites, native RT seams and production export ordering. -->

**Status:** APPROVED supporting inventory · verified 2026-09-16 · static source audit only.

Authority: [design](SCENE_MODIFIER_RT_DESIGN.md), [acceptance](SCENE_MODIFIER_RT_ACCEPTANCE.md). Source base `2a356c5b18966084245bd4d9885cd788f78b78cb`; subsequent concurrent commits at audit completion changed documentation only. Re-run the searches on the implementation branch. Missing/moved anchors require a delta review; extend the existing seams. No LSP service was available, so declaration, blanket-implementation and consumer searches were used. No runtime claims follow from this inventory.

## Native RT seams and planned migration

| Existing seam | Current declaration/consumer | Committed replacement |
|---|---|---|
| `ShadowRayTracer::{build_accel,refit_accel}` | `crates/manifold-gpu/src/metal/raytrace/tracer.rs:143`, implementations `:1238`; helpers `accel.rs` | Design §4 plan/prepare/caller-ordered encode; delete old private-submit path. |
| `MetalShadowRayTracer` / `RtPipelines` | `tracer.rs:459`, `:508` | Retain backend names; prewarm new descriptor-copy/emissive kernels here. |
| `Blas` / `RtAccel` | `accel.rs:49`, `:64`; `encode_blas_build` | Retain descriptors and both scratch kinds; preserve per-object indexing and validation. Existing `refit_scratch` belongs to TLAS. |
| `RtObjectGeometry` / `RtNormalSource` | `accel.rs:207`, `params.rs:335` | Add checked appearance inputs and snapshots; maintain Rust/MSL layouts together. |
| `ShadowRayParams` / `FireflyClampParams` | `params.rs:93`, `:754`; `shadow_rays.msl` | Replace CPU emissive scalars with resident GPU stats buffer; update all kernel/debug callers. |
| CPU emissive maintenance | `emissive.rs:147` (`build_emissive_table`), `:392` (`refit_emissive_table`) | GPU current-geometry preparation; delete cached local CPU triangles and mapped vertex/index reads. |
| `GpuEncoder::raw_cmd_buf` | `crates/manifold-gpu/src/metal/encoder.rs:205` | Existing encoder handoff for ordered descriptor/AS/emission work; no new queue/thread. |
| `rt_accel_maintenance` / flags | `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:2788`, `:5270` | Revision-driven selective updates before current-frame flags; remove settle/readiness gating. |
| Appearance RT rejection | same file `:1977`; raster `shaders/render_scene.wgsl:889` | Replace rejection with tested candidate-hit coverage/brightness (§5.2). |
| `EffectGraphDefExt::into_graph` | `node_graph/persistence.rs:454`, `:468`, `:569` | Append prepared-rules argument; raw definitions pass empty map. |
| `instantiate_def` / `NodeInstantiation` | `node_graph/graph_loader.rs:689`, `:160`, stable-ID resolution `:913` | Append rules argument, install through numeric `id_map` after source/params; handle names are not stable IDs. |
| `from_render_def` / chain splice | `preset_runtime/build.rs:277`; `node_graph/chain_spec.rs:83` | Forward prepared rules through existing shared loader. |

All abbreviated renderer paths below are under `crates/manifold-renderer/src/`; primitive filenames are under `node_graph/primitives/`. GPU abbreviated paths above are under `crates/manifold-gpu/src/metal/raytrace/`, except `shadow_rays.msl` under `metal/`. Line ranges are inspection aids; function/type names are the migration anchors.

## Stock scene-modifier recipes

There are 13 JSON recipes: 9 available and 4 hidden (`jq -s '[.[] | .presetMetadata.available] | {total:length,available:map(select(.==true))|length,hidden:map(select(.==false))|length}' crates/manifold-renderer/assets/scene-modifier-presets/*.json`). Hidden/compat recipes are included below.

| recipe (availability) | geometry/instance nodes | runtime layout result |
|---|---|---|
| ElasticSculpture (true) | `wave_shear_mesh` x2, `mesh_spatial_mask`, `morph_mesh` (JSON lines 521, 537, 607, 675) | triangle stream stays fixed: each shear/morph writes one record per input index; mask is a same-capacity weight stream |
| MaskedPeel (false) | `transform_mesh_patches`, `mesh_spatial_mask`, `morph_mesh` (490, 560, 628) | direct patch/morph are one-per-index, but preparation recognizes patch as a fragment and inserts cut-map/remap nodes; prepared mesh layout/capacity can change |
| OrderedRecon (true) | `ordered_recon_mesh`, `mesh_spatial_mask`, `mesh_stagger_envelope`, `morph_mesh` (849, 909, 977, 1037) | direct recon is one-per-index; fragment preparation inserts directional cut-map/remap, so prepared layout/capacity can change |
| OrderedReconHit (false) | same four nodes (833, 893, 961, 1017) | same cut/remap expansion as OrderedRecon |
| RenderMode (true) | no mesh/instance producer (recipe starts at sceneModifier 187) | no geometry mutation |
| SceneFog (true) | no mesh/instance producer (sceneModifier 49) | no geometry mutation |
| SceneLoop (true) | `scene_array`, `loop_camera`, `camera_switch` (nodes 1196-1225, 1309) | `scene_array` emits a fixed `WINDOW_CAPACITY = 32` instance buffer (scene_array.rs:42, 200-214); camera changes content, not capacity |
| SpatialEchoes (true) | `analytic_echo_instances` (SpatialEchoes.json:68) | instance layout/capacity changes to source capacity × 8; count only changes active content (analytic_echo_instances.rs:21, 135, 148-239) |
| SurfacePeel (true) | `transform_mesh_patches`, `mesh_spatial_mask`, `morph_mesh` (768, 892, 960) | patch is expanded to cut-map/remap layout; morph/weights remain one-per-lineage index |
| SurfacePeelHit (false) | `transform_mesh_patches` only (548) | patch is expanded to cut-map/remap layout |
| SurfaceWaves (true) | `normal_wave_mesh`, `mesh_spatial_mask`, `morph_mesh` (454, 508, 576) | fixed-layout triangle stream |
| VortexFragments (true) | `transform_mesh_patches`, `mesh_spatial_mask`, `morph_mesh` (550, 620, 688) | patch is expanded to cut-map/remap layout |
| WavesEchoes (false) | `normal_wave_mesh`, `mesh_spatial_mask`, `morph_mesh`, `analytic_echo_instances` (627, 681, 749, 921) | mesh stream fixed; instance output is source × 8 capacity |

Direct primitive evidence: `normal_wave_mesh` is MeshVertex→MeshVertex, pointwise, same input capacity and one dispatch/write per input (`crates/manifold-renderer/src/node_graph/primitives/normal_wave_mesh.rs:38-95,97-155`; shader gathers the three current triangle corners and returns one element at `shaders/normal_wave_mesh_body.wgsl:45-101`). `wave_shear_mesh` has the same contract (`wave_shear_mesh.rs:37-96,98-165`; body `wave_shear_mesh_body.wgsl:14-41`). `morph_mesh` is corresponding-by-index and writes one output, but capacity is `min(in, b)` (`morph_mesh.rs:39-110`; body `morph_mesh_body.wgsl:15-78`). `mesh_spatial_mask` and `mesh_stagger_envelope` produce same-capacity `Array<f32>` weights, not geometry (`mesh_spatial_mask.rs:43-120,122-225`; `mesh_stagger_envelope.rs:39-102,104-180`). `transform_mesh_patches` and `ordered_recon_mesh` are direct one-per-index MeshVertex transforms with equal-capacity guards (`transform_mesh_patches.rs:38-113,115-190`; `ordered_recon_mesh.rs:47-119,121-185`), but their stock scene-modifier preparation is the fragment path below.

Fragment expansion is the layout-changing seam. `fragment_cuts.rs:105-110` identifies only `node.ordered_recon_mesh` and `node.transform_mesh_patches` as fragment cutters; `362-420` selects `cut_mesh_bands`/`cut_mesh_cells` and wires the reference; `439-467` emits two `node.remap_mesh_cut` nodes; `470-601` remaps morph/unary/weights and wires the final map to every `node.scene_object` topology input (`line 600`). Cut maps carry barycentric/source-triangle provenance and have extra capacity (`mesh_cut.rs:1-20,93-97,300-399,546-625`); remap output capacity follows the map (`remap_mesh_cut.rs:11-35`, shared execution `mesh_cut_remap.rs:8-81`).

## Existing topology hint and alternate paths

`SceneObject` already has `topology: Option<(Slot,u64)>`, documented as cut-map identity/write generation (`node_graph/scene_object.rs:51-60`). `node.scene_object` declares the optional topology input (`primitives/scene_object.rs:35-69`), reads the input slot and its write generation (`131-132`), and emits it in the Copy object (`152-182`). `MeshTopologyHistory` hashes resource epoch, object order, and each object topology tuple (`scene_object.rs:119-140`); its test confirms ordinary deformation leaves the revision unchanged while a cut-map generation changes history (`148-160`). `render_scene` consumes exactly this hint at `primitives/render_scene.rs:5800-5809` when deciding temporal reset; RT topology is separately checked before consumer flags at `5261-5363`. The RT topology key treats vertex-buffer identity/count/capacity as topology, while mesh write generations are content (`render_scene.rs:1045-1070`).

The current glTF/import path wires skin/morph/raw mesh output into `node.scene_object.vertices` (`gltf_import/object_group.rs:723-743`) and does not wire a topology port there; the single Object output crosses the group boundary (`990-1023`). The legacy photoscan validator likewise follows a stage's `vertices` output to a `node.scene_object` `vertices` terminal (`scene_modifier_legacy_migration/photoscan.rs:510-535`). Thus those older/direct paths have no topology hint until fragment preparation inserts the final cut map into `scene_object.topology` (`fragment_cuts.rs:569-601`). `render_scene` has one Object input path: `bindings.rs:189-203,276-283` and `render_scene.rs:1896-1920`; no alternate legacy per-mesh object ports remain.

## Primitive metadata and custom WGSL

`PrimitiveSpec` declares static identity, IO, params, fusion kind/boundary, WGSL body, input access, and fused output capacity (`node_graph/primitive.rs:55-159`); `description()` exposes only authored identity/IO/params (`261-274`). The `Primitive` trait carries runtime WGSL source, formats/scales, capacities and resource lifetime (`primitive.rs:285-345,537-605`). Its blanket `EffectNode` implementation forwards these (`primitive.rs:643-842`), while `EffectNode` defaults for fusion, WGSL body/access/capacity and array output capacity are at `effect_node.rs:1139-1157,1259-1285,1491-1532`.

`WgslCompute` stores authored/full or fragment source, fusion kind/body, derived IO/bindings, output canvas scales, dispatch count, and `fused_output_capacity`; there is no topology/mesh-layout revision field (`primitives/wgsl_compute.rs:94-118,132-147,176-192`). Its `EffectNode` implementation returns authored source, access/precision views, fusion kind/boundary, fragment body, output scale, and evaluates capacity from the fused-output marker (`wgsl_compute.rs:1902-2058`). Parsing accepts `@input_access`, `@precision_critical`, `@fused_output_capacity`, `@dispatch_count_param`, derived-uniform, reset/pure markers (`wgsl_compute.rs:717-740,472-525`); no geometry topology marker exists. Therefore a WGSL marker cannot currently establish a truthful mesh refit promise.

## Fusion metadata forwarding and consumers

`FusedDef` is the internal precedent for non-serialized fusion sidecars: it contains the fused `EffectGraphDef`, binding `retarget`, node attribution `node_retarget`, and texture `expected_spaces` (`freeze/install.rs:1318-1339`). Fusion stamps expected texture element spaces and output scales (`1984-2005`), forwards texture access/precision markers only for pure texture regions (`2009-2083`), creates fused runtime nodes as `node.wgsl_compute` (`2085-2098`), and rewires control inputs onto port-shadowed fused uniform fields (`2204-2215`). The final sidecar is returned at `2230`.

The lossy generator accessors are exact: `fused_generator_def_for` returns only `Arc<EffectGraphDef>` (`freeze/install.rs:406-418`); `fuse_generator_def_masked` unwraps only `view.def` (`1188-1194`). The richer `FusedGeneratorView` carries `def`, `retarget`, and `node_retarget`, but not `expected_spaces` or any mesh metadata (`420-428`). `fuse_generator_view_masked` consumes `expected_spaces` only for build validation and returns the three fields (`1196-1225`).

Runtime consumers: generator registry passes its canonical/relight-augmented definition into `PresetRuntime::from_def_for_render` (the nearby def-only-fusion comment is stale) (`generators/registry.rs:245-276`); scene-modifier `modifier_runtime` uses the richer view, swaps `view.def`, installs `view.retarget`, and uses `view.node_retarget` for prepared buffer/control state (`preset_runtime/modifier_runtime.rs:107-160`). Per-card effect chain selection chooses a fused `LoadedPresetView` (`preset_runtime/core.rs:832-860`) and carries `view.fused_retarget` into `BoundGraph` (`core.rs:1091-1121`); segment splicing carries only `SegmentView.def`, card bindings, and `retarget` (`freeze/install.rs:641-657`, with runtime installation in `core.rs:700-750`). `LoadedPresetView` itself has only `canonical_def`, bindings, and `fused_retarget` (`loaded_preset_view.rs:54-83`); plain canonical construction leaves the map empty (`140-165`). These view/segment structs are the existing no-serialization runtime-carrier seam; the def-only generator APIs and `SegmentView` currently drop any additional sidecar.

## Re-derivation commands

```sh
# recipe count and direct geometry-node census
jq -s '[.[] | .presetMetadata.available] | {total:length,available:map(select(.==true))|length,hidden:map(select(.==false))|length}' crates/manifold-renderer/assets/scene-modifier-presets/*.json
for f in crates/manifold-renderer/assets/scene-modifier-presets/*.json; do
  printf '%s\n' "$(basename "$f" .json)"
  jq -r '[..|objects|select(has("typeId"))|select(.typeId|test("^node\\.(normal_wave_mesh|wave_shear_mesh|transform_mesh_patches|ordered_recon_mesh|morph_mesh|analytic_echo_instances|mesh_spatial_mask|mesh_stagger_envelope|scene_array)$"))]|map(.typeId)|join(",")' "$f"
done

# all primitive/recipe/fusion anchors
rg -n 'type_id: "node\.(normal_wave_mesh|wave_shear_mesh|transform_mesh_patches|ordered_recon_mesh|morph_mesh|mesh_spatial_mask|mesh_stagger_envelope|analytic_echo_instances|scene_array)"|"typeId": "node\.(normal_wave_mesh|wave_shear_mesh|transform_mesh_patches|ordered_recon_mesh|morph_mesh|mesh_spatial_mask|mesh_stagger_envelope|analytic_echo_instances|scene_array)"' crates/manifold-renderer/src crates/manifold-renderer/assets/scene-modifier-presets
rg -n 'is_fragment|is_mesh_unary|is_weight_source|remap_mesh_cut|topology|MeshTopologyHistory' crates/manifold-renderer/src/node_graph/scene_modifier_expand crates/manifold-renderer/src/node_graph/{scene_object.rs,primitives/scene_object.rs,primitives/render_scene.rs}
rg -n 'FusedGeneratorView|FusedDef|fused_generator_def_for|fuse_generator_def|fused_effect_view_for|SegmentView|LoadedPresetView|expected_spaces|node_retarget|fused_retarget' crates/manifold-renderer/src/node_graph/freeze/install.rs crates/manifold-renderer/src/{generators,preset_runtime,node_graph}/ -g '*.rs'
```

## Repository-wide MeshVertex output census

There are 36 primitive declarations whose `outputs` contain `Array(MeshVertex)`. The declaration and capacity anchors below are sufficient to re-derive the complete list. `Written positions` means the output position depends on the current frame/params while preserving the input triangle indexing; it does not claim a topology change.

| primitive | classification for a mesh-output rule | declaration / capacity anchors |
|---|---|---|
| `node.bend_mesh` | unary deformer: input topology, Written positions | `bend_mesh.rs:52-110` |
| `node.push_mesh` (`displace_mesh.rs`) | unary deformer: input topology, Written positions; height texture is content | `displace_mesh.rs:45-116` |
| `node.extrude_curve` | topology builder: curve length/close/steps determine layout and capacity | `extrude_curve.rs:42-116` |
| `node.facet_normals` | eligible special case: input topology + positions; writes facet normals | `facet_normals.rs:41-78` |
| `node.fold_mesh` | unary deformer: input topology, Written positions | `fold_mesh.rs:36-96` |
| `node.cube_mesh` | source: 36 triangle vertices at default; `max_capacity` may structurally enlarge the padded buffer | `generate_cube_mesh.rs:40-77` |
| `node.grid_mesh` | grid source: `max_capacity` is structural; output is a positions grid, not triangles | `generate_grid_mesh.rs:48-112` |
| `node.glitch_jitter` | unary deformer: input topology, Written positions; stepped time changes content only | `glitch_jitter.rs:41-131` |
| `node.gltf_mesh_source` | external source: source file/mesh selection and `max_capacity` determine geometry/capacity; content readiness is asynchronous | `gltf_mesh_source.rs:117-166,305-324,453-501` |
| `node.gltf_morph_deltas_source` | auxiliary MeshVertex delta source, not a render mesh; external target count/capacity | `gltf_morph_deltas_source.rs:35-82,109-185` |
| `node.gltf_skinned_mesh_source` | external bind-pose mesh source; source file and `max_capacity` determine geometry/capacity | `gltf_skinned_mesh_source.rs:33-97,134-218` |
| `node.melt_mesh` | unary deformer: input topology, Written positions | `melt_mesh.rs:38-109` |
| `node.morph_mesh` | two-mesh morph: topology depends on both `in` and `b`; structural output capacity is `min(in,b)` | `morph_mesh.rs:41-112,135-151` |
| `node.morph_targets_blend` | multi-input morph: base topology follows `in`; delta buffer/target count are structural/content dependencies and must be validated against the base | `morph_targets_blend.rs:59-108,131-148` |
| `node.noise_displace` | unary deformer: input topology, Written positions; time/noise changes content only | `noise_displace.rs:40-130` |
| `node.normal_wave_mesh` | eligible unary deformer: input topology, Written positions | `normal_wave_mesh.rs:40-97,115-135` |
| `node.ordered_recon_mesh` | direct two-input one-per-index transform, but stock fragment preparation changes layout through cut/remap | `ordered_recon_mesh.rs:49-121,145-171`; preparation `fragment_cuts.rs:105-110,362-467` |
| `node.plane_mesh` | source: 6 triangle vertices at default; `max_capacity` may structurally enlarge the padded buffer | `plane_mesh.rs:46-76` |
| `node.platonic_solid_points` | fixed-capacity source: `PLATONIC_MAX_VERTS = 20`; shape changes active records, capacity stays fixed | `polytope_vertices.rs:72-120` |
| `node.push_along_normals` | eligible unary deformer: input topology, Written positions; optional field/weights are content | `push_along_normals.rs:48-113` |
| `node.remap_mesh_cut` | remap: source topology plus map content; output capacity follows map, so map generation/layout is structural | `remap_mesh_cut.rs:13-54`; `mesh_cut_remap.rs:8-81` |
| `node.revolve_curve` | topology builder: profile length and sweep/segment parameters determine grid layout/capacity | `revolve_curve.rs:40-104` |
| `node.ripple_mesh` | unary deformer: input topology, Written positions | `ripple_mesh.rs:41-137` |
| `node.rotate_3d` | unary deformer: input topology, Written positions and rotated normals | `rotate_3d.rs:33-102,114-131` |
| `node.sample_mesh_triangles` | bounded sampler: output is fixed 1536 vertices, selected source faces/content vary with density; not an identity topology stream | `sample_mesh_triangles.rs:27-52,63-73` |
| `node.sample_triangle_grid` | fixed source: 1536 vertices / 512 triangles; density changes active degenerate slots only | `sample_triangle_grid.rs:28-57,61-78` |
| `node.shatter_mesh` | unary triangle gather deformer: input topology, Written positions; reads neighboring corners for face-normal displacement | `shatter_mesh.rs:35-97,111-129` |
| `node.skin_mesh` | eligible unary deformer: input topology, Written positions; joints/weights/matrices are content dependencies | `skin_mesh.rs:58-108,136-158` |
| `node.slice_mesh` | unary deformer: input topology, Written positions (clamped vertices can degenerate but do not re-index) | `slice_mesh.rs:34-94,111-129` |
| `node.taper_mesh` | unary deformer: input topology, Written positions | `taper_mesh.rs:44-123` |
| `node.transform_mesh_patches` | direct two-input one-per-index transform, but stock fragment preparation changes layout through cut/remap | `transform_mesh_patches.rs:40-115,140-166`; preparation `fragment_cuts.rs:105-110,362-467` |
| `node.make_triangles` (`triangulate_grid.rs`) | topology builder: grid input plus `src_cols/src_rows` creates `(cols-1)*(rows-1)*6` records | `triangulate_grid.rs:37-97` |
| `node.tube_from_path` | topology builder: path length and sides determine sweep layout/capacity | `tube_from_path.rs:46-112` |
| `node.twist_mesh` | unary deformer: input topology, Written positions and rotated frame attributes | `twist_mesh.rs:47-117` |
| `node.voxelize_mesh` | unary deformer: input topology, Written positions | `voxelize_mesh.rs:41-94` |
| `node.wave_shear_mesh` | eligible unary deformer: input topology, Written positions | `wave_shear_mesh.rs:39-98,121-145` |

P2 must implement the minimal audited declarations for the current stock scene modifiers: unary wave/shear and normal-wave outputs depend on input topology and write positions; `facet_normals` additionally reads input positions and writes normals; `morph_mesh` depends on both mesh inputs and declares structural `min` capacity; `remap_mesh_cut` depends on source topology plus map content/generation and follows map capacity. Patch/recon nodes cannot be treated as generic unary deformers in prepared scene modifiers because `fragment_cuts` explicitly inserts topology-changing maps. Other producers retain the conservative default unless their listed topology-preserving contract receives the A1/A3 proofs; the required fallback is `Written/Written → rebuild` rule until their source/capacity and attribute contracts are explicitly proven.


## Export callchain and frame timing

Production path:

`ContentThread::run` handles `StartExport` in `crates/manifold-app/src/content_thread.rs:389-405` -> `ContentThread::run_export` in `crates/manifold-app/src/content_export.rs:190-319` -> `run_export_section` at `content_export.rs:321-692` -> `export_one_frame` at `content_export.rs:695-800`.

`run_export_section` computes beat-range duration with `TempoMapConverter` and `total_frames = (duration * fps).round()` (`content_export.rs:345-367`), stops/seeks/plays the engine (`493-500`), creates `ExportSession::new_with_device` when a native device exists (`501-533`), warms non-generator video decoders with up to 120 `engine.tick` calls and re-seeks (`535-571`), then sets `engine.set_export_origin(start_time)` (`573-577`).

Per frame, `export_one_frame` creates `TickContext` with frame 0 `dt=0`, later `dt=1/fps`, `realtime_now=frame_idx*frame_dt`, `frame_count`, and `export_fixed_dt=frame_dt` (`content_export.rs:708-722`); it feeds offline audio, calls `engine.tick` (`731-735`), flushes pending decodes (`737-743`), then calls `ContentPipeline::render_content(..., export_mode=true, ...)` (`745-754`). It flushes background compositor work (`756-760`), chooses compositor output or PQ-encoded output (`762-772`), waits for export GPU completion (`774-779`), then passes the native texture pointer to `ExportSession::encode_frame` (`781-789`).

`Engine::tick_playing` uses `export_origin_seconds + frame_count * export_fixed_dt` in export mode (`crates/manifold-playback/src/engine.rs:903-937`) and then `sync_clips_to_time`; export skips normal drift correction (`1034-1056`). `VideoRenderer::flush_pending_decodes` is bounded to 2 seconds and clears `decode_pending` on timeout/no result to avoid wedging (`crates/manifold-media/src/video_renderer.rs:883-915`).

## Native GPU completion and encode

`ContentPipeline::render_content_native` (`crates/manifold-app/src/content_pipeline.rs:2031+`) commits generator work first (`2214-2267`, commit at `2379-2388`), because the generator writes must be visible to compositor per-layer command buffers. It gathers clip/object/layer descriptors (`2426-2504`), builds `CompositorFrame` (`2512-2542`), and invokes compositor render (`2546-2581`). Export skips IOSurface wait/blit/swap and uses direct `export_output_texture` (`render_content` docs and `2957-2960`). The final native command buffer signals `native_event` with `native_signal_value` and commits (`3228-3252`).

`wait_for_export_complete` polls the shared event for up to 5 seconds while checking `manifold_gpu::gpu_fault::fault_count()` and `submissions_ignored()` (`content_pipeline.rs:3798-3815`, helper `content_export.rs:21-39`). A changed fault count or ignored submission is an error even if the fence appears signalled; timeout is also an error. The completion helper has unit tests for fault-overrides-signalled-fence and timeout semantics (`content_export.rs:42-56`).

For HDR export, `pq_encode_for_export` creates a second encoder, encodes compositor output, signals the same event, updates the signal value, and commits (`content_pipeline.rs:3855-3909`); the caller then waits before taking the texture pointer. `ExportSession::encode_frame` (`crates/manifold-media/src/export_session.rs:191-204`) invokes the native encoder. `MetalEncoderPlugin.m:476-563` binds source/destination textures, dispatches a 16x16 compute copy, commits and waits for command completion (`496-506`), creates rational frame timestamps (`508-513`), applies bounded 10-second writer backpressure (`515-547`), and appends. Finalization waits up to 30 seconds for AVAssetWriter and checks failure (`569-610`); audio mux failures preserve a video-only artifact (`export_session.rs:344-390+`).

Cancellation is polled nonblocking in the frame loop (`content_export.rs:583-628`), calls `session.cancel()`, removes partial output and sidecar video-only files during finalization (`630-692`), and reports `ExportFinished`. GPU frame errors are tagged `gpu=true` and trigger `abort_gpu_work` after cleanup (`774-779`, `630-692`). Preflight errors (zero frames, ffmpeg missing when audio requested, session creation failure) report failure before frame 0 and clean temporary WAVs (`345-367`, `442-465`, `501-533`).


## Frame validity propagation seam

`EffectNodeContext::error` (`node_graph/effect_node.rs:430–455`) only logs; current export completion detects GPU faults/timeouts, not render diagnostics. Design §5.4 adds a separate status value through existing renderer `gpu_encoder::GpuEncoder` wrappers, merged into `ContentPipeline` before export. Audit all wrapper constructors/early returns with:

```sh
rg -n 'GpuEncoder::(new|with_pool)|commit_and_continue|fn render_content|fn export_one_frame|fn render_all' crates/manifold-renderer/src/gpu_encoder.rs crates/manifold-renderer/src/layer_compositor.rs crates/manifold-renderer/src/generator_renderer.rs crates/manifold-app/src/content_pipeline.rs crates/manifold-app/src/content_export.rs
```

No function signature change is required in the compositor/generator render APIs; status travels through their existing mutable wrapper. Wrapper constructors default to Complete; nested wrappers must explicitly merge back. Do not substitute the global hardware fault counter for a per-frame scene error.

## Exact API migration census

Scope: all `crates/**/*.rs`, including integration tests and binaries. These are source-line counts at the audit base, excluding comments and the `msl_block` string matcher; compiler errors remain the completeness gate. `into_graph` includes the trait and impl declarations. The renderer-source-only count is 89 calls; whole-workspace count below includes the app and integration tests. Do not miss those consumers.

| Symbol | Declarations | Call lines |
|---|---:|---:|
| `build_accel` | 3 | 11 |
| `refit_accel` | 3 | 2 |
| `dispatch_shadow_rays` | 2 | 11 |
| `firefly_clamp` | 2 | 1 |
| `instantiate_def` | 1 | 17 |
| `into_graph` | 2 | 94 |
| `from_render_def` | 1 | 1 |
| `splice_def_into_chain` | 1 | 12 |
| `fused_generator_def_for` | 1 | 5 |
| `fused_generator_def_by_id` | 1 | 2 |
| `fuse_generator_def` | 1 | 19 |
| `fuse_generator_def_masked` | 1 | 1 |

Re-derive locations and counts with this exact read-only command from repo root:

```sh
python3 - <<'PYCOUNT'
from pathlib import Path
import re
terms = ['build_accel', 'refit_accel', 'dispatch_shadow_rays', 'firefly_clamp',
         'instantiate_def', 'into_graph', 'from_render_def', 'splice_def_into_chain',
         'fused_generator_def_for', 'fused_generator_def_by_id',
         'fuse_generator_def', 'fuse_generator_def_masked']
for term in terms:
    declarations = calls = 0
    for path in sorted(Path('crates').rglob('*.rs')):
        for number, line in enumerate(path.read_text(errors='replace').splitlines(), 1):
            if line.lstrip().startswith('//') or 'msl_block(implementation,' in line:
                continue
            if not re.search(rf'\b{term}\s*\(', line):
                continue
            if re.match(rf'^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+{term}\s*\(', line):
                declarations += 1
            else:
                calls += 1
            print(f'{path}:{number}: {line.strip()}')
    print(f'{term}: declarations={declarations}, calls={calls}')
PYCOUNT
```

### `build_accel`

* `crates/manifold-gpu/src/metal/raytrace/accel.rs:555`
* `crates/manifold-gpu/src/metal/raytrace/tracer.rs:152,1238,1239`
* `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:3027`
* `crates/manifold-renderer/tests/gpu_proofs/rt_emissive_instancing.rs:224`
* `crates/manifold-renderer/tests/gpu_proofs/rt_instancing.rs:241`
* `crates/manifold-renderer/tests/gpu_proofs/rt_p1_shadow.rs:144,421`
* `crates/manifold-renderer/tests/gpu_proofs/rt_r3_textured_roughness.rs:194`
* `crates/manifold-renderer/tests/gpu_proofs/rt_t2a_alpha_mask.rs:148`
* `crates/manifold-renderer/tests/gpu_proofs/rt_t2c_shadow_temporal_stability.rs:196`
* `crates/manifold-renderer/tests/gpu_proofs/rt_tl_b_transmission.rs:125`
* `crates/manifold-renderer/tests/gpu_proofs/rt_tl_c_sun_tint.rs:127`

### `refit_accel`

* `crates/manifold-gpu/src/metal/raytrace/accel.rs:777`
* `crates/manifold-gpu/src/metal/raytrace/tracer.rs:162,1242,1243`
* `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:3063`

### `dispatch_shadow_rays`

* `crates/manifold-gpu/src/metal/raytrace/tracer.rs:187,1251`
* `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:3480,3515`
* `crates/manifold-renderer/tests/gpu_proofs/rt_emissive_instancing.rs:314`
* `crates/manifold-renderer/tests/gpu_proofs/rt_instancing.rs:353`
* `crates/manifold-renderer/tests/gpu_proofs/rt_p1_shadow.rs:272,537`
* `crates/manifold-renderer/tests/gpu_proofs/rt_r3_textured_roughness.rs:311`
* `crates/manifold-renderer/tests/gpu_proofs/rt_t2a_alpha_mask.rs:258`
* `crates/manifold-renderer/tests/gpu_proofs/rt_t2c_shadow_temporal_stability.rs:288`
* `crates/manifold-renderer/tests/gpu_proofs/rt_tl_b_transmission.rs:262`
* `crates/manifold-renderer/tests/gpu_proofs/rt_tl_c_sun_tint.rs:247`

### `firefly_clamp`

* `crates/manifold-gpu/src/metal/raytrace/tracer.rs:298,1653`
* `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:4953`

### `instantiate_def`

* `crates/manifold-renderer/src/node_graph/chain_spec.rs:107`
* `crates/manifold-renderer/src/node_graph/freeze/proof.rs:4032,4111`
* `crates/manifold-renderer/src/node_graph/freeze/space.rs:52`
* `crates/manifold-renderer/src/node_graph/graph_loader.rs:689,1719,1728,1756,1799,1872,1918,1949,1982,2568,2612`
* `crates/manifold-renderer/src/node_graph/persistence.rs:576`
* `crates/manifold-renderer/src/node_graph/relight.rs:696`
* `crates/manifold-renderer/src/preset_runtime/tests/bound_param_survives_rebuild.rs:109`

### `into_graph`

* `crates/manifold-app/src/ui_snapshot/render.rs:807`
* `crates/manifold-renderer/src/bin/freeze_profile.rs:166,283,354,412,982,1102,1275`
* `crates/manifold-renderer/src/node_graph/bundled_presets.rs:274,302`
* `crates/manifold-renderer/src/node_graph/freeze/install.rs:3093,3168`
* `crates/manifold-renderer/src/node_graph/freeze/proof/audio_visual.rs:20`
* `crates/manifold-renderer/src/node_graph/freeze/proof.rs:395,490,498,510,638,639,762,783,869,912,992,1045,1109,1180,1189,1487,1495,1545,1553,1643,1659,1750,1772,1839,1855,1891,2060,2067,2504,2523,2613,2736,2832,4696,4704,4758,4766,4826,4834,4896,4909`
* `crates/manifold-renderer/src/node_graph/gltf_import/tests.rs:2985,3022,3207,3265,3339,3805`
* `crates/manifold-renderer/src/node_graph/persistence.rs:468,569,907,960,1017,1057,1110,1171,1208,1273,1306,1340,1361,1380,1395,1470,1509`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/tests.rs:214,288,378,398,632`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler.rs:406`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/control_state.rs:269`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/event_state.rs:428`
* `crates/manifold-renderer/src/node_graph/snapshot.rs:542`
* `crates/manifold-renderer/src/node_graph/validate.rs:262,828`
* `crates/manifold-renderer/src/preset_runtime/build.rs:409`
* `crates/manifold-renderer/src/preset_runtime/tests/amount_zero_passthrough.rs:117`
* `crates/manifold-renderer/src/preset_runtime/tests/bool_convert_heal.rs:38`
* `crates/manifold-renderer/src/preset_thumbnail.rs:471`
* `crates/manifold-renderer/tests/fragment_cut_scene.rs:143,210,272`
* `crates/manifold-renderer/tests/gpu_proofs/film_grain_decorrelation.rs:116`

### `from_render_def`

* `crates/manifold-renderer/src/preset_runtime/build.rs:277`
* `crates/manifold-renderer/src/preset_runtime/modifier_runtime.rs:116`

### `splice_def_into_chain`

* `crates/manifold-renderer/src/node_graph/bundled_presets.rs:448,518,615,795,987`
* `crates/manifold-renderer/src/node_graph/chain_spec.rs:83`
* `crates/manifold-renderer/src/node_graph/freeze/proof.rs:1330`
* `crates/manifold-renderer/src/node_graph/relight.rs:686`
* `crates/manifold-renderer/src/preset_runtime/core.rs:615,879,897`
* `crates/manifold-renderer/tests/card_binding_shadow_corpus.rs:40,161`

### `fused_generator_def_for`

* `crates/manifold-renderer/src/bin/freeze_profile.rs:1234`
* `crates/manifold-renderer/src/node_graph/freeze/install.rs:416,1172,3082`
* `crates/manifold-renderer/tests/gpu_proofs/motion_blur_visibility.rs:243,244`

### `fused_generator_def_by_id`

* `crates/manifold-renderer/src/node_graph/freeze/install.rs:1169`
* `crates/manifold-renderer/src/node_graph/freeze/proof.rs:2203,2245`

### `fuse_generator_def`

* `crates/manifold-renderer/src/node_graph/freeze/install.rs:1179,3145`
* `crates/manifold-renderer/src/node_graph/freeze/markers.rs:421`
* `crates/manifold-renderer/src/node_graph/freeze/proof/audio_visual.rs:138,236`
* `crates/manifold-renderer/src/node_graph/freeze/proof.rs:2326,2429,2986,3058,3159,3327,3684,3757,3850,3962,4185,4291,4350,4417,4520`

### `fuse_generator_def_masked`

* `crates/manifold-renderer/src/node_graph/freeze/install.rs:1183,1188`

## Canonical and prepared entrypoint census

Whole-workspace lexical census, using the same script above with the following additional terms. `from_def` includes same-named non-runtime helpers; listed sites must be classified during migration, not all blindly changed. Canonical factories retain signatures; their internal handoff receives the sidecar only after fusion, as design §3.3 specifies.

### `from_def` — 3 declarations, 92 call lines

* `crates/manifold-app/src/content_thread.rs:1554,1578,1629`
* `crates/manifold-app/src/scene_modifier_edit.rs:731`
* `crates/manifold-app/src/scene_modifier_performance.rs:313`
* `crates/manifold-app/src/ui_bridge/inspector.rs:72`
* `crates/manifold-app/src/ui_bridge/project.rs:697,1478,1743,1802,1870`
* `crates/manifold-app/src/ui_bridge/projection/cards.rs:1073`
* `crates/manifold-app/src/ui_snapshot/mod.rs:715`
* `crates/manifold-app/src/viewport_p6_demo.rs:136,162,188`
* `crates/manifold-app/src/window_input.rs:1009,1110`
* `crates/manifold-renderer/src/node_graph/gltf_import/card_precedence_tests.rs:75`
* `crates/manifold-renderer/src/node_graph/gltf_import/tests.rs:64,385,447,746,988,1001,1581,2181,2246,2807,2903,3212,3270,3543,3598,3832`
* `crates/manifold-renderer/src/node_graph/loaded_preset_view.rs:230`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/tests.rs:264`
* `crates/manifold-renderer/src/node_graph/scene_vm.rs:482,1231,1237,1250,1279,1283,1322,1357,1375,1401,1468,1508,1542,1577,1614,1645,1683,1719,1737,1738,1754,1787,1803,1823,1842,1862,1898,1929,1980,2002,2100,2141`
* `crates/manifold-renderer/src/node_graph/snapshot/scene_modifier_tests.rs:16,26`
* `crates/manifold-renderer/src/node_graph/snapshot.rs:530,1054,1274,1326,1393,1428`
* `crates/manifold-renderer/src/preset_runtime/build.rs:250,266,671`
* `crates/manifold-renderer/src/preset_runtime/tests/bool_convert_heal.rs:53`
* `crates/manifold-renderer/src/preset_runtime/tests/generator_runtime.rs:389,667,814`
* `crates/manifold-renderer/tests/card_binding_shadow_corpus.rs:71`
* `crates/manifold-renderer/tests/mosh_presets.rs:182`
* `crates/manifold-renderer/tests/photoscan_modifier_plans.rs:83,106`
* `crates/manifold-renderer/tests/scene_modifier_file_authoring.rs:379`
* `crates/manifold-renderer/tests/scene_modifier_inv_gate.rs:80,120`
* `crates/manifold-renderer/tests/scene_setup_round_trip.rs:109,140,243`
* `crates/manifold-renderer/tests/wave_pilot_presets.rs:213`

### `from_def_for_render` — 1 declarations, 10 call lines

* `crates/manifold-renderer/src/generators/registry.rs:268`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/parameter_guard_tests.rs:126,285`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/tests.rs:153`
* `crates/manifold-renderer/src/preset_runtime/build.rs:271`
* `crates/manifold-renderer/src/preset_runtime/modifier_runtime.rs:51`
* `crates/manifold-renderer/src/preset_runtime/tests/math_view.rs:45,123,414`
* `crates/manifold-renderer/src/preset_runtime/tests/modifier_events.rs:187,400`

### `from_def_for_render_view` — 1 declarations, 6 call lines

* `crates/manifold-renderer/src/preset_runtime/math_view.rs:117,124`
* `crates/manifold-renderer/src/preset_runtime/modifier_runtime.rs:59,61,66`
* `crates/manifold-renderer/src/preset_runtime/tests/math_view.rs:36,112`

### `prepare_scene_modifiers` — 1 declarations, 41 call lines

* `crates/manifold-app/src/scene_modifier_journey/periodic.rs:63`
* `crates/manifold-app/src/scene_modifier_journey.rs:231`
* `crates/manifold-renderer/src/bin/check_presets.rs:254`
* `crates/manifold-renderer/src/node_graph/freeze/fusion_report.rs:78`
* `crates/manifold-renderer/src/node_graph/graph_loader.rs:712`
* `crates/manifold-renderer/src/node_graph/loaded_preset_view.rs:144`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/tests.rs:152,261,281,375,610`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler.rs:135,140`
* `crates/manifold-renderer/src/node_graph/scene_modifier_legacy_migration/sources.rs:621,622`
* `crates/manifold-renderer/src/node_graph/scene_modifier_legacy_migration.rs:38`
* `crates/manifold-renderer/src/preset_runtime/modifier_runtime.rs:79`
* `crates/manifold-renderer/src/preset_runtime/tests/modifier_events.rs:183,396`
* `crates/manifold-renderer/tests/fragment_cut_scene.rs:33,142,206,259,267`
* `crates/manifold-renderer/tests/photoscan_modifier_plans.rs:75,103`
* `crates/manifold-renderer/tests/scene_loop_e2e_import.rs:45`
* `crates/manifold-renderer/tests/scene_loop_roundtrip_gate.rs:66,113,125,127`
* `crates/manifold-renderer/tests/scene_modifier_file_authoring.rs:269`
* `crates/manifold-renderer/tests/scene_modifier_inv_gate.rs:65,87,127`
* `crates/manifold-renderer/tests/scene_modifier_stock.rs:188,244,338,396,406,422,470`

### `prepare_scene_modifier_math_view` — 1 declarations, 5 call lines

* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler/tests.rs:762,840,881,889`
* `crates/manifold-renderer/src/node_graph/scene_modifier_expand/compiler.rs:148`
* `crates/manifold-renderer/src/preset_runtime/modifier_runtime.rs:78`

## Required deletion and preservation checks

After P5/P6, production source must contain no old settle helpers (`rt_deferred_build_decision`, `rt_refit_eligible`), CPU emissive caches (`EmissiveTriangleCpu`, `local_triangles`, `refit_emissive_table`), or def-only executable fusion accessors listed above. Test/contract historical text is exempt only when explicitly identified. Search raw/private command-buffer `commit`/wait calls in `accel.rs` and `emissive.rs`: none may remain. Search mapped vertex/index reads in production emission maintenance: none may remain. `ready` diagnostics may remain but cannot gate same-command-buffer consumers. Never delete unrelated mapped uploads or histories globally to satisfy a text gate.

Preserve existing descriptor composition, cast-mask and material/instance ID proofs. Existing `rt_instancing` includes a static eight-frame test asserting exactly one descriptor dispatch; its count must stay unchanged. The shared GPU proof harness enables RT through real manifest bindings and `assert_rt_dispatched`. Existing `scene_modifier_legacy`, `scene_modifier_stock`, `scene_modifier_inv_gate` and `journey_proof.rs` supply attachment, production export and numerical test infrastructure; extend them rather than inventing a second scene loader.
