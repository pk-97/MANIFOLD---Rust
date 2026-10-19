# Coupling census, rendering side (evidence for the BUG-bfktg design pass)

Base commit: 25af1c932 (branch lane/overnight-coupling-rendering). Epic: BUG-bfktg (rendering-side decomposition). No recommendations here; the design pass makes those.

## Method (commands run, all from the worktree, read-only)

- Sizes: `fd -e rs . --base-directory <crate>/src | while read f; do wc -l ...; done | sort -rn`.
- Production line count: an awk pass that subtracts lines inside `#[cfg(test)]` blocks (attribute to closing `^}` or `mod x;`). Approximate: a `cfg(all(test, feature=...))` or `cfg(any(...))` gate is not subtracted. Where I read the file, the corrected figure is marked `*`.
- Item outlines: `rg -n '^(pub(\([a-z]+\))? )?(fn|struct|enum|impl|trait|static|const|type|mod)'` plus a fn-length pass (next fn start minus this start; fns over 100 lines listed).
- Intra-engine edges (symbol detail via `rg -o crate::<mod>::[A-Za-z_]+ | uniq -c`): `rg -o --with-filename --field-match-separator '|' 'crate::(exec|freeze|load|runtime|primitives|scene|gpu|atomic|mesh|param_binding|snapshot)\b'`, grouped by source top dir, tests and testkit excluded.
- Cross-crate edges: `rg -o 'manifold_node_engine::[a-z_]+(::[a-z_]+)?'` per dependant crate, tests excluded; Cargo.toml `[dependencies]` for crate edges.
- nodes-scene internal edges: `rg -c '\b<module>\b'` per file for gltf_load, decode_cache, scene_vm, render_scene, gltf_import, scene_exposure and others.

## Not covered

- Counts are reference occurrences, not resolved paths: grouped imports (`use x::{a, b}`) and `super::` paths are undercounted (grouped `use manifold_node_engine::{` appears in 11 nodes-scene files and 24 nodes-image files, not counted below). Treat counts as lower bounds and ratios as indicative.
- No type-level resolution (no cargo, no rust-analyzer). "Shared types" below are the symbols seen in paths.
- Inline `#[cfg(test)]` removal is approximate (see Method). Test-only files (`tests.rs`, `*_tests/`) are excluded from tables.
- Not read: manifold-nodes-water, manifold-renderer (dir exists, no Cargo.toml), shader (.wgsl/.msl) coupling, runtime hot-path behaviour.

## 1. Production files over 1,500 lines (six crates)

| File | Total | Prod (approx) | Notes |
|---|---|---|---|
| node-engine/src/exec/execution.rs | 6089 | 3430 | tests at 143, 3033, 5672 |
| node-engine/src/primitives/wgsl_compute.rs | 3626 | 3536 | tests from 2588 are 90 lines |
| node-engine/src/freeze/region.rs | 3512 | 2931* | |
| node-engine/src/freeze/install.rs | 3332 | 2662* | |
| node-engine/src/runtime/core.rs | 2046 | 2046 | no tests |
| node-engine/src/exec/effect_node.rs | 2008 | 2008 | no tests |
| node-engine/src/load/expand/compiler.rs | 1714 | 1714 | no tests |
| node-engine/src/primitive.rs | 2158 | 1632* | |
| node-engine/src/load/graph_loader.rs | 2175 | 1631* | |
| node-engine/src/exec/metal_backend.rs | 1572 | ~1070 | inline test mods at 1080, 1400 (not subtracted by awk) |
| nodes-scene/.../primitives/render_scene.rs | 9726 | 9708 | tests are separate files |
| nodes-scene/.../gltf_load.rs | 6110 | ~4564* | tests 4565-6110 |
| nodes-scene/.../scene_vm.rs | 3295 | 1991* | |
| nodes-scene/.../primitives/gltf_texture_source.rs | 1925 | 1885 | |
| nodes-scene/.../primitives/pbr_material.rs | 2353 | 1877 | |
| compositor/src/layer_compositor.rs | 4381 | ~3846* | tests from 3847 (gpu-proofs gated) |
| compositor/src/generator_renderer.rs | 1866 | 1866 | |
| gpu/src/metal/encoder.rs | 3742 | 3016* | |
| gpu/src/metal/raytrace/tracer.rs | 3248 | 2895* | |
| gpu/src/metal/device.rs | 2403 | 2403 | |
| gpu/src/metal/raytrace/accel.rs | 1946 | 1925 | |

Also over 1,500 total but not production: nodes-image `image_tests/codegen.rs` 2444, nodes-scene `gltf_import/tests.rs` 2805 and `render_scene/gpu_tests.rs` 2256. manifold-nodes: `bin/freeze_profile.rs` 1769 (a bin, 18 profile fns; `profile_scene` 291, `attribute_def` 197). nodes-image has no production file over 1,500 (largest `primitives/temporal.rs` 1152).

## 2. Item outlines by domain

### render_scene.rs (9708 prod lines)

| Lines | Domain | Items |
|---|---|---|
| 69-287 | Capture and proof scaffolding | submods rt_changes, volume_optics, live_draw_args, rt_proof; `blend_snapshot_proof`, `water_perf`, `RtCaptureSlot` + arm/disarm/take_rt_captures |
| 287-545 | Constants | 32 top-level consts (MSAA, light/caster strides, sun cone, AO, IBL sizes, firefly, RT scale) plus uniform structs Prefilter/Irradiance/Lut |
| 545-912 | Output ports + GPU uniform layouts | RENDER_SCENE_OUTPUTS[10], RenderSceneUniforms (848 B), MaterialMapUniform, ShadowUniforms |
| 914-1567 | `struct RenderScene` | 194 fields: 93 `rt_*`, 29 denoise/temporal/firefly/upscale, 10 shaft, 9 IBL, 6 shadow |
| 1568-2115 | Free helpers and per-frame structs | shaft/RT cache keys, ObjectDraw, FramePrelude, FrameRtState, render-mode materials (wireframe/clay) |
| 2116-8778 | `impl RenderScene` (6660 lines, ~95 fns) | see method groups below |
| 8778-9086 | Matrix helpers + `build_uniforms` | `model_matrix`, `mat4_mul`, `mat4_inverse`, `euler_xyz_columns`, `uv_m/uv_t` |
| 9087-9300 | `impl EffectNode for RenderScene` | evaluate, outputs, viewport_pass, rendered_camera/lights, rt_probe_rays |
| 9301-9726 | single-mesh render path, frame driver, submods subsurface, scene_viewport, extent | |

`impl RenderScene` method groups (fn:lines): frame setup `frame_preliminaries` 511, `collect_object_draws` 491, `ensure_gpu_resources` 283; draw `pass2_draw` 833; raster shadows `raster_shadow_prepasses` 169; ray tracing `rt_accel_maintenance` 339, `rt_trace_accumulate` 834, `collect_rt_objects` 113, `author_rt_flags` 124, `ensure_rt_*` (9 fns, ~600 total); post `shafts_pass` 154, `denoise_tail` 186, `temporal_upscale_tail` 110, `firefly_clamp_pass`; IBL `run_ibl_convolution` 276, `ensure_ibl_resources`, `ensure_rt_irradiance` 180; allocation `ensure_*` ~40 fns (MSAA, scratch, samplers, dummies); pipelines `pipeline_for`, `prewarm_pipelines` 177, `rebuild` 234; `new` 206.

### gltf_load.rs (~4564 prod lines)

| Lines | Domain |
|---|---|
| 46-356 | Document/buffer parse, URI validation, WebP decode, node resolution |
| 388-500 | Mat4/Mat3/vec3 math helpers (pub(crate) `mat4_mul`, `mat3_inverse`, `normalize3`, `cross3`, ...) |
| 509-600 | Punctual lights (GltfLightKind, collect_punctual_lights) |
| 629-1030 | Accessor reads, node transforms, `flatten_primitive`, `load_gltf_mesh*` |
| 1032-1222 | Texture snapshot and decode (`parse_texture_snapshot`, `decode_texture_snapshot`) |
| 1224-1830 | Sampler info, `GltfMaterialInfo` (326 lines), spec-gloss, UV transform |
| 1828-1920 | `GltfImportSummary`, camera info |
| 1922-2310 | Animation tracks and `parse_animations`, `resolve_object_animation` |
| 2311-3153 | Skins, morphs, rigid multi-node, `load_gltf_skinned_mesh`, `load_gltf_morph_deltas`, vertex colour accum |
| 3154-4564 | Import summary: `summarize_node`, `resolve_*_for_key`, `gltf_import_summary` (940 lines) |

### engine files

| File | Domain items |
|---|---|
| exec/execution.rs (3430) | `Executor` struct 146-510; `impl Executor` 558-3027 (2470 lines, ~75 fns): profiling/dump setters (~25 small fns), scene-viewport setters, `execute_frame*`, `compute_live_steps` 156, `commit_mesh_revisions` 106, `execute_frame_inner` 272, `run_step` 886, `bind_step_inputs`, `capture_step`; submods node_pairs, array_growth, substep_region |
| freeze/region.rs (2931) | Region types 56-240; `partition_regions` 233; virtual-chain absorption 453-840 (`absorb_virtual_chains` 257); node classification 1031-1713 (`classify_node` 326, `classify_buffer_node` 144); `build_region` 722; port/wire predicates 2435-2896; UnionFind |
| freeze/install.rs (2662) | Fused effect/generator/segment view caches + LruCache 201-700; background segment compile worker + job/result types 780-1100; `compile_segment_view` 1125; mesh-rule substitution 1598-1790; `fuse_canonical_def_masked` 712 (1816-2527); param/effective-value readers 2551-2660 |
| primitives/wgsl_compute.rs (3536) | `WgslCompute` + BindingSlot/UniformLayout types 53-620; WGSL introspection (`introspect` 392, uniform parsing, channel-skip, derived-uniform and fusion markers) 621-1916; `EffectNode::evaluate` 352 at 2159 |
| runtime/core.rs (2046) | `PresetRuntime` 74-200, `EffectSlot` 226-354, `build_into` 894, `run` 242, `output_resource` 323 |
| exec/effect_node.rs (2008) | `EffectNodeContext` 330-730 (400 lines), SimStep/FrameTime/RtQuality, name interning, `trait EffectNode` 792-2008 (~1200 lines) |
| load/expand/compiler.rs (1714) | Scene-modifier expansion: `prepare_scene_modifiers_impl` 236, `finish_math_view` 152, `context` 209, Builder, MathViewCapture |
| load/graph_loader.rs (1631) | `instantiate_def` 536, error enums, resource pre-allocation (1237-1560), retired-param migration, mesh-rule install |
| primitive.rs (1632) | `trait PrimitiveSpec` 55-298, `trait Primitive` 298-773, blanket EffectNode impl 773-1050 |

### Other files

| File | Domain items |
|---|---|
| scene_vm.rs (1991) | Type-id consts 55-106; ~35 view-model structs/enums 108-535; `impl SceneVm` 574-686; `trace_objects` 177, `trace_scene_object` 278, fluid/water-family discovery 834-1040, camera/light/env/atmosphere tracing 1759-1946 |
| layer_compositor.rs (~3846) | Blend resources and PingPong 47-260; owner-key and scope helpers 260-410; LedRoute/LayerOutput 379-460; `LayerCompositor` 463-3043 with `texture` 222, prewarm fns (5 fns, 125-195 each), `generate_layers` 299, `blend_layers_to_led` 337, `fold_groups` 165; `impl Compositor` 3043-3846 with `render` 237. 449 LED-mentioning lines, 15 `led` fns |
| generator_renderer.rs (1866) | `GeneratorRenderer` 60-148, `impl` 153-1459 (`render_all` 334, `acquire_clip` 139, `prewarm_layer` 193), `impl ClipRenderer` 1459+; submods physics_events, state, testkit |
| gpu/device.rs (2403), raytrace/accel.rs (1925), pbr_material.rs (1877), gltf_texture_source.rs (1885), metal_backend.rs (~1070) | device: 30 `create_*` fns incl. six render-pipeline creators of 137-216 lines (710-2185); accel: types 38-440, `plan_accel` 122 / `prepare_accel` 147 / `encode_accel_update` 282 (972-1560), normal sources 1689-1925; pbr_material: one 1176-line param/macro item 46-1221 then Primitive impl; gltf_texture_source: `run` 241, `run_provided_output` 150; metal_backend: `MetalBackend` 55-690, `impl Backend` 691-1053 |
| gpu/metal/encoder.rs (3016) | `GpuEncoder` + bind caches 25-130; `impl GpuEncoder` 338-2998 (72 fns): profiling 403-530, compute 642-1080 (`dispatch_compute_grid` 218, `dispatch_compute_with_accel` 186), fullscreen/instanced/depth-MSAA draws 1081-2000, render pass 1966-2230, copy/clear/blit 2233-2630, events and completion handlers 2662-2850, commit 2855-2998 |
| gpu/metal/raytrace/tracer.rs (2895) | MSL consts and pipeline compile helpers 35-225; `trait ShadowRayTracer` 226-550; trace kinds/passes 561-630; debug ray structs 630-665; `MetalShadowRayTracer` 666; `RtPipelines` 745-1224 (`compile_with_prelude` 426); impl 1224-2017; `impl ShadowRayTracer` 2017-2869 (`dispatch_shadow_rays` 264, `accumulate_irradiance` 229) |

## 3. Engine internal edges (node-engine, `crate::<mod>` references, production)

Top dirs: exec 19473 lines/21 files, root files 18496/28, freeze 13872/24, load 12788/31, runtime 8431/40, primitives 5926/10, scene 5179/22, gpu 1487/10.

| From -> To | Refs | Symbols crossing (count) |
|---|---|---|
| exec -> scene | 150 | DepthRule 67, LiveExtent 7, Transform 6, MeshRevision 6, Camera 6, SceneObject 5, RenderMode 5, MeshSource 5, Material 5, Light 5, FinalOutput 5, Atmosphere 5 |
| runtime -> load | 51 | PreAllocationError 6, SceneModifierExpandError 5, catalog_generation 3, prepare_stage 3, PreparedModifierEvents 3, retire_params 2, pre_allocate_resources 2 |
| freeze -> exec | 42 | EffectNode 17, ParamValues 11, NodeInstanceId 5, intern_name 4, region_body, SimMetricsSink, SimStep, FrameTime, DenseBufferFusion |
| runtime -> freeze | 23 | install::segment_generation 3, fused_effect_generation 3, def_content_key 2, convert_for_fused_field 2, card_prefix 2, def_is_segment_stateless |
| primitives -> exec | 21 | EffectNodeContext 4, Backend 4, ResourceId 3, ParamValues 2 |
| load -> exec | 20 | ArrayScratch 3, intern_name 3, NodeInstanceId 2, ResourceId, MetalBackend |
| exec -> freeze | 17 | classify::InputAccess 6, BoundaryReason 3, FusionKind 2, FusedOutputCapacity 2, region::partition_regions, install::fused_param_value |
| runtime -> gpu, exec -> gpu | 15, 15 | |
| exec -> runtime | 14 | FrameRenderStatus 7, FrameRenderFailure 3, LayerSkinRegistry/LayerSkinPtr 4 |
| load -> scene | 13 | PreparedMeshRules 7, boundary_nodes::Source |

Reverse and cycles: exec <-> freeze (42 / 17), exec <-> runtime (30 / 14), freeze <-> load (12 / 1), runtime -> load -> exec -> runtime chain exists; load <-> scene (13 / 0). `exec/execution.rs` carries scene-viewport and RT-quality accessors on `Executor` (set_scene_viewport, clear_scene_viewport, scene_viewport_texture/status/errors, set_rt_quality, lines 924-981).

Scene-modifier machinery inside the engine: `load/expand/*` 8283 lines (incl. `fragment_cuts.rs` 915, `bindings` 731, `event_state` 631, `buffer_budget` 621, `value_sources` 557, `routes` 474), `runtime/modifier_runtime.rs` 377 and `modifier_preview.rs` 125. Edges out of `load/expand`: exec::resource_allocation 7, scene::mesh_change 2, freeze::install 1, exec::execution_plan 1. Scene-family type-id string literals in engine production code: 20 occurrences outside test files (frames.rs 4, param_tooltips_bulk.rs 3, fragment_cuts.rs 3, validate.rs 2, compiler.rs 2, others 1 each).

## 4. Crate edges and what dependants import from node-engine

Cargo `[dependencies]` (manifold-*): node-engine -> core, foundation, gpu, playback. nodes-scene -> core, foundation, gpu, node-engine. nodes-image -> core, foundation, gpu, native, node-engine. compositor -> core, gpu, node-engine, playback. nodes -> nodes-scene, nodes-water, nodes-image, water-liquid, water-gpu-mpm, water-whitewater, node-engine, core, gpu. gpu -> foundation only. app depends on all of the above.

Engine references per dependant (`manifold_node_engine` occurrences / files): nodes-scene 1407/159, nodes-image 1581/211, compositor 171/12, nodes 121/10, nodes-water 153/30, app 294/54.

| Engine module imported | nodes-scene | nodes-image | compositor | nodes | nodes-water | app |
|---|---|---|---|---|---|---|
| exec::effect_node | 272 | 300 | 23 | 10 | 17 | 9 |
| primitive | 230 | 380 | 0 | 0 | 4 | 0 |
| parameters | 106 | 195 | 0 | 3 | 9 | 1 |
| freeze::codegen | 108 | 82 | 0 | 3 | 1 | 0 |
| freeze::classify | 23 | 36 | 0 | 1 | 0 | 0 |
| freeze::derived_uniform_registry | 20 | 12 | 0 | 1 | 0 | 0 |
| primitives::standalone_pipeline | 60 | 130 | 0 | 0 | 0 | 0 |
| mesh | 90 | 9 | 0 | 0 | 1 | 0 |
| scene::* (all) | 174 | 72 | 19 | 20 | 10 | 11 |
| gpu::* (all) | 55 | 71 | 40 | 11 | 2 | 41 |
| exec::execution_plan / backend / extent | 34 / 40 / 15 | 29 / 16 / 13 | 1 / 0 / 0 | 4 / 0 / 1 | 2 / 1 / 3 | 2 / 0 / 0 |
| load::* (all) | 15 | 0 | 6 | 20 | 28 | 51 |
| runtime::* (all) | 11 | 7 | 53 | 11 | 11 | 52 |
| persistence | 8 | 41 | 0 | 8 | 12 | 15 |
| snapshot / descriptor / validate | 0 | 0 | 4 | 1 | 0 | 63 / 21 / 4 |

## 5. Shared types that would have to cross a crate boundary

Symbols used by node crates, from engine modules that are the node contract (counts from section 4 paths; lower bounds):

| Contract surface | Symbols (use counts) | Used by |
|---|---|---|
| Node trait and context | `EffectNodeContext` (nodes-scene 98, image 172), `ParamValues` (114, 66), `EffectNode` (16, 13), `NodeInstanceId` (12, 11), `FrameTime` (9, 3), `RtQuality` (1), `NodeRequires` (0, 2) | scene, image, compositor, water |
| Primitive trait | `Primitive` (100, 181), `PrimitiveSpec` (22, 15), `PrimitiveDescription` | scene, image |
| Parameters | `ParamDef`, `ParamValue` (3, 15), `ParamType`, `TableData` | scene, image, nodes |
| Mesh data | `MeshVertex` (41, 2), `InstanceTransform` (15, 2), `EdgePair`, `JointMatrix`, `mesh::pipeline` | scene (mainly), image |
| Codegen | `standalone_for_spec` (53, 24), `ENTRY` (41, 25), `VOLUME_WORKGROUP_` 15 (image), `standalone_for_boundary_spec`, `generate_fused` | scene, image |
| Fusion classification | `FusionKind`, `BoundaryReason` (2, 21), `FusedOutputCapacity`/`CapacityExpr`, `InputAccess` | scene, image |
| Scene contract | `Camera`, `Material`, `Light`, `Transform`, `SceneObject`, `Atmosphere`, `RenderMode`, `MeshSource`, `MeshRevision`, `LiveExtent`, `DepthRule` (image 56, scene 17), `FinalOutput`/`boundary_nodes`, `ViewportPass`/`scene_viewport` | engine exec (150 refs), scene, image (DepthRule 56), compositor (scene_viewport 15), water |
| Preset load | `loaded_preset_view`, `preset_loader`, `catalog_source`, `chain_spec`, `load::expand` (`SceneModifierExpandError`, `PreparedModifier*`), `migration` | nodes, water, app, runtime |

In nodes-scene, shared types that cross candidate subsystems (ray tracing, glTF, modifiers, scene core):

| Crossing | Evidence |
|---|---|
| gltf_import -> gltf_load | ~14 direct module refs; gltf_load symbols used outside: `gltf_import_summary` 11, `GltfInterp` 8, `DEFAULT_MATERIAL_MESH_PARAM` 8, `GltfNodeAnimation` 6, `GltfMaterialInfo` 5, `GltfImportSummary` 5, `DEFAULT_MATERIAL_SENTINEL` 5, `GltfWrapMode` 4, `GltfLightKind` 3, `GltfFilterMode` 3 |
| gltf_load module referenced by | gltf_import/* (~70 refs across 12 files, `object_group` 20), gltf_anim_cache 13, gltf_skeleton_pose 8, gltf_mesh_source 5, gltf_skinned_mesh_source 4, gltf_morph_deltas_source 4, edges_from_mesh 2 |
| gltf_load -> render_scene | 11 refs: `model_matrix`, `mat4_*` shared (`render_scene::model_matrix` 5 outside users, `RenderScene` 5, `OBJECT_SAFETY_MAX` 2, `RtCaptureSlot` 2) |
| gltf_import -> render_scene | `scene.rs` 14, `object_group` 11, `merge` 11, `migration` 9, `gltf_load` 11, `testkit` 3: scene-graph construction uses `RENDER_SCENE_TYPE_ID` and RenderScene param names |
| gltf_import -> scene_vm / scene_exposure | scene_vm 5 files, scene_exposure 6 files (`bounds_probe`, `scene`, `object_group`, `upgrade/project`, `scene_scale`) |
| scene_vm -> render_scene | 11 refs (type-id consts; `RENDER_SCENE_TYPE_ID` imported by 3 outside files); `viewport_gizmo` -> scene_vm 9 |
| Duplicate matrix math | `Mat4`, `mat4_mul`, `mat3_inverse`, `normalize3`, `cross3`, `sub3` (gltf_load, pub(crate)) used by gltf_anim_cache, gltf_skeleton_pose, digital_plants_render, nested_cubes_geometry; `mat4_mul`, `mat4_inverse`, `model_matrix` also defined in render_scene.rs (8795-8930) and used by transform_3d, render_mesh_diagram, viewport_gizmo, gltf_mesh_source, gltf_animation_source, subsurface, volume_optics; accel.rs in gpu defines its own `model_matrix_from_transform`, `normal_matrix_from_model` |
| decode_cache | referenced by gltf_mesh_source 2, hdri_source 1, mod.rs; file itself 903 prod lines (disk cache, locking, manifest) with no gltf types |
| scene_modifier_authoring / legacy_migration | referenced only from `node_graph/mod.rs` (1 each); legacy_migration has 5 files, 2,700 lines (photoscan.rs 1452 prod) |
| render_scene <-> raytracing | 61 refs to `manifold_gpu::raytrace::*`: RtObjectGeometry 8, GiMaterial 8, RtNormalSource 5, RtCasterParams 5, ShadowRayParams 4, MAX_RT_CASTERS 4, encode_inline_copy 2, RtMaterialTextures 2, MetalShadowRayTracer 2, FireflyClampParams 2; 93 of 194 struct fields are `rt_*`; `MAX_RASTER_SHADOW_CASTERS` asserted against `MAX_RT_CASTERS` (line 334). RT code in render_scene.rs is inside the same `impl RenderScene` as raster paths. |

## 6. manifold-gpu internal edges (ray tracing subsystem)

| Item | Evidence |
|---|---|
| raytrace dir | tracer.rs 3248, accel.rs 1946, params.rs 1007, emissive.rs 523, trace_timing.rs 272; raytrace.rs 50 |
| raytrace -> rest of metal | gpu_fault 11, device 3 (+`alloc_log_enabled` 2, `alloc_log_backtrace` 1), types 2, `encoder::GpuEncoder` 2 |
| rest of metal -> raytrace | device.rs 4 refs (`rt_pipelines` 1358, `record_accel_allocation` 469), encoder.rs 2 refs (`dispatch_compute_with_accel` 884-1070, `make_acceleration_structure_encoder` 615; 77 accel/rt mentions), types.rs 1, mod.rs 1 |
| External crate users of `manifold_gpu::raytrace` | nodes-scene render_scene.rs (61 refs), nodes registry.rs, 23 gpu_proofs files |
| GPU wrappers in node-engine | `manifold-node-engine/src/gpu/` (1487 lines): gpu_encoder 352, gpu_readback 306, render_target 298, headless_readback 286, render_target_pool 105, uniform_arena 72; compositor imports these (headless_readback 22, gpu_encoder 9, render_target 7) |

## 7. Compositor edges (crate-internal: crate::compositor 7, tonemap 4, layer_compositor 3)
| From | To | Refs |
|---|---|---|
| layer_compositor.rs | manifold_node_engine | 74 (exec::effect_node 14, scene::scene_viewport 11, runtime::layer_skin 6, prewarm_worker_pending_count 5, load::loaded_preset_view 4, sim_metrics 4, preset_context 3, frame_status 3, PreparedRuntimeResize 3, preview_encoding 3) |
| layer_compositor.rs | manifold_gpu / manifold_core / manifold_playback | 36 / 48 / 0 |
| generator_renderer.rs | manifold_node_engine | 33 (effect_node 6, ModifierPreviewContext 4, layer_skin 3, frame_status 3, ModifierPreviewError 2, sim_metrics 2, scene_viewport 1, camera 1) |
| generator_renderer.rs | manifold_gpu / core / playback | 7 / 29 / 3 (ClipRenderer, modulation::TriggerPulse, clear_all_trigger_edges) |

